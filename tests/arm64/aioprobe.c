// x86Sim (2026-11-24): POSIX asynchronous I/O, as XNU's bsd/kern/kern_aio.c answers it. WebKit's GPU process writes its
// media byte cache with aio_write (MediaToolbox); Darling had no AIO syscalls (ENOSYS), so Safari played only the first
// <video>/<audio> of a session. Darling now completes each request when it is submitted; this checks that callers
// see what they see on macOS / Apple's simulator: aio_error/aio_return per request (once), aio_suspend, lio_listio
// (LIO_WAIT, LIO_NOWAIT + a completion signal), SIGEV_SIGNAL, the per-process limit (256, EAGAIN beyond), invalid
// requests (their errno; lio_listio: the call fails, nothing submitted), aio_cancel (AIO_ALLDONE), aio_fsync, and a thread in aio_suspend woken by another's submission.
// Usage: aioprobe.sim [dir]   (default $TMPDIR or /tmp). Prints "aioprobe: N checks, F failed"; exit 0 = all passed.
#include <aio.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int checks, failed;
#define CHECK(cond, ...) do { checks++; if (!(cond)) { failed++; printf("FAIL %s:%d: ", __func__, __LINE__); \
	printf(__VA_ARGS__); printf(" (errno %d)\n", errno); } } while (0)

static volatile sig_atomic_t usr1, usr2;
static void on_usr1(int s) { (void) s; usr1++; }
static void on_usr2(int s) { (void) s; usr2++; }

static void wait_done(struct aiocb* cb)
{
	const struct aiocb* list[1] = { cb };
	while (aio_error(cb) == EINPROGRESS)
		aio_suspend(list, 1, NULL);
}

static void setcb(struct aiocb* cb, int fd, void* buf, size_t n, off_t off)
{
	memset(cb, 0, sizeof(*cb));
	cb->aio_fildes = fd;
	cb->aio_buf = buf;
	cb->aio_nbytes = n;
	cb->aio_offset = off;
	cb->aio_sigevent.sigev_notify = SIGEV_NONE;
}

static void basic(int fd)
{
	static char out[4096], in[4096];
	struct aiocb cb;

	for (int i = 0; i < (int) sizeof(out); i++)
		out[i] = (char) (i * 7 + 3);
	setcb(&cb, fd, out, sizeof(out), 100);
	CHECK(aio_write(&cb) == 0, "aio_write");
	wait_done(&cb);
	CHECK(aio_error(&cb) == 0, "aio_error after write = %d", aio_error(&cb));
	ssize_t r = aio_return(&cb);
	CHECK(r == (ssize_t) sizeof(out), "aio_return write = %zd", r);
	errno = 0;
	r = aio_return(&cb);
	CHECK(r == -1 && errno == EINVAL, "second aio_return = %zd", r);
	errno = 0;
	CHECK(aio_error(&cb) == -1 && errno == EINVAL, "aio_error after aio_return");

	setcb(&cb, fd, in, sizeof(in), 100);
	CHECK(aio_read(&cb) == 0, "aio_read");
	wait_done(&cb);
	CHECK(aio_error(&cb) == 0, "aio_error after read");
	r = aio_return(&cb);
	CHECK(r == (ssize_t) sizeof(in) && memcmp(in, out, sizeof(in)) == 0, "aio_read data, return %zd", r);

	// read past the end: 0 bytes, no error
	setcb(&cb, fd, in, sizeof(in), 1 << 20);
	CHECK(aio_read(&cb) == 0, "aio_read at EOF");
	wait_done(&cb);
	CHECK(aio_error(&cb) == 0 && aio_return(&cb) == 0, "aio_read at EOF result");

	setcb(&cb, fd, NULL, 0, 0);
	CHECK(aio_fsync(O_SYNC, &cb) == 0, "aio_fsync O_SYNC");
	wait_done(&cb);
	CHECK(aio_error(&cb) == 0 && aio_return(&cb) == 0, "aio_fsync result");
	errno = 0;
	CHECK(aio_fsync(12345, &cb) == -1 && errno == EINVAL, "aio_fsync bad op");

	// aio_cancel(fd, NULL) with only done requests of fd: AIO_ALLDONE (with some in flight, Apple's answer depends
	// on timing: not checked)
	setcb(&cb, fd, in, 8, 0);
	CHECK(aio_read(&cb) == 0, "aio_read");
	wait_done(&cb);
	int c = aio_cancel(fd, NULL);
	CHECK(c == AIO_ALLDONE, "aio_cancel of the descriptor's done requests: %d", c);
	CHECK(aio_return(&cb) == 8, "its result");
}

