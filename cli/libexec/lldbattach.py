#!/usr/bin/env python3
"""x86sim debug: LLDB on an app (or any process) running in a phone.

Guest code is arm64 translated by libarm64jit, so the debug stub is in libarm64jit (linux/arm64jit/gdbstub.cpp): it
listens on /run/user/<uid>/x86sim-dbg-<UDID>-<pid>.sock when asked (X86SIM_A64_DEBUG at launch, or SIGUSR1 with a
request file /run/user/<uid>/x86sim-dbg-req-<UDID>-<pid> at run time) and speaks the GDB remote protocol the way
debugserver does. lldb connects with `process connect --plugin gdb-remote unix-connect://<socket>`.

  x86sim debug [PHONE] BUNDLEID|PID [--launch | --wait] [--symbols FILE] [--batch] [-o CMD]... [-- lldb args]
      --launch   (re)launch the app stopped before dyld runs (as simctl launch -w), then attach: breakpoints in main,
                 initializers and +load work
      --wait     wait until the app runs (you start it), then attach
      (neither)  attach to the running app (its threads stop where they are)
      --symbols  debug info for the executable: a .dSYM (its DWARF file) or an unstripped copy
      --batch    run the -o commands and exit (lldb -b); else an interactive lldb
  x86sim debug [PHONE] --start BUNDLEID|PID [--launch | --wait] [--symbols FILE]   (JSON, for the MCP server)
      a background lldb session (Python SB API) for this phone; then
  x86sim debug [PHONE] --cmd 'COMMAND' [--timeout S]   one lldb command: its output and the process state (JSON)
  x86sim debug [PHONE] --stop                          detach (the app runs on) and end the session (JSON)

lldb: the Swift toolchain's lldb when there is one (swiftlldb.py: set up on first use or by `x86sim dev setup lldb`,
with a private libpython and the Darwin Swift resource dir): Swift values in frame variable, po and expression. Else
/usr/bin/lldb (LLVM 17 or later): C, Objective-C, breakpoints, stepping, backtraces and registers; Swift values only as
raw memory. X86SIM_LLDB=/path/to/lldb uses that one as is; X86SIM_SWIFT_LLDB=0 skips the Swift lldb.
"""
import json, os, signal, socket, struct, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "lib"))
import x86config  # noqa: E402

import swiftlldb  # noqa: E402

SYSTEM_LLDB = "/usr/bin/lldb"
RUN = "/run/user/%d" % os.getuid()


def die(msg, code=1, js=False):
    if js:
        print(json.dumps({"ok": False, "error": msg}))
    else:
        print("x86sim debug: " + msg, file=sys.stderr)
    sys.exit(code)


def udid():
    env = os.path.join(x86config.device(), "sim.env")
    try:
        for l in open(env):
            if l.startswith("SIMULATOR_UDID="):
                return l.split("=", 1)[1].strip()
    except OSError:
        pass
    die("no phone (%s)" % env)


def linux_pid(gpid, u):
    """The host pid of the phone's process whose Darwin pid is gpid."""
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open("/proc/%s/status" % d) as f:
                ns = next((l.split()[1:] for l in f if l.startswith("NSpid:")), [])
            if len(ns) < 2 or int(ns[-1]) != gpid:
                continue
            with open("/proc/%s/environ" % d, "rb") as f:
                if ("SIMULATOR_UDID=" + u).encode() in f.read().split(b"\0"):
                    return int(d)
        except (OSError, ValueError):
            continue
    return None


def sock_path(u, gpid):
    return "%s/x86sim-dbg-%s-%d.sock" % (RUN, u, gpid)


def prune(u):
    """Sockets of this phone's processes that are gone (killed: an exit removes its own)."""
    pre = "x86sim-dbg-%s-" % u
    for f in os.listdir(RUN):
        if f.startswith(pre) and f.endswith(".sock") and f[len(pre):-5].isdigit() and linux_pid(int(f[len(pre):-5]), u) is None:
            try:
                os.unlink(os.path.join(RUN, f))
            except OSError:
                pass


