// x86sim debug target (tests/lldb/run.sh): a loop calling dbg_target() once per 200 ms on the main thread and a
// worker thread, so lldb can attach, list threads, break on dbg_target, step, read `n` and `acc`, continue, detach.
// Exits after DBG_SECS seconds (default 60).
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

volatile long g_total;

__attribute__((noinline)) long dbg_target(int n) {
    long acc = 0;
    for (int i = 0; i <= n; i++) acc += i * 3;
    g_total += acc;
    return acc;
}

static void *worker(void *arg) {
    (void)arg;
    for (;;) { dbg_target(5); usleep(300000); }
    return NULL;
}

int main(void) {
    const char *s = getenv("DBG_SECS");
    const int secs = s ? atoi(s) : 60;
    pthread_t th;
    pthread_create(&th, NULL, worker, NULL);
    printf("dbgtarget: pid %d\n", (int)getpid());
    fflush(stdout);
    for (int k = 0; k < secs * 5; k++) {
        dbg_target(k % 10 + 1);
        usleep(200000);
    }
    printf("dbgtarget: total %ld\n", g_total);
    return 0;
}
