// x86Sim (2026-11-24): Mach port notifications caused by another process's death. A survivor holding a send right to a
// port of a process that dies must get MACH_NOTIFY_DEAD_NAME (raw, and libdispatch's DISPATCH_MACH_SEND_DEAD); a port
// whose only sender dies MACH_NOTIFY_NO_SENDERS; a receive right a dying process holds, with a port-destroyed request,
// must come back in MACH_NOTIFY_PORT_DESTROYED; the dead process's task port and task name port become dead names; its
// exit is a DISPATCH_SOURCE_TYPE_PROC event; and XPC sees it both ways (a server's peer connection is invalidated, a
// client's connection interrupted). Rounds: the child forked or posix_spawned, exiting or SIGKILLed, idle or "busy"
// (its ports watched by libdispatch, a thread parked in a receive), then XPC.
// Why: a report against x86Sim 0.5.2 said dead-name notifications lost across processes (XPC interruption unnoticed,
// permission prompts of apps that quit staying up). Not reproduced, on 0.5.2's darlingserver or ours: every check
// passes in a bare container and inside a booted phone's container, and a watcher holding send rights to all 106
// receive rights of Maps got 106 dead-name notifications when Maps was terminated. The prompt that stays after its app
// quits (Maps' location prompt) stays on Apple's own simulator too. This stays as the regression test.
// The children get the parent's ports by task_for_pid + mach_port_extract_right (Darling's fork does not carry
// mach_ports_register's ports over). The in-process checks are the control.
// Usage: deadname.sim [fork|forkkill|spawn|spawnkill|forkbusy|forkkillbusy|spawnbusy|spawnkillbusy|xpc]   (default all)
//   prints one "DEADNAME: <check> ok|FAIL" line per check and "DEADNAME: N failed" (exit 0 if none)
//   tests/arm64/run.sh tests/arm64/deadname.sim; in a booted phone's container: A64_DPREFIX=<its prefix> run.sh ...
#include <crt_externs.h>
#include <dispatch/dispatch.h>
#include <xpc/xpc.h>
#include <mach/mach.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(x) do { kern_return_t kr_ = (x); if (kr_) { printf("DEADNAME: %s: %s (%d)\n", #x, mach_error_string(kr_), kr_); exit(2); } } while (0)

typedef struct {
	mach_msg_header_t h;
	mach_msg_body_t b;
	mach_msg_port_descriptor_t p[3];
} rights_msg_t;
typedef struct { rights_msg_t m; mach_msg_max_trailer_t t; } rights_rcv_t;
typedef union {
	mach_msg_header_t h;
	mach_dead_name_notification_t dn;
	mach_no_senders_notification_t ns;
	mach_port_destroyed_notification_t pd;
	char buf[256];
} notif_t;

static int failed;

static void result(const char *what, int ok) {
	printf("DEADNAME: %s %s\n", what, ok ? "ok" : "FAIL");
	if (!ok) failed++;
}

// waits up to ms for a notification with msgh_id == id on port; returns its message (in *out) or 0 on timeout
static int wait_notif(mach_port_t port, mach_msg_id_t id, notif_t *out, int ms) {
	for (;;) {
		memset(out, 0, sizeof *out);
		kern_return_t kr = mach_msg(&out->h, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, sizeof *out, port, ms, MACH_PORT_NULL);
		if (kr != KERN_SUCCESS) return 0;
		if (out->h.msgh_id == id) return 1;
		printf("DEADNAME: (unexpected message id %d)\n", out->h.msgh_id);
	}
}

static mach_port_t new_port(void) {
	mach_port_t p;
	CHECK(mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &p));
	return p;
}

static mach_port_t request(mach_port_t name, mach_msg_id_t what, mach_port_mscount_t sync, mach_port_t notify) {
	mach_port_t prev = MACH_PORT_NULL;
	CHECK(mach_port_request_notification(mach_task_self(), name, what, sync, notify, MACH_MSG_TYPE_MAKE_SEND_ONCE, &prev));
	return prev;
}

