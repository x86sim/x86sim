// Build: xcrun -sdk iphonesimulator clang -target arm64-apple-ios17.0-simulator -O1 -o threadstack.sim threadstack.c
// Run: tests/arm64/run.sh tests/arm64/threadstack.sim   (PASS / FAIL; Safari Web Workers depend on it)
// Does pthread_get_stackaddr_np(child), called by the parent, describe the stack the child actually runs on?
// (WebKit's WTF::Thread::create does this; JSC worker VMs then check StackBounds::contains.)
#include <pthread.h>
#include <stdio.h>
#include <stdint.h>
#include <unistd.h>
static volatile int go;
static volatile uintptr_t child_sp, child_self_addr; static volatile size_t child_self_size;
static void *fn(void *a) {
    int local;
    child_self_addr = (uintptr_t)pthread_get_stackaddr_np(pthread_self());
    child_self_size = pthread_get_stacksize_np(pthread_self());
    __atomic_store_n(&child_sp, (uintptr_t)&local, __ATOMIC_RELEASE);
    while (!go) usleep(1000);
    return 0;
}
static void *fn_exit(void *a) {
    fn(a);
    pthread_exit(0);
}
int main(void) {
    int bad = 0;
    for (int k = 0; k < 2; k++) {
        pthread_t t; pthread_attr_t at; pthread_attr_init(&at);
        if (k) pthread_attr_setstacksize(&at, 4 << 20);
        go = 0; child_sp = 0;
        pthread_create(&t, &at, fn, 0);
        uintptr_t pa = (uintptr_t)pthread_get_stackaddr_np(t); size_t ps = pthread_get_stacksize_np(t);
        printf("parent asked right after pthread_create: size %zu\n", ps);
        while (!child_sp) usleep(1000);
        int in_parent = child_sp < pa && child_sp >= pa - ps;
        int in_self = child_sp < child_self_addr && child_sp >= child_self_addr - child_self_size;
        printf("%s: child sp %#lx | parent view [%#lx-%#lx) %s | child view [%#lx-%#lx) %s\n", k ? "4MB attr" : "default",
               (unsigned long)child_sp, (unsigned long)(pa - ps), (unsigned long)pa, in_parent ? "OK" : "WRONG",
               (unsigned long)(child_self_addr - child_self_size), (unsigned long)child_self_addr, in_self ? "OK" : "WRONG");
        bad += !in_parent + !in_self;
        go = 1; pthread_join(t, 0);
    }
    // A pthread_t is reused once its thread is joined: the parent's view of the new thread must be the new thread's
    // stack, never what an earlier thread with that handle left in the translator's table (half of them end through
    // pthread_exit, the longjmp path). Sizes alternate so a stale answer shows.
    int reused = 0, wrong = 0; pthread_t prev = 0;
    for (int k = 0; k < 40; k++) {
        pthread_t t; pthread_attr_t at; pthread_attr_init(&at);
        pthread_attr_setstacksize(&at, (k & 1 ? 2 : 6) << 20);
        go = 0; child_sp = 0;
        pthread_create(&t, &at, k & 2 ? fn_exit : fn, 0);
        reused += t == prev;
        uintptr_t pa = (uintptr_t)pthread_get_stackaddr_np(t); size_t ps = pthread_get_stacksize_np(t);
        while (!child_sp) usleep(1000);
        wrong += !(child_sp < pa && child_sp >= pa - ps) || ps != (size_t)((k & 1 ? 2 : 6) << 20);
        go = 1; pthread_join(t, 0);
        prev = t;
    }
    printf("handle reuse: %d of 40 creations reused the previous handle, %d wrong parent views %s\n", reused, wrong, wrong ? "WRONG" : "OK");
    bad += wrong;
    puts(bad ? "FAIL" : "PASS");
    return bad != 0;
}
