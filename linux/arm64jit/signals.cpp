// libarm64jit: guest signal handlers.
//
// __sigaction installs one native handler for every signal the guest handles. When it runs (from Darling's
// _sigtramp, on whatever the thread was doing) it only records the signal in the thread's GuestThread and halts the
// thread's Jit; dynarmic stops at the next block boundary (or when the current svc handler returns to guest code),
// GuestThread::call() then delivers: it builds an arm64 Darwin ucontext (mcontext64: exception, thread and NEON
// state) on the guest stack, blocks sa_mask + the signal, calls the guest handler with (sig, siginfo, ucontext), and
// resumes from the (possibly modified) thread state. Signals on threads that run no guest code are dropped with a log.
// The frame goes on the guest's sigaltstack for SA_ONSTACK handlers (h_sigaltstack keeps it per GuestThread).
// Synchronous signals of guest code (faults in translated code or a thunk's native code: host.cpp on_fault; brk / udf /
// a jump to unmapped memory: jit.cpp ExceptionRaised) go to the guest's handler with the faulting instruction's state;
// without a guest handler, libarm64jit's own crash report runs as before.
#include "a64.h"
#include <atomic>
#include <csignal>
#include <cerrno>
#include <mutex>
#include <array>
#include <ctime>
#include <link.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <ucontext.h>
#include <unistd.h>