static void invalid(int fd, const char* path)
{
	static char buf[16];
	struct aiocb cb;
	int p[2];

	// no requests at all: aio_error/aio_suspend EINVAL, aio_cancel AIO_ALLDONE
	setcb(&cb, fd, buf, sizeof(buf), 0);
	errno = 0;
	CHECK(aio_error(&cb) == -1 && errno == EINVAL, "aio_error of an unknown aiocb");
	const struct aiocb* list[1] = { &cb };
	errno = 0;
	CHECK(aio_suspend(list, 1, NULL) == -1 && errno == EINVAL, "aio_suspend with no requests");
	CHECK(aio_cancel(fd, NULL) == AIO_ALLDONE, "aio_cancel with no requests");

	setcb(&cb, 9999, buf, sizeof(buf), 0);
	errno = 0;
	CHECK(aio_write(&cb) == -1 && errno == EBADF, "aio_write bad fd");
	setcb(&cb, fd, buf, sizeof(buf), -1);
	errno = 0;
	CHECK(aio_write(&cb) == -1 && errno == EINVAL, "aio_write negative offset");
	setcb(&cb, fd, NULL, sizeof(buf), 0);
	errno = 0;
	CHECK(aio_read(&cb) == -1 && errno == EINVAL, "aio_read NULL buffer");
	setcb(&cb, fd, buf, sizeof(buf), 0);
	cb.aio_sigevent.sigev_notify = SIGEV_SIGNAL;
	cb.aio_sigevent.sigev_signo = SIGKILL;
	errno = 0;
	CHECK(aio_write(&cb) == -1 && errno == EINVAL, "aio_write SIGEV_SIGNAL SIGKILL");
	cb.aio_sigevent.sigev_notify = SIGEV_THREAD;
	errno = 0;
	CHECK(aio_write(&cb) == -1 && errno == EINVAL, "aio_write SIGEV_THREAD");

	CHECK(pipe(p) == 0, "pipe");
	setcb(&cb, p[1], buf, sizeof(buf), 0);
	errno = 0;
	CHECK(aio_write(&cb) == -1 && errno == ESPIPE, "aio_write to a pipe");
	close(p[0]);
	close(p[1]);

	CHECK(pwrite(fd, "0123456789abcdef", 16, 0) == 16, "pwrite");
	int ro = open(path, O_RDONLY);
	setcb(&cb, ro, buf, sizeof(buf), 0);
	errno = 0;
	CHECK(aio_write(&cb) == -1 && errno == EBADF, "aio_write to a read-only descriptor");
	CHECK(aio_read(&cb) == 0, "aio_read from a read-only descriptor");
	wait_done(&cb);
	CHECK(aio_return(&cb) == sizeof(buf), "aio_read from a read-only descriptor result");
	close(ro);
}

