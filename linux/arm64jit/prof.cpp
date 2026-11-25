// libarm64jit: x86sim profile, a sampling CPU profiler of a guest process (Instruments' Time Profiler).
//
// Off unless asked for at run time, through the debugger's request channel (gdbstub.cpp): `x86sim profile` writes
// /run/user/<uid>/x86sim-prof-req-<UDID>-<pid> ("start <period us> <max s> <output path>" or "stop") and sends SIGUSR1;
// host.cpp's memlog thread reads it here. Nothing runs and nothing is registered while no profile is taken.
//
// Sampling: one perf_event_open software task-clock counter per guest thread (user mode only, allowed at
// perf_event_paranoid 2 for one's own threads), so a thread is sampled only while it runs on a CPU, once per period of
// its CPU time, and the kernel records the exact host instruction pointer. A sampler thread polls the ring buffers and,
// at each sample's wakeup, reads that thread's guest state (the thunk it is in, guest pc / lr / fp) and walks the guest
// frame-pointer chain (process_vm_readv: a bad frame pointer cannot fault). At stop, every distinct host address is
// resolved to the guest block that contains it (Jit::GuestBlockAtHostAddress: translated code), else it is host code:
// the native function of the thunk the thread was in, or the translator itself (dynarmic's compiler, the dispatcher,
// handlers). If perf_event_open is refused, the sampler falls back to polling the threads' last block PC.
//
// Output (text, written to "<path>.part" and renamed to <path> when complete; tools/launcher/core/src/profile.rs
// symbolicates it):
//   X86SIMPROF 1 / P <pid> <exe> / M perf|poll <period us> / U <udid>
//   S <time ns> <tid> <host ip> <thunk|-1|-2> <guest pc> <lr> <frame return addresses...>   (hex; -2: no guest call)
//   B <host ip> <guest block pc>   H <host ip> <host object or mapping (anon: the code cache)> [symbol]   N <thunk> <name>   T <tid> <name>
//   I <mach header> <Darwin path>  C <dyld cache Linux path>  E <samples> <lost>
#include "a64.h"
#include <atomic>
#include <cerrno>
#include <climits>
#include <csignal>
#include <cstdio>
#include <cstring>
#include <map>
#include <mutex>
#include <set>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>
#include <dirent.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <linux/perf_event.h>
#include <poll.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

namespace a64 {
std::string dbg_dir();                                                     // gdbstub.cpp
void for_each_guest_thread(void (*fn)(GuestThread *, void *), void *arg);  // signals.cpp
void a64_image_list(std::vector<std::pair<u64, std::string>> &out);        // host.cpp

namespace {
std::atomic<bool> g_active{false};  // a session runs (one at a time)
std::atomic<bool> g_stop{false};

struct Cfg { u32 period_us = 1000; u32 max_s = 600; std::string out; };

u64 now_ns() { timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts); return (u64)ts.tv_sec * 1000000000ull + ts.tv_nsec; }

u64 rd64(u64 a) {
    u64 v = 0;
    iovec lo{&v, 8}, re{reinterpret_cast<void *>(a), 8};
    return syscall(SYS_process_vm_readv, getpid(), &lo, 1, &re, 1, 0) == 8 ? v : 0;
}

// the guest's state of one thread, as one "S" line's tail (registry lock held)
void guest_state(GuestThread *t, char *&o, char *end) {
    const int depth = __atomic_load_n(&t->depth, __ATOMIC_RELAXED);
    if (!t->jit || depth <= 0) { o += snprintf(o, end - o, " -2"); return; }
    const int32_t th = __atomic_load_n(&t->in_thunk, __ATOMIC_RELAXED);
    const u64 pc = *t->st.pc, lr = t->x(30);
    o += snprintf(o, end - o, " %d %lx %lx", (int)th, (unsigned long)pc, (unsigned long)lr);
    u64 fp = t->x(29);
    for (int k = 0; k < 160 && fp > 0x10000 && !(fp & 7) && end - o > 24; k++) {
        const u64 ret = rd64(fp + 8) & 0x0000ffffffffffffull;  // (arm64e-style signed return addresses: no PAC here)
        if (!ret) break;
        o += snprintf(o, end - o, " %lx", (unsigned long)ret);
        const u64 next = rd64(fp);
        if (next <= fp || next - fp > (64ull << 20)) break;
        fp = next;
    }
}

// Linux thread names by the tid this process sees (Darling's pid namespace): /proc is the host's, so the tasks are
// found under the host pid and matched by NSpid
std::map<int, std::string> thread_names() {
    std::map<int, std::string> r;
    char self[64] = "";
    const ssize_t n = readlink("/proc/self", self, sizeof self - 1);
    if (n <= 0) return r;
    self[n] = 0;
    const std::string dir = std::string("/proc/") + self + "/task";
    DIR *d = opendir(dir.c_str());
    if (!d) return r;
    while (dirent *e = readdir(d)) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
        FILE *f = fopen((dir + "/" + e->d_name + "/status").c_str(), "r");
        if (!f) continue;
        char line[256], name[64] = "";
        int ns = atoi(e->d_name);
        while (fgets(line, sizeof line, f)) {
            if (!strncmp(line, "Name:", 5)) sscanf(line + 5, " %63[^\n]", name);
            if (!strncmp(line, "NSpid:", 6)) { const char *p = strrchr(line, '\t'); p = p ? p : strrchr(line, ' '); if (p) ns = atoi(p + 1); }
        }
        fclose(f);
        r[ns] = name;
    }
    closedir(d);
    return r;
}

