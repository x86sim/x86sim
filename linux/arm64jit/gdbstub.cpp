// libarm64jit: a GDB remote serial protocol stub for LLDB (x86sim debug).
//
// Guest code is arm64 translated by dynarmic, so no debugserver can see it: the stub lives here, next to the guest
// state. It speaks the Darwin flavour lldb expects from debugserver (qHostInfo / qProcessInfo with arm64 and
// <platform>simulator, qRegisterInfo, qShlibInfoAddr = dyld_sim's dyld_all_image_infos, qThreadStopInfo, Z0/z0, vCont)
// on a Unix socket, /run/user/<uid>/x86sim-dbg-<UDID>-<pid>.sock (pid: the guest's, as `x86sim pidof`).
//
// Off unless asked for: X86SIM_A64_DEBUG=1 at launch (listen), =wait (listen and park the main thread before dyld_sim
// runs), or at run time: SIGUSR1 with a request file /run/user/<uid>/x86sim-dbg-req-<UDID>-<pid> (host.cpp's memlog
// thread starts the stub instead of dumping). Translated code pays nothing while no debugger is attached: one relaxed
// load per call() loop iteration and per thunk, and one per translated instruction word while breakpoints exist.
//
// All-stop: g_stop is set and every guest thread's Jit halted; a thread parks at its next safe point (the call() loop
// between Run()s, a thunk's entry or return, an exception). A thread inside a thunk's native code (blocked in mach_msg)
// counts as stopped at once: its guest registers are in its Jit, and it parks when it returns to guest code.
// Breakpoints never write guest memory: the translator's code fetch (MemoryReadCode) answers "svc #kDbgSvc" at a
// breakpoint, and the blocks around it are invalidated (a 4 KB window before it: AOT-image and loaded blocks only
// register their first instruction for invalidation). A step is jit->Step() with the breakpoint at pc hidden.
#include "a64.h"
#include <algorithm>
#include <atomic>
#include <cstdarg>
#include <cstring>
#include <cerrno>
#include <csignal>
#include <cstdio>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
#include <fcntl.h>
#include <linux/futex.h>
#include <poll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/un.h>
#include <unistd.h>

