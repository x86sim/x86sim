#!/usr/bin/env python3
"""x86Sim: simctl's device settings and lifecycle for the arm64 phones (as xcrun simctl ui / status_bar / pbcopy /
pbpaste / erase / clone / keychain), plus language, time zone, fidelity and an app's logs. A running phone is changed
through host/openurl (devctl.m: what CoreSimulatorBridge does in a Mac's simulator); a phone that is off gets the same
preferences written into its data directory (read at its next boot, as `simctl` does for a shutdown device).

  ui [appearance|content_size|increase_contrast|reduce_transparency|reduce_motion] [VALUE]
         no VALUE: print the setting(s). appearance light|dark; content_size extra-small .. extra-extra-extra-large,
         accessibility-medium .. accessibility-extra-extra-extra-large, increment, decrement; the others enabled|disabled
  status_bar override [--time T] [--dataNetwork hide|wifi|3g|4g|lte|lte-a|lte+|5g|5g+|5g-uwb|5g-uc]
         [--wifiMode searching|failed|active] [--wifiBars 0-3] [--cellularMode notSupported|searching|failed|active]
         [--cellularBars 0-4] [--operatorName NAME] [--batteryState charging|charged|discharging] [--batteryLevel 0-100]
         | status_bar clear | status_bar list          (running phone, as simctl; overrides add up until clear)
  pbcopy [--host] [TEXT]      the phone's pasteboard from TEXT, stdin, or (--host) the host clipboard
  pbpaste [--host]            print the phone's pasteboard (--host: also put it on the host clipboard)
  pbsync [--interval S]       keep the phone's pasteboard and the host clipboard the same until Ctrl-C
  keychain reset              delete every keychain item (simctl keychain reset)
  locale [--language xx[-YY]...] [--locale xx_YY]   AppleLanguages / AppleLocale (apps pick it up at their next start)
  timezone [ZONE|host]        the phone's time zone (Olson name, e.g. Europe/Paris; default the host's); next boot
  fidelity [fast|high]        fast: a 3x iPhone at 2x scale (fewer pixels to draw) + Reduce Transparency and Reduce
                              Motion; high: native scale, every effect. The scale applies at the next boot
  erase                       the phone back to a new one (simctl erase; the phone must be off)
  clone NEWNAME               a copy of the phone as a new phone (simctl clone; the phone must be off)
  applog BUNDLEID [--follow] [--relaunch] [--lines N]
                              an app's os_log (data/var/log/oslog.<exe>.*.log) and the crash report of its last run,
                              symbolicated (the translator's guest backtrace: dyld cache exports, the app's symbols)
"""
import glob, json, os, plistlib, re, shutil, signal, subprocess, sys, time
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "lib"))
import x86config  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = x86config.ROOT
# real paths: boot-a64.sh boots a device by its real path (realpath -m), and the paths in its data must be that one
# (a checkout whose var/ is a symlink otherwise rewrote a clone's stores to the symlinked path)
DEV = os.path.realpath(x86config.device())
DATA = os.path.join(DEV, "data")
PREFS = os.path.join(DATA, "Library/Preferences")


def die(msg, code=1):
    print(msg, file=sys.stderr)
    sys.exit(code)


def running(dev=DEV):
    """The device's launchd_sim is alive (as the launcher's Monitor finds it)."""
    marker = "/Volumes/SystemRoot%s/data/var/run/launchd_bootstrap.plist" % os.path.realpath(dev)
    marker2 = "/Volumes/SystemRoot%s/data/var/run/launchd_bootstrap.plist" % dev
    for p in os.listdir("/proc"):
        if not p.isdigit():
            continue
        try:
            if open("/proc/%s/comm" % p).read().strip() != "launchd_sim":
                continue
            cl = open("/proc/%s/cmdline" % p, "rb").read().decode(errors="replace")
        except OSError:
            continue
        if marker in cl or marker2 in cl:
            return True
    return False