namespace a64 {

namespace {
struct GuestAction { std::atomic<u64> handler{0}; std::atomic<uint32_t> mask{0}; std::atomic<int> flags{0}; };
GuestAction g_act[32];
constexpr int kSA_ONSTACK = 0x1, kSA_SIGINFO = 0x40, kSA_RESETHAND = 0x4, kSA_NODEFER = 0x10;  // Darwin values
constexpr int kSIG_BLOCK = 1, kSIG_SETMASK = 3;
constexpr int kSS_ONSTACK = 1, kSS_DISABLE = 4;
constexpr u64 kMINSIGSTKSZ = 32768;
constexpr int kDSIGBUS = 10, kDSIGSEGV = 11;

// Darwin arm64 ucontext + mcontext64.
struct ArmUcontext {
    int32_t uc_onstack; uint32_t uc_sigmask;
    u64 ss_sp, ss_size; int32_t ss_flags, pad0;
    u64 uc_link, uc_mcsize, uc_mcontext;
};
struct ArmMcontext {
    u64 far; uint32_t esr, exception;                // __darwin_arm_exception_state64
    u64 x[29], fp, lr, sp, pc; uint32_t cpsr, pad;   // __darwin_arm_thread_state64
    Dynarmic::A64::Vector v[32]; uint32_t fpsr, fpcr; // __darwin_arm_neon_state64
    u64 pad2;
};
static_assert(sizeof(ArmUcontext) == 56 && offsetof(ArmMcontext, x) == 16 && offsetof(ArmMcontext, v) == 288 && sizeof(ArmMcontext) == 816);
// Darwin siginfo: si_signo, si_errno, si_code, si_pid, si_uid, si_status, si_addr (+24), si_value, si_band, pad
void fill_siginfo(GuestThread &t, int sig, int code, u64 addr) {
    uint8_t *si = t.sig_info[sig];
    memset(si, 0, sizeof t.sig_info[sig]);
    const int32_t w[3] = {sig, 0, code};
    memcpy(si, w, sizeof w);
    memcpy(si + 24, &addr, 8);
}

void native_handler(int sig, void *info, void *) {
    GuestThread *t = GuestThread::current();
    if (!t || !t->jit || sig <= 0 || sig >= 32) {
        static const char m[] = "arm64host: signal on a thread without guest code: dropped\n";
        if (write(2, m, sizeof m - 1) < 0) {}
        return;
    }
    if (info) {
        memcpy(t->sig_info[sig], info, sizeof t->sig_info[sig]);
        // Linux SI_TKILL (raise, pthread_kill, abort): XNU's pthread_kill leaves si_code 0
        int32_t code;
        memcpy(&code, t->sig_info[sig] + 8, 4);
        if (code == -6) { code = 0; memcpy(t->sig_info[sig] + 8, &code, 4); }
    } else memset(t->sig_info[sig], 0, sizeof t->sig_info[sig]);
    t->sig_sync[sig].valid = false;
    t->sig_pending.fetch_or(1u << sig);
    t->jit->HaltExecution(Dynarmic::HaltReason::UserDefined2);
}
}  // namespace

// __sigaction(int, struct __sigaction *{handler, tramp, mask, flags}, struct sigaction *{handler, mask, flags}) and
// __platform_sigaction(int, struct sigaction *, struct sigaction *) (what iOS 27's libc sigaction() calls).
struct NSA { u64 handler, tramp; uint32_t mask; int flags; };
struct OSA { u64 handler; uint32_t mask; int flags; };
template <bool Kernel> static void sigaction_like(GuestThread &t, u32 i) {
    int sig = (int)t.x(0);
    OSA *osa = (OSA *)t.x(2);
    OSA g{};  // the guest's request
    bool set = t.x(1) != 0;
    if (set) {
        if (Kernel) { NSA *n = (NSA *)t.x(1); g = {n->handler, n->mask, n->flags}; }
        else g = *(OSA *)t.x(1);
    }
    u64 nh = g.handler; int nf = g.flags;
    if (set && sig > 0 && sig < 32 && g.handler > 1) { nh = (u64)&native_handler; nf |= kSA_SIGINFO; }  // not DFL/IGN
    u64 old_guest = sig > 0 && sig < 32 ? g_act[sig].handler.load() : 0;
    int old_flags = sig > 0 && sig < 32 ? g_act[sig].flags.load() : 0;
    int r;
    if (Kernel) {
        static u64 tramp = (u64)dsym("_sigtramp");
        NSA n{nh, tramp ? tramp : ((NSA *)t.x(1) ? ((NSA *)t.x(1))->tramp : 0), g.mask, nf};
        r = ((int (*)(int, NSA *, OSA *))g_thunks[i].native)(sig, set ? &n : nullptr, osa);
    } else {
        OSA n{nh, g.mask, nf};
        r = ((int (*)(int, OSA *, OSA *))g_thunks[i].native)(sig, set ? &n : nullptr, osa);
    }
    if (r == 0 && osa && osa->handler == (u64)&native_handler) { osa->handler = old_guest; osa->flags = old_flags; }
    if (r == 0 && set && sig > 0 && sig < 32) { g_act[sig].handler = g.handler; g_act[sig].mask = g.mask; g_act[sig].flags = g.flags; }
    if (r == 0 && set && sig > 0 && sig < 32) retake_host_signal(sig);
    t.set_x(0, (u32)r);
}
// Whether the guest set an action other than SIG_DFL for a (Darwin) signal: host.cpp's SIGUSR1 memlog handler then
// passes the signal on to Darling's handler (which runs the guest's handler, or drops an ignored signal).
bool guest_sig_action_set(int sig) { return sig > 0 && sig < 32 && g_act[sig].handler.load(std::memory_order_relaxed) != 0; }
bool guest_sig_handled(GuestThread &t, int sig) {
    return sig > 0 && sig < 32 && g_act[sig].handler.load() > 1 && !(t.in_handler & (1u << sig));
}
void h_sigaction(GuestThread &t, u32 i) { sigaction_like<true>(t, i); }
void h_platform_sigaction(GuestThread &t, u32 i) { sigaction_like<false>(t, i); }

// sigaltstack(const stack_t *ss, stack_t *oss): the guest's alternate signal stack is the GuestThread's (guest
// handlers run on the guest stack, built by deliver_one). Darling's would replace the thread's Linux sigaltstack, on
// which Darling's and libarm64jit's own handlers run, with the guest's buffer.
void h_sigaltstack(GuestThread &t, u32) {
    struct DStack { u64 sp, size; int32_t flags, pad; };
    const auto *ss = (const DStack *)t.x(0);
    auto *oss = (DStack *)t.x(1);
    const u64 sp = t.sp();
    const bool on = t.alt_size && sp >= t.alt_sp && sp < t.alt_sp + t.alt_size;
    int err = 0;
    DStack in{};
    if (ss) in = *ss;
    const DStack out{t.alt_sp, t.alt_size, t.alt_size ? (on ? kSS_ONSTACK : 0) : kSS_DISABLE, 0};
    if (ss) {
        if (on) err = 1;                                   // EPERM
        else if (in.flags & ~kSS_DISABLE) err = 22;        // EINVAL
        else if (in.flags & kSS_DISABLE) t.alt_sp = t.alt_size = 0;
        else if (in.size < kMINSIGSTKSZ) err = 12;         // ENOMEM
        else { t.alt_sp = in.sp; t.alt_size = in.size; }
    }
    if (!err && oss) *oss = out;
    if (err) { *g_darwin->error() = err; t.set_x(0, (u64)-1); }
    else t.set_x(0, 0);
}

// Runs the guest handler of one signal on this thread (its state at a safe point, or a fault's: sig_sync). Returns
// whether the handler changed the thread state in its ucontext (the Jit state has the ucontext's state either way).
static bool deliver_one(GuestThread &t, int sig) {
    static auto *dsigmask = (int (*)(int, const uint32_t *, uint32_t *))dsym("pthread_sigmask");
    GuestThread::SyncSig sync = t.sig_sync[sig];
    t.sig_sync[sig].valid = false;
    u64 h = g_act[sig].handler.load();
    int flags = g_act[sig].flags.load();
    if (g_trace) logf("signal %d -> guest handler %#lx flags %#x", sig, (unsigned long)h, flags);
    if (h <= 1) return false;  // reset meanwhile
    if (flags & kSA_RESETHAND) g_act[sig].handler = 0;
    auto &j = *t.jit;
    // frame below the red zone, or at the top of the alternate stack (SA_ONSTACK, not already on it)
    const u64 cur = j.GetSP();
    const bool was_on = t.alt_size && cur >= t.alt_sp && cur < t.alt_sp + t.alt_size;
    const bool to_alt = (flags & kSA_ONSTACK) && t.alt_size && !was_on;
    const u64 base = to_alt ? t.alt_sp + t.alt_size : cur - 128;
    u64 sp = (base - sizeof(ArmMcontext) - sizeof(ArmUcontext) - 128) & ~15ull;
    auto *mc = (ArmMcontext *)sp;
    auto *uc = (ArmUcontext *)(sp + sizeof(ArmMcontext));
    auto *si = (uint8_t *)(sp + sizeof(ArmMcontext) + sizeof(ArmUcontext));
    memset(mc, 0, sizeof *mc); memset(uc, 0, sizeof *uc);
    memcpy(si, t.sig_info[sig], 104);
    auto regs = j.GetRegisters();
    for (int k = 0; k < 29; k++) mc->x[k] = regs[k];
    mc->fp = regs[29]; mc->lr = regs[30]; mc->sp = cur; mc->pc = sync.valid ? sync.pc : j.GetPC(); mc->cpsr = j.GetPstate();
    if (sync.valid) { mc->far = sync.far; mc->esr = sync.esr; }
    auto vec = j.GetVectors();
    for (int k = 0; k < 32; k++) mc->v[k] = vec[k];
    mc->fpsr = j.GetFpsr(); mc->fpcr = j.GetFpcr();
    const ArmMcontext before = *mc;
    // the handler's mask, applied by Darling to the Linux thread mask; not SIGSEGV/SIGBUS there (translated code's
    // own faults: fastmem, the stack guard), whose blocking in_handler emulates for guest faults
    const uint32_t own = (flags & kSA_NODEFER) ? 0 : 1u << (sig - 1);
    uint32_t old_mask = 0, block = (g_act[sig].mask.load() | own) & ~((1u << (kDSIGBUS - 1)) | (1u << (kDSIGSEGV - 1)));
    if (dsigmask) dsigmask(kSIG_BLOCK, &block, &old_mask);
    uc->uc_onstack = to_alt || was_on; uc->ss_sp = t.alt_sp; uc->ss_size = t.alt_size;
    uc->ss_flags = t.alt_size ? 0 : kSS_DISABLE;
    uc->uc_sigmask = old_mask; uc->uc_mcsize = sizeof(ArmMcontext); uc->uc_mcontext = (u64)mc;
    const uint32_t old_in = t.in_handler;
    if (own) t.in_handler |= 1u << sig;
    // the handler runs as a nested call below the frame
    j.SetSP(sp);
    u64 args[3] = {(u64)sig, (u64)si, (u64)uc};
    t.call(h, args, (flags & kSA_SIGINFO) ? 3 : 1);
    t.in_handler = old_in;
    if (dsigmask) dsigmask(kSIG_SETMASK, &uc->uc_sigmask, nullptr);
    const bool changed = memcmp(&before, mc, offsetof(ArmMcontext, v)) != 0;
    // resume from the ucontext (handlers may change it)
    for (int k = 0; k < 29; k++) regs[k] = mc->x[k];
    regs[29] = mc->fp; regs[30] = mc->lr;
    j.SetRegisters(regs); j.SetSP(mc->sp); j.SetPC(mc->pc); j.SetPstate(mc->cpsr);
    for (int k = 0; k < 32; k++) vec[k] = mc->v[k];
    j.SetVectors(vec); j.SetFpsr(mc->fpsr); j.SetFpcr(mc->fpcr);
    return changed;
}

// Called by GuestThread::call() when Run() returned with signals pending; the guest state is at a block boundary.
void deliver_signals(GuestThread &t) {
    while (uint32_t pend = t.sig_pending.load()) {
        int sig = __builtin_ctz(pend);
        t.sig_pending.fetch_and(~(1u << sig));
        deliver_one(t, sig);
    }
}

// ---- synchronous signals: faults, brk / udf, abort ------------------------------------------------------------------
// XNU runs the handler of a fault on the faulting thread, with the faulting instruction's state in the ucontext (pc:
// that instruction; far: the address) and the sigaltstack (SA_ONSTACK) honored; in-app crash reporters (PLCrashReporter,
// KSCrash, Sentry, Crashlytics) rely on it, and on returning: the instruction runs again (dies by the default action
// after the reporter reset it and re-raised, or continues if the handler fixed the cause or changed the context).
bool guest_sync_signal(GuestThread &t, int sig, int code, u64 addr, u64 pc, u64 far, uint32_t esr) {
    if (!guest_sig_handled(t, sig)) return false;
    fill_siginfo(t, sig, code, addr);
    t.sig_sync[sig] = {true, pc, far, esr};
    t.sig_pending.fetch_or(1u << sig);
    t.jit->HaltExecution(Dynarmic::HaltReason::UserDefined2);
    return true;
}

extern "C" void a64_call_on_stack(void (*fn)(void *), void *arg, void *stack_top);  // callconv.S
namespace {
struct FaultCall { GuestThread *t; int sig; bool changed, switched; };
void fault_call(void *p) {
    auto *f = (FaultCall *)p;
    // Off the Linux sigaltstack now (Darling's, which this SA_ONSTACK handler runs on): a signal during the guest's
    // handler (suspension, a timer) would build its frame over this handler's at the top of that stack. Off for the
    // handler; such a signal goes on the current stack instead.
    stack_t old{}, off{};
    off.ss_flags = SS_DISABLE;
    const bool alt = f->switched && sigaltstack(nullptr, &old) == 0 && !(old.ss_flags & SS_DISABLE) && sigaltstack(&off, nullptr) == 0;
    f->changed = deliver_one(*f->t, f->sig);
    if (alt) { old.ss_flags = 0; sigaltstack(&old, nullptr); }
}
}  // namespace
int guest_fault_now(GuestThread &t, int sig, int code, u64 addr, u64 pc, uint32_t esr, void *host_ctx, bool in_jit, bool need_alt) {
    if (!guest_sig_handled(t, sig)) return 0;
    // a stack overflow: only a handler on the alternate stack can run (XNU kills the process otherwise)
    if (need_alt && (!(g_act[sig].flags.load() & kSA_ONSTACK) || !t.alt_size)) return 0;
    auto *uc = (ucontext_t *)host_ctx;
    if (in_jit) {
        const u64 h[4] = {(u64)uc->uc_mcontext.gregs[REG_R12], (u64)uc->uc_mcontext.gregs[REG_R13],
                          (u64)uc->uc_mcontext.gregs[REG_R14], (u64)uc->uc_mcontext.gregs[REG_RBP]};
        t.jit->LoadPinnedFromHost(h);
    }
    fill_siginfo(t, sig, code, addr);
    t.sig_sync[sig] = {true, pc, addr, esr};
    // the guest's handler (and the native calls it makes) runs on the interrupted thread's stack below its red zone,
    // not on the 48 KB Linux sigaltstack this handler is on (when it is: else right here)
    const u64 irsp = (u64)uc->uc_mcontext.gregs[REG_RSP], here = (u64)__builtin_frame_address(0);
    FaultCall f{&t, sig, false, !(here < irsp && irsp - here < (1u << 20))};
    if (f.switched) a64_call_on_stack(fault_call, &f, (void *)((irsp - 256) & ~15ull));
    else fault_call(&f);
    return f.changed ? 2 : 1;
}

// The guest instruction whose access faulted at addr, in the block starting at block_pc: the first load / store whose
// address (from the Jit state at the fault: the block's register writes before it are there) covers addr. Translated
// code keeps no host->guest map per instruction.
namespace {
bool read_insn(u64 pc, uint32_t *w) {
    iovec l{w, 4}, r{(void *)pc, 4};
    return process_vm_readv(getpid(), &l, 1, &r, 1, 0) == 4;
}
int64_t sext(u64 v, int bits) { return (int64_t)(v << (64 - bits)) >> (64 - bits); }
// effective address, size, 1 read / 2 write / 3 either; false: not a (faulting) memory access
bool mem_access(GuestThread &t, uint32_t in, u64 pc, u64 *ea, u64 *size, int *kind) {
    auto reg = [&](int n, bool sp) -> u64 { return n == 31 ? (sp ? t.sp() : 0) : t.x(n); };
    const int rn = (in >> 5) & 31, sz = in >> 30, v = (in >> 26) & 1, opc = (in >> 22) & 3;
    int scale = sz;
    if (v && (opc & 2) && sz == 0) scale = 4;
    const int ldst = v ? ((opc & 1) ? 1 : 2) : (opc == 0 ? 2 : 1);
    if ((in & 0x3B000000) == 0x39000000) {  // unsigned immediate
        if (!v && sz == 3 && opc == 2) return false;  // PRFM
        *ea = reg(rn, true) + ((u64)((in >> 10) & 0xfff) << scale); *size = 1ull << scale; *kind = ldst; return true;
    }
    if ((in & 0x3B200000) == 0x38000000) {  // unscaled, pre / post index, unprivileged
        const int mode = (in >> 10) & 3;
        if (!v && sz == 3 && opc == 2) return false;  // PRFUM
        *ea = reg(rn, true) + (mode == 1 ? 0 : sext((in >> 12) & 0x1ff, 9)); *size = 1ull << scale; *kind = ldst; return true;
    }
    if ((in & 0x3B200C00) == 0x38200800) {  // register offset
        if (!v && sz == 3 && opc == 2) return false;
        const u64 m = reg((in >> 16) & 31, false);
        const int option = (in >> 13) & 7;
        const u64 e = option == 2 ? (u64)(uint32_t)m : option == 6 ? (u64)(int64_t)(int32_t)m : m;
        *ea = reg(rn, true) + (e << (((in >> 12) & 1) ? scale : 0)); *size = 1ull << scale; *kind = ldst; return true;
    }
    if ((in & 0x3B000000) == 0x18000000) {  // literal
        if (!v && sz == 3) return false;  // PRFM literal
        *ea = pc + (sext((in >> 5) & 0x7ffff, 19) << 2);
        *size = v ? 4ull << sz : (sz == 1 ? 8 : 4); *kind = 1; return true;
    }
    if ((in & 0x3A000000) == 0x28000000) {  // pair
        const int mode = (in >> 23) & 3;
        const int ps = v ? 2 + sz : (sz == 2 ? 3 : 2);
        *ea = reg(rn, true) + (mode == 1 ? 0 : (u64)(sext((in >> 15) & 0x7f, 7) << ps));
        *size = 2ull << ps; *kind = ((in >> 22) & 1) ? 1 : 2; return true;
    }
    if ((in & 0x3F000000) == 0x08000000) {  // exclusive, ordered, compare-and-swap
        *ea = reg(rn, true); *size = (1ull << sz) * (((in >> 21) & 1) ? 2 : 1); *kind = 3; return true;
    }
    if ((in & 0x3B200C00) == 0x38200000) {  // atomic memory operations
        *ea = reg(rn, true); *size = 1ull << sz; *kind = 3; return true;
    }
    if ((in & 0x3F200C00) == 0x19000000) {  // LDAPUR / STLUR
        *ea = reg(rn, true) + sext((in >> 12) & 0x1ff, 9); *size = 1ull << sz; *kind = 3; return true;
    }
    if ((in & 0xBFBF0000) == 0x0C000000 || (in & 0xBFA00000) == 0x0C800000) {  // SIMD multiple structures
        const int op = (in >> 12) & 15;
        const int n = op == 0 || op == 2 ? 4 : op == 4 || op == 6 ? 3 : op == 8 || op == 10 ? 2 : 1;
        *ea = reg(rn, true); *size = (u64)n * (((in >> 30) & 1) ? 16 : 8); *kind = ((in >> 22) & 1) ? 1 : 2; return true;
    }
    if ((in & 0xBF9F0000) == 0x0D000000 || (in & 0xBF800000) == 0x0D800000) {  // SIMD single structure
        *ea = reg(rn, true); *size = 64; *kind = ((in >> 22) & 1) ? 1 : 2; return true;
    }
    return false;
}
bool ends_block(uint32_t in) {
    return (in & 0x7C000000) == 0x14000000 || (in & 0xFF000010) == 0x54000000 || (in & 0x7E000000) == 0x34000000 ||
           (in & 0x7E000000) == 0x36000000 || (in & 0xFE000000) == 0xD6000000 || (in & 0xFF000000) == 0xD4000000;
}
}  // namespace
u64 guest_fault_pc(GuestThread &t, u64 block_pc, u64 addr, bool write) {
    constexpr u64 kLow = 0x00ffffffffffffffull;  // top-byte-ignore
    const u64 a = addr & kLow;
    u64 any = 0;
    for (u64 pc = block_pc; pc < block_pc + 4 * 1024; pc += 4) {
        uint32_t in;
        if (!read_insn(pc, &in)) break;
        u64 ea, size; int kind;
        if (mem_access(t, in, pc, &ea, &size, &kind)) {
            ea &= kLow;
            if (a >= ea && a - ea < size) {
                if (kind & (write ? 2 : 1)) return pc;
                if (!any) any = pc;
            }
        }
        if (ends_block(in)) break;
    }
    return any ? any : block_pc;
}

// ---- Darling's thread suspension at safe points ---------------------------------------------------------------------
// Darling implements thread_suspend / task_suspend by sending SIGRTMIN (SIGNAL_SIGEXC_SUSPEND) to the Linux thread; its
// handler parks the thread in darlingserver until thread_resume. Delivered wherever the thread is, that stopped
// threads inside the translator, dispatcher or a libarm64jit callback holding the shared emitter mutex or a glibc malloc
// lock: JavaScriptCore's GC (thread_suspend + thread_get_state + conservative scan + thread_resume) then blocked on the
// same lock and never resumed Safari's main thread (Safari black screen). XNU stops a thread in user code, which here
// is guest code or Darling's native libraries, never our runtime. So the signal is wrapped:
//  - interrupted in Darling's native code (a thunk's native callee, or a thread outside guest code), i.e. outside
//    every ELF object (libarm64jit+dynarmic, glibc, libstdc++) and outside guest code: Darling's handler parks it now
//    (a thread blocked in mach_msg, the usual GC target, must be suspendable);
//  - anywhere else (guest code, the JIT, an ELF callback): pending + halt the Jit; it parks at the next safe point:
//    Run() returning to call() (a block boundary), a thunk's entry or its return to guest code. A 1 ms timer re-sends
//    the signal so a thread that went from ELF code into a blocking native call is parked there.
// At every park the guest registers are in the Jit (block boundary or svc), so thread_get_state (ARM flavors) for a
// guest thread is answered from them (Darling's server only has the x86 state).
namespace {
constexpr int kSuspSig = 34;  // LINUX_SIGRTMIN as Darling computes it (glibc SIGRTMIN)
void (*g_darling_susp)(int, siginfo_t *, void *) = nullptr;
std::mutex g_susp_install;
std::atomic<u64> g_n_now{0}, g_n_defer{0}, g_n_safe{0}, g_n_state{0};
struct Range { u64 lo, hi; };
Range g_elf[128];
std::atomic<int> g_nelf{0};
int elf_cb(dl_phdr_info *info, size_t, void *) {
    for (int k = 0; k < info->dlpi_phnum; k++) {
        const ElfW(Phdr) &ph = info->dlpi_phdr[k];
        if (ph.p_type != PT_LOAD || !(ph.p_flags & PF_X)) continue;
        int n = g_nelf.load();
        if (n < 128) { g_elf[n] = {info->dlpi_addr + ph.p_vaddr, info->dlpi_addr + ph.p_vaddr + ph.p_memsz}; g_nelf = n + 1; }
    }
    return 0;
}
bool in_elf(u64 rip) {
    for (int k = 0, n = g_nelf.load(std::memory_order_acquire); k < n; k++) if (rip >= g_elf[k].lo && rip < g_elf[k].hi) return true;
    return false;
}
std::once_flag g_elf_once;
// guest threads by mach port (the suspender names its target by port)
constexpr int kReg = 8192;
std::atomic<GuestThread *> g_reg_t[kReg];
std::atomic<uint32_t> g_reg_port[kReg];
uint32_t port_of(GuestThread &t) { return t.native_tsd ? (uint32_t)t.native_tsd[3] : 0; }  // _PTHREAD_TSD_SLOT_MACH_THREAD_SELF
void reg_port(GuestThread &t) {
    const uint32_t p = port_of(t);
    if (!p) return;
    for (int k = 0; k < kReg; k++) if (g_reg_t[k].load(std::memory_order_relaxed) == &t) { g_reg_port[k].store(p); return; }
}
GuestThread *find_port(uint32_t p) {
    if (!p) return nullptr;
    for (int k = 0; k < kReg; k++) if (g_reg_port[k].load(std::memory_order_acquire) == p) {
        GuestThread *t = g_reg_t[k].load();
        if (t && port_of(*t) == p) return t;
    }
    return nullptr;
}
void arm_timer(GuestThread &t, long ns) {
    if (!t.susp_timer) return;
    itimerspec its{}; its.it_value.tv_nsec = ns;
    timer_settime((timer_t)t.susp_timer, 0, &its, nullptr);
}
void forward(GuestThread *t, int sig, siginfo_t *si, void *uc) {
    if (t) {
        if (t->susp_at_safe.load()) g_n_safe++; else g_n_now++;
        t->susp_pending = 0; t->susp_tries = 0;
        arm_timer(*t, 0);
        reg_port(*t);
        t->susp_parked = 1;
    }
    g_darling_susp(sig, si, uc);
    if (t) t->susp_parked = 0;
}
void susp_handler(int sig, siginfo_t *si, void *ucv) {
    const int saved_errno = errno;
    GuestThread *t = GuestThread::current();
    auto *uc = (ucontext_t *)ucv;
    if (!t || !t->jit || t->susp_at_safe.load()) { forward(t, sig, si, ucv); errno = saved_errno; return; }
    const bool timer = si && si->si_code == SI_TIMER;
    if (timer && !t->susp_pending.load()) { errno = saved_errno; return; }  // stale re-send
    const bool in_guest = t->depth > 0 && !t->native;
    const u64 rip = (u64)uc->uc_mcontext.gregs[REG_RIP];
    // 2 s without a safe point (a GuestThread idle in ELF code for good): park where it is
    if ((!in_guest && !in_elf(rip)) || t->susp_tries > 2000) { forward(t, sig, si, ucv); errno = saved_errno; return; }
    if (!t->susp_pending.load()) g_n_defer++;
    t->susp_tries++;
    t->susp_pending = 1;
    if (in_guest) t->jit->HaltExecution(Dynarmic::HaltReason::UserDefined3);
    arm_timer(*t, 1000000);
    errno = saved_errno;
}
void install_wrapper() {
    std::lock_guard<std::mutex> g(g_susp_install);
    struct sigaction cur{};
    if (sigaction(kSuspSig, nullptr, &cur) || !(cur.sa_flags & SA_SIGINFO)) return;
    if (cur.sa_sigaction == susp_handler || (void *)cur.sa_handler == SIG_DFL || (void *)cur.sa_handler == SIG_IGN) return;
    std::call_once(g_elf_once, [] { dl_iterate_phdr(elf_cb, nullptr); });
    g_darling_susp = cur.sa_sigaction;
    struct sigaction sa = cur;
    sa.sa_sigaction = susp_handler;
    sa.sa_flags = (cur.sa_flags | SA_SIGINFO) & ~SA_RESETHAND;
    sigaction(kSuspSig, &sa, nullptr);
}
bool susp_off() { static const bool v = getenv("X86SIM_A64_SUSP_DEFER") && atoi(getenv("X86SIM_A64_SUSP_DEFER")) == 0; return v; }
}  // namespace

bool in_elf_code(u64 rip) {
    std::call_once(g_elf_once, [] { dl_iterate_phdr(elf_cb, nullptr); });
    return in_elf(rip);
}

void GuestThread::susp_safe_point() {
    if (!susp_pending.load(std::memory_order_relaxed)) return;
    susp_at_safe = 1;
    syscall(SYS_tgkill, getpid(), tid, kSuspSig);  // delivered before tgkill returns (unless blocked: then later)
    susp_at_safe = 0;
    const u64 n = g_n_safe.load();
    if (n && !(n & (n - 1)))
        logf("suspend: %llu parked at a safe point (deferred %llu), %llu in native code; thread_get_state served %llu",
             (unsigned long long)n, (unsigned long long)g_n_defer.load(), (unsigned long long)g_n_now.load(), (unsigned long long)g_n_state.load());
}

void susp_thread_init(GuestThread &t) {
    t.tid = (int)syscall(SYS_gettid);
    if (susp_off()) return;
    install_wrapper();  // again per thread: Darling may have re-installed its handler (exec, fork)
    sigevent sev{};
    sev.sigev_notify = SIGEV_THREAD_ID;
    sev.sigev_signo = kSuspSig;
    sev._sigev_un._tid = t.tid;
    timer_t tm;
    if (timer_create(CLOCK_MONOTONIC, &sev, &tm) == 0) t.susp_timer = (void *)tm;
    for (int k = 0; k < kReg; k++) {
        GuestThread *e = nullptr;
        if (g_reg_t[k].compare_exchange_strong(e, &t)) { g_reg_port[k].store(port_of(t)); break; }
    }
}
// every registered guest thread (x86sim debug; callers hold dbg_threads_mutex())
void for_each_guest_thread(void (*fn)(GuestThread *, void *), void *arg) {
    for (int k = 0; k < kReg; k++) if (GuestThread *t = g_reg_t[k].load()) fn(t, arg);
}
void susp_thread_fini(GuestThread &t) {
    // the debugger's thread list holds GuestThreads under its mutex: not removed (and freed) meanwhile
    // (x86sim profile's sampler too: always taken, a profile can start at any time; uncontended otherwise)
    std::lock_guard<std::mutex> dl(dbg_threads_mutex());
    for (int k = 0; k < kReg; k++) if (g_reg_t[k].load() == &t) { g_reg_port[k].store(0); g_reg_t[k].store(nullptr); break; }
    if (t.susp_timer) { timer_delete((timer_t)t.susp_timer); t.susp_timer = nullptr; }
}

// thread_get_state / thread_set_state(thread, flavor, state, count): ARM flavors of a guest thread of this process
// come from / go to its Jit (it is parked at a safe point); anything else goes to Darling.
namespace {
constexpr int kArmThreadState = 1, kArmThreadState64 = 6, kArmNeonState64 = 17;
constexpr uint32_t kTs64Count = 68, kUnifiedCount = 70, kNeonCount = 132;
GuestThread *state_target(GuestThread &t, int flavor) {
    if (flavor != kArmThreadState && flavor != kArmThreadState64 && flavor != kArmNeonState64) return nullptr;
    const uint32_t port = (uint32_t)t.x(0);
    if (port_of(t) == port) return &t;
    GuestThread *g = find_port(port);
    if (!g || !g->jit) return nullptr;
    for (int k = 0; k < 4000 && !g->susp_parked.load(); k++) usleep(50);  // thread_suspend returns before it parks (<= 200 ms)
    return g;
}
}  // namespace
void h_thread_get_state(GuestThread &t, u32 i) {
    const int flavor = (int)t.x(1);
    GuestThread *g = state_target(t, flavor);
    static std::atomic<int> nlog{0};
    if (nlog++ < 4) logf("thread_get_state(%#x, flavor %d): %s", (unsigned)t.x(0), flavor, g ? (g->susp_parked.load() ? "guest thread, parked" : "guest thread, NOT parked") : "to Darling");
    if (!g) { h_generic(t, i); return; }
    auto *cnt = (uint32_t *)t.x(3);
    auto *out = (uint32_t *)t.x(2);
    auto &j = *g->jit;
    if (flavor == kArmNeonState64) {
        if (*cnt < kNeonCount) { t.set_x(0, 4); return; }  // KERN_INVALID_ARGUMENT
        auto v = j.GetVectors();
        memcpy(out, v.data(), 512);
        out[128] = j.GetFpsr(); out[129] = j.GetFpcr(); out[130] = out[131] = 0;
        *cnt = kNeonCount; t.set_x(0, 0); return;
    }
    if (flavor == kArmThreadState) {
        if (*cnt < kUnifiedCount) { t.set_x(0, 4); return; }
        out[0] = kArmThreadState64; out[1] = kTs64Count; out += 2;
        *cnt = kUnifiedCount;
    } else {
        if (*cnt < kTs64Count) { t.set_x(0, 4); return; }
        *cnt = kTs64Count;
    }
    g_n_state++;
    auto *o = (u64 *)out;
    auto r = j.GetRegisters();
    for (int k = 0; k < 31; k++) o[k] = r[k];  // x0-x28, fp, lr
    o[31] = j.GetSP(); o[32] = j.GetPC();
    out[66] = j.GetPstate(); out[67] = 0;
    t.set_x(0, 0);
}
// thread_get_register_pointer_values(thread, uintptr_t *sp, size_t *length, uintptr_t *values): what JavaScriptCore's
// conservative GC really calls (WTF::Thread::getRegisters). Darling's would answer with the host (x86) stack pointer and
// registers of a guest thread: JSC then scanned the native stack range instead of the guest stack. As libsyscall's
// arm64 version: sp minus the 128-byte red zone, lr and x0-x28 + fp values >= the page size.
void h_thread_get_register_pointer_values(GuestThread &t, u32 i) {
    GuestThread *g = state_target(t, kArmThreadState64);
    static std::atomic<int> nlog{0};
    if (nlog++ < 2) logf("thread_get_register_pointer_values(%#x): %s", (unsigned)t.x(0), g ? (g->susp_parked.load() ? "guest thread, parked" : "guest thread, NOT parked") : "to Darling");
    if (!g) { h_generic(t, i); return; }
    auto *sp = (u64 *)t.x(1); auto *len = (size_t *)t.x(2); auto *vals = (u64 *)t.x(3);
    if (!len || (*len > 0 && !vals)) { t.set_x(0, 4); return; }  // KERN_INVALID_ARGUMENT
    g_n_state++;
    auto &j = *g->jit;
    const size_t in = *len;
    size_t out = 0;
    auto push = [&](u64 v) { if (v >= 0x4000) { if (out < in) vals[out] = v; out++; } };
    auto r = j.GetRegisters();
    push(r[30]);
    for (int k = 0; k < 30; k++) push(r[k]);
    if (sp) { const u64 s = j.GetSP(); *sp = s > 128 ? s - 128 : 0; }
    *len = out;
    t.set_x(0, in == 0 || out > in ? 52 /* KERN_INSUFFICIENT_BUFFER_SIZE */ : 0);
}
void h_thread_set_state(GuestThread &t, u32 i) {
    const int flavor = (int)t.x(1);
    GuestThread *g = flavor == kArmNeonState64 ? nullptr : state_target(t, flavor);
    if (!g || g == &t) { h_generic(t, i); return; }
    const uint32_t cnt = (uint32_t)t.x(3);
    auto *in = (const uint32_t *)t.x(2);
    if (flavor == kArmThreadState) { if (cnt < kUnifiedCount) { t.set_x(0, 4); return; } in += 2; }
    else if (cnt < kTs64Count) { t.set_x(0, 4); return; }
    auto *s = (const u64 *)in;
    auto &j = *g->jit;
    std::array<u64, 31> r;
    for (int k = 0; k < 31; k++) r[k] = s[k];
    j.SetRegisters(r); j.SetSP(s[31]); j.SetPC(s[32]); j.SetPstate(in[66]);
    t.set_x(0, 0);
}

}  // namespace a64