namespace a64 {

std::atomic<int> g_dbg_on{0};
std::atomic<int> g_dbg_nbp{0};
void for_each_guest_thread(void (*fn)(GuestThread *, void *), void *arg);  // signals.cpp
void dbg_invalidate(u64 addr, u64 len);                                    // jit.cpp
u64 a64_all_image_infos();                                                 // host.cpp
u64 a64_main_header();

namespace {
constexpr int kMaxBp = 512;
std::atomic<u64> g_bp[kMaxBp];
std::atomic<int> g_bp_hi{0};       // slots in use are below
thread_local u64 t_skip = 0;       // a step at a breakpoint: the original instruction
std::atomic<int> g_stop{0};        // every guest thread parks at its next safe point
std::atomic<uint32_t> g_gen{0};    // futex word, bumped on every resume
std::atomic<bool> g_started{false};
std::mutex g_thr_mu;               // the registry's GuestThreads stay alive while held (susp_thread_fini takes it)
int g_evfd = -1;                   // a thread stopped with a reason (breakpoint, step, exception)
std::atomic<int> g_session_fd{-1};
std::string g_sock_path;
enum Reason { kNone = 0, kBreakpoint, kTrace, kException, kSignal };
enum Action { kCont = 0, kStep = 1, kHold = 2 };

int futex(std::atomic<uint32_t> *w, int op, uint32_t v, const timespec *ts) {
    return (int)syscall(SYS_futex, reinterpret_cast<uint32_t *>(w), op, v, ts, nullptr, 0);
}
void notify() {
    const uint64_t one = 1;
    if (g_evfd >= 0 && write(g_evfd, &one, 8) < 0) {}
}
void wake_all() {
    g_gen.fetch_add(1);
    futex(&g_gen, FUTEX_WAKE_PRIVATE, 0x7fffffff, nullptr);
}

// Records why t stopped and stops the world (the stub halts the other threads).
void report(GuestThread &t, int reason, int sig, u64 metype = 0, u64 code = 0, u64 sub = 0) {
    t.dbg_reason = reason; t.dbg_sig = sig;
    t.dbg_info[0] = metype; t.dbg_info[1] = code; t.dbg_info[2] = sub;
    t.dbg_reported = false;
    g_stop.store(1);
    notify();
}

void park(GuestThread &t) {
    t.dbg_parked.store(1);
    for (;;) {
        const uint32_t gen = g_gen.load();
        if (!g_dbg_on.load() || (!g_stop.load() && t.dbg_action.load() != kHold)) break;
        timespec ts{0, 50 * 1000000};
        futex(&g_gen, FUTEX_WAIT_PRIVATE, gen, &ts);
        // Darling's thread_suspend (JSC's GC) while parked: let it park there as at any safe point
        if (t.susp_pending.load(std::memory_order_relaxed)) t.susp_safe_point();
    }
    t.dbg_parked.store(0);
}

// One guest instruction on this thread (a breakpoint at pc is not hit: the original instruction runs).
void step_one(GuestThread &t) {
    auto &j = *t.jit;
    const u64 pc = j.GetPC();
    if (t.depth > 1) {
        // a nested call: the outer call()'s Run() is on this thread's stack and dynarmic's Step() needs a Jit that is
        // not executing. A thunk's svc is made here (CallSVC, as the translated block would); anything else is stepped
        // on a second Jit of this thread with the same state.
        const uint32_t insn = guest_code_word(pc).value_or(0);
        if ((insn & 0xFFE0001Fu) == 0xD4000001u) {
            j.SetPC(pc + 4);
            t.CallSVC((insn >> 5) & 0xffff);
            return;
        }
        if (!t.dbg_aux) t.dbg_aux = dbg_make_jit(t);
        auto &a = *t.dbg_aux;
        a.SetRegisters(j.GetRegisters()); a.SetSP(j.GetSP()); a.SetPC(pc); a.SetPstate(j.GetPstate());
        a.SetVectors(j.GetVectors()); a.SetFpcr(j.GetFpcr()); a.SetFpsr(j.GetFpsr());
        t_skip = pc;
        a.Step();
        t_skip = 0;
        j.SetRegisters(a.GetRegisters()); j.SetSP(a.GetSP()); j.SetPC(a.GetPC()); j.SetPstate(a.GetPstate());
        j.SetVectors(a.GetVectors()); j.SetFpcr(a.GetFpcr()); j.SetFpsr(a.GetFpsr());
    } else {
        t_skip = pc;
        j.Step();
        t_skip = 0;
    }
    dbg_invalidate(pc, 4);  // the step block at pc holds the original instruction
}
}  // namespace

std::mutex &dbg_threads_mutex() { return g_thr_mu; }
bool dbg_started() { return g_started.load(std::memory_order_relaxed); }

static u64 g_notifier;  // host.cpp: lldb's image list notifier, a "ret" only the code fetch has
void dbg_set_notifier(u64 a) { g_notifier = a; }

// Translator code fetch: "svc #kDbgSvc" at a breakpoint; "ret" at the notifier.
std::optional<uint32_t> dbg_read_code(u64 a) {
    a &= GuestThread::kTbi;
    if (a != t_skip)
        for (int i = 0, n = g_bp_hi.load(std::memory_order_acquire); i < n; i++)
            if (g_bp[i].load(std::memory_order_relaxed) == a) return 0xD4000001u | (kDbgSvc << 5);
    if (a == g_notifier && a) return 0xD65F03C0u;
    return std::nullopt;
}

bool dbg_bp_active(u64 a) {
    for (int i = 0, n = g_bp_hi.load(); i < n; i++) if (g_bp[i].load() == a) return true;
    return false;
}

// CallSVC: a breakpoint's svc ran. The thread stops at the breakpoint (pc = its address) when Run() returns.
void dbg_svc(GuestThread &t, u64 addr) {
    t.jit->SetPC(addr);
    if (g_dbg_on.load() && dbg_bp_active(addr)) report(t, kBreakpoint, 5);
    else dbg_invalidate(addr, 4);  // removed meanwhile: the block is translated again without it
    t.jit->HaltExecution(Dynarmic::HaltReason::UserDefined4);
}

// Safe points (jit.cpp): where 0 = the call() loop before Run() (pc: the next instruction), 1 / 2 = a thunk's entry /
// return (inside Run(), pc = svc + 4), 3 = an exception (inside Run()).
void dbg_point(GuestThread &t, int where) {
    if (!g_dbg_on.load(std::memory_order_acquire)) { t.dbg_step_pending = false; return; }
    if (t.dbg_step_pending && where != 1) {  // a step that left its call(): it ends here
        t.dbg_step_pending = false;
        report(t, kTrace, 5);
    }
    for (;;) {
        if (g_stop.load() || t.dbg_action.load() == kHold) park(t);
        if (!g_dbg_on.load()) return;
        if (t.dbg_action.load() != kStep) return;
        t.dbg_action.store(kCont);
        if (where != 0) {  // inside Run(): stop when the block ends (nothing to step at a thunk / exception)
            t.dbg_step_pending = true;
            t.jit->HaltExecution(Dynarmic::HaltReason::UserDefined4);
            return;
        }
        step_one(t);
        if (t.done && *t.done) { t.dbg_step_pending = true; return; }
        report(t, kTrace, 5);
    }
}

// A guest exception (brk / udf / jump to unmapped memory, a fault in translated code) while a debugger is attached:
// it sees it first. Returns after the debugger resumed; the caller goes on as without one (guest handler or death).
void dbg_exception(GuestThread &t, u64 pc, int sig, u64 metype, u64 code, u64 sub) {
    if (!g_dbg_on.load()) return;
    t.jit->SetPC(pc);
    report(t, kException, sig, metype, code, sub);
    dbg_point(t, 3);
}

// ---- the server ---------------------------------------------------------------------------------------------------
namespace {
struct Io { int fd; bool noack = false; std::string in; };

bool io_send(Io &io, const std::string &payload) {
    std::string p = "$" + payload + "#";
    unsigned cs = 0;
    for (unsigned char c : payload) cs += c;
    char t[3]; snprintf(t, sizeof t, "%02x", cs & 0xff);
    p += t;
    for (size_t off = 0; off < p.size();) {
        ssize_t n = send(io.fd, p.data() + off, p.size() - off, MSG_NOSIGNAL);
        if (n <= 0) { if (n < 0 && errno == EINTR) continue; return false; }
        off += (size_t)n;
    }
    return true;
}
// -1: closed; 0: nothing yet (timeout); 1: a packet in out; 2: an interrupt (^C)
int io_get(Io &io, std::string &out, int timeout_ms) {
    for (;;) {
        // parse
        size_t i = 0;
        while (i < io.in.size() && (io.in[i] == '+' || io.in[i] == '-')) i++;
        if (i < io.in.size() && io.in[i] == '\x03') { io.in.erase(0, i + 1); return 2; }
        if (i < io.in.size() && io.in[i] == '$') {
            size_t h = io.in.find('#', i);
            if (h != std::string::npos && h + 2 < io.in.size()) {
                std::string raw = io.in.substr(i + 1, h - i - 1);
                io.in.erase(0, h + 3);
                if (!io.noack && send(io.fd, "+", 1, MSG_NOSIGNAL) < 0) return -1;
                out.clear();
                for (size_t k = 0; k < raw.size(); k++) {
                    if (raw[k] == '}' && k + 1 < raw.size()) out += (char)(raw[++k] ^ 0x20);
                    else out += raw[k];
                }
                return 1;
            }
        } else if (i < io.in.size()) { io.in.erase(0, i + 1); continue; }  // noise
        else io.in.clear();
        pollfd p{io.fd, POLLIN, 0};
        int r = poll(&p, 1, timeout_ms);
        if (r < 0 && errno == EINTR) continue;
        if (r == 0) return 0;
        char b[65536];
        ssize_t n = recv(io.fd, b, sizeof b, 0);
        if (n <= 0) { if (n < 0 && errno == EINTR) continue; return -1; }
        io.in.append(b, (size_t)n);
    }
}

std::string hexs(const void *p, size_t n) {
    static const char *d = "0123456789abcdef";
    std::string s; s.reserve(n * 2);
    for (size_t i = 0; i < n; i++) { uint8_t c = ((const uint8_t *)p)[i]; s += d[c >> 4]; s += d[c & 15]; }
    return s;
}
std::string hexstr(const std::string &s) { return hexs(s.data(), s.size()); }
int hv(char c) { return c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1; }
bool unhex(const std::string &s, std::vector<uint8_t> &out) {
    if (s.size() & 1) return false;
    out.resize(s.size() / 2);
    for (size_t i = 0; i < out.size(); i++) {
        int a = hv(s[2 * i]), b = hv(s[2 * i + 1]);
        if (a < 0 || b < 0) return false;
        out[i] = (uint8_t)(a << 4 | b);
    }
    return true;
}
u64 num(const std::string &s, size_t *pos = nullptr, size_t from = 0) {
    u64 v = 0; size_t i = from;
    while (i < s.size() && hv(s[i]) >= 0) v = v << 4 | (u64)hv(s[i++]);
    if (pos) *pos = i;
    return v;
}
std::string fmt(const char *f, ...) __attribute__((format(printf, 1, 2)));
std::string fmt(const char *f, ...) {
    char b[1024];
    va_list ap; va_start(ap, f); vsnprintf(b, sizeof b, f, ap); va_end(ap);
    return b;
}

// ---- registers: x0-x28, fp, lr, sp, pc, cpsr, v0-v31, fpsr, fpcr (debugserver's arm64 names) -----------------------
constexpr int kNRegs = 68, kRegBytes = 8 * 33 + 4 + 16 * 32 + 8;
int reg_size(int n) { return n <= 32 ? 8 : n == 33 || n >= 66 ? 4 : 16; }
int reg_off(int n) { return n <= 32 ? 8 * n : n == 33 ? 264 : n < 66 ? 268 + 16 * (n - 34) : 780 + 4 * (n - 66); }
std::string reg_info(int n) {
    if (n >= kNRegs) return "E45";
    std::string name, extra;
    const char *set = n <= 33 ? "General Purpose Registers" : "Floating Point Registers";
    int dwarf = -1;
    const char *generic = nullptr;
    if (n < 29) { name = fmt("x%d", n); dwarf = n; if (n < 8) extra = fmt("alt-name:arg%d;", n + 1); }
    else if (n == 29) { name = "fp"; extra = "alt-name:x29;"; dwarf = 29; generic = "fp"; }
    else if (n == 30) { name = "lr"; extra = "alt-name:x30;"; dwarf = 30; generic = "ra"; }
    else if (n == 31) { name = "sp"; extra = "alt-name:x31;"; dwarf = 31; generic = "sp"; }
    else if (n == 32) { name = "pc"; dwarf = 32; generic = "pc"; }
    else if (n == 33) { name = "cpsr"; generic = "flags"; }
    else if (n < 66) { name = fmt("v%d", n - 34); dwarf = 64 + n - 34; }
    else name = n == 66 ? "fpsr" : "fpcr";
    if (n < 8) generic = n == 0 ? "arg1" : n == 1 ? "arg2" : n == 2 ? "arg3" : n == 3 ? "arg4" : n == 4 ? "arg5" : n == 5 ? "arg6" : n == 6 ? "arg7" : "arg8";
    std::string r = fmt("name:%s;%sbitsize:%d;offset:%d;", name.c_str(), extra.c_str(), reg_size(n) * 8, reg_off(n));
    r += (n >= 34 && n < 66) ? "encoding:vector;format:vector-uint8;" : "encoding:uint;format:hex;";
    r += fmt("set:%s;", set);
    if (dwarf >= 0) r += fmt("ehframe:%d;dwarf:%d;", dwarf, dwarf);
    if (generic) r += fmt("generic:%s;", generic);
    return r;
}
void get_regs(GuestThread &t, uint8_t *o) {
    auto &j = *t.jit;
    auto r = j.GetRegisters();
    memcpy(o, r.data(), 31 * 8);
    u64 sp = j.GetSP(), pc = j.GetPC();
    memcpy(o + 248, &sp, 8); memcpy(o + 256, &pc, 8);
    uint32_t ps = j.GetPstate(); memcpy(o + 264, &ps, 4);
    auto v = j.GetVectors();
    memcpy(o + 268, v.data(), 512);
    uint32_t f[2] = {j.GetFpsr(), j.GetFpcr()};
    memcpy(o + 780, f, 8);
}
void set_regs(GuestThread &t, const uint8_t *o, uint64_t mask = ~0ull, bool vec = true) {
    auto &j = *t.jit;
    auto r = j.GetRegisters();
    memcpy(r.data(), o, 31 * 8);
    j.SetRegisters(r);
    u64 sp, pc; memcpy(&sp, o + 248, 8); memcpy(&pc, o + 256, 8);
    j.SetSP(sp); j.SetPC(pc);
    uint32_t ps; memcpy(&ps, o + 264, 4); j.SetPstate(ps);
    if (vec) {
        auto v = j.GetVectors();
        memcpy(v.data(), o + 268, 512);
        j.SetVectors(v);
        uint32_t f[2]; memcpy(f, o + 780, 8);
        j.SetFpsr(f[0]); j.SetFpcr(f[1]);
    }
    (void)mask;
}

// ---- threads -------------------------------------------------------------------------------------------------------
std::vector<GuestThread *> threads() {  // with g_thr_mu held
    std::vector<GuestThread *> v;
    for_each_guest_thread([](GuestThread *t, void *a) {
        if (t->jit && (t->dbg_parked.load() || __atomic_load_n(&t->depth, __ATOMIC_RELAXED) > 0))
            ((std::vector<GuestThread *> *)a)->push_back(t);
    }, &v);
    std::sort(v.begin(), v.end(), [](GuestThread *a, GuestThread *b) { return a->tid < b->tid; });
    return v;
}
GuestThread *find(int tid) {
    for (GuestThread *t : threads()) if (t->tid == tid) return t;
    return nullptr;
}
bool stopped(GuestThread *t) {
    return t->dbg_parked.load() || t->susp_parked.load() || __atomic_load_n(&t->native, __ATOMIC_RELAXED) ||
           __atomic_load_n(&t->depth, __ATOMIC_RELAXED) == 0;
}
// Stops every guest thread (at most 3 s: a thread that does not get to a safe point is reported where it is).
void stop_all() {
    g_stop.store(1);
    for (int k = 0; k < 3000; k++) {
        bool all = true;
        {
            std::lock_guard<std::mutex> l(g_thr_mu);
            for (GuestThread *t : threads())
                if (!stopped(t)) { all = false; t->jit->HaltExecution(Dynarmic::HaltReason::UserDefined4); }
        }
        if (all) return;
        usleep(1000);
    }
    logf("debug: some threads did not stop in 3 s");
}

std::string thread_name(int tid) {
    char p[64], b[64] = "";
    snprintf(p, sizeof p, "/proc/self/task/%d/comm", tid);
    int fd = open(p, O_RDONLY | O_CLOEXEC);
    if (fd >= 0) { ssize_t n = read(fd, b, sizeof b - 1); if (n > 0) b[n] = 0; close(fd); }
    std::string s = b;
    while (!s.empty() && s.back() == '\n') s.pop_back();
    return s;
}

int g_sel = 0;         // Hg / the reported thread
int g_stop_tid = 0;

std::string stop_packet(GuestThread &t) {  // with g_thr_mu held
    const int sig = t.dbg_reason ? t.dbg_sig : 0;
    std::string r = fmt("T%02xthread:%x;", sig, t.tid);
    const std::string nm = thread_name(t.tid);
    if (!nm.empty()) r += "hexname:" + hexstr(nm) + ";";
    auto all = threads();
    r += "threads:";
    for (size_t i = 0; i < all.size(); i++) r += fmt("%s%x", i ? "," : "", all[i]->tid);
    r += ";thread-pcs:";
    for (size_t i = 0; i < all.size(); i++) r += fmt("%s%lx", i ? "," : "", (unsigned long)all[i]->jit->GetPC());
    r += ";";
    uint8_t regs[kRegBytes];
    get_regs(t, regs);
    for (int n : {29, 30, 31, 32}) r += fmt("%02x:", n) + hexs(regs + reg_off(n), 8) + ";";
    switch (t.dbg_reason) {
    case kBreakpoint: r += "reason:breakpoint;"; break;
    case kTrace: r += "reason:trace;"; break;
    case kException:
        r += fmt("metype:%lx;mecount:2;medata:%lx;medata:%lx;", (unsigned long)t.dbg_info[0], (unsigned long)t.dbg_info[1], (unsigned long)t.dbg_info[2]);
        r += "reason:exception;";
        break;
    case kSignal: r += "reason:signal;"; break;
    default: break;
    }
    return r;
}

// The thread a stop is reported for: a new reason first, then any reason, then the main thread.
std::string report_stop(int interrupt_sig) {
    std::lock_guard<std::mutex> l(g_thr_mu);
    auto all = threads();
    if (all.empty()) return "W00";
    GuestThread *pick = nullptr;
    for (GuestThread *t : all) if (t->dbg_reason && !t->dbg_reported) { pick = t; break; }
    if (!pick) for (GuestThread *t : all) if (t->dbg_reason) { pick = t; break; }
    if (!pick) {
        for (GuestThread *t : all) if (t->tid == (int)getpid()) pick = t;
        if (!pick) pick = all[0];
        if (interrupt_sig) { pick->dbg_reason = kSignal; pick->dbg_sig = interrupt_sig; }
    }
    pick->dbg_reported = true;
    g_sel = g_stop_tid = pick->tid;
    return stop_packet(*pick);
}

// Resume: per-thread actions (vCont); threads without one get dflt.
void resume(const std::map<int, int> &acts, int dflt) {
    {
        std::lock_guard<std::mutex> l(g_thr_mu);
        for (GuestThread *t : threads()) {
            auto it = acts.find(t->tid);
            const int a = it != acts.end() ? it->second : dflt;
            t->dbg_action.store(a);
            if (a != kHold) { t->dbg_reason = kNone; t->dbg_reported = false; }
        }
    }
    uint64_t v;
    while (read(g_evfd, &v, 8) == 8) {}  // stale notifications
    g_stop.store(0);
    wake_all();
}

// ---- memory ----------------------------------------------------------------------------------------------------------
size_t read_mem(u64 a, uint8_t *o, size_t n) {
    iovec l{o, n}, r{(void *)a, n};
    ssize_t k = process_vm_readv(getpid(), &l, 1, &r, 1, 0);
    if (k == (ssize_t)n) return n;
    size_t done = 0;  // up to the first unreadable page
    while (done < n) {
        const size_t c = std::min<size_t>(n - done, 0x1000 - ((a + done) & 0xfff));
        iovec l2{o + done, c}, r2{(void *)(a + done), c};
        if (process_vm_readv(getpid(), &l2, 1, &r2, 1, 0) != (ssize_t)c) break;
        done += c;
    }
    return done;
}
bool write_mem(u64 a, const uint8_t *p, size_t n) {
    static int memfd = open("/proc/self/mem", O_RDWR | O_CLOEXEC);  // writes read-only (code) pages too
    bool ok = memfd >= 0 && pwrite(memfd, p, n, (off_t)a) == (ssize_t)n;
    if (!ok) {
        iovec l{(void *)p, n}, r{(void *)a, n};
        ok = process_vm_writev(getpid(), &l, 1, &r, 1, 0) == (ssize_t)n;
    }
    if (ok) dbg_invalidate(a & ~3ull, ((a & 3) + n + 3) & ~3ull);  // code written by the debugger
    return ok;
}
std::string region_info(u64 a) {
    FILE *m = fopen("/proc/self/maps", "r");
    if (!m) return "E01";
    char line[768];
    u64 next = ~0ull;
    std::string r;
    while (fgets(line, sizeof line, m)) {
        unsigned long lo, hi; char perm[8] = "", path[512] = "";
        if (sscanf(line, "%lx-%lx %7s %*s %*s %*s %511[^\n]", &lo, &hi, perm, path) < 3) continue;
        if (a >= lo && a < hi) {
            std::string p;
            if (perm[0] == 'r') p += 'r';
            if (perm[1] == 'w') p += 'w';
            // guest code is never host-executable (map_macho, the dyld cache): a readable file mapping (a Mach-O, the
            // cache, the memfd stub dylibs) counts as code for lldb (return addresses, unwinding)
            if (perm[2] == 'x' || (perm[0] == 'r' && path[0] == '/')) p += 'x';
            r = fmt("start:%lx;size:%lx;permissions:%s;", lo, hi - lo, p.c_str());
            break;
        }
        if (lo > a && lo < next) next = lo;
    }
    fclose(m);
    if (r.empty()) r = fmt("start:%lx;size:%lx;", (unsigned long)a, (unsigned long)((next == ~0ull ? 0x800000000000ull : next) - a));
    return r;
}

// ---- breakpoints -----------------------------------------------------------------------------------------------------
bool bp_set(u64 a) {
    if (dbg_bp_active(a)) return true;
    for (int i = 0; i < kMaxBp; i++) {
        u64 z = 0;
        if (g_bp[i].compare_exchange_strong(z, a)) {
            int hi = g_bp_hi.load();
            while (hi < i + 1 && !g_bp_hi.compare_exchange_weak(hi, i + 1)) {}
            g_dbg_nbp.fetch_add(1);
            // every block containing a: translated ones by their range, AOT-image / loaded ones by their first
            // instruction (a block is at most ~1000 instructions)
            dbg_invalidate(a >= 4092 ? a - 4092 : 0, a >= 4092 ? 4096 : a + 4);
            return true;
        }
    }
    return false;
}
void bp_clear(u64 a) {
    for (int i = 0, n = g_bp_hi.load(); i < n; i++) {
        u64 e = a;
        if (g_bp[i].compare_exchange_strong(e, 0)) { g_dbg_nbp.fetch_sub(1); dbg_invalidate(a, 4); }
    }
}
void bp_clear_all() {
    for (int i = 0, n = g_bp_hi.load(); i < n; i++) if (u64 a = g_bp[i].load()) bp_clear(a);
}

std::string process_info() {
    // the platform from the main executable's LC_BUILD_VERSION (7 iOS, 8 tvOS, 9 watchOS, 12 visionOS simulator)
    const char *os = "iossimulator";
    if (u64 mh = a64_main_header()) {
        uint32_t h[8];
        if (read_mem(mh, (uint8_t *)h, 32) == 32 && h[0] == 0xfeedfacf) {
            u64 p = mh + 32;
            for (uint32_t k = 0; k < h[4]; k++) {
                uint32_t lc[3];
                if (read_mem(p, (uint8_t *)lc, 12) != 12 || lc[1] < 8) break;
                if (lc[0] == 0x32) {
                    os = lc[2] == 8 ? "tvossimulator" : lc[2] == 9 ? "watchossimulator" : lc[2] == 12 ? "xrossimulator" : "iossimulator";
                    break;
                }
                p += lc[1];
            }
        }
    }
    return fmt("pid:%x;parent-pid:%x;real-uid:%x;real-gid:%x;effective-uid:%x;effective-gid:%x;cputype:100000c;cpusubtype:0;"
               "ptrsize:8;ostype:%s;vendor:apple;endian:little;",
               (int)getpid(), (int)getppid(), (int)getuid(), (int)getgid(), (int)geteuid(), (int)getegid(), os);
}

// thread from a ";thread:<tid>;" suffix (removed from p), else the selected thread
int take_thread(std::string &p) {
    size_t k = p.find(";thread:");
    if (k == std::string::npos) return g_sel;
    int tid = (int)num(p, nullptr, k + 8);
    p.resize(k);
    return tid;
}

std::map<u64, std::vector<uint8_t>> g_saved_regs;
u64 g_save_id = 0;
std::map<u64, u64> g_allocs;

enum class After { Reply, Resume, Detach, Kill };

After handle(Io &io, std::string p, std::string &reply) {
    reply.clear();
    auto starts = [&](const char *s) { return p.compare(0, strlen(s), s) == 0; };
    if (p == "?") { reply = report_stop(0x11); return After::Reply; }
    if (starts("qSupported")) {
        reply = "PacketSize=20000;QStartNoAckMode+;QThreadSuffixSupported+;QListThreadsInStopReply+;vContSupported+;qXfer:features:read-";
        return After::Reply;
    }
    if (p == "QStartNoAckMode") { reply = "OK"; io_send(io, reply); io.noack = true; reply = "\x01"; return After::Reply; }
    if (p == "QThreadSuffixSupported" || p == "QListThreadsInStopReply" || starts("QEnableErrorStrings") ||
        starts("QSetDetachOnError") || p == "qSymbol::" || starts("QSetSTDIO") || starts("QLaunchArch")) { reply = "OK"; return After::Reply; }
    if (p == "qHostInfo") {
        char hn[256] = "x86sim";
        gethostname(hn, sizeof hn);
        reply = "cputype:16777228;cpusubtype:0;ostype:macosx;vendor:apple;endian:little;ptrsize:8;hostname:" + hexstr(hn) + ";";
        // os_version: the Swift lldb writes it into each expression as @available(iOS <version>, *); without one it is
        // "iOS 0", which does not parse, and every expression in a class context fails. It is not the runtime's
        // version on purpose: lldb uses the libdyld SPI loader (jGetLoadedDynamicLibrariesInfos, a notifier with
        // another ABI; not implemented here) for iOS 10 and later and the all_image_infos one this stub serves below
        // that (DynamicLoaderDarwin::UseDYLDSPI). Expressions are compiled for the module's deployment target anyway,
        // so 9.0 only has to parse.
        reply += "os_version:9.0;";
        return After::Reply;
    }
    if (p == "qProcessInfo") { reply = process_info(); return After::Reply; }
    if (p == "qC") { reply = fmt("QC%x", g_sel); return After::Reply; }
    if (p == "qShlibInfoAddr") { reply = fmt("%lx", (unsigned long)a64_all_image_infos()); return After::Reply; }
    if (starts("qRegisterInfo")) { reply = reg_info((int)num(p, nullptr, 13)); return After::Reply; }
    if (p == "qfThreadInfo") {
        std::lock_guard<std::mutex> l(g_thr_mu);
        reply = "m";
        auto all = threads();
        for (size_t i = 0; i < all.size(); i++) reply += fmt("%s%x", i ? "," : "", all[i]->tid);
        if (all.empty()) reply = "l";
        return After::Reply;
    }
    if (p == "qsThreadInfo") { reply = "l"; return After::Reply; }
    if (starts("qThreadStopInfo")) {
        std::lock_guard<std::mutex> l(g_thr_mu);
        GuestThread *t = find((int)num(p, nullptr, 15));
        reply = t ? stop_packet(*t) : "E10";
        return After::Reply;
    }
    if (starts("qThreadExtraInfo,")) {
        reply = hexstr(thread_name((int)num(p, nullptr, 17)));
        return After::Reply;
    }
    if (starts("qMemoryRegionInfo:")) { reply = region_info(num(p, nullptr, 18)); return After::Reply; }
    if (p == "qMemoryRegionInfo") { reply = "OK"; return After::Reply; }
    if (starts("qWatchpointSupportInfo")) { reply = "num:0;"; return After::Reply; }
    if (starts("H")) {  // Hg<tid> / Hc<tid>
        int tid = p.size() > 2 && p[2] != '-' ? (int)num(p, nullptr, 2) : 0;
        if (p[1] == 'g' && tid > 0) g_sel = tid;
        reply = "OK";
        return After::Reply;
    }
    if (starts("p") || starts("P") || starts("g") || starts("G") || starts("QSaveRegisterState") || starts("QRestoreRegisterState")) {
        const int tid = take_thread(p);
        std::lock_guard<std::mutex> l(g_thr_mu);
        GuestThread *t = find(tid);
        if (!t) { reply = "E10"; return After::Reply; }
        uint8_t regs[kRegBytes];
        get_regs(*t, regs);
        if (p[0] == 'g') reply = hexs(regs, kRegBytes);
        else if (p[0] == 'p') {
            int n = (int)num(p, nullptr, 1);
            reply = n < kNRegs ? hexs(regs + reg_off(n), reg_size(n)) : "E45";
        } else if (p[0] == 'G') {
            std::vector<uint8_t> v;
            if (!unhex(p.substr(1), v) || v.size() < (size_t)kRegBytes) { reply = "E11"; return After::Reply; }
            set_regs(*t, v.data());
            reply = "OK";
        } else if (p[0] == 'P') {
            size_t eq = p.find('=');
            int n = (int)num(p, nullptr, 1);
            std::vector<uint8_t> v;
            if (eq == std::string::npos || n >= kNRegs || !unhex(p.substr(eq + 1), v) || v.size() != (size_t)reg_size(n)) { reply = "E11"; return After::Reply; }
            memcpy(regs + reg_off(n), v.data(), v.size());
            set_regs(*t, regs, ~0ull, n >= 34);
            reply = "OK";
        } else if (starts("QSaveRegisterState")) {
            g_saved_regs[++g_save_id].assign(regs, regs + kRegBytes);
            reply = fmt("%lx", (unsigned long)g_save_id);
        } else {
            auto it = g_saved_regs.find(num(p, nullptr, 22));
            if (it == g_saved_regs.end()) { reply = "E12"; return After::Reply; }
            set_regs(*t, it->second.data());
            g_saved_regs.erase(it);
            reply = "OK";
        }
        return After::Reply;
    }
    if (p[0] == 'm' || p[0] == 'x') {
        size_t c;
        u64 a = num(p, &c, 1), n = c < p.size() && p[c] == ',' ? num(p, nullptr, c + 1) : 0;
        if (n == 0) { reply = p[0] == 'x' ? "OK" : ""; return After::Reply; }
        n = std::min<u64>(n, 0x10000);
        std::vector<uint8_t> b(n);
        size_t got = read_mem(a, b.data(), n);
        if (!got) { reply = "E08"; return After::Reply; }
        if (p[0] == 'm') reply = hexs(b.data(), got);
        else {
            reply.reserve(got + 16);
            for (size_t i = 0; i < got; i++) {
                const char ch = (char)b[i];
                if (ch == '#' || ch == '$' || ch == '}' || ch == '*') { reply += '}'; reply += (char)(ch ^ 0x20); }
                else reply += ch;
            }
            if (reply.empty()) reply = "E08";
        }
        return After::Reply;
    }
    if (p[0] == 'M' || p[0] == 'X') {
        size_t c, colon = p.find(':');
        u64 a = num(p, &c, 1), n = num(p, nullptr, c + 1);
        if (colon == std::string::npos) { reply = "E11"; return After::Reply; }
        std::vector<uint8_t> v;
        if (p[0] == 'M') { if (!unhex(p.substr(colon + 1), v)) { reply = "E11"; return After::Reply; } }
        else v.assign(p.begin() + colon + 1, p.end());
        if (v.size() < n) { reply = "E11"; return After::Reply; }
        reply = n == 0 || write_mem(a, v.data(), n) ? "OK" : "E09";
        return After::Reply;
    }
    if (starts("Z0,") || starts("z0,")) {
        u64 a = num(p, nullptr, 3);
        if (p[0] == 'Z') reply = bp_set(a) ? "OK" : "E0e";
        else { bp_clear(a); reply = "OK"; }
        return After::Reply;
    }
    if (starts("_M")) {  // allocate (lldb's expressions): _M<size>,<rwx>
        size_t c;
        u64 n = num(p, &c, 2);
        int prot = 0;
        for (size_t k = c + 1; k < p.size(); k++) prot |= p[k] == 'r' ? PROT_READ : p[k] == 'w' ? PROT_WRITE : p[k] == 'x' ? PROT_EXEC : 0;
        void *m = mmap(nullptr, n, prot ? prot : PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (m == MAP_FAILED) { reply = "E07"; return After::Reply; }
        g_allocs[(u64)m] = n;
        reply = fmt("%lx", (unsigned long)(u64)m);
        return After::Reply;
    }
    if (starts("_m")) {
        u64 a = num(p, nullptr, 2);
        auto it = g_allocs.find(a);
        if (it == g_allocs.end()) { reply = "E07"; return After::Reply; }
        munmap((void *)a, it->second);
        g_allocs.erase(it);
        reply = "OK";
        return After::Reply;
    }
    if (p == "vCont?") { reply = "vCont;c;C;s;S"; return After::Reply; }
    if (starts("vCont;") || p[0] == 'c' || p[0] == 'C' || p[0] == 's' || p[0] == 'S') {
        std::map<int, int> acts;
        int dflt = kHold;
        if (starts("vCont;")) {
            size_t i = 6;
            while (i < p.size()) {
                size_t e = p.find(';', i);
                std::string a = p.substr(i, e == std::string::npos ? std::string::npos : e - i);
                i = e == std::string::npos ? p.size() : e + 1;
                if (a.empty()) continue;
                const int act = (a[0] == 's' || a[0] == 'S') ? kStep : kCont;
                size_t colon = a.find(':');
                if (colon == std::string::npos) dflt = act;
                else acts[(int)num(a, nullptr, colon + 1)] = act;
            }
        } else if (p[0] == 'c' || p[0] == 'C') dflt = kCont;
        else acts[g_sel] = kStep;  // s: the selected thread, the others stay
        resume(acts, dflt);
        return After::Resume;
    }
    if (p[0] == 'D') { reply = "OK"; return After::Detach; }
    if (p[0] == 'k') return After::Kill;
    return After::Reply;  // unsupported: empty reply
}

void detach() {
    bp_clear_all();
    g_dbg_on.store(0);
    {
        std::lock_guard<std::mutex> l(g_thr_mu);
        for (GuestThread *t : threads()) { t->dbg_action.store(kCont); t->dbg_reason = kNone; t->dbg_step_pending = false; }
    }
    g_stop.store(0);
    wake_all();
}

void session(int fd) {
    Io io{fd};
    g_session_fd.store(fd);
    g_dbg_on.store(1);
    stop_all();
    logf("debug: debugger attached");
    std::string p, reply;
    for (;;) {
        int r = io_get(io, p, -1);
        if (r < 0) break;
        if (r == 2) { reply = report_stop(0x11); io_send(io, reply); continue; }
        if (r == 0) continue;
        After a = handle(io, p, reply);
        if (a == After::Kill) {
            io_send(io, "X09");
            logf("debug: killed by the debugger");
            kill(getpid(), SIGKILL);
            for (;;) pause();
        }
        if (a == After::Detach) { io_send(io, reply); break; }
        if (a == After::Reply) { if (reply != "\x01" && !io_send(io, reply)) break; continue; }
        // resumed: wait for a stop (a thread's report), an interrupt or the end of the connection
        bool closed = false;
        for (;;) {
            pollfd pf[2] = {{fd, POLLIN, 0}, {g_evfd, POLLIN, 0}};
            if (io.in.empty() && poll(pf, 2, -1) < 0 && errno != EINTR) { closed = true; break; }
            if (!io.in.empty() || (pf[0].revents & (POLLIN | POLLHUP | POLLERR))) {
                int k = io_get(io, p, 0);
                if (k < 0) { closed = true; break; }
                if (k == 2) { stop_all(); io_send(io, report_stop(0x11)); break; }
                // lldb sends nothing else while running
            }
            if (pf[1].revents & POLLIN) {
                uint64_t v;
                if (read(g_evfd, &v, 8) != 8) {}
                stop_all();
                io_send(io, report_stop(0));
                break;
            }
        }
        if (closed) break;
    }
    g_session_fd.store(-1);
    detach();
    logf("debug: debugger detached");
}

void serve(int lfd) {
    g_non_darling_thread = true;
    g_log_linux_path = true;
    for (;;) {
        int fd = accept4(lfd, nullptr, nullptr, SOCK_CLOEXEC);
        if (fd < 0) { if (errno == EINTR) continue; usleep(100000); continue; }
        session(fd);
        close(fd);
    }
}
}  // namespace

// /run/user/<uid>, or X86SIM_A64_DEBUG_DIR (a rootless Darling's uid 0 has none)
std::string dbg_dir() {  // also prof.cpp's request files
    if (const char *d = getenv("X86SIM_A64_DEBUG_DIR"); d && *d) return d;
    return "/run/user/" + std::to_string((int)getuid());
}

// The socket and its server thread (once per process). wait: the main thread parks before running guest code.
void dbg_start(bool wait) {
    bool exp = false;
    if (!g_started.compare_exchange_strong(exp, true)) return;
    const char *udid = getenv("SIMULATOR_UDID");
    char path[108];
    snprintf(path, sizeof path, "%s/x86sim-dbg-%s-%d.sock", dbg_dir().c_str(), udid && *udid ? udid : "none", (int)getpid());
    g_sock_path = path;
    g_evfd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    int lfd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    sockaddr_un sa{};
    sa.sun_family = AF_UNIX;
    snprintf(sa.sun_path, sizeof sa.sun_path, "%s", path);
    unlink(path);
    if (lfd < 0 || bind(lfd, (sockaddr *)&sa, sizeof sa) != 0 || listen(lfd, 2) != 0) {
        logf("debug: cannot listen on %s: %s", path, strerror(errno));
        if (lfd >= 0) close(lfd);
        return;
    }
    chmod(path, 0600);
    if (wait) { g_dbg_on.store(1); g_stop.store(1); }
    sigset_t all, old;  // native threads block every signal (gheap.cpp start_scavenger)
    sigfillset(&all);
    pthread_sigmask(SIG_SETMASK, &all, &old);
    std::thread([lfd] { serve(lfd); }).detach();
    pthread_sigmask(SIG_SETMASK, &old, nullptr);
    logf("debug: listening on %s%s", path, wait ? " (waiting for the debugger)" : "");
}

// X86SIM_A64_DEBUG=1|wait (a64_host_main)
void dbg_init_from_env() {
    const char *e = getenv("X86SIM_A64_DEBUG");
    if (!e || !*e || !strcmp(e, "0")) return;
    dbg_start(!strcmp(e, "wait"));
}

// SIGUSR1 (host.cpp memlog thread): a request file for this process starts the stub. True if it was one.
bool dbg_check_request() {
    const char *udid = getenv("SIMULATOR_UDID");
    char path[300];
    snprintf(path, sizeof path, "%s/x86sim-dbg-req-%s-%d", dbg_dir().c_str(), udid && *udid ? udid : "none", (int)getpid());
    if (access(path, F_OK) != 0) return false;
    unlink(path);
    dbg_start(false);
    return true;
}

// The process is going away (a crash after the debugger let it continue): tell lldb.
void dbg_exiting(int linux_sig) {
    if (!g_sock_path.empty()) unlink(g_sock_path.c_str());  // (a SIGKILL leaves it: x86sim debug prunes those)
    int fd = g_session_fd.load();
    if (fd < 0) return;
    static const int darwin[32] = {0, 1, 2, 3, 4, 5, 6, 10, 8, 9, 30, 11, 31, 13, 14, 15};
    Io io{fd, true};
    io_send(io, fmt("X%02x", linux_sig > 0 && linux_sig < 16 ? darwin[linux_sig] : linux_sig));
}

void dbg_exit_code(int status) {
    if (!g_sock_path.empty()) unlink(g_sock_path.c_str());
    int fd = g_session_fd.load();
    if (fd < 0) return;
    Io io{fd, true};
    io_send(io, fmt("W%02x", status & 0xff));
}

}  // namespace a64
