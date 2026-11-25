#!/usr/bin/env python3
"""x86sim: the Swift toolchain's lldb, made to run here, for `x86sim debug` (lldbattach.py).

  swiftlldb.py status [--json]   what `x86sim debug` would use, and why
  swiftlldb.py setup [--json]    set it up (x86sim dev setup lldb; also done on the first `x86sim debug`)

The Swift toolchain's lldb (swift.org's, or a distribution's swift package) is the one with Swift support: `frame
variable` of Swift structs / arrays / strings through the reflection metadata, `po` and `expression` through its Swift
compiler. Upstream lldb (/usr/bin/lldb) has neither. Two things keep it from working on a desktop Linux as installed:

1. liblldb links the libpython3.X it was built against (3.12 for swift.org 6.4 builds) and the system usually has a
   different one. A private CPython of that version (python-build-standalone's shared build, pinned with sha256 below)
   goes in <dev home>/swift-lldb/python-3.X, and lldb runs with LD_LIBRARY_PATH / PYTHONHOME pointing there. Nothing
   of the system's Python is touched; no root.
2. Swift expressions (po, expression) compile against the target's SDK with the resource dir next to liblldb
   (<liblldb dir>/swift). A Linux toolchain has the Linux one there, whose dispatch / os / CoreFoundation module maps
   collide with the iOS SDK's ("redefinition of module 'Dispatch'", CoreGraphics fails to build). lldb derives that
   dir from liblldb's real path (symlinks resolved), so <dev home>/swift-lldb/toolchain is a small tree with a copy of
   the lldb binary, a copy of liblldb (cp --reflink=auto: free on btrfs / xfs), and usr/lib/swift -> the Darwin
   resource dir of the SDK bundle `x86sim dev setup` builds (darwin.artifactbundle's XcodeDefault.xctoolchain). The
   rest links to the toolchain. Without the SDK bundle, frame variable still works and po / expression do not.

lldbattach.py then runs that lldb with `settings set target.sdk-path` set to the phone's simulator SDK.
"""
import hashlib, json, os, re, shutil, struct, subprocess, sys, tarfile, tempfile, urllib.request

# python-build-standalone, x86_64-unknown-linux-gnu, install_only_stripped (a shared libpython3.X.so.1.0)
PBS = {
    "3.9": ("20251014", "3.9.24", "0339804b69bc00d5dde58c6694174c8e97e6f16c8ace90fe9a1b1a15456ac510"),
    "3.10": ("20251014", "3.10.19", "c0b09b744293f2aad85b1ef84544f8a7ba383675b29d1f7efd1e96bb9984399f"),
    "3.11": ("20261026", "3.11.18", "17c4978a36c496dc53c08899ef8dad1ad2734f6325f08d6033a1e66419e0af26"),
    "3.12": ("20261026", "3.12.16", "ce4f6d0a6cfdb3bed6e44c2f9affd85943c76a42e25ad3a259002f31d6fb8a20"),
    "3.13": ("20261026", "3.13.18", "df15fe60d3ff55bf9dcf25824b5572756bb8816ff93865a273d3301d7066a8b7"),
    "3.14": ("20261026", "3.14.10", "4d3c16c59ed0802a3d1d34d8bce504b15ceeaff39d62dadecc6a9fa803412aed"),
}
PBS_URL = "https://github.com/astral-sh/python-build-standalone/releases/download/{tag}/cpython-{ver}%2B{tag}-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz"
STAMP_VERSION = 1
SIM_SDKS = {"ios": "iPhoneSimulator", "tvos": "AppleTVSimulator", "watchos": "WatchSimulator", "xros": "XRSimulator"}


def home():
    return os.environ.get("HOME", "/")


def dev_home():
    """Where x86sim-dev installs things (dev.rs dev_home())"""
    if os.environ.get("X86SIM_DEV_HOME"):
        return os.environ["X86SIM_DEV_HOME"]
    return os.path.join(os.environ.get("XDG_DATA_HOME") or os.path.join(home(), ".local/share"), "x86sim-dev")


def base():
    return os.path.join(dev_home(), "swift-lldb")