static void in_process(void) {
	notif_t n;
	mach_port_t notify = new_port();
	// dead name: a send right (same name as the receive right in one task) left behind when the receive right goes
	mach_port_t q = new_port();
	CHECK(mach_port_insert_right(mach_task_self(), q, q, MACH_MSG_TYPE_MAKE_SEND));
	request(q, MACH_NOTIFY_DEAD_NAME, 0, notify);
	CHECK(mach_port_mod_refs(mach_task_self(), q, MACH_PORT_RIGHT_RECEIVE, -1));
	result("in-process dead-name", wait_notif(notify, MACH_NOTIFY_DEAD_NAME, &n, 2000) && n.dn.not_port == q);
	mach_port_deallocate(mach_task_self(), q);
	// no senders: make a send right, request, drop it
	mach_port_t r = new_port();
	CHECK(mach_port_insert_right(mach_task_self(), r, r, MACH_MSG_TYPE_MAKE_SEND));
	request(r, MACH_NOTIFY_NO_SENDERS, 1, notify);
	CHECK(mach_port_deallocate(mach_task_self(), r));
	result("in-process no-senders", wait_notif(notify, MACH_NOTIFY_NO_SENDERS, &n, 2000));
	mach_port_mod_refs(mach_task_self(), r, MACH_PORT_RIGHT_RECEIVE, -1);
	mach_port_mod_refs(mach_task_self(), notify, MACH_PORT_RIGHT_RECEIVE, -1);
}

// the child: send the parent send rights to two ports of ours (dead-name by request, by libdispatch: one request per
// name, so a name each) and to an inbox, and wait for the parent's message, which carries a send right (no-senders) and
// a receive right with a port-destroyed request (port-destroyed); then exit, or wait to be killed
static void *park(void *arg) {
	rights_rcv_t r;
	mach_msg(&r.m.h, MACH_RCV_MSG, 0, sizeof r, (mach_port_t)(uintptr_t)arg, 0, MACH_PORT_NULL);
	return NULL;
}

static void child(pid_t parent, mach_port_t hello_name, int wait_kill, int busy) {
	mach_port_t ptask = MACH_PORT_NULL, hello = MACH_PORT_NULL;
	mach_msg_type_name_t t;
	CHECK(task_for_pid(mach_task_self(), parent, &ptask));
	CHECK(mach_port_extract_right(ptask, hello_name, MACH_MSG_TYPE_MAKE_SEND, &hello, &t));
	mach_port_deallocate(mach_task_self(), ptask);
	mach_port_t dn = new_port(), dd = new_port(), inbox = new_port();
	rights_msg_t m;
	memset(&m, 0, sizeof m);
	m.h.msgh_bits = MACH_MSGH_BITS_SET(MACH_MSG_TYPE_COPY_SEND, 0, 0, MACH_MSGH_BITS_COMPLEX);
	m.h.msgh_size = sizeof m;
	m.h.msgh_remote_port = hello;
	m.h.msgh_id = 1;
	m.b.msgh_descriptor_count = 3;
	m.p[0] = (mach_msg_port_descriptor_t){ .name = dn, .disposition = MACH_MSG_TYPE_MAKE_SEND, .type = MACH_MSG_PORT_DESCRIPTOR };
	m.p[1] = (mach_msg_port_descriptor_t){ .name = dd, .disposition = MACH_MSG_TYPE_MAKE_SEND, .type = MACH_MSG_PORT_DESCRIPTOR };
	m.p[2] = (mach_msg_port_descriptor_t){ .name = inbox, .disposition = MACH_MSG_TYPE_MAKE_SEND, .type = MACH_MSG_PORT_DESCRIPTOR };
	if (busy) {
		// as a real process: its ports watched by libdispatch (a kqchan in darlingserver) and a thread parked in a
		// receive (a microthread waiting in darlingserver) when it dies
		dispatch_source_t s = dispatch_source_create(DISPATCH_SOURCE_TYPE_MACH_RECV, dn, 0, dispatch_get_global_queue(0, 0));
		dispatch_source_set_event_handler(s, ^{});
		dispatch_resume(s);
		pthread_t th;
		pthread_create(&th, NULL, park, (void *)(uintptr_t)dd);
		usleep(100000);
	}
	CHECK(mach_msg(&m.h, MACH_SEND_MSG, sizeof m, 0, MACH_PORT_NULL, 0, MACH_PORT_NULL));
	rights_rcv_t r;
	memset(&r, 0, sizeof r);
	CHECK(mach_msg(&r.m.h, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, sizeof r, inbox, 10000, MACH_PORT_NULL));
	if (wait_kill) {
		// tell the parent we hold everything, then wait for SIGKILL
		CHECK(mach_msg(&r.m.h, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, sizeof r, inbox, 30000, MACH_PORT_NULL));
	}
	// hold everything and die: the kernel cleans up our space
	_exit(0);
}