def ensure_stub(u, gpid, wait=15):
    """The process's debug stub socket: asks a running process to start it (SIGUSR1 + request file)."""
    prune(u)
    p = sock_path(u, gpid)
    hp = linux_pid(gpid, u)
    if hp is None:
        if os.path.exists(p):
            os.unlink(p)   # a dead process's socket
        die("no process %d in this phone" % gpid)
    # (never probed with connect(): the stub takes every connection as a debugger and stops the process. One debugger
    # at a time: a second waits until the first detaches.)
    if not os.path.exists(p):
        open("%s/x86sim-dbg-req-%s-%d" % (RUN, u, gpid), "w").close()
        os.kill(hp, signal.SIGUSR1)
    end = time.monotonic() + wait
    while time.monotonic() < end:
        if os.path.exists(p):
            return p
        time.sleep(0.1)
    die("process %d did not start its debug stub (an app that handles SIGUSR1 itself cannot be attached at run "
        "time: use --launch)" % gpid)


def macho_platform(path):
    """ios / tvos / watchos / xros from LC_BUILD_VERSION (the simulator platforms 7, 8, 9, 12)."""
    try:
        with open(path, "rb") as f:
            h = f.read(65536)
        magic, _, _, _, ncmds, _ = struct.unpack_from("<6I", h)
        if magic == 0xcafebabe or magic == 0xbebafeca:   # fat: the arm64 slice
            n = struct.unpack_from(">I", h, 4)[0]
            for i in range(n):
                ct, _, off, _, _ = struct.unpack_from(">5I", h, 8 + 20 * i)
                if ct == 0x0100000c:
                    with open(path, "rb") as f:
                        f.seek(off)
                        h = f.read(65536)
                    magic, _, _, _, ncmds, _ = struct.unpack_from("<6I", h)
        o = 32
        for _ in range(ncmds):
            cmd, size = struct.unpack_from("<II", h, o)
            if cmd == 0x32:
                return {8: "tvos", 9: "watchos", 12: "xros"}.get(struct.unpack_from("<I", h, o + 8)[0], "ios")
            o += size
    except (OSError, struct.error):
        pass
    return "ios"


def resolve(target, launch=False, wait=False):
    """(guest pid, executable's Linux path) of BUNDLEID or PID; launches / waits as asked."""
    os.environ.setdefault("X86SIM_DEVICE", x86config.device())
    import simctl
    exe = None
    if target.isdigit():
        gpid = int(target)
        hp = linux_pid(gpid, udid())
        if hp:
            try:
                with open("/proc/%d/cmdline" % hp, "rb") as f:
                    exe = simctl.dview_to_linux(f.read().split(b"\0")[0].decode())
            except OSError:
                pass
        return gpid, exe
    app = simctl.app_info(target)
    exe = os.path.join(simctl.dview_to_linux(app["Path"]), app["CFBundleExecutable"])
    if launch:
        # the stub parks the main thread before dyld_sim runs (X86SIM_A64_DEBUG=wait) until the debugger continues
        r = subprocess.run([sys.executable, os.path.join(HERE, "simctl.py"), "launch", "--terminate-running-process",
                            "--env", "X86SIM_A64_DEBUG=wait", target], capture_output=True, text=True)
        gpid = None
        for l in r.stdout.splitlines():
            if l.startswith(target + ":"):
                try:
                    gpid = int(l.split(":", 1)[1])
                except ValueError:
                    pass
        if not gpid or gpid < 0:
            gpid = simctl.pidof(target, wait=20)
        if not gpid:
            die("launch of %s failed: %s" % (target, (r.stderr or r.stdout).strip()))
        return gpid, exe
    gpid = simctl.pidof(target, wait=3600 if wait else 0)
    if not gpid:
        die("%s is not running (x86sim launch %s, or --launch / --wait)" % (target, target))
    return gpid, exe