def repo_root():
    return os.environ.get("X86SIM_ROOT") or os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def sdk_bundle():
    """darwin.artifactbundle, found as dev.rs swiftpm_dir() finds it"""
    x = os.environ.get("XDG_CONFIG_HOME")
    if x:
        d = os.path.join(x, "swiftpm")
    elif not os.path.exists(os.path.join(home(), ".swiftpm/swift-sdks")) and os.path.exists(os.path.join(home(), ".config/swiftpm/swift-sdks")):
        d = os.path.join(home(), ".config/swiftpm")
    else:
        d = os.path.join(home(), ".swiftpm")
    return os.path.join(d, "swift-sdks/darwin.artifactbundle")


def darwin_resource_dir():
    """A Darwin-layout Swift resource dir (iphonesimulator/, clang/, shims): the SDK bundle's, or the repo's sdks/"""
    for d in (os.path.join(sdk_bundle(), "Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift"),
              os.path.join(repo_root(), "sdks/swift-lib")):
        if os.path.isdir(os.path.join(d, "iphonesimulator")):
            return os.path.realpath(d)
    return None


def sim_sdk(platform):
    """The simulator SDK of ios / tvos / watchos / xros"""
    name = SIM_SDKS.get(platform, "iPhoneSimulator")
    for d in (os.path.join(sdk_bundle(), "Developer/Platforms/%s.platform/Developer/SDKs/%s.sdk" % (name, name)),
              os.path.join(repo_root(), "sdks/%s.sdk" % name)):
        if os.path.isfile(os.path.join(d, "SDKSettings.json")):
            return os.path.realpath(d)
    return None


def which(cmd):
    for d in os.environ.get("PATH", "").split(":"):
        p = os.path.join(d, cmd)
        if d and os.path.isfile(p) and os.access(p, os.X_OK):
            return p
    return None


def toolchains():
    """usr/ dirs of Swift toolchains that have an lldb, most preferred first"""
    c = []
    if os.environ.get("X86SIM_SWIFT_TOOLCHAIN"):
        c.append(os.path.join(os.environ["X86SIM_SWIFT_TOOLCHAIN"], "usr"))
    c.append(os.path.join(dev_home(), "swift/usr"))   # x86sim dev setup swift
    sw = which("swift")
    if sw:
        c.append(os.path.dirname(os.path.dirname(os.path.realpath(sw))))
    c += ["/usr/lib/swift/usr", "/usr/share/swift/usr", "/opt/swift/usr"]
    out = []
    for u in c:
        u = os.path.realpath(u)
        if u in out or not os.path.isfile(os.path.join(u, "bin/lldb")) or not liblldb(u):
            continue
        if not os.path.isdir(os.path.join(u, "lib/swift")):   # an upstream LLVM, not a Swift toolchain
            continue
        out.append(u)
    return out


def liblldb(usr):
    """The toolchain's real liblldb file"""
    d = os.path.join(usr, "lib")
    try:
        names = sorted(f for f in os.listdir(d) if f.startswith("liblldb.so"))
    except OSError:
        return None
    return os.path.realpath(os.path.join(d, names[-1])) if names else None


def elf_needed(path):
    """DT_NEEDED entries of an ELF64 little-endian file (its section headers: no full read of a 200 MB library)"""
    with open(path, "rb") as f:
        h = f.read(64)
        if h[:4] != b"\x7fELF" or h[4] != 2 or h[5] != 1:
            return []
        shoff, = struct.unpack_from("<Q", h, 0x28)
        shentsize, shnum = struct.unpack_from("<HH", h, 0x3a)
        f.seek(shoff)
        sh = f.read(shentsize * shnum)
        secs = [struct.unpack_from("<IIQQQQIIQQ", sh, i * shentsize) for i in range(shnum)]
        out = []
        for s in secs:
            if s[1] != 6:   # SHT_DYNAMIC
                continue
            strtab = secs[s[6]]
            f.seek(strtab[4]); strs = f.read(strtab[5])
            f.seek(s[4]); dyn = f.read(s[5])
            for i in range(0, len(dyn), 16):
                tag, val = struct.unpack_from("<qQ", dyn, i)
                if tag == 0:
                    break
                if tag == 1:
                    out.append(strs[val:strs.index(b"\0", val)].decode())
        return out