def boot_processes(dev=DEV):
    """Pids of anything booting, running or stopping the phone: a process whose environment names it (A64_DEVICE: the
    launcher's boot-a64.sh, and everything boot-a64.sh starts after it exports it; a relative one resolved against the
    process's cwd), a boot-a64.sh started without A64_DEVICE (the default phone), or its launchd_sim. running() alone
    missed a phone still starting (no launchd_sim yet)."""
    real = os.path.realpath(dev)
    default = os.path.realpath(os.path.join(ROOT, "var/device-a64"))
    # ourselves and our ancestors (a shell that exported A64_DEVICE runs this erase) are not the phone
    mine, q = set(), os.getpid()
    while q > 1 and q not in mine:
        mine.add(q)
        try:
            q = int(open("/proc/%d/stat" % q).read().rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            break
    out = []
    for p in os.listdir("/proc"):
        if not p.isdigit() or int(p) in mine:
            continue
        try:
            env = open("/proc/%s/environ" % p, "rb").read().split(b"\0")
            cmd = open("/proc/%s/cmdline" % p, "rb").read()
        except OSError:
            continue
        val = next((e[len(b"A64_DEVICE="):].decode(errors="replace") for e in env if e.startswith(b"A64_DEVICE=")), None)
        if val is not None:
            if not os.path.isabs(val):
                try:
                    val = os.path.join(os.readlink("/proc/%s/cwd" % p), val)
                except OSError:
                    continue
            if os.path.realpath(val) == real:
                out.append(int(p))
        elif b"boot-a64.sh" in cmd and real == default:
            out.append(int(p))
    if running(dev):
        out.append(-1)
    return out


class Lifecycle:
    """An exclusive flock on <phone>/.lifecycle.lock while erase / clone work: boot-a64.sh refuses to start a phone
    whose lock is held (it checks after its own process exists, and erase checks for boot processes after taking the
    lock: whichever comes second sees the other)."""

    def __init__(self, dev, what):
        self.dev, self.what = dev, what

    def __enter__(self):
        import fcntl
        self.f = open(os.path.join(self.dev, ".lifecycle.lock"), "a")
        try:
            fcntl.flock(self.f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            die("x86sim %s: the device is being erased or cloned already" % self.what)
        if boot_processes(self.dev):
            die("x86sim %s: stop the device first: it is running, starting or stopping (as simctl: Unable to %s in "
                "current state: Booted)" % (self.what, self.what))
        return self

    def __exit__(self, *a):
        self.f.close()


def helper(cmd, timeout=30, **kw):
    import simctl
    return simctl.check(simctl.request(cmd, timeout=timeout, **kw), cmd)


def edit_plist(name, **kv):
    """Set keys of a preferences plist in the device's data (device off: cfprefsd reads it at the next boot)."""
    p = os.path.join(PREFS, name)
    try:
        with open(p, "rb") as f:
            d = plistlib.load(f)
    except (OSError, plistlib.InvalidFileException):
        d = {}
    for k, v in kv.items():
        if v is None:
            d.pop(k, None)
        else:
            d[k] = v
    os.makedirs(PREFS, exist_ok=True)
    tmp = p + ".x86sim-new"
    with open(tmp, "wb") as f:
        plistlib.dump(d, f, fmt=plistlib.FMT_BINARY)
    os.replace(tmp, p)


def read_plist(name):
    try:
        with open(os.path.join(PREFS, name), "rb") as f:
            return plistlib.load(f)
    except (OSError, plistlib.InvalidFileException):
        return {}


# ---- ui ----

SIZES = ["extra-small", "small", "medium", "large", "extra-large", "extra-extra-large", "extra-extra-extra-large",
         "accessibility-medium", "accessibility-large", "accessibility-extra-large", "accessibility-extra-extra-large",
         "accessibility-extra-extra-extra-large"]
SIZE_CAT = dict(zip(SIZES, ["UICTContentSizeCategory" + s for s in
                            ["XS", "S", "M", "L", "XL", "XXL", "XXXL", "AccessibilityM", "AccessibilityL", "AccessibilityXL",
                             "AccessibilityXXL", "AccessibilityXXXL"]]))
# the preferences each setting is kept in (what Apple's simulator writes: see devctl.m), for a phone that is off
AX = "com.apple.Accessibility.plist"
AX_KEYS = {"increase_contrast": "DarkenSystemColors", "reduce_transparency": "EnhancedBackgroundContrastEnabled",
           "reduce_motion": "ReduceMotionEnabled"}
STYLE = "com.apple.uikitservices.userInterfaceStyleMode.plist"
WHATS = ["appearance", "content_size", "increase_contrast", "reduce_transparency", "reduce_motion"]


def ui_offline_state():
    mode = read_plist(STYLE).get("UserInterfaceStyleMode", 1)
    cat = read_plist("com.apple.UIKit.plist").get("UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryL")
    ax = read_plist(AX)
    st = {"appearance": "dark" if mode == 2 else "light",
          "content_size": next((k for k, v in SIZE_CAT.items() if v == cat), cat)}
    for k, key in AX_KEYS.items():
        st[k] = "enabled" if ax.get(key) else "disabled"
    return st


def ui_set_offline(what, value):
    if what == "appearance":
        if value not in ("light", "dark"):
            die("x86sim ui appearance: light or dark", 2)
        edit_plist(STYLE, UserInterfaceStyleMode=2 if value == "dark" else 1)
    elif what == "content_size":
        if value in ("increment", "decrement"):
            cur = ui_offline_state()["content_size"]
            i = SIZES.index(cur) if cur in SIZES else 3
            value = SIZES[min(i + 1, len(SIZES) - 1) if value == "increment" else max(i - 1, 0)]
        if value not in SIZE_CAT:
            die("x86sim ui content_size: one of %s, increment, decrement" % ", ".join(SIZES), 2)
        edit_plist("com.apple.UIKit.plist", UIPreferredContentSizeCategoryName=SIZE_CAT[value])
        if value.startswith("accessibility-"):
            edit_plist(AX, LargeTextUsesExtendedRange=1)
    elif what in AX_KEYS:
        on = {"enabled": 1, "on": 1, "1": 1, "disabled": 0, "off": 0, "0": 0}.get(value)
        if on is None:
            die("x86sim ui %s: enabled or disabled" % what, 2)
        edit_plist(AX, **{AX_KEYS[what]: on})
    else:
        die("x86sim ui: %s" % " | ".join(WHATS), 2)


def platform():
    """The device's platform: ios, tvos, watchos or xros (its runtimes/ dir name)."""
    try:
        p = open(os.path.join(DEV, "runtime")).read().strip().split("-")[0]
    except OSError:
        return "ios"
    return p if p in ("ios", "tvos", "watchos", "xros") else "ios"


# Apple's tvOS and watchOS simulators have no appearance setting and no status bar overrides: simctl ui appearance
# prints "unsupported" and setting it, like status_bar override, fails with ENOTSUP ("Runtime does not support
# userInterfaceStyle"; Xcode 27.2). visionOS has both.
NO_STYLE = ("tvos", "watchos")


def ui(a):
    what = a[0] if a else None
    value = a[1] if len(a) > 1 else None
    if what and what not in WHATS:
        die("usage: x86sim ui [%s] [VALUE]" % "|".join(WHATS), 2)
    if what == "appearance" and platform() in NO_STYLE:
        if value:
            die("x86sim ui appearance: Operation not supported (the %s runtime does not support userInterfaceStyle, as in "
                "Apple's simulator)" % {"tvos": "tvOS", "watchos": "watchOS"}[platform()], 45)
        print("unsupported")
        return
    if running():
        st = helper("ui", what=what or "", value=value or "")
        st = {k: st[k] for k in WHATS if k in st}
        if value and what == "content_size" and value not in ("increment", "decrement") and st.get("content_size") != value:
            die("x86sim ui: content_size is %s, not %s" % (st.get("content_size"), value))
    else:
        if value:
            ui_set_offline(what, value)
        st = ui_offline_state()
        st["note"] = "device off: applies at its next boot"
    if platform() in NO_STYLE:
        st["appearance"] = "unsupported"
    if what and not value:
        print(st[what])
    else:
        print(json.dumps(st, indent=1) if "--json" in a else "\n".join("%s: %s" % kv for kv in st.items()))


# ---- status bar ----

SB_OPTS = ["time", "dataNetwork", "wifiMode", "wifiBars", "cellularMode", "cellularBars", "operatorName", "batteryState",
           "batteryLevel"]


def status_bar(a):
    if not a or a[0] not in ("override", "clear", "list"):
        die("usage: x86sim status_bar override [--time T] [--dataNetwork N] [--wifiMode M] [--wifiBars 0-3] "
            "[--cellularMode M] [--cellularBars 0-4] [--operatorName S] [--batteryState S] [--batteryLevel 0-100] "
            "| clear | list", 2)
    if platform() in NO_STYLE:
        if a[0] == "list":
            print("Current Status Bar Overrides:\n=============================")
            return
        die("x86sim status_bar: Operation not supported (no status bar overrides on %s, as in Apple's simulator)"
            % {"tvos": "tvOS", "watchos": "watchOS"}[platform()], 45)
    if not running():
        die("x86sim status_bar: the device is not running (overrides live in the running SpringBoard, as in Apple's simulator)")
    kw = {}
    rest = a[1:]
    while rest:
        o = rest.pop(0)
        k, _, v = o.lstrip("-").partition("=")
        if k not in SB_OPTS:
            die("x86sim status_bar: unknown option %s" % o, 2)
        if not v:
            if not rest:
                die("x86sim status_bar: %s needs a value" % o, 2)
            v = rest.pop(0)
        kw[k] = int(v) if k in ("wifiBars", "cellularBars", "batteryLevel") else v
    r = helper("statusbar", action=a[0], **kw)
    for k, v in sorted(r.get("overrides", {}).items()):
        print("%s: %s" % (k, v))


# ---- pasteboard ----

def host_clip_cmd(write):
    if os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-copy"):
        return ["wl-copy"] if write else ["wl-paste", "--no-newline"]
    if shutil.which("xclip"):
        return ["xclip", "-selection", "clipboard"] + ([] if write else ["-o"])
    if shutil.which("xsel"):
        return ["xsel", "--clipboard"] + (["--input"] if write else ["--output"])
    die("x86sim: no host clipboard tool (wl-clipboard, xclip or xsel)")


def host_get():
    r = subprocess.run(host_clip_cmd(False), capture_output=True, timeout=5)
    return r.stdout.decode(errors="replace") if r.returncode == 0 else ""


def host_set(s):
    # wl-copy stays in the background to serve the selection: don't wait for it
    p = subprocess.Popen(host_clip_cmd(True), stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         start_new_session=True)
    p.stdin.write(s.encode())
    p.stdin.close()


def need_running(what):
    if not running():
        die("x86sim %s: the device is not running" % what)


def pbcopy(a):
    need_running("pbcopy")
    # "--": everything after it is the text, even "--host"
    opts, text = (a[:a.index("--")], a[a.index("--") + 1:]) if "--" in a else ([x for x in a if x == "--host"], [x for x in a if x != "--host"])
    if "--host" in opts:
        s = host_get()
    elif text:
        s = " ".join(text)
    else:
        s = sys.stdin.read()
    helper("pasteboard", action="set", string=s)


def pbpaste(a):
    need_running("pbpaste")
    s = helper("pasteboard", action="get").get("string")
    s = s if isinstance(s, str) else ""
    if "--host" in a:
        host_set(s)
    sys.stdout.write(s)
    sys.stdout.flush()


def pbsync(a):
    """Both ways, by polling: the device's changeCount and the host clipboard's text; the newer change wins."""
    need_running("pbsync")
    every = float(a[a.index("--interval") + 1]) if "--interval" in a else 1.0
    r = helper("pasteboard", action="get")
    count, phone = r.get("changeCount"), r.get("string") or ""
    host = host_get()
    print("x86sim pbsync: syncing the device's pasteboard with the host clipboard (Ctrl-C to stop)", file=sys.stderr)
    signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
    while True:
        time.sleep(every)
        h = host_get()
        if h != host and h != phone:
            r = helper("pasteboard", action="set", string=h)
            count, phone, host = r.get("changeCount"), h, h
            print("host -> device: %d chars" % len(h), file=sys.stderr, flush=True)
            continue
        host = h
        c = helper("pasteboard", action="count").get("changeCount")
        if c != count:
            r = helper("pasteboard", action="get")
            count, s = r.get("changeCount"), r.get("string")
            if isinstance(s, str) and s != host:
                host_set(s)
                host = s
                print("device -> host: %d chars" % len(s), file=sys.stderr, flush=True)
            phone = s if isinstance(s, str) else phone


# ---- keychain / locale / time zone ----

def keychain(a):
    if a[:1] != ["reset"]:
        die("usage: x86sim keychain reset", 2)
    if running():
        helper("keychain")
    else:
        n = 0
        for f in glob.glob(os.path.join(DATA, "Library/Keychains/*")):
            if os.path.isdir(f):
                shutil.rmtree(f)
            else:
                os.unlink(f)
            n += 1
        print("x86sim keychain: removed %d keychain files (device off)" % n, file=sys.stderr)
    print("keychain reset")


def locale(a):
    langs, loc = [], None
    i = 0
    while i < len(a):
        if a[i] in ("--language", "--languages"):
            langs += [x for x in a[i + 1].split(",") if x]
            i += 2
        elif a[i] == "--locale":
            loc = a[i + 1]
            i += 2
        else:
            die("usage: x86sim locale [--language xx[-YY][,..]] [--locale xx_YY]", 2)
    for l in langs:
        if not re.match(r"^[a-z]{2,3}([-_][A-Za-z0-9]+)*$", l):
            die("x86sim locale: %r is not a language code (en, fr-CA, zh-Hans)" % l, 2)
    if loc and not re.match(r"^[a-z]{2,3}(_[A-Za-z0-9]+)*(@.*)?$", loc):
        die("x86sim locale: %r is not a locale (en_US, fr_FR)" % loc, 2)
    if running():
        kw = {}
        if langs:
            kw["languages"] = langs
        if loc:
            kw["locale"] = loc
        r = helper("globalprefs", **kw)
        langs_now, loc_now = r.get("languages"), r.get("locale")
        if kw:
            print("x86sim locale: set; running apps take it at their next start (SpringBoard: x86sim restart)", file=sys.stderr)
    else:
        kv = dict(([("AppleLanguages", langs)] if langs else []) + ([("AppleLocale", loc)] if loc else []))
        if kv:
            edit_plist(".GlobalPreferences.plist", **kv)
        gp = read_plist(".GlobalPreferences.plist")
        langs_now, loc_now = gp.get("AppleLanguages"), gp.get("AppleLocale")
    print("languages: %s\nlocale: %s" % (",".join(langs_now or []), loc_now or ""))


def host_zone():
    return re.sub(r"^.*/zoneinfo/", "", os.path.realpath("/etc/localtime"))


def timezone(a):
    f = os.path.join(DEV, "timezone")
    if a:
        z = a[0]
        if z == "host":
            try:
                os.unlink(f)
            except FileNotFoundError:
                pass
        else:
            if z.startswith(("/", "-")) or ".." in z or not os.path.isfile(os.path.join("/usr/share/zoneinfo", z)):
                die("x86sim timezone: unknown zone %r (an Olson name such as Europe/Paris, or host)" % z, 2)
            with open(f, "w") as fh:
                fh.write(z + "\n")
        if running():
            print("x86sim timezone: applies at the device's next boot (x86sim restart)", file=sys.stderr)
    try:
        print(open(f).read().strip() + " (this device's)")
    except OSError:
        print(host_zone() + " (the host's)")


# ---- fidelity ----

def fidelity(a):
    f = os.path.join(DEV, "fidelity")
    if a:
        p = a[0]
        if p not in ("fast", "high"):
            die("usage: x86sim fidelity [fast|high]", 2)
        with open(f, "w") as fh:
            fh.write(p + "\n")
        v = "enabled" if p == "fast" else "disabled"
        if running():
            for w in ("reduce_transparency", "reduce_motion"):
                helper("ui", what=w, value=v)
            print("x86sim fidelity: effects changed now; the screen scale applies at the next boot (x86sim restart)",
                  file=sys.stderr)
        else:
            for w in ("reduce_transparency", "reduce_motion"):
                ui_set_offline(w, v)
    try:
        p = open(f).read().strip()
    except OSError:
        p = "high"
    env = {}
    try:
        env = dict(l.split("=", 1) for l in open(os.path.join(DEV, "sim.env")).read().splitlines() if "=" in l)
    except OSError:
        pass
    print("%s (screen %sx%s at %sx)" % (p, env.get("SIMULATOR_MAINSCREEN_WIDTH", "?"), env.get("SIMULATOR_MAINSCREEN_HEIGHT", "?"),
                                        env.get("SIMULATOR_MAINSCREEN_SCALE", "?").rstrip("0").rstrip(".")))


# ---- erase / clone ----

def is_default(dev):
    return os.path.basename(dev.rstrip("/")) == "device-a64"


def regenerate(dev):
    """sim.env, capabilities, LaunchDaemons for the device (mksimdevice.py, as device-a64.py create runs it)."""
    try:
        rt = open(os.path.join(dev, "runtime")).read().strip()
    except OSError:
        rt = "ios-27.2-24B8117-arm64"
    env = dict(os.environ, X86SIM_ARCH="arm64", X86SIM_DEVICE=dev, X86SIM_RUNTIME=os.path.join(ROOT, "runtimes", rt, "RuntimeRoot"),
               X86SIM_JOBLOG="1")
    env.pop("X86SIM_STATE", None)
    subprocess.run([sys.executable, os.path.join(HERE, "mksimdevice.py"), dev], env=env, check=True, stdout=subprocess.DEVNULL)


def own_phone(what):
    """erase / clone act on an arm64 device's own dir only (var/device-a64*), never on another configured device"""
    if not os.path.basename(DEV).startswith("device-a64") or not os.path.exists(os.path.join(DEV, "runtime")):
        die("x86sim %s: %s is not an arm64 device (x86sim device list)" % (what, DEV))


def erase(a):
    own_phone("erase")
    if is_default(DEV) and "--force" not in a:
        die("x86sim erase: this is the main device (var/device-a64); add --force to erase it anyway")
    with Lifecycle(DEV, "erase"):
        erase_locked()


def erase_locked():
    data = os.path.join(DEV, "data")
    if os.path.isdir(data):
        shutil.rmtree(data)
    for f in (".home-reached", "capabilities.plist", "sim.env"):
        try:
            os.unlink(os.path.join(DEV, f))
        except FileNotFoundError:
            pass
    shutil.rmtree(os.path.join(DEV, "LaunchDaemons"), ignore_errors=True)
    regenerate(DEV)
    # the pre-translated code of apps that were only on this phone goes at the next upkeep; prune now (best effort)
    aot = os.path.join(ROOT, "tools/launcher/target/release/x86sim-aot")
    if os.path.exists(aot):
        subprocess.run([aot, "--device", DEV, "app", "prune"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    print("erased %s (its next boot is a first boot)" % DEV)


def clone(a):
    if a[:1] == ["--"]:
        a = a[1:]
    if not a:
        die("usage: x86sim clone NEWNAME", 2)
    name = " ".join(a)
    own_phone("clone")
    slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")[:24].strip("-")
    if not slug:
        die("x86sim clone: give the new device a name", 2)
    with Lifecycle(DEV, "clone"):
        clone_locked(name, slug)


def clone_locked(name, slug):
    new = os.path.join(os.path.realpath(os.path.join(ROOT, "var")), "device-a64-" + slug)
    # made here (atomically: a second clone of the same name fails), then filled
    try:
        os.mkdir(new)
    except FileExistsError:
        die("x86sim clone: %s exists" % new)
    # reflinks: a clone costs no space until it changes; the source is never written
    r = subprocess.run(["cp", "-a", "--reflink=auto", DEV + "/.", new])
    if r.returncode != 0:
        shutil.rmtree(new, ignore_errors=True)
        die("x86sim clone: copying failed")
    for x in ("var/log", "var/run", "tmp"):
        shutil.rmtree(os.path.join(new, "data", x), ignore_errors=True)
        os.makedirs(os.path.join(new, "data", x))
    for x in ("data/simhid.sock", "data/framebuffer.bgra", "launcher.json.lock", ".lifecycle.lock"):
        try:
            os.unlink(os.path.join(new, x))
        except OSError:
            pass
    with open(os.path.join(new, "name"), "w") as f:
        f.write(name + "\n")
    # its stores keep the source's absolute paths (lsd's csstore, FrontBoard's db): rewritten to the clone's own
    subprocess.run([sys.executable, os.path.join(HERE, "arm64/device-repath.py"), new, DEV], stdout=subprocess.DEVNULL)
    regenerate(new)   # sim.env with its own paths and UDID (from its path)
    print(new)


# ---- app logs ----

def app_bundle(bid):
    """(bundle path, executable) of an installed or system app, from the files (the device may be off)."""
    cands = glob.glob(os.path.join(DATA, "Containers/Bundle/Application/*/*.app"))
    try:
        rt = open(os.path.join(DEV, "runtime")).read().strip()
    except OSError:
        rt = "ios-27.2-24B8117-arm64"
    rr = os.path.join(ROOT, "runtimes", rt, "RuntimeRoot")
    cands += glob.glob(os.path.join(rr, "Applications/*.app")) + glob.glob(os.path.join(rr, "System/Library/CoreServices/*.app"))
    for app in cands:
        try:
            with open(os.path.join(app, "Info.plist"), "rb") as f:
                info = plistlib.load(f)
        except (OSError, plistlib.InvalidFileException):
            continue
        if info.get("CFBundleIdentifier") == bid:
            return app, info.get("CFBundleExecutable") or os.path.splitext(os.path.basename(app))[0]
    return None, None


def prefix_dir():
    sfx = os.path.basename(DEV)[len("device-a64"):]
    if sfx and not sfx.startswith("-"):
        sfx = "-" + sfx
    return os.environ.get("A64_DPREFIX") or os.path.join(x86config.get("X86SIM_SCRATCH"), "a64prefix" + sfx)


CRASH = re.compile(r"no guest handler|SIGABRT \(code|fatal native fault|guest fault at pc|stack overflow")


def last_crash(exe):
    """The newest translator log of `exe` (this boot's or the last one's) that records a crash."""
    logs = []
    for d in ("private/var/tmp/x86sim-a64", "private/var/tmp/x86sim-a64.prev"):
        logs += glob.glob(os.path.join(prefix_dir(), d, "%s.*.log" % glob.escape(exe)))
    for p in sorted(logs, key=lambda p: os.path.getmtime(p), reverse=True):
        try:
            txt = open(p, errors="replace").read()
        except OSError:
            continue
        if CRASH.search(txt):
            return p, txt
    return None, None


def apple_reports(exe):
    out = []
    for pat in ("Library/Logs/DiagnosticReports/%s*.ips", "Library/Logs/DiagnosticReports/%s*.crash", "Library/Logs/CrashReporter/%s*"):
        out += glob.glob(os.path.join(DATA, pat % glob.escape(exe)))
    return sorted(out, key=os.path.getmtime)


class Symbolizer:
    def __init__(self, app):
        sys.path.insert(0, os.path.join(ROOT, "tools/debug"))
        import dscsym, gbt   # noqa: E401
        self.gbt = gbt
        try:
            rt = open(os.path.join(DEV, "runtime")).read().strip()
        except OSError:
            rt = "ios-27.2-24B8117-arm64"
        cache = os.path.join(ROOT, "runtimes", rt, "RuntimeRoot/System/Library/Caches/com.apple.dyld/dyld_sim_shared_cache_arm64")
        self.cache = dscsym.Cache(cache) if os.path.exists(cache) else None
        self.ex = {}
        self.app = app

    def cache_sym(self, a):
        c = self.cache
        img = c.image_of(a) if c else None
        if not img:
            return ""
        if img[0] not in self.ex:
            self.ex[img[0]] = c.exports(img[0])
        exps, text = self.ex[img[0]]
        s = None
        for e, n in exps:
            if e <= a:
                s = (e, n)
            else:
                break
        name = img[1].rsplit("/", 1)[-1]
        inside = s and text and text[0] <= a < text[0] + text[1]
        return "%s  %s" % (name, "%s + %d" % (s[1], a - s[0]) if inside else "")

    def file_sym(self, path, off):
        import bisect
        p = path[len("/Volumes/SystemRoot"):] if path.startswith("/Volumes/SystemRoot") else path
        text, syms = self.gbt.macho_syms(p)
        vm = text + off
        i = bisect.bisect_right(syms, (vm, "\xff")) - 1
        return "%s  %s" % (os.path.basename(p), "%s + %d" % (syms[i][1], vm - syms[i][0]) if i >= 0 else "")

    def line(self, l):
        m = re.match(r"^(\s*(?:#-?\d+|lr)\s+)0x([0-9a-f]+)\s+cache\s*$", l)
        if m:
            a = int(m.group(2), 16)
            return "%s0x%s  %s" % (m.group(1), m.group(2), self.cache_sym(a) or "(dyld cache)")
        m = re.match(r"^(\s*(?:#-?\d+|lr)\s+)0x([0-9a-f]+)\s+(/\S+)\+0x([0-9a-f]+)\s*$", l)
        if m:
            try:
                return "%s0x%s  %s" % (m.group(1), m.group(2), self.file_sym(m.group(3), int(m.group(4), 16)))
            except Exception:
                return l
        return l


def applog(a):
    if not a:
        die("usage: x86sim logs BUNDLEID [--follow] [--relaunch] [--lines N]", 2)
    bid = a[0]
    follow, relaunch = "--follow" in a or "-f" in a, "--relaunch" in a
    n = int(a[a.index("--lines") + 1]) if "--lines" in a else 40
    app, exe = app_bundle(bid)
    if not exe:
        die("x86sim logs: no app %s on %s (x86sim listapps)" % (bid, os.path.basename(DEV)))
    print("== %s (%s, executable %s)" % (bid, os.path.basename(DEV), exe))
    # crash report of the last run that crashed
    path, txt = last_crash(exe)
    reports = apple_reports(exe)
    if reports:
        print("\n== crash report %s" % reports[-1])
        sys.stdout.write(open(reports[-1], errors="replace").read()[-20000:])
    if path:
        pid = path.rsplit(".", 2)[-2]
        print("\n== crash of %s pid %s, %s (translator log %s)" % (exe, pid, time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(os.path.getmtime(path))), path))
        try:
            sym = Symbolizer(app)
        except Exception as e:  # noqa: BLE001
            print("(no symbolizer: %s)" % e)
            sym = None
        start = CRASH.search(txt).start()
        start = txt.rfind("\n", 0, start) + 1
        for l in txt[start:].splitlines()[:80]:
            print(sym.line(l) if sym else l)
    elif not reports:
        print("\n== no crash recorded for %s (this boot or the last)" % exe)
    # os_log
    d = os.path.join(DATA, "var/log")
    logs = sorted(glob.glob(os.path.join(d, "oslog.%s.*.log" % glob.escape(exe))), key=os.path.getmtime)
    if relaunch:
        need_running("logs --relaunch")
        import simctl
        r = simctl.check(simctl.request("launch", timeout=50, bundle=bid, env=simctl.oslog_env(), terminate=True), "launch")
        print("\n== %s relaunched with its os_log to a file (pid %s)" % (bid, r.get("pid")), flush=True)
        follow = True
    elif logs:
        print("\n== os_log %s (last %d lines)" % (logs[-1], n))
        with open(logs[-1], errors="replace") as f:
            for l in f.readlines()[-n:]:
                sys.stdout.write(l)
    else:
        print("\n== no os_log file for %s: x86sim logs %s --relaunch (or x86sim launch --oslog) records it" % (exe, bid))
    if follow:
        sys.stdout.flush()
        sys.argv = ["simctl.py"]
        import simctl
        simctl.cmd_log(["stream", "--process", exe])


CMDS = {"ui": ui, "status_bar": status_bar, "pbcopy": pbcopy, "pbpaste": pbpaste, "pbsync": pbsync, "keychain": keychain,
        "locale": locale, "timezone": timezone, "fidelity": fidelity, "erase": erase, "clone": clone, "applog": applog}

if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in CMDS:
        die(__doc__, 2)
    CMDS[sys.argv[1]](sys.argv[2:])