struct Ev { int tid; int fd; uint8_t *ring; size_t size; GuestThread *t; bool live; };
constexpr size_t kRingPages = 16;  // data pages per thread (a power of two): 64 KB, 2000 samples

void run(Cfg c) {
    g_non_darling_thread = true;
    const std::string part = c.out + ".part";
    FILE *out = fopen(part.c_str(), "w");
    if (!out) { g_active = false; return; }
    static char buf[1 << 20];
    setvbuf(out, buf, _IOFBF, sizeof buf);
    const char *exe = getenv("X86SIM_A64_EXE"), *udid = getenv("SIMULATOR_UDID");
    const long page = sysconf(_SC_PAGESIZE);
    std::vector<Ev> evs;
    std::set<u64> ips;
    std::set<int> thunks, tids;
    u64 samples = 0, lost = 0;
    bool perf = true;
    int perf_err = 0;
    auto open_ev = [&](int tid) -> bool {
        perf_event_attr a{};
        a.size = sizeof a;
        a.type = PERF_TYPE_SOFTWARE;
        a.config = PERF_COUNT_SW_TASK_CLOCK;
        a.sample_period = (u64)c.period_us * 1000;
        a.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME;
        a.exclude_kernel = 1; a.exclude_hv = 1;
        a.wakeup_events = 1;
        a.use_clockid = 1; a.clockid = CLOCK_MONOTONIC;
        const int fd = (int)syscall(SYS_perf_event_open, &a, tid, -1, -1, PERF_FLAG_FD_CLOEXEC);
        if (fd < 0) { perf_err = errno; return false; }
        const size_t sz = (1 + kRingPages) * page;
        void *m = mmap(nullptr, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (m == MAP_FAILED) { perf_err = errno; close(fd); return false; }
        evs.push_back({tid, fd, (uint8_t *)m, sz, nullptr, true});
        return true;
    };
    // the registry's threads (lock held): new ones get a counter, gone ones are closed
    auto refresh = [&]() {
        for (Ev &e : evs) { e.t = nullptr; e.live = false; }
        struct A { std::vector<Ev> *evs; std::vector<std::pair<int, GuestThread *>> fresh; } a{&evs, {}};
        for_each_guest_thread([](GuestThread *t, void *p) {
            auto *a = (A *)p;
            if (!t->tid) return;
            for (Ev &e : *a->evs) if (e.tid == t->tid) { e.t = t; e.live = true; return; }
            a->fresh.push_back({t->tid, t});
        }, &a);
        for (auto &[tid, t] : a.fresh) {
            if (perf && !open_ev(tid)) {
                if (evs.empty()) perf = false;  // refused outright: poll instead
                continue;
            }
            if (!perf) evs.push_back({tid, -1, nullptr, 0, nullptr, true});
            evs.back().t = t; evs.back().live = true;
        }
        for (size_t i = 0; i < evs.size();) {
            if (evs[i].live) { i++; continue; }
            if (evs[i].ring) munmap(evs[i].ring, evs[i].size);
            if (evs[i].fd >= 0) close(evs[i].fd);
            evs.erase(evs.begin() + i);
        }
    };
    static char line[8192];
    auto sample = [&](u64 time, int tid, u64 ip, GuestThread *t) {
        char *o = line, *end = line + sizeof line - 32;
        o += snprintf(o, end - o, "S %lu %d %lx", (unsigned long)time, tid, (unsigned long)ip);
        const int32_t th = t ? __atomic_load_n(&t->in_thunk, __ATOMIC_RELAXED) : -2;
        if (t) guest_state(t, o, end); else o += snprintf(o, end - o, " -2");
        *o++ = '\n'; *o = 0;
        fputs(line, out);
        if (ip) ips.insert(ip);
        if (th >= 0) thunks.insert(th);
        tids.insert(tid);
        samples++;
    };
    {
        std::lock_guard<std::mutex> l(dbg_threads_mutex());
        refresh();
    }
    fprintf(out, "X86SIMPROF 1\nP %d %s\nM %s %u\nU %s\n", (int)getpid(), exe ? exe : "?", perf ? "perf" : "poll", c.period_us,
            udid && *udid ? udid : "-");
    if (!perf) fprintf(out, "W perf_event_open: %s\n", strerror(perf_err));
    const u64 t0 = now_ns();
    u64 last_refresh = t0;
    std::vector<pollfd> pfd;
    while (!g_stop.load() && now_ns() - t0 < (u64)c.max_s * 1000000000ull && ftell(out) < (512l << 20)) {
        if (!perf) {
            usleep(c.period_us);
            std::lock_guard<std::mutex> l(dbg_threads_mutex());
            if (now_ns() - last_refresh > 20000000) { refresh(); last_refresh = now_ns(); }
            for (Ev &e : evs)  // threads running guest code (no CPU time known): the last block PC, no host ip
                if (e.t && e.t->jit && __atomic_load_n(&e.t->depth, __ATOMIC_RELAXED) > 0 && __atomic_load_n(&e.t->in_thunk, __ATOMIC_RELAXED) < 0)
                    sample(now_ns(), e.tid, 0, e.t);
            continue;
        }
        pfd.clear();
        for (Ev &e : evs) pfd.push_back({e.fd, POLLIN, 0});
        poll(pfd.data(), pfd.size(), 20);
        std::lock_guard<std::mutex> l(dbg_threads_mutex());
        if (now_ns() - last_refresh > 20000000) { refresh(); last_refresh = now_ns(); }
        for (Ev &e : evs) {
            auto *mp = (perf_event_mmap_page *)e.ring;
            const u64 head = __atomic_load_n(&mp->data_head, __ATOMIC_ACQUIRE);
            u64 tail = mp->data_tail;
            const uint8_t *data = e.ring + page;
            const u64 dsz = kRingPages * page;
            while (tail < head) {
                uint8_t rec[64];
                perf_event_header h;
                for (size_t k = 0; k < sizeof h; k++) ((uint8_t *)&h)[k] = data[(tail + k) % dsz];
                if (h.size < sizeof h) { tail = head; break; }
                const size_t n = h.size < sizeof rec ? h.size : sizeof rec;
                for (size_t k = 0; k < n; k++) rec[k] = data[(tail + k) % dsz];
                tail += h.size;
                if (h.type == PERF_RECORD_SAMPLE && n >= 8 + 24) {
                    u64 ip, tm; uint32_t pt[2];
                    memcpy(&ip, rec + 8, 8); memcpy(pt, rec + 16, 8); memcpy(&tm, rec + 24, 8);
                    sample(tm, e.tid, ip, e.t);
                } else if (h.type == PERF_RECORD_LOST && n >= 24) {
                    u64 k; memcpy(&k, rec + 16, 8); lost += k;
                }
            }
            __atomic_store_n(&mp->data_tail, tail, __ATOMIC_RELEASE);
        }
    }
    for (Ev &e : evs) { if (e.ring) munmap(e.ring, e.size); if (e.fd >= 0) close(e.fd); }
    // host addresses: translated blocks (any Jit: the translations are shared), else the host object
    {
        std::lock_guard<std::mutex> l(dbg_threads_mutex());
        GuestThread *any = nullptr;
        for_each_guest_thread([](GuestThread *t, void *p) { if (t->jit && !*(GuestThread **)p) *(GuestThread **)p = t; }, &any);
        // the mappings, for host code dladdr does not know (Darling's Mach-O libraries, the code cache: "anon")
        std::vector<std::pair<std::pair<u64, u64>, std::string>> maps;
        if (FILE *m = fopen("/proc/self/maps", "r")) {
            char l[600];
            while (fgets(l, sizeof l, m)) {
                unsigned long lo, hi; int n = 0;
                if (sscanf(l, "%lx-%lx %*s %*s %*s %*s %n", &lo, &hi, &n) < 2) continue;
                std::string p = n > 0 ? l + n : "";
                while (!p.empty() && (p.back() == '\n' || p.back() == ' ')) p.pop_back();
                maps.push_back({{lo, hi}, p.empty() ? "anon" : p});
            }
            fclose(m);
        }
        for (u64 ip : ips) {
            std::optional<u64> b = any ? any->jit->GuestBlockAtHostAddress(ip) : std::nullopt;
            if (b) { fprintf(out, "B %lx %lx\n", (unsigned long)ip, (unsigned long)*b); continue; }
            Dl_info di{};
            const char *obj = dladdr((void *)ip, &di) && di.dli_fname ? di.dli_fname : "-";
            if (!strcmp(obj, "-") || !*obj)
                for (auto &[r, p] : maps) if (ip >= r.first && ip < r.second) { obj = p.c_str(); break; }
            if (!strcmp(obj, "-") || !*obj) obj = "anon";
            const char *b2 = strrchr(obj, '/');
            fprintf(out, "H %lx %s%s%s\n", (unsigned long)ip, b2 ? b2 + 1 : obj, di.dli_sname ? " " : "", di.dli_sname ? di.dli_sname : "");
        }
    }
    const uint32_t nt = thunk_count();
    for (int th : thunks) if ((uint32_t)th < nt) fprintf(out, "N %d %s\n", th, g_thunks[th].name.c_str());
    const auto names = thread_names();
    for (int tid : tids) {
        auto it = names.find(tid);
        fprintf(out, "T %d %s\n", tid, tid == (int)getpid() ? "main" : it != names.end() && !it->second.empty() ? it->second.c_str() : "-");
    }
    std::vector<std::pair<u64, std::string>> imgs;
    a64_image_list(imgs);
    for (auto &[mh, p] : imgs) fprintf(out, "I %lx %s\n", (unsigned long)mh, p.c_str());
    if (!jitcache_cache_file().empty()) fprintf(out, "C %s\n", jitcache_cache_file().c_str());
    fprintf(out, "E %lu %lu\n", (unsigned long)samples, (unsigned long)lost);
    const bool ok = fclose(out) == 0;
    if (ok) rename(part.c_str(), c.out.c_str()); else unlink(part.c_str());
    g_active = false;
}

void start(Cfg c) {
    bool exp = false;
    if (!g_active.compare_exchange_strong(exp, true)) return;  // one session at a time
    g_stop = false;
    sigset_t all, old;  // native threads block every signal (see gheap.cpp start_scavenger)
    sigfillset(&all);
    pthread_sigmask(SIG_SETMASK, &all, &old);
    std::thread(run, std::move(c)).detach();
    pthread_sigmask(SIG_SETMASK, &old, nullptr);
}
}  // namespace

// x86sim profile: a pending request for this process (the memlog thread, after a SIGUSR1)
bool prof_check_request() {
    const char *udid = getenv("SIMULATOR_UDID");
    char path[300];
    snprintf(path, sizeof path, "%s/x86sim-prof-req-%s-%d", dbg_dir().c_str(), udid && *udid ? udid : "none", (int)getpid());
    FILE *f = fopen(path, "r");
    if (!f) return false;
    char req[PATH_MAX + 64] = "";
    const size_t n = fread(req, 1, sizeof req - 1, f);
    req[n] = 0;
    fclose(f);
    unlink(path);
    Cfg c;
    char outp[PATH_MAX] = "";
    if (!strncmp(req, "stop", 4)) g_stop = true;
    else if (sscanf(req, "start %u %u %4095[^\n]", &c.period_us, &c.max_s, outp) == 3 && outp[0] == '/') {
        if (c.period_us < 100) c.period_us = 100;
        c.out = outp;
        start(std::move(c));
    }
    return true;
}

bool prof_active() { return g_active.load(std::memory_order_relaxed); }

}  // namespace a64
