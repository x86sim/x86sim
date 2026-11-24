#!/usr/bin/env python3
# x86Sim (Speed 31): give an installed app's Mach-Os (executable, its dylibs, embedded frameworks, plug-ins) slots in
# <dir>/placement.txt without moving any existing slot (mkplacement.py packs in path order, so a Containers/ key
# would shift every System/ slot and force their image chunks to rebuild). A reinstalled file (other size/mtime) keeps
# its slot when it still fits. Afterwards the image gets the app tier's chunks (one per file, keyed by the Mach-O UUID
# + a hash of the file + its slot). x86sim-aot runs this on a staging copy of the table (`x86sim-aot app install`),
# under its lock, and commits the table only together with an image built for it.
#   placeapp.py [--jc JC] DIR APP_DIR...   (APP_DIR: <device>/data/Containers/Bundle/Application/<UUID>/<Name>.app,
#                                          or a single Mach-O file, e.g. a RuntimeRoot framework outside the dyld cache)
#   placeapp.py --prune [--jc JC] DIR [DATA_DIR]   (uninstall: drop every Containers/ row whose file exists on no
#                                                  phone; exit 0 = placement changed, 3 = nothing pruned)
# DIR/placement.txt is read and rewritten; JC (default DIR) is the jitcache dir, for the apps' last use (LRU).
#
# AOT cache 1 (self-managing): the app tier's slots are a bounded pool that is reused instead of growing at the end:
# - stale rows go on every run: a Containers/ row whose file exists on no phone (replaced build: new container UUID;
#   app deleted; phone deleted). Before, only rows of the same .app name went, and their ranges were never reused:
#   every reinstall of a 4 MB app took 4 MB more of the window until new installs were left unplaced.
# - new rows go into the first free gap that fits (freed slots, gaps the table always had), app area first (above the
#   shipped table's end, where the shipped table never grows into), then anywhere in the window, then after the end.
# - an app slot has 1/8 headroom, so a rebuilt app (a little larger) keeps its slot and its neighbours stay put.
# - when nothing fits, the least recently used other apps' rows are evicted (last use: the newest of its container's
#   install time and its program's training / .live files); their apps run live until installed again.
# System rows (RuntimeRoot keys) are never moved or evicted: their addresses are part of every system chunk's key.
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mkplacement import arm64_vmsize, key_of, LO, HI, GUARD

ALIGN = 0x10000


def align(v, a=ALIGN):
    return (v + a - 1) & ~(a - 1)


def is_app(key):
    return key.startswith("Containers/")