def python_needed(lib):
    for n in elf_needed(lib):
        m = re.match(r"libpython(3\.\d+)\.so", n)
        if m:
            return m.group(1)
    return None


def runs(argv, env):
    try:
        r = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=60)
        return r.returncode == 0, (r.stdout + r.stderr).strip()
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, str(e)


def file_key(p):
    st = os.stat(p)
    return [p, st.st_size, int(st.st_mtime)]


# ---- setup ----------------------------------------------------------------------------------------------------------
def install_python(ver, say):
    """The private CPython ver (python-build-standalone) in base()/python-<ver>; its prefix"""
    dest = os.path.join(base(), "python-" + ver)
    if os.path.isfile(os.path.join(dest, "lib", "libpython%s.so.1.0" % ver)):
        return dest
    if ver not in PBS:
        raise RuntimeError("lldb needs libpython%s and x86sim has no private build of that version pinned "
                           "(install python %s's shared library, or X86SIM_LLDB=/path/to/lldb)" % (ver, ver))
    if os.uname().machine != "x86_64":
        raise RuntimeError("no private Python for %s hosts (install libpython%s)" % (os.uname().machine, ver))
    tag, full, sha = PBS[ver]
    url = PBS_URL.format(tag=tag, ver=full)
    os.makedirs(base(), exist_ok=True)
    tmp = tempfile.mkdtemp(prefix=".python-", dir=base())
    try:
        tgz = os.path.join(tmp, "python.tar.gz")
        say("== swift lldb: download Python %s (python-build-standalone %s, %s)" % (full, tag, url))
        h = hashlib.sha256()
        with urllib.request.urlopen(url, timeout=60) as r, open(tgz, "wb") as f:
            total, n, last = int(r.headers.get("Content-Length") or 0), 0, -1
            while True:
                b = r.read(1 << 20)
                if not b:
                    break
                f.write(b); h.update(b); n += len(b)
                if total and n * 10 // total != last:
                    last = n * 10 // total
                    say("   %d%% of %.0f MB" % (last * 10, total / 1e6))
        if h.hexdigest() != sha:
            raise RuntimeError("Python download: sha256 %s, expected %s" % (h.hexdigest(), sha))
        say("== swift lldb: unpack into " + dest)
        out = os.path.join(tmp, "x")
        with tarfile.open(tgz) as t:
            t.extractall(out, filter="data") if hasattr(tarfile, "data_filter") else t.extractall(out)
        src = os.path.join(out, "python")
        if not os.path.isfile(os.path.join(src, "lib", "libpython%s.so.1.0" % ver)):
            raise RuntimeError("no libpython%s.so.1.0 in the archive" % ver)
        # lldb needs libpython and the standard library: not Tk, IDLE, pip, the headers or the man pages (~30 MB)
        lib = os.path.join(src, "lib")
        for d in ["share", "include"] + ["lib/" + f for f in os.listdir(lib) if re.match(r"(lib)?(tcl|tk|itcl|thread)", f)] + \
                 ["lib/python%s/%s" % (ver, f) for f in ("idlelib", "ensurepip", "tkinter", "turtledemo", "lib2to3", "site-packages")]:
            p = os.path.join(src, d)
            shutil.rmtree(p, ignore_errors=True) if os.path.isdir(p) and not os.path.islink(p) else (os.path.lexists(p) and os.unlink(p))
        os.makedirs(os.path.join(lib, "python%s" % ver, "site-packages"), exist_ok=True)
        shutil.rmtree(dest, ignore_errors=True)
        os.rename(src, dest)
        return dest
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def make_shadow(usr, lib, res, say):
    """base()/toolchain/usr: lldb + liblldb copies whose usr/lib/swift is the Darwin resource dir (see the header)"""
    root = os.path.join(base(), "toolchain")
    tmp = root + ".tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    say("== swift lldb: toolchain view with the Darwin resource dir in " + root)
    b, l = os.path.join(tmp, "usr/bin"), os.path.join(tmp, "usr/lib")
    os.makedirs(b); os.makedirs(l)
    shutil.copy2(os.path.join(usr, "bin/lldb"), b)
    for f in os.listdir(os.path.join(usr, "bin")):
        if f.startswith("lldb") or f == "repl_swift":
            if not os.path.exists(os.path.join(b, f)):
                os.symlink(os.path.join(usr, "bin", f), os.path.join(b, f))
    name = os.path.basename(lib)
    m = re.match(r"(liblldb\.so\.\d+\.\d+)", name)
    soname = m.group(1) if m else name
    # lldb finds its resource dirs from liblldb's resolved path: a copy (a reflink where the file system has them)
    r = subprocess.run(["cp", "--reflink=auto", lib, os.path.join(l, soname)], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError("copying liblldb failed: " + r.stderr.strip())
    if soname != "liblldb.so":
        os.symlink(soname, os.path.join(l, "liblldb.so"))
    os.symlink(res, os.path.join(l, "swift"))
    for f in os.listdir(os.path.join(usr, "lib")):
        if f in ("clang", "lldb") or (f.startswith("lib") and not f.startswith("liblldb")):
            os.symlink(os.path.join(usr, "lib", f), os.path.join(l, f))
    if os.path.isdir(os.path.join(usr, "local")):   # the lldb Python package (its _lldb links ../lib/liblldb.so)
        shutil.copytree(os.path.join(usr, "local"), os.path.join(tmp, "usr/local"), symlinks=True)
    shutil.rmtree(root, ignore_errors=True)
    os.rename(tmp, root)
    return os.path.join(root, "usr")


def lldb_env(info, base_env=None):
    """The environment to run info's lldb with"""
    env = dict(os.environ if base_env is None else base_env)
    env.pop("DEBUGINFOD_URLS", None)   # a debuginfod lookup per image (0.4 s each, ~1000 images in an app)
    ld = [p for p in info.get("ld_library_path", []) if p]
    if ld:
        env["LD_LIBRARY_PATH"] = ":".join(ld + ([env["LD_LIBRARY_PATH"]] if env.get("LD_LIBRARY_PATH") else []))
    if info.get("python_home"):
        env["PYTHONHOME"] = info["python_home"]
        env.pop("PYTHONPATH", None)
    return env


def setup(say=print):
    """Sets up the newest usable Swift toolchain's lldb; returns the stamp (what lldbattach.py runs)"""
    tcs = toolchains()
    if not tcs:
        raise RuntimeError("no Swift toolchain with lldb found (x86sim dev setup swift, or a swift package)")
    errors = []
    for usr in tcs:
        try:
            return setup_one(usr, say)
        except RuntimeError as e:
            errors.append("%s: %s" % (usr, e))
            say("   " + errors[-1])
    raise RuntimeError("; ".join(errors))


def setup_one(usr, say):
    lib = liblldb(usr)
    ver = python_needed(lib)
    say("== swift lldb: %s (liblldb needs %s)" % (os.path.join(usr, "bin/lldb"), "libpython" + ver if ver else "no Python"))
    info = {"version": STAMP_VERSION, "toolchain": usr, "liblldb": file_key(lib), "python_version": ver,
            "python_home": None, "python_exe": None, "ld_library_path": [], "resource_dir": None}
    if ver:
        plain = lldb_env(info)
        ok, _ = runs([os.path.join(usr, "bin/lldb"), "--version"], plain)
        if ok and subprocess.run([sys.executable, "-c", "import sys; sys.exit(0 if sys.version.startswith('%s.') else 1)" % ver]).returncode == 0:
            info["python_exe"] = sys.executable   # the system has it
        elif ok:
            info["python_exe"] = which("python" + ver)
        else:
            py = install_python(ver, say)
            info["python_home"] = py
            info["python_exe"] = os.path.join(py, "bin", "python" + ver)
            info["ld_library_path"] = [os.path.join(py, "lib")]
    res = darwin_resource_dir()
    lldb = os.path.join(usr, "bin/lldb")
    if res:
        lldb = os.path.join(make_shadow(usr, lib, res, say), "bin/lldb")
        info["resource_dir"] = res
        # liblldb's own RUNPATH ($ORIGIN/../lib/swift/linux, .../host/compiler) now points into the view: the originals
        info["ld_library_path"] += [os.path.join(usr, "lib/swift/linux"), os.path.join(usr, "lib/swift/host/compiler")]
    else:
        say("   no Darwin Swift resource dir (the SDK bundle of `x86sim dev setup`): frame variable works, po / "
            "expression of Swift values do not")
    info["lldb"] = lldb
    env = lldb_env(info)
    ok, out = runs([lldb, "--version"], env)
    if not ok or "Swift" not in out:
        raise RuntimeError("lldb does not start: " + out[-400:])
    info["lldb_version"] = " ".join(out.split())
    ok, ppath = runs([lldb, "-P"], env)
    info["python_path"] = ppath.splitlines()[-1] if ok and ppath else None
    if info["python_exe"] and info["python_path"]:
        e = dict(env, PYTHONPATH=info["python_path"])
        ok, out = runs([info["python_exe"], "-c", "import lldb; print(lldb.SBDebugger.GetVersionString())"], e)
        if not ok:
            say("   lldb's Python module does not load (%s): the MCP debug session falls back to /usr/bin/lldb" % out[-200:])
            info["python_path"] = None
    os.makedirs(base(), exist_ok=True)
    with open(os.path.join(base(), "stamp.json"), "w") as f:
        json.dump(info, f, indent=1)
    say("Swift lldb ready: %s (%s)" % (lldb, info["lldb_version"]))
    return info


def load():
    """The set-up Swift lldb (stamp.json) if it is still valid, else None"""
    try:
        with open(os.path.join(base(), "stamp.json")) as f:
            info = json.load(f)
    except (OSError, ValueError):
        return None
    try:
        if info.get("version") != STAMP_VERSION or file_key(info["liblldb"][0]) != info["liblldb"]:
            return None   # the toolchain changed (an update): set up again
    except (OSError, KeyError, IndexError):
        return None
    if not os.path.isfile(info.get("lldb", "")) or (info.get("python_home") and not os.path.isdir(info["python_home"])):
        return None
    if info.get("resource_dir") and not os.path.isdir(info["resource_dir"]):
        return None
    if not info.get("resource_dir") and darwin_resource_dir():
        return None   # the SDK bundle arrived since: set up again for po / expression
    if toolchains() and toolchains()[0] != info["toolchain"]:
        return None   # a preferred toolchain appeared (x86sim dev setup swift)
    return info


def ensure(say=print):
    """load() or setup(); None (with the reason said) when there is no Swift lldb"""
    info = load()
    if info:
        return info
    try:
        return setup(say)
    except (RuntimeError, OSError) as e:
        say("Swift lldb unavailable: %s" % e)
        return None


def status():
    info = load()
    tcs = toolchains()
    if info:
        return {"state": "ok", "lldb": info["lldb"], "detail": "%s%s" % (info.get("lldb_version", ""),
                "" if info.get("resource_dir") else "; no Darwin resource dir: po / expression of Swift values do not work"),
                "info": info}
    if not tcs:
        return {"state": "missing", "detail": "no Swift toolchain with lldb (x86sim dev setup swift)", "fix": "x86sim dev setup swift"}
    return {"state": "warn", "detail": "%s/bin/lldb not set up yet (done on the first x86sim debug)" % tcs[0],
            "fix": "x86sim dev setup lldb"}


def main(a):
    js = "--json" in a
    a = [x for x in a if x != "--json"]
    cmd = a[0] if a else "status"
    if cmd == "status":
        s = status()
        print(json.dumps(s) if js else "%s  %s%s" % (s["state"], s.get("lldb") or "", s["detail"] if not s.get("lldb") else "  " + s["detail"]))
        return 0
    if cmd == "setup":
        try:
            info = setup(print if not js else (lambda m: print(m, file=sys.stderr)))
        except (RuntimeError, OSError) as e:
            print(json.dumps({"ok": False, "error": str(e)}) if js else "swift lldb: %s" % e, file=sys.stdout if js else sys.stderr)
            return 1
        if js:
            print(json.dumps({"ok": True, **info}))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