// one round: mode "fork" / "forkkill" (fork()ed child exits / is SIGKILLed), "spawn" / "spawnkill" (posix_spawn of this
// binary, as launchd starts apps and XPC services)
static void scenario(const char *mode, const char *self) {
	int kill_it = strstr(mode, "kill") != NULL, spawned = strncmp(mode, "spawn", 5) == 0, busy = strstr(mode, "busy") != NULL;
	char what[128];
	mach_port_t hello = new_port(), notify = new_port();
	pid_t pid;
	if (spawned) {
		char a1[32], a2[32];
		snprintf(a1, sizeof a1, "%d", getpid());
		snprintf(a2, sizeof a2, "%u", hello);
		char *argv[] = { (char *)self, "child", a1, a2, kill_it ? "kill" : "exit", busy ? "busy" : "idle", NULL };
		int e = posix_spawn(&pid, self, NULL, NULL, argv, *_NSGetEnviron());
		if (e) { printf("DEADNAME: posix_spawn: %s\n", strerror(e)); exit(2); }
	} else {
		pid_t parent = getpid();
		pid = fork();
		if (pid < 0) { perror("fork"); exit(2); }
		if (pid == 0) child(parent, hello, kill_it, busy);
	}

	rights_rcv_t r;
	memset(&r, 0, sizeof r);
	CHECK(mach_msg(&r.m.h, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, sizeof r, hello, 30000, MACH_PORT_NULL));
	mach_port_t dn = r.m.p[0].name, dd = r.m.p[1].name, inbox = r.m.p[2].name;

	request(dn, MACH_NOTIFY_DEAD_NAME, 0, notify);
	dispatch_semaphore_t dsem = dispatch_semaphore_create(0);
	dispatch_queue_t q = dispatch_queue_create("deadname", NULL);
	dispatch_source_t ds = dispatch_source_create(DISPATCH_SOURCE_TYPE_MACH_SEND, dd, DISPATCH_MACH_SEND_DEAD, q);
	dispatch_source_set_event_handler(ds, ^{ dispatch_semaphore_signal(dsem); });
	dispatch_resume(ds);

	// the child's exit as a kevent (BaseBoard's BSProcessDeathWatcher, RunningBoard)
	dispatch_semaphore_t psem = dispatch_semaphore_create(0);
	dispatch_source_t ps = dispatch_source_create(DISPATCH_SOURCE_TYPE_PROC, pid, DISPATCH_PROC_EXIT, q);
	dispatch_source_set_event_handler(ps, ^{ dispatch_semaphore_signal(psem); });
	dispatch_resume(ps);

	// the child's task port and task name port: kernel ports that die with the task
	mach_port_t ctask = MACH_PORT_NULL, cname = MACH_PORT_NULL;
	CHECK(task_for_pid(mach_task_self(), pid, &ctask));
	CHECK(task_name_for_pid(mach_task_self(), pid, &cname));
	request(ctask, MACH_NOTIFY_DEAD_NAME, 0, notify);
	if (cname != ctask) request(cname, MACH_NOTIFY_DEAD_NAME, 0, notify);

	// no senders: a port whose only send right goes to the child
	mach_port_t lonely = new_port();
	request(lonely, MACH_NOTIFY_NO_SENDERS, 0, notify);
	// port destroyed: a receive right moved to the child, with a request to get it back
	mach_port_t boomerang = new_port();
	request(boomerang, MACH_NOTIFY_PORT_DESTROYED, 0, notify);

	rights_msg_t m;
	memset(&m, 0, sizeof m);
	m.h.msgh_bits = MACH_MSGH_BITS_SET(MACH_MSG_TYPE_COPY_SEND, 0, 0, MACH_MSGH_BITS_COMPLEX);
	m.h.msgh_size = sizeof m;
	m.h.msgh_remote_port = inbox;
	m.h.msgh_id = 2;
	m.b.msgh_descriptor_count = 2;
	m.p[0] = (mach_msg_port_descriptor_t){ .name = lonely, .disposition = MACH_MSG_TYPE_MAKE_SEND, .type = MACH_MSG_PORT_DESCRIPTOR };
	m.p[1] = (mach_msg_port_descriptor_t){ .name = boomerang, .disposition = MACH_MSG_TYPE_MOVE_RECEIVE, .type = MACH_MSG_PORT_DESCRIPTOR };
	CHECK(mach_msg(&m.h, MACH_SEND_MSG, sizeof m, 0, MACH_PORT_NULL, 0, MACH_PORT_NULL));
	if (kill_it) {
		usleep(300000); // the child has received the rights (its next receive waits for the kill)
		kill(pid, SIGKILL);
	}

	int st = 0;
	waitpid(pid, &st, 0);
	printf("DEADNAME: [%s] child exited (status 0x%x)\n", mode, st);

	// the notifications go to one port, in any order
	int got_dn = 0, got_ns = 0, got_pd = 0, got_task = 0, got_name = 0;
	notif_t n;
	for (int i = 0; i < 5; i++) {
		memset(&n, 0, sizeof n);
		if (mach_msg(&n.h, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, sizeof n, notify, 5000, MACH_PORT_NULL)) break;
		if (n.h.msgh_id == MACH_NOTIFY_DEAD_NAME && n.dn.not_port == dn) got_dn = 1;
		else if (n.h.msgh_id == MACH_NOTIFY_DEAD_NAME && n.dn.not_port == ctask) got_task = 1;
		else if (n.h.msgh_id == MACH_NOTIFY_DEAD_NAME && n.dn.not_port == cname) got_name = 1;
		else if (n.h.msgh_id == MACH_NOTIFY_NO_SENDERS) got_ns = 1;
		else if (n.h.msgh_id == MACH_NOTIFY_PORT_DESTROYED) got_pd = 1;
		else printf("DEADNAME: (unexpected message id %d)\n", n.h.msgh_id);
	}
	mach_port_type_t type = 0;
	mach_port_type(mach_task_self(), dn, &type);
#define R(s, ok) do { snprintf(what, sizeof what, "[%s] %s", mode, s); result(what, ok); } while (0)
	R("peer's port is a dead name", (type & MACH_PORT_TYPE_DEAD_NAME) != 0);
	R("dead-name", got_dn);
	R("DISPATCH_MACH_SEND_DEAD", dispatch_semaphore_wait(dsem, dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC)) == 0);
	R("no-senders", got_ns);
	R("port-destroyed", got_pd);
	R("task port dead-name", got_task);
	R("DISPATCH_PROC_EXIT", dispatch_semaphore_wait(psem, dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC)) == 0);
	dispatch_source_cancel(ps);
	R("task name port dead-name", got_name || cname == ctask);
	dispatch_source_cancel(ds);
}