#define LIMIT 256 // kern.aioprocmax on macOS 26/27 (the Mac mini, where Apple's simulator runs)
static void limit(int fd)
{
	static char buf[LIMIT + 1][8];
	static struct aiocb cbs[LIMIT + 1];
	int ok = 0;

	for (int i = 0; i < LIMIT; i++)
	{
		setcb(&cbs[i], fd, buf[i], sizeof(buf[i]), i * 8);
		ok += aio_write(&cbs[i]) == 0;
	}
	CHECK(ok == LIMIT, "%d requests not reaped: %d submitted", LIMIT, ok);
	setcb(&cbs[LIMIT], fd, buf[0], sizeof(buf[0]), 0);
	errno = 0;
	int r = aio_write(&cbs[LIMIT]);
	CHECK(r == -1 && errno == EAGAIN, "one more request: %d", r);

	// aio_cancel of a done request: AIO_ALLDONE; a descriptor mismatch: EBADF
	r = aio_cancel(fd, &cbs[3]);
	CHECK(r == AIO_ALLDONE, "aio_cancel of a done request: %d", r);
	errno = 0;
	r = aio_cancel(fd + 100, &cbs[3]);
	CHECK(r == -1 && errno == EBADF, "aio_cancel with another descriptor: %d", r);

	// aio_suspend: returns at once with one done in the list, times out (EAGAIN) with none
	const struct aiocb* list[2] = { NULL, &cbs[5] };
	CHECK(aio_suspend(list, 2, NULL) == 0, "aio_suspend, one done");
	list[1] = &cbs[LIMIT];
	struct timespec ts = { 0, 50 * 1000 * 1000 };
	errno = 0;
	r = aio_suspend(list, 2, &ts);
	CHECK(r == -1 && errno == EAGAIN, "aio_suspend timeout: %d", r);

	int reaped = 0;
	for (int i = 0; i < LIMIT; i++)
	{
		wait_done(&cbs[i]);
		reaped += aio_error(&cbs[i]) == 0 && aio_return(&cbs[i]) == 8;
	}
	CHECK(reaped == LIMIT, "%d reaped: %d", LIMIT, reaped);
	CHECK(aio_write(&cbs[LIMIT]) == 0, "a request after reaping");
	wait_done(&cbs[LIMIT]);
	CHECK(aio_return(&cbs[LIMIT]) == 8, "its result");
}

static void listio(int fd)
{
	static char a[64], b[64], c[64];
	struct aiocb ca, cb, cc;
	struct aiocb* list[4];

	memset(a, 'a', sizeof(a));
	memset(b, 'b', sizeof(b));
	setcb(&ca, fd, a, sizeof(a), 10000);
	ca.aio_lio_opcode = LIO_WRITE;
	setcb(&cb, fd, b, sizeof(b), 10064);
	cb.aio_lio_opcode = LIO_WRITE;
	setcb(&cc, fd, c, sizeof(c), 0);
	cc.aio_lio_opcode = LIO_NOP;
	list[0] = &ca; list[1] = NULL; list[2] = &cb; list[3] = &cc;
	CHECK(lio_listio(LIO_WAIT, list, 4, NULL) == 0, "lio_listio LIO_WAIT");
	CHECK(aio_error(&ca) == 0 && aio_return(&ca) == 64, "lio write 1");
	CHECK(aio_error(&cb) == 0 && aio_return(&cb) == 64, "lio write 2");
	int e = aio_error(&cc);
	CHECK(e == 0 && aio_return(&cc) == 0, "LIO_NOP request error = %d", e);

	// read both back with LIO_NOWAIT and a completion signal
	static char ra[64], rb[64];
	setcb(&ca, fd, ra, sizeof(ra), 10000);
	ca.aio_lio_opcode = LIO_READ;
	setcb(&cb, fd, rb, sizeof(rb), 10064);
	cb.aio_lio_opcode = LIO_READ;
	list[0] = &ca; list[1] = &cb;
	struct sigevent sev = { .sigev_notify = SIGEV_SIGNAL, .sigev_signo = SIGUSR1 };
	usr1 = 0;
	CHECK(lio_listio(LIO_NOWAIT, list, 2, &sev) == 0, "lio_listio LIO_NOWAIT");
	wait_done(&ca);
	wait_done(&cb);
	for (int i = 0; i < 200 && usr1 == 0; i++)
		usleep(5000);
	CHECK(usr1 == 1, "lio_listio completion signal: %d", (int) usr1);
	CHECK(aio_return(&ca) == 64 && aio_return(&cb) == 64 && memcmp(ra, a, 64) == 0 && memcmp(rb, b, 64) == 0,
		"lio reads");

	// an invalid entry fails the whole call with EAGAIN, submitting nothing
	setcb(&ca, fd, ra, sizeof(ra), 10000);
	ca.aio_lio_opcode = LIO_READ;
	setcb(&cb, 9999, rb, sizeof(rb), 0);
	cb.aio_lio_opcode = LIO_READ;
	errno = 0;
	int r = lio_listio(LIO_WAIT, list, 2, NULL);
	int le = errno;
	CHECK(r == -1 && le == EBADF, "lio_listio with an invalid entry: %d errno %d", r, le);
	errno = 0;
	r = aio_error(&ca);
	CHECK(r == -1 && errno == EINVAL, "nothing submitted: aio_error %d", r);
	if (r != -1)
	{
		wait_done(&ca);
		aio_return(&ca);
	}
	errno = 0;
	CHECK(lio_listio(7, list, 2, NULL) == -1 && errno == EINVAL, "lio_listio bad mode");
	setcb(&cb, fd, rb, sizeof(rb), 0);
	cb.aio_lio_opcode = 7;
	errno = 0;
	r = lio_listio(LIO_WAIT, list, 2, NULL);
	le = errno;
	CHECK(r == -1 && le == EINVAL, "lio_listio with a bad opcode: %d errno %d", r, le);
	errno = 0;
	CHECK(aio_error(&ca) == -1 && errno == EINVAL, "nothing submitted");
}

