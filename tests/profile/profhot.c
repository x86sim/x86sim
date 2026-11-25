// x86sim profile standalone target (tests/profile/run.sh --standalone, tests/arm64/run.sh): the "prof.worker" thread
// spins in prof_round / prof_hot / prof_leaf, the main thread sleeps. Exits after PROF_SECS seconds (default 30).
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include "profwork.h"

int main(void) {
    const char *s = getenv("PROF_SECS");
    const int secs = s ? atoi(s) : 30;
    prof_start_worker();
    printf("profhot: pid %d\n", (int)getpid());
    fflush(stdout);
    for (int i = 0; i < secs * 10; i++) usleep(100000);
    printf("profhot: %ld rounds\n", g_prof_rounds);
    return 0;
}