def find_symbols(exe, given):
    if given:
        if given.endswith(".dSYM") or os.path.isdir(given):
            d = os.path.join(given, "Contents/Resources/DWARF")
            if os.path.isdir(d) and os.listdir(d):
                return os.path.join(d, os.listdir(d)[0])
        return given
    if exe:   # <App>.app.dSYM next to the bundle (where Xcode and tests/apps/build.sh put it)
        app = os.path.dirname(exe)
        for d in (app + ".dSYM", exe + ".dSYM"):
            dw = os.path.join(d, "Contents/Resources/DWARF")
            if os.path.isdir(dw) and os.listdir(dw):
                return os.path.join(dw, os.listdir(dw)[0])
    return None


def recorded_sdk(files):
    """The simulator SDK named in the debug info (DW_AT_LLVM_sysroot, the Swift AST's -sdk) of the first of files that
    has one and still exists"""
    import re
    for f in files:
        if not f or not os.path.isfile(f):
            continue
        try:
            with open(f, "rb") as fh:
                data = fh.read(256 << 20)
        except OSError:
            continue
        for m in re.finditer(rb'/[^\0\n"]*?Simulator[0-9.]*\.sdk(?=[/\0"])', data):
            d = m.group(0).decode(errors="replace")
            if os.path.isfile(os.path.join(d, "SDKSettings.json")):
                return d
    return None


def choose_lldb(js=False):
    """{lldb, info, python_exe, python_path, swift}: the lldb to run, its swiftlldb info (its environment:
    swiftlldb.lldb_env(info)), the Python that loads its SB API module and that module's dir (None: `lldb -P`)"""
    say = lambda m: print("x86sim debug: " + m, file=sys.stderr)   # noqa: E731 - stdout is JSON in --start mode
    if os.environ.get("X86SIM_LLDB"):
        p = os.environ["X86SIM_LLDB"]
        if not os.path.exists(p):
            die("X86SIM_LLDB=%s not found" % p, js=js)
        return {"lldb": p, "info": {}, "python_exe": None, "python_path": None, "swift": None}
    if os.environ.get("X86SIM_SWIFT_LLDB", "1") != "0":
        info = swiftlldb.load()
        if not info and swiftlldb.toolchains():
            say("setting up the Swift toolchain's lldb (once; x86sim dev setup lldb does the same)")
            info = swiftlldb.ensure(say)
        if info:
            return {"lldb": info["lldb"], "info": info, "python_exe": info.get("python_exe"),
                    "python_path": info.get("python_path"), "swift": info}
        say("no Swift lldb (%s): using %s, which shows Swift values only as raw memory" % (swiftlldb.status()["detail"], SYSTEM_LLDB))
    if not os.path.exists(SYSTEM_LLDB):
        die("%s not found (pacman -S lldb, x86sim dev setup swift, or X86SIM_LLDB=/path/to/lldb)" % SYSTEM_LLDB, js=js)
    return {"lldb": SYSTEM_LLDB, "info": {}, "python_exe": None, "python_path": None, "swift": None}


def setup_commands(exe, sock, syms, ll):
    c = ["settings set target.load-script-from-symbol-file false",
         "platform select remote-ios"]
    plat = macho_platform(exe) if exe and os.path.exists(exe) else "ios"
    if ll.get("swift"):
        # the Swift expression parser compiles against the app's SDK (po, expression); without it frame variable works.
        # The SDK the app was built with first: its module's serialized flags name that SDK's shims, and a second copy
        # of the SDK would define every shim module twice.
        sdk = recorded_sdk([syms, exe]) or swiftlldb.sim_sdk(plat)
        if sdk and ll["swift"].get("resource_dir"):
            c.append('settings set target.sdk-path "%s"' % sdk)
    if exe and os.path.exists(exe):
        c.append('target create --arch arm64-apple-%s-simulator "%s"' % (plat, exe))
        if syms:
            c.append('target symbols add "%s"' % syms)
    c.append("process connect --plugin gdb-remote unix-connect://" + sock)
    return c