static void sigev(int fd)
{
	static char buf[32];
	struct aiocb cb;

	setcb(&cb, fd, buf, sizeof(buf), 20000);
	cb.aio_sigevent.sigev_notify = SIGEV_SIGNAL;
	cb.aio_sigevent.sigev_signo = SIGUSR2;
	usr2 = 0;
	CHECK(aio_write(&cb) == 0, "aio_write SIGEV_SIGNAL");
	wait_done(&cb);
	for (int i = 0; i < 200 && usr2 == 0; i++)
		usleep(5000);
	CHECK(usr2 == 1, "completion signal: %d", (int) usr2);
	CHECK(aio_return(&cb) == 32, "its result");
}

static struct aiocb waited;
static void* waiter(void* arg)
{
	(void) arg;
	const struct aiocb* list[1] = { &waited };
	return (void*) (long) aio_suspend(list, 1, NULL);
}

static void threaded(int fd)
{
	static char buf[32], other[8];
	struct aiocb keep;
	pthread_t t;

	// the process needs a request for aio_suspend to wait at all
	setcb(&keep, fd, other, sizeof(other), 0);
	CHECK(aio_read(&keep) == 0, "a request kept");
	setcb(&waited, fd, buf, sizeof(buf), 30000);
	pthread_create(&t, NULL, waiter, NULL);
	usleep(100 * 1000);
	CHECK(aio_write(&waited) == 0, "aio_write while another thread waits");
	void* r;
	pthread_join(t, &r);
	CHECK((long) r == 0, "aio_suspend in another thread = %ld", (long) r);
	CHECK(aio_return(&waited) == 32 && aio_return(&keep) == 8, "results");
}

int main(int argc, char** argv)
{
	const char* dir = argc > 1 ? argv[1] : getenv("TMPDIR") ? getenv("TMPDIR") : "/tmp";
	char path[1024];
	snprintf(path, sizeof(path), "%s/aioprobe.%d", dir, (int) getpid());
	int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
	if (fd < 0)
	{
		perror(path);
		return 2;
	}
	signal(SIGUSR1, on_usr1);
	signal(SIGUSR2, on_usr2);

	invalid(fd, path);
	basic(fd);
	limit(fd);
	listio(fd);
	sigev(fd);
	threaded(fd);

	close(fd);
	unlink(path);
	printf("aioprobe: %d checks, %d failed\n", checks, failed);
	return failed != 0;
}