// XPC across processes, both ways: the parent listens on an anonymous connection and the child connects (the parent's
// peer connection must get XPC_ERROR_CONNECTION_INVALID when the child dies: tccd noticing that an app quit), and the
// child listens and the parent connects (the parent's connection must get XPC_ERROR_CONNECTION_INTERRUPTED: a client
// noticing that its service died). The listeners' ports go across as Mach send rights (task_for_pid +
// mach_port_extract_right, both ways).
extern mach_port_t xpc_endpoint_copy_listener_port_4sim(xpc_endpoint_t);
extern xpc_endpoint_t xpc_endpoint_create_mach_port_4sim(mach_port_t);

static xpc_connection_t listen_anon(dispatch_queue_t q, void (^on_peer_error)(xpc_object_t)) {
	xpc_connection_t l = xpc_connection_create(NULL, q);
	xpc_connection_set_event_handler(l, ^(xpc_object_t peer) {
		if (xpc_get_type(peer) != XPC_TYPE_CONNECTION) return;
		xpc_connection_set_event_handler(peer, ^(xpc_object_t ev) {
			if (xpc_get_type(ev) == XPC_TYPE_ERROR) { if (on_peer_error) on_peer_error(ev); return; }
			xpc_object_t reply = xpc_dictionary_create_reply(ev);
			if (reply) { xpc_dictionary_set_int64(reply, "ok", 1); xpc_connection_send_message(peer, reply); }
		});
		xpc_connection_resume(peer);
	});
	xpc_connection_resume(l);
	return l;
}