# ---- the background session for the MCP server (lldb's Python SB API) -------------------------------------------------
def session_sock():
    return os.path.join(x86config.device(), "data", "tmp", "x86sim-lldb.sock")


def serve(cmds, ready_fd, lldb_bin):
    """Runs in the background (lldbattach.py --serve-session, under the Python lldb's module is built for): an
    SBDebugger with the process attached, one command per connection."""
    if not any(os.path.isdir(os.path.join(p, "lldb")) for p in sys.path if p):
        sys.path.insert(0, subprocess.run([lldb_bin, "-P"], capture_output=True, text=True).stdout.strip())
    import lldb
    dbg = lldb.SBDebugger.Create()
    dbg.SetAsync(True)
    ci = dbg.GetCommandInterpreter()

    def target():
        return dbg.GetSelectedTarget()

    listener = dbg.GetListener()
    RUNNING = ("running", "stepping", "launching", "attaching", "connected")

    def state():
        p = target().GetProcess()
        return lldb.SBDebugger.StateAsCString(p.GetState()) if p and p.IsValid() else "none"

    def drain():
        """Consumes pending events: lldb's public process state only advances as its events are taken (async mode)."""
        ev = lldb.SBEvent()
        while listener.GetNextEvent(ev):
            pass

    def settle(timeout):
        """After a command: waits until a resumed process stops again (or exits), at most timeout seconds."""
        end = time.monotonic() + 0.5   # a resume may not have produced its event yet
        while time.monotonic() < end and state() not in RUNNING:
            drain()
            time.sleep(0.02)
        end = time.monotonic() + timeout
        ev = lldb.SBEvent()
        while state() in RUNNING and time.monotonic() < end:
            listener.WaitForEvent(1, ev)
            drain()

    def run(c, timeout=30):
        r = lldb.SBCommandReturnObject()
        drain()
        p = target().GetProcess()
        sid = p.GetStopID() if p and p.IsValid() else -1
        before = state()
        ci.HandleCommand(c, r)
        settle(timeout)
        out = (r.GetOutput() or "") + (r.GetError() or "")
        st = state()
        p = target().GetProcess()
        if st == "stopped" and (before != "stopped" or (p.IsValid() and p.GetStopID() != sid)):
            r2 = lldb.SBCommandReturnObject()
            ci.HandleCommand("thread list", r2)
            out += r2.GetOutput() or ""
        return {"ok": r.Succeeded(), "output": out, "state": st}

    first = {"setup": []}
    for c in cmds:
        res = run(c, 60)
        first["setup"].append({"command": c, **res})
    r = run("thread list")
    first.update({"ok": state() == "stopped", "state": state(), "threads": r["output"]})
    os.write(ready_fd, (json.dumps(first) + "\n").encode())
    os.close(ready_fd)
    path = session_sock()
    try:
        os.unlink(path)
    except OSError:
        pass
    ls = socket.socket(socket.AF_UNIX)
    ls.bind(path)
    ls.listen(4)
    while True:
        c, _ = ls.accept()
        try:
            req = json.loads(c.makefile().readline() or "{}")
            if req.get("cmd") == "__stop__":
                p = target().GetProcess()
                if p and p.IsValid() and p.GetState() != lldb.eStateExited:
                    p.Detach()
                c.sendall((json.dumps({"ok": True, "state": "detached"}) + "\n").encode())
                c.close()
                break
            if not req.get("cmd", "").strip():   # lldb repeats the last command on an empty line
                c.sendall((json.dumps({"ok": False, "error": "no command"}) + "\n").encode())
                continue
            res = run(req["cmd"], float(req.get("timeout", 30)))
            c.sendall((json.dumps(res) + "\n").encode())
        except Exception as e:   # noqa: BLE001 - one bad request must not end the session
            try:
                c.sendall((json.dumps({"ok": False, "error": str(e)}) + "\n").encode())
            except OSError:
                pass
        finally:
            c.close()
    for f in (path, path + ".pid"):
        try:
            os.unlink(f)
        except OSError:
            pass
    lldb.SBDebugger.Destroy(dbg)


