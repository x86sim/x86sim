// x86sim profile test code (tests/profile/run.sh): known hot functions with frame pointers, so a profile has to show
// prof_leaf on top of the self time, under prof_hot, under prof_worker (the "prof.worker" thread).
#include <pthread.h>
#include <stdint.h>

volatile double g_prof_sink;
volatile long g_prof_rounds;

__attribute__((noinline)) double prof_leaf(double x) {
    for (int i = 0; i < 400; i++) x = x * 1.0000001 + 0.5;
    return x;
}

__attribute__((noinline)) double prof_hot(int n) {
    double a = 0;
    for (int i = 0; i < n; i++) a += prof_leaf((double)i);
    return a;
}

// a little work of its own (self time) plus calls into prof_hot
__attribute__((noinline)) void prof_round(void) {
    uint64_t h = 1469598103934665603ull;
    for (int i = 0; i < 20000; i++) h = (h ^ (uint64_t)i) * 1099511628211ull;
    g_prof_sink += prof_hot(2000) + (double)(h & 1);
    g_prof_rounds++;
}

static void *prof_worker(void *arg) {
    (void)arg;
    pthread_setname_np("prof.worker");
    for (;;) prof_round();
    return 0;
}

static void prof_start_worker(void) {
    pthread_t th;
    pthread_create(&th, 0, prof_worker, 0);
}