static xpc_connection_t connect_port(mach_port_t port, dispatch_queue_t q, void (^on_error)(xpc_object_t)) {
	xpc_connection_t c = xpc_connection_create_from_endpoint(xpc_endpoint_create_mach_port_4sim(port));
	xpc_connection_set_target_queue(c, q);
	xpc_connection_set_event_handler(c, ^(xpc_object_t ev) {
		if (xpc_get_type(ev) == XPC_TYPE_ERROR && on_error) on_error(ev);
	});
	xpc_connection_resume(c);
	return c;
}

static int ping(xpc_connection_t c) {
	xpc_object_t m = xpc_dictionary_create(NULL, NULL, 0);
	xpc_object_t r = xpc_connection_send_message_with_reply_sync(c, m);
	return xpc_get_type(r) == XPC_TYPE_DICTIONARY && xpc_dictionary_get_int64(r, "ok") == 1;
}

static void xpc_child(pid_t parent, mach_port_t listener_name) {
	mach_port_t ptask = MACH_PORT_NULL, port = MACH_PORT_NULL;
	mach_msg_type_name_t t;
	CHECK(task_for_pid(mach_task_self(), parent, &ptask));
	CHECK(mach_port_extract_right(ptask, listener_name, MACH_MSG_TYPE_COPY_SEND, &port, &t));
	dispatch_queue_t q = dispatch_queue_create("child", NULL);
	xpc_connection_t c = connect_port(port, q, NULL);
	if (!ping(c)) { printf("DEADNAME: xpc child: no reply from the parent\n"); _exit(3); }
	// our listener, for the parent to connect to: the parent takes a send right to its port by name (task_for_pid +
	// mach_port_extract_right) from the line we print on the pipe that is our stdout
	xpc_connection_t l = listen_anon(q, NULL);
	mach_port_t mine = xpc_endpoint_copy_listener_port_4sim(xpc_endpoint_create(l));
	printf("XPCCHILD %u\n", mine);
	sleep(2);
	_exit(0);
}