def session_alive():
    try:
        os.kill(int(open(session_sock() + ".pid").read()), 0)
        return os.path.exists(session_sock())
    except (OSError, ValueError):
        return False


def ask(req, timeout):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(timeout + 30)
    try:
        s.connect(session_sock())
    except OSError:
        die("no debug session on this phone (x86sim debug --start BUNDLEID)", js=True)
    s.sendall((json.dumps(req) + "\n").encode())
    print(s.makefile().readline().strip())


def main(a):
    if a[:1] == ["--serve-session"]:   # the background session (mode start), in its own process
        try:
            serve(json.loads(a[3]), int(a[1]), a[2])
        finally:
            os._exit(0)
    mode, target, launch, wait, syms, batch, ocmds, extra, timeout = "lldb", None, False, False, None, False, [], [], 30.0
    while a:
        x = a.pop(0)
        if x == "--launch":
            launch = True
        elif x == "--wait":
            wait = True
        elif x == "--symbols":
            syms = a.pop(0)
        elif x == "--batch":
            batch = True
        elif x == "-o":
            ocmds.append(a.pop(0))
        elif x == "--start":
            mode, target = "start", a.pop(0)
        elif x == "--cmd":
            mode, target = "cmd", a.pop(0)
        elif x == "--timeout":
            timeout = float(a.pop(0))
        elif x == "--stop":
            mode = "stop"
        elif x == "--":
            extra = a
            break
        elif x.startswith("-"):
            die("unknown option %s\n%s" % (x, __doc__), 2)
        else:
            target = x
    js = mode != "lldb"
    if mode == "cmd":
        return ask({"cmd": target, "timeout": timeout}, timeout)
    if mode == "stop":
        return ask({"cmd": "__stop__"}, 10)
    if not target:
        die(__doc__, 2)
    ll = choose_lldb(js)
    u = udid()
    gpid, exe = resolve(target, launch, wait)
    sock = ensure_stub(u, gpid)
    cmds = setup_commands(exe, sock, find_symbols(exe, syms), ll)
    env = swiftlldb.lldb_env(ll["info"])
    if mode == "start":
        if session_alive():
            die("a debug session is already running on this phone (x86sim debug --stop)", js=True)
        # the session runs under the Python that lldb's SB API module is built for (the Swift lldb's private one)
        py = ll["python_exe"] or sys.executable
        if ll["python_path"]:
            env["PYTHONPATH"] = ll["python_path"]
        env["X86SIM_DEVICE"] = x86config.device()
        r, w = os.pipe()
        logf = os.path.join(x86config.device(), "data", "tmp", "x86sim-lldb.log")
        with open(logf, "w") as log:
            p = subprocess.Popen([py, os.path.abspath(__file__), "--serve-session", str(w), ll["lldb"], json.dumps(cmds + ocmds)],
                                 env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, pass_fds=(w,), start_new_session=True)
        with open(session_sock() + ".pid", "w") as f:
            f.write(str(p.pid))
        os.close(w)
        out = os.fdopen(r).readline().strip()
        if not out:
            die("the lldb session ended at once (see %s)" % logf, js=True)
        res = json.loads(out)
        res.update({"pid": gpid, "socket": sock, "executable": exe, "lldb": ll["lldb"], "swift": bool(ll["swift"])})
        print(json.dumps(res))
        return
    argv = [ll["lldb"]] + (["-b"] if batch else [])
    for c in cmds + ocmds:
        argv += ["-o", c]
    os.execve(ll["lldb"], argv + extra, env)


if __name__ == "__main__":
    main(sys.argv[1:])
