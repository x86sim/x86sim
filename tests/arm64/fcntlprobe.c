// tests/arm64/fcntlprobe.c: fcntl commands whose Darling emulation came from upstream (2026-11-28 sync), against what
// macOS 27 answers (measured on the Mac with the same program):
//   F_RDAHEAD with 0, 1, 2, -1, 100: all 0 (XNU: any nonzero value turns read-ahead on; upstream refused other values)
//   F_SETNOSIGPIPE on a pipe: 0, F_GETNOSIGPIPE then 1, and a write after the reader closed fails with EPIPE without
//   SIGPIPE (XNU keeps the flag on the open file; Darling kept it only for sockets)
//   F_FULLFSYNC on a file: 0
// Prints one line per check; "BAD" lines are differences. Exit status: the number of BAD lines.
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

static volatile sig_atomic_t sigpipes;
static void on_sigpipe(int s) { (void)s; sigpipes++; }
static int bad;
#define CHECK(ok, ...) do { printf("%s ", (ok) ? "ok " : "BAD"); printf(__VA_ARGS__); printf("\n"); if (!(ok)) bad++; } while (0)

int main(void) {
    char path[] = "/tmp/fcntlprobe.XXXXXX";
    int fd = mkstemp(path);
    long vals[] = {0, 1, 2, -1, 100};
    for (int i = 0; i < 5; i++) {
        errno = 0;
        int r = fcntl(fd, F_RDAHEAD, vals[i]);
        CHECK(r == 0, "F_RDAHEAD %ld -> %d errno %d (macOS: 0)", vals[i], r, errno);
    }
    errno = 0;
    int r = fcntl(fd, F_FULLFSYNC);
    CHECK(r == 0, "F_FULLFSYNC -> %d errno %d (macOS: 0)", r, errno);
    close(fd);
    unlink(path);

    signal(SIGPIPE, on_sigpipe);
    int p[2];
    pipe(p);
    r = fcntl(p[1], F_SETNOSIGPIPE, 1);
    int g = fcntl(p[1], F_GETNOSIGPIPE);
    CHECK(r == 0 && g == 1, "pipe F_SETNOSIGPIPE -> %d, F_GETNOSIGPIPE -> %d (macOS: 0, 1)", r, g);
    int other = fcntl(p[0], F_GETNOSIGPIPE);
    CHECK(other == 0, "the read end F_GETNOSIGPIPE -> %d (macOS: 0)", other);
    close(p[0]);
    errno = 0;
    ssize_t w = write(p[1], "x", 1);
    int e = errno;
    CHECK(w == -1 && e == EPIPE && sigpipes == 0, "write with no reader -> %zd errno %d, %d SIGPIPE (macOS: -1, EPIPE, 0)",
          w, e, (int)sigpipes);
    // without the flag Linux and XNU both raise SIGPIPE
    int q[2];
    pipe(q);
    close(q[0]);
    errno = 0;
    w = write(q[1], "x", 1);
    e = errno;
    CHECK(w == -1 && e == EPIPE && sigpipes == 1, "no flag: write -> %zd errno %d, %d SIGPIPE (macOS: -1, EPIPE, 1)", w, e,
          (int)sigpipes);
    // the flag off again
    r = fcntl(p[1], F_SETNOSIGPIPE, 0);
    g = fcntl(p[1], F_GETNOSIGPIPE);
    CHECK(r == 0 && g == 0, "F_SETNOSIGPIPE 0 -> %d, F_GETNOSIGPIPE -> %d (macOS: 0, 0)", r, g);
    printf("fcntlprobe: %d BAD\n", bad);
    return bad;
}