def app_slot(vs):
    return align(vs + vs // 8)


def fit(rows, size, lo):
    """first address >= lo where size (plus the guard before the next row) fits between rows, else None"""
    at = align(lo)
    for a, s in sorted((r[0], r[1]) for r in rows):
        if a + s + GUARD <= at:
            continue
        if a >= at + size + GUARD:
            return at
        at = max(at, align(a + s + GUARD))
    return at if at + size <= HI else None


def main():
    args = sys.argv[1:]
    prune_all = "--prune" in args
    if prune_all:
        args.remove("--prune")
    jc = None
    if "--jc" in args:
        i = args.index("--jc")
        jc = args[i + 1]
        del args[i:i + 2]
    d, apps = args[0], args[1:]
    jc = jc or d
    extra_data = apps[:1] if prune_all else []
    if prune_all:
        apps = []
    path = os.path.join(d, "placement.txt")
    lines = open(path).read().splitlines() if os.path.exists(path) else []
    rows = []  # [addr, slot, fsize, mtime, key]
    for l in lines:
        f = l.split(None, 4)
        if len(f) == 5:
            rows.append([int(f[0], 16), int(f[1], 16), int(f[2]), int(f[3]), f[4]])
    by_key = {r[4]: r for r in rows}
    changed = 0
    # every phone's data dir (the jitcache dir is shared by every device of this runtime), the installing device's and
    # the one named on the command line
    datas = set()
    root = os.path.realpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../.."))
    vd = os.path.join(root, "var")
    if os.path.isdir(vd):
        datas |= {os.path.join(vd, x, "data") for x in os.listdir(vd) if x.startswith("device")}
    for app in apps:
        rp = os.path.realpath(app)
        i = rp.find("/Containers/")
        if i >= 0:
            datas.add(rp[:i])
    datas |= {os.path.realpath(x) for x in extra_data}
    for r in list(rows):
        if is_app(r[4]) and not any(os.path.isfile(os.path.join(x, r[4])) for x in datas):
            rows.remove(r)
            by_key.pop(r[4], None)
            changed += 1
            print(f"placeapp: pruned stale {r[4]}")
    # the app area: above the shipped table's end (jc-<build> -> cli/libexec/arm64/placement-<build>.txt)
    area = LO
    build = os.path.basename(os.path.realpath(jc))[3:] if os.path.basename(os.path.realpath(jc)).startswith("jc-") else ""
    shipped = os.path.join(root, f"cli/libexec/arm64/placement-{build}.txt")
    if build and os.path.exists(shipped):
        ends = [int(f[0], 16) + int(f[1], 16) for f in (l.split(None, 4) for l in open(shipped)) if len(f) == 5]
        area = align(max(ends, default=LO) + GUARD)
    newest = None  # program name -> newest training / .live file in the jitcache dir

    def last_use(r):
        nonlocal newest
        if newest is None:
            newest = {}
            try:
                for e in os.scandir(jc):
                    n = e.name
                    if n.endswith(".live") or n.endswith(".bin"):
                        i, j = n.find("."), n.rfind(".")
                        if j > i > 0:
                            exe = n[i + 1:j]
                            newest[exe] = max(newest.get(exe, 0), e.stat().st_mtime)
            except OSError:
                pass
        f = r[4].split("/")
        t = newest.get(f[-1], 0)
        for x in datas:
            try:
                t = max(t, os.stat(os.path.join(x, *f[:4])).st_ctime)
            except OSError:
                pass
        return t

    mine = set()  # keys this run places (never evicted for each other)
    todo = []
    for app in apps:
        rp = os.path.realpath(app)  # Perf 22: a file names itself (RuntimeRoot frameworks a process loads)
        walk = [(os.path.dirname(rp), [], [os.path.basename(rp)])] if os.path.isfile(rp) else os.walk(rp)
        for dd, _, fs in walk:
            for n in sorted(fs):
                p = os.path.join(dd, n)
                if os.path.islink(p) or not os.path.isfile(p):
                    continue
                vs, k = arm64_vmsize(p), key_of(p)
                if not vs or not k:
                    continue
                mine.add(k)
                todo.append((p, vs, k))
    for p, vs, k in todo:
        st = os.stat(p)
        slot = app_slot(vs) if is_app(k) else (vs + 0x3FFF) & ~0x3FFF
        r = by_key.get(k)
        if r and (r[2], r[3]) == (st.st_size, int(st.st_mtime)):
            continue
        if r and ((vs + 0x3FFF) & ~0x3FFF) <= r[1]:
            r[2], r[3] = st.st_size, int(st.st_mtime)
        else:
            if r:
                rows.remove(r)
                by_key.pop(k, None)
            a = fit(rows, slot, area) if is_app(k) else None
            if a is None:
                a = fit(rows, slot, LO)
            while a is None and is_app(k):
                victims = sorted((x for x in rows if is_app(x[4]) and x[4] not in mine), key=last_use)
                if not victims:
                    break
                # the least recently used app whose slot alone makes room (with the free space around it); else
                # the least recently used one, and look again
                enough = [x for x in victims if fit([y for y in rows if y is not x], slot, LO) is not None]
                v = enough[0] if enough else victims[0]
                rows.remove(v)
                by_key.pop(v[4], None)
                changed += 1
                print(f"placeapp: evicted least recently used {v[4]} ({v[1] >> 10} KB)")
                a = fit(rows, slot, area)
                if a is None:
                    a = fit(rows, slot, LO)
            if a is None:
                print(f"placeapp: window full, {k} ({slot >> 10} KB) stays unplaced", file=sys.stderr)
                continue
            r = [a, slot, st.st_size, int(st.st_mtime), k]
            rows.append(r)
            by_key[k] = r
        changed += 1
        print(f"placeapp: {r[0]:x} {r[1] >> 10} KB {k}")
    if not changed:
        print("placeapp: nothing to prune" if prune_all else "placeapp: nothing to place")
        if prune_all:
            sys.exit(3)
        return
    # rows stay in table order (existing rows keep their lines; new ones at the end), as before
    tmp = path + ".new"
    with open(tmp, "w") as f:
        f.writelines(f"{a:x} {s:x} {fs} {mt} {k}\n" for a, s, fs, mt, k in rows)
    os.replace(tmp, path)
    used = sum(r[1] for r in rows if is_app(r[4]))
    print(f"placeapp: app tier {sum(1 for r in rows if is_app(r[4]))} files, {used >> 20} MB")


if __name__ == "__main__":
    main()