static void xpc_scenario(const char *self) {
	dispatch_queue_t q = dispatch_queue_create("parent", NULL);
	dispatch_semaphore_t peer_gone = dispatch_semaphore_create(0), server_gone = dispatch_semaphore_create(0);
	__block int peer_err = 0, server_err = 0;
	xpc_connection_t l = listen_anon(q, ^(xpc_object_t ev) {
		if (ev == XPC_ERROR_CONNECTION_INVALID) { peer_err = 1; dispatch_semaphore_signal(peer_gone); }
	});
	mach_port_t lport = xpc_endpoint_copy_listener_port_4sim(xpc_endpoint_create(l));
	int fds[2];
	if (pipe(fds)) { perror("pipe"); exit(2); }
	char a1[32], a2[32];
	snprintf(a1, sizeof a1, "%d", getpid());
	snprintf(a2, sizeof a2, "%u", lport);
	char *argv[] = { (char *)self, "xpcchild", a1, a2, NULL };
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, fds[1], 1);
	posix_spawn_file_actions_addclose(&fa, fds[0]);
	pid_t pid;
	int e = posix_spawn(&pid, self, &fa, NULL, argv, *_NSGetEnviron());
	if (e) { printf("DEADNAME: posix_spawn: %s\n", strerror(e)); exit(2); }
	close(fds[1]);
	FILE *f = fdopen(fds[0], "r");
	char line[128];
	unsigned name = 0;
	while (fgets(line, sizeof line, f)) {
		if (sscanf(line, "XPCCHILD %u", &name) == 1) break;
		fputs(line, stdout);
	}
	xpc_connection_t c = NULL;
	if (name) {
		mach_port_t ctask = MACH_PORT_NULL, port = MACH_PORT_NULL;
		mach_msg_type_name_t t;
		CHECK(task_for_pid(mach_task_self(), pid, &ctask));
		CHECK(mach_port_extract_right(ctask, name, MACH_MSG_TYPE_COPY_SEND, &port, &t));
		c = connect_port(port, q, ^(xpc_object_t ev) {
			if (ev == XPC_ERROR_CONNECTION_INTERRUPTED || ev == XPC_ERROR_CONNECTION_INVALID) {
				server_err = ev == XPC_ERROR_CONNECTION_INTERRUPTED ? 1 : 2;
				dispatch_semaphore_signal(server_gone);
			}
		});
		result("[xpc] connected to the child's listener", ping(c));
	} else {
		result("[xpc] child sent its listener", 0);
	}
	int st = 0;
	waitpid(pid, &st, 0);
	printf("DEADNAME: [xpc] child exited (status 0x%x)\n", st);
	dispatch_time_t dl = dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC);
	result("[xpc] server's peer connection invalidated", dispatch_semaphore_wait(peer_gone, dl) == 0 && peer_err);
	if (c) {
		dispatch_semaphore_wait(server_gone, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
		result("[xpc] client's connection interrupted", server_err == 1);
	}
}

int main(int argc, char **argv) {
	setvbuf(stdout, NULL, _IOLBF, 0);
	if (argc >= 6 && !strcmp(argv[1], "child")) {
		child(atoi(argv[2]), (mach_port_t)strtoul(argv[3], NULL, 10), !strcmp(argv[4], "kill"), !strcmp(argv[5], "busy"));
	}
	if (argc >= 4 && !strcmp(argv[1], "xpcchild")) {
		xpc_child(atoi(argv[2]), (mach_port_t)strtoul(argv[3], NULL, 10));
	}
	in_process();
	const char *modes[] = { "fork", "forkkill", "spawn", "spawnkill", "forkbusy", "forkkillbusy", "spawnbusy", "spawnkillbusy" };
	for (int i = 0; i < 8; i++) {
		if (argc > 1 && strcmp(argv[1], modes[i])) continue;
		scenario(modes[i], argv[0]);
	}
	if (argc == 1 || !strcmp(argv[1], "xpc")) xpc_scenario(argv[0]);
	printf("DEADNAME: %d failed\n", failed);
	return failed ? 1 : 0;
}
