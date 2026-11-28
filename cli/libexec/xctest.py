#!/usr/bin/env python3
"""x86Sim: run XCTest / XCUITest bundles on a phone, as `xcodebuild test-without-building` does in a Mac's simulator
(x86sim test). Three kinds of test targets, as Xcode runs them:

  logic    a test bundle without a host app: the xctest tool (Xcode's <Platform>.platform/Developer/Library/Xcode/
           Agents/xctest, else the runtime's /Developer/usr/bin/xctest) loads it, run in the device as simctl spawn
  hosted   a unit test bundle inside its app (PlugIns/X.xctest): the app is installed and launched with
           libXCTestBundleInject.dylib inserted, which loads the bundle and runs it in the app's process
  ui       an XCUITest runner (<Name>UITests-Runner.app) and the app it tests: both installed, the runner launched; its
           XCUIApplication launches and drives the app through testmanagerd (the phone's `xctest` feature)

Each process gets an .xctestconfiguration (an NSKeyedArchiver XCTestConfiguration, written here as Xcode does) through
XCTestConfigurationFilePath, without an IDE session: XCTest then runs the selected tests and prints its usual log
(Test Suite / Test Case lines, "file:line: error: -[Class test] : message"), which is parsed into per-test results.

Inputs: an .xctestrun (what `xcodebuild build-for-testing` writes next to the products; FormatVersion 1 or 2),
explicit bundles (--app / --test-bundle / --ui-test-runner), or a Swift package (--spm DIR: swift build --build-tests
for the simulator, then its logic bundle). XCTest's own frameworks are Xcode's, not the runtime's: --platform DIR,
$X86SIM_XCTEST_PLATFORMS, sdks/<Platform>.platform, or the xtool SDK from `x86sim dev setup` (an Xcode .xip).
Results: per-test lines while it runs, a summary as xcodebuild prints it, results.json and junit.xml in the output dir,
and the tests' XCTest attachments (screenshots, strings, data; libsimshim saves them) in attachments/<Class>.<test>/,
listed per test in results.json (deleteOnSuccess ones of passed tests are dropped, as Xcode does), each with the
activity it was added in, and XCTest's activity tree per test ("activities", libsimshim's); with --coverage,
code coverage in coverage/ (LLVM_PROFILE_FILE as Xcode sets it, llvm-profdata + llvm-cov for xccov's report);
exit status 0 when no test failed (passed, skipped or an expected failure: XCTExpectFailure, withKnownIssue), 1
when one failed, 2 when nothing could run.
"""
import atexit, fcntl
import glob, hashlib, tempfile
import json, os, plistlib, re, shutil, signal, struct, subprocess, sys, time, urllib.parse, uuid
import xml.etree.ElementTree as ET
from plistlib import UID

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "lib"))
import x86config  # noqa: E402

USAGE = """usage: x86sim [--phone NAME] test [options]
       x86sim test fetch-platform [ios|tvos|watchos|xros] [--platform DIR]   (copy XCTest's platform files into sdks/)

What to run (one of):
  --xctestrun FILE        an .xctestrun from `xcodebuild build-for-testing` (products next to it, as Xcode's
                          __TESTROOT__; --products DIR if they were moved)
  --app A.app [--test-bundle B.xctest]
                          unit tests hosted in A (default: every bundle in A/PlugIns)
  --test-bundle B.xctest  logic tests (no host app), run by the xctest tool
  --ui-test-runner R.app --app A.app
                          UI tests: R's PlugIns/*.xctest drive A (needs the device's xctest feature)
  --spm DIR               a Swift package: swift build --build-tests for the phone's simulator, then its tests
Options:
  --only-testing T, --skip-testing T
                          Target[/Class[/test]] or Class[/test] (repeatable), as xcodebuild -only-testing:
  --target NAME           only this test target of the .xctestrun (repeatable)
  --platform DIR          a dir holding <Platform>.platform (Xcode's Contents/Developer/Platforms)
  --out DIR               logs, results.json, junit.xml (default $X86SIM_SCRATCH/xctest/<phone>-<time>)
  --result-bundle B.xcresult
                          also write an Xcode result bundle (what xcodebuild -resultBundlePath writes; xcresulttool,
                          xccov and CI tools read it): results, failures, attachments, coverage with --coverage
  --built                 the products were built for this run (xcodebuild test, not test-without-building): the
                          result bundle's build action succeeded instead of "not requested"
  --build-log FILE        the same, with the build's log and issues (the xcodebuild stand-in's build-log.json); a
                          failed build runs nothing and writes the result bundle Xcode writes then (testing cancelled)
  --json FILE, --junit FILE
                          also write the JSON summary / JUnit XML there ("-": JSON to stdout, nothing else printed)
  --timeout S             per test target (default 1800)
  --no-screen-recording   UI tests: no screen recording of each test (by default, as Xcode: a failed test keeps one,
                          attachments/<Class>.<test>/Screen_Recording.mp4)
  --coverage              collect code coverage, as xcodebuild -enableCodeCoverage YES: the test processes (and the
                          app a UI test drives) write LLVM profiles; merged into coverage/Coverage.profdata, reported
                          per product and file (coverage/report.txt, coverage.json, lcov.info; "coverage" in
                          results.json). The products must be built with coverage (CLANG_COVERAGE_MAPPING=YES)
  --verbose               everything the test processes print (default: XCTest's own lines)
  --quiet                 only the summary
"""

DV = x86config.get("X86SIM_DVIEW")
ROOT = x86config.ROOT
PLATFORMS = {"ios": "iPhoneSimulator", "tvos": "AppleTVSimulator", "watchos": "WatchSimulator", "xros": "XRSimulator"}
TRIPLES = {"ios": "arm64-apple-ios-simulator", "tvos": "arm64-apple-tvos-simulator",
           "watchos": "arm64-apple-watchos-simulator", "xros": "arm64-apple-xros-simulator"}


def die(msg, code=2):
    print("x86sim test: " + msg, file=sys.stderr)
    sys.exit(code)


def dv(p):
    """A Linux path as processes in the device see it."""
    return DV + os.path.realpath(p)


def undv(p):
    return p[len(DV):] if p.startswith(DV + "/") else p


# ---------------------------------------------------------------------------------------------------------------------
# XCTestConfiguration, as NSKeyedArchiver writes it (layout of Xcode 27.2's archive: every key XCTestConfiguration's
# initWithCoder: reads, the nested policy objects with their defaults)

class Obj:
    def __init__(self, classes, fields):
        self.classes, self.fields = classes, fields


class URL:
    def __init__(self, path):
        self.path = path


class Num(int):
    """An NSNumber archived as an object (encodeObject:), not inline"""


class MutableDict(dict):
    """An NSMutableDictionary (XCTest's aggregateStatisticsBeforeCrash is one)"""


class NSUUID:
    def __init__(self, u):
        self.u = u


def archive(root):
    objs = ["$null"]
    classes = {}

    def cls(chain):
        k = tuple(chain)
        if k not in classes:
            objs.append({"$classes": list(chain), "$classname": chain[0]})
            classes[k] = UID(len(objs) - 1)
        return classes[k]

    def enc(v):
        if v is None:
            return UID(0)
        if isinstance(v, (bool, int, float)) and not isinstance(v, Num):
            return v
        i = len(objs)
        objs.append(None)
        if isinstance(v, (str, Num)):
            objs[i] = int(v) if isinstance(v, Num) else v
        elif isinstance(v, URL):
            u = v.path if "://" in v.path else "file://" + v.path
            objs[i] = {"NS.base": UID(0), "NS.relative": enc(u), "$class": cls(["NSURL", "NSObject"])}
        elif isinstance(v, NSUUID):
            objs[i] = {"NS.uuidbytes": uuid.UUID(v.u).bytes, "$class": cls(["NSUUID", "NSObject"])}
        elif isinstance(v, (list, tuple)):
            objs[i] = {"NS.objects": [enc(x) for x in v], "$class": cls(["NSArray", "NSObject"])}
        elif isinstance(v, (set, frozenset)):
            objs[i] = {"NS.objects": [enc(x) for x in sorted(v)], "$class": cls(["NSSet", "NSObject"])}
        elif isinstance(v, dict):
            ks = list(v)
            objs[i] = {"NS.keys": [enc(k) for k in ks], "NS.objects": [enc(v[k]) for k in ks],
                       "$class": cls(["NSMutableDictionary", "NSDictionary", "NSObject"] if isinstance(v, MutableDict)
                                     else ["NSDictionary", "NSObject"])}
        elif isinstance(v, Obj):
            d = {k: (x if isinstance(x, bool) else enc(x)) for k, x in v.fields.items()}
            d["$class"] = cls(v.classes)
            objs[i] = d
        else:
            raise TypeError(type(v))
        return UID(i)

    top = enc(root)
    return plistlib.dumps({"$archiver": "NSKeyedArchiver", "$objects": objs, "$top": {"root": top}, "$version": 100000},
                          fmt=plistlib.FMT_BINARY)


def test_identifiers(s, skip=False):
    """The identifiers Xcode 27.2 puts in an XCTestConfiguration for an -only-testing / -skip-testing string (read
    from the configuration it hands a test process): XCTest's ("Class": o 3; "Class/method": o 2, "Class/method()":
    o 6, a skip list both) and Swift Testing's (-[XCTTestIdentifier initWithSwiftTestingStringRepresentation:]: the
    components as written, o 0 for a test, 5 for one with labelled arguments "f(x:)", 1 for a suite; "Class" also
    gets o 0). The bits: 1 container (oc), 2 XCTest method (ocm, with argument IDs), 4 Swift (os)."""
    parts = [p for p in s.split("/") if p]
    if not parts:
        return []

    def ident(c, o):
        d = {"a": [] if o & 3 == 2 else None, "c": c, "o": o}
        d.update({k: True for k, bit in (("oc", 1), ("ocm", 2), ("os", 4)) if o & bit})
        return Obj(["XCTTestIdentifier", "NSObject"], d)

    last = parts[-1]
    out = []
    if len(parts) == 1 and not last.endswith(")"):
        out.append(ident(parts, 3))
    elif len(parts) == 2:
        c = [parts[0], re.sub(r"\(.*\)$", "", last)]
        swift = last.endswith(")")
        out += [ident(c, 6)] if swift and not skip else [ident(c, 2)] + ([ident(c, 6)] if skip else [])
    out.append(ident(parts, 5 if last.endswith(":)") else 0 if last.endswith(")") else 1))
    if len(parts) == 1 and not last.endswith(")"):
        out.append(ident(parts, 0))
    return out


def url_part(s):
    return urllib.parse.quote(s, safe="()-._~!$&'*+,;=:@")


def rel_bundle(bundle):
    """testBundleRelativePath: a hosted bundle's path in its host app (PlugIns/<name>.xctest), None for a logic test's"""
    m = re.search(r"\.app/(PlugIns/[^/]+\.xctest)/?$", bundle)
    return m.group(1) if m else None


def identifier_set(ids, skip=False):
    return Obj(["XCTTestIdentifierSet", "NSObject"], {"identifiers": [i for x in ids for i in test_identifiers(x, skip)]})


# Swift Testing's Interop.Mode, as an .xctestrun names it (TestInteropMode); Xcode 27.2 writes "complete"
INTEROP_MODES = {"none": 0, "limited": 1, "complete": 2, "strict": 3}


# an .xctestrun's SystemAttachmentLifetime / UserAttachmentLifetime as XCTestConfiguration has them (XCTAttachmentLifetime)
LIFETIMES = {"keepAlways": 0, "deleteOnSuccess": 1, "keepNever": 2}


def configuration(bundle, module, target_path=None, target_bid=None, target_args=(), target_env=None, ui=False,
                  only=None, skip=(), interop=2, t=None, scheme=None):
    """The XCTestConfiguration Xcode 27.2 hands a test process in `xcodebuild test-without-building` (read on the Mac
    from XCTestConfiguration.activeTestConfiguration), less what only its IDE session has (reportResultsToIDE,
    sessionIdentifier, IDECapabilities, testApplicationDependencies). t: the .xctestrun target (attachment
    lifetimes); scheme: Runner.scheme_info() (the project's name and the scheme's products: crash report emphasis,
    symbolication, the apps' bundle infos), None for none."""
    t, scheme = t or {}, scheme or {}
    to_run = identifier_set(only) if only else None
    sel = Obj(["XCTTestSelection", "NSObject"], {
        "IdentifiersToRun": to_run, "IdentifiersToSkip": identifier_set(skip, skip=True),
        "TagsToRun": None,
        "TagsToSkip": Obj(["XCTTagSelection", "NSObject"], {"XCTTagSelectionIncludeXCTests": False,
                                                          "XCTTagSelectionMode": 1, "XCTTagSelectionTagSet": set()})})
    f = {
        "ApplicationBundleInfos": scheme.get("bundle_infos") if ui else None,
        "EnablePerformanceTestsDiagnostics": None, "IDECapabilities": None,
        "IsMemoryTaggingAddressSanitizerEnabled": False,
        "aggregateStatisticsBeforeCrash": MutableDict(XCSuiteRecordsKey=MutableDict()),
        "applicationCrashDetectionPolicy": Obj(["XCTApplicationCrashDetectionPolicy", "NSObject"],
                                               {"XCTApplicationCrashDetectionSeverity": 2}),
        "automationFrameworkPath": scheme.get("automation"),
        "baseTestsURL": URL("test://com.apple.xcode/%s/%s" % (url_part(scheme["project"]), url_part(module)))
        if scheme.get("project") else None,
        "baselineFileRelativePath": None, "baselineFileURL": None,
        "bundleIDsForCrashReportEmphasis": scheme.get("bundle_ids"), "defaultTestExecutionTimeAllowance": Num(600),
        "deferSymbolicationToHarness": False, "disablePerformanceMetrics": False, "emitOSLogs": False, "formatVersion": 2,
        "gatherLocalizableStringsData": False,
        "hangDetectionPolicy": Obj(["XCTHangDetectionPolicy", "NSObject"],
                                   {"detectionEnabled": False, "durationThreshold": 1.0, "severity": 0}),
        "inProcessParallelizationEnabled": True, "initializeForMultiDevice": False, "initializeForUITesting": ui,
        "leaksCheckingMode": 0, "maximumTestExecutionTimeAllowance": None, "multiDevicePlatformVersionMap": None,
        "multiDeviceRemoteRunnerPlatformMap": None, "multiDeviceRequirementsFilePath": None,
        "performanceTestConfiguration": None, "preferredScreenCaptureFormat": 1, "productModuleName": module,
        "randomExecutionOrderingSeed": None,
        "repetitionPolicy": Obj(["XCTRepetitionPolicy", "NSObject"],
                                {"XCTMaximumIterations": 1, "XCTRepeatInNewRunnerProcess": False, "XCTRepetitionMode": 0}),
        # no IDE session: XCTest reports to its log only (reportResultsToIDE / sessionIdentifier unset)
        "reportActivities": True, "reportResultsToIDE": False, "resultChannelConfiguration": None,
        "runAsMultiDeviceRemoteRunner": False,
        "runtimeDiagnosticsPolicy": Obj(["XCTRuntimeDiagnosticsPolicy", "NSObject"], {}),
        "sessionIdentifier": None, "shouldExtractMultiDeviceRequirements": False,
        "symbolicationImageNames": scheme.get("image_names"),
        # deleteOnSuccess by default, as Xcode: failure screenshots are kept (#50)
        "systemAttachmentLifetime": LIFETIMES.get(t.get("SystemAttachmentLifetime"), 1),
        "targetApplicationArguments": list(target_args) if target_path else None,
        "targetApplicationBundleID": target_bid,
        "targetApplicationEnvironment": dict(target_env or {}) if target_path else None,
        # testApplicationDependencies (Xcode: the scheme's apps by bundle id) is left out: with it, XCUIApplication has
        # Xcode launch the app over the IDE session ("Failed to launch ... via Xcode: no connection to Xcode exists")
        "targetApplicationPath": target_path, "testApplicationDependencies": None,
        "testApplicationUserOverrides": None,
        "testBundleRelativePath": rel_bundle(bundle), "testBundleURL": URL(bundle.rstrip("/") + "/"),
        "testExecutionOrdering": 0, "testIdentifiersToRun": to_run, "testInstructionsSet": None,
        "testInteropMode": interop, "testSelection": sel, "testTimeoutsEnabled": False,
        "testsDrivenByIDE": False, "testsMustRunOnMainThread": ui, "traceCollectionEnabled_v2": None,
        "treatMissingBaselinesAsFailures": False,
        "userAttachmentLifetime": LIFETIMES.get(t.get("UserAttachmentLifetime"), 1),
    }
    return archive(Obj(["XCTestConfiguration", "NSObject"], f))


# ---------------------------------------------------------------------------------------------------------------------
# The phone and XCTest's frameworks

def device_platform(dev):
    try:
        rt = open(os.path.join(dev, "runtime")).read().strip()
    except OSError:
        rt = "ios"
    return rt.split("-")[0] if rt.split("-")[0] in PLATFORMS else "ios", rt


def runtime_root(rt):
    for p in (os.path.join(ROOT, "runtimes", rt, "RuntimeRoot"), x86config.get("X86SIM_RUNTIME")):
        if os.path.isdir(p):
            return os.path.realpath(p)
    die("no runtime %s in runtimes/" % rt)


def find_platform(name, given):
    """Xcode's <name>.platform (holding Developer/Library/Frameworks/XCTest.framework)."""
    xtool = os.path.expanduser("~/.config/swiftpm/swift-sdks/darwin.artifactbundle/Developer/Platforms")
    cands = [given, os.environ.get("X86SIM_XCTEST_PLATFORMS"), os.path.join(ROOT, "sdks"), xtool]
    for c in filter(None, cands):
        for p in (os.path.join(c, name + ".platform"), c):
            if os.path.isdir(os.path.join(p, "Developer/Library/Frameworks/XCTest.framework")):
                return os.path.realpath(p)
    die("no XCTest frameworks for %s: set up the Darwin SDK from an Xcode .xip on this machine (x86sim dev setup sdk "
        "--xip Xcode.xip: Xcode from developer.apple.com, free Apple ID), or point --platform / X86SIM_XCTEST_PLATFORMS "
        "at a copy of Xcode's Contents/Developer/Platforms" % name)


# the parts of <Platform>.platform/Developer a test process loads
PLATFORM_PARTS = ["Library/Frameworks", "Library/PrivateFrameworks/XCTestCore.framework",
                  "Library/PrivateFrameworks/XCTestSupport.framework",
                  "Library/PrivateFrameworks/XCTAutomationSupport.framework", "usr/lib/libXCTestBundleInject.dylib",
                  "usr/lib/libXCTestSwiftSupport.dylib", "usr/lib/lib_TestingInterop.dylib",
                  "Library/Xcode/Agents/xctest"]


def macho_archs(p):
    try:
        with open(p, "rb") as f:
            m = f.read(4)
    except OSError:
        return False
    return m in (b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca")   # fat (universal)


def thin_platform(src, name):
    """An arm64-only copy of what a test process loads from the platform, in the scratch dir. A universal binary is
    started as x86_64 by Darling's posix_spawn (dyld_sim then refuses it: "not compatible with the loaded process"),
    so the xctest agent and, for consistency, every library are thinned. Remade when the source changes."""
    dst_root = os.path.join(x86config.get("X86SIM_SCRATCH"), "xctest", "platforms")
    dst = os.path.join(dst_root, name + ".platform")
    stamp = "%s %s\n" % (src, os.stat(os.path.join(src, "Developer/Library/Frameworks/XCTest.framework")).st_mtime)
    os.makedirs(dst_root, exist_ok=True)
    # Issue #49: runs on several phones at once share this copy. One builds it under a lock, in a sibling directory
    # renamed into place; the others wait for the lock and use the finished copy (they used to find a half-made one:
    # FileExistsError in copytree). A run still using the previous copy keeps its files until it ends (renamed aside).
    with open(os.path.join(dst_root, ".%s.lock" % name), "w") as lk:
        fcntl.flock(lk, fcntl.LOCK_EX)
        try:
            if open(os.path.join(dst, ".x86sim-stamp")).read() == stamp:
                return dst_root
        except OSError:
            pass
        tmp = "%s.tmp.%d" % (dst, os.getpid())
        shutil.rmtree(tmp, ignore_errors=True)
        lipo = shutil.which("llvm-lipo") or shutil.which("lipo")
        for part in PLATFORM_PARTS:
            s, d = os.path.join(src, "Developer", part), os.path.join(tmp, "Developer", part)
            if not os.path.exists(s):
                continue
            os.makedirs(os.path.dirname(d), exist_ok=True)
            if os.path.isdir(s):
                shutil.copytree(s, d, symlinks=True)
            else:
                shutil.copy2(s, d)
        for dp, _, fs in os.walk(tmp):
            for f in fs:
                p = os.path.join(dp, f)
                if not os.path.islink(p) and macho_archs(p):
                    if not lipo:
                        shutil.rmtree(tmp, ignore_errors=True)
                        die("llvm-lipo is needed to thin %s (install llvm)" % p)
                    subprocess.run([lipo, "-thin", "arm64", p, "-output", p + ".thin"], check=True)
                    os.replace(p + ".thin", p)
        open(os.path.join(tmp, ".x86sim-stamp"), "w").write(stamp)
        old = "%s.old.%d" % (dst, os.getpid())
        if os.path.lexists(dst):
            os.rename(dst, old)
        os.rename(tmp, dst)
        shutil.rmtree(old, ignore_errors=True)
    return dst_root


def simswt(src, plat):
    """libsimswt.dylib (host/simswt) for this platform and its Testing.framework, built here on first use and kept in
    the scratch dir: inserted into the test process, it writes Swift Testing's events as JSON lines (each test's
    suite path, issues with their source locations, skips), which the console lines do not tell. None when it cannot
    be built (no Swift toolchain): Swift Testing's results then come from the console."""
    here = os.path.join(ROOT, "host", "simswt")
    tface = os.path.join(src, "Developer/Library/Frameworks/Testing.framework/Modules/Testing.swiftmodule",
                         TRIPLES[plat] + ".swiftinterface")
    try:
        h = hashlib.sha256(plat.encode())
        for p in sorted(glob.glob(os.path.join(here, "*"))) + [tface]:
            h.update(open(p, "rb").read())
    except OSError:
        return None
    d = os.path.join(x86config.get("X86SIM_SCRATCH"), "xctest", "simswt")
    out = os.path.join(d, "libsimswt-%s-%s.dylib" % (plat, h.hexdigest()[:16]))
    if os.path.exists(out):
        return out
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, ".lock"), "w") as lk:   # several phones' runs at once: one builds
        fcntl.flock(lk, fcntl.LOCK_EX)
        if not os.path.exists(out):
            r = subprocess.run([os.path.join(here, "build.sh"), src, plat, out], capture_output=True, text=True)
            if r.returncode:
                print("x86sim test: libsimswt not built (Swift Testing's results from its console lines): %s"
                      % (r.stderr.strip().splitlines() or ["?"])[-1], file=sys.stderr)
                return None
    return out


# ---------------------------------------------------------------------------------------------------------------------
# Test targets

def bundle_id(app):
    try:
        return plistlib.load(open(os.path.join(app, "Info.plist"), "rb"))["CFBundleIdentifier"]
    except (OSError, KeyError, plistlib.InvalidFileException):
        raise RuntimeError("%s: no CFBundleIdentifier in its Info.plist" % app)


def bundle_module(b):
    try:
        info = plistlib.load(open(os.path.join(b, "Info.plist"), "rb"))
        return info.get("CFBundleExecutable") or os.path.splitext(os.path.basename(b))[0]
    except (OSError, plistlib.InvalidFileException):
        return os.path.splitext(os.path.basename(b))[0]


def load_xctestrun(path, products):
    with open(path, "rb") as f:
        d = plistlib.load(f)
    root = os.path.realpath(products or os.path.dirname(os.path.abspath(path)))
    meta = d.get("__xctestrun_metadata__", {})
    if meta.get("FormatVersion", 1) >= 2 or "TestConfigurations" in d:
        targets = []
        for c in d.get("TestConfigurations", []):
            for t in c.get("TestTargets", []):
                t = dict(t)
                t.setdefault("_name", t.get("BlueprintName") or t.get("ProductModuleName"))
                t["_config"] = c.get("Name")
                targets.append(t)
    else:
        targets = [dict(v, _name=k) for k, v in d.items() if not k.startswith("__") and isinstance(v, dict)]
    targets.sort(key=lambda t: t.get("RunOrder", 0))
    container = (meta.get("ContainerInfo") or d.get("ContainerInfo") or {}).get("ContainerName")
    for t in targets:
        t["_container"] = container or t.get("BlueprintProviderName")
    return root, targets


def kind_of(t):
    if t.get("IsUITestBundle") or t.get("UITargetAppPath"):
        return "ui"
    host = t.get("TestHostPath", "")
    if not host or host.endswith("/xctest") or "/Agents/" in host:
        return "logic"
    return "hosted"


def explicit_targets(a):
    """Targets in the .xctestrun schema from --app / --test-bundle / --ui-test-runner."""
    ts = []
    if a["runner"]:
        if not a["app"]:
            die("--ui-test-runner needs --app (the app the UI tests drive)")
        r = os.path.realpath(a["runner"])
        for b in sorted(x for x in os.listdir(os.path.join(r, "PlugIns")) if x.endswith(".xctest")) \
                if os.path.isdir(os.path.join(r, "PlugIns")) else []:
            ts.append({"_name": b[:-7], "IsUITestBundle": True, "TestHostPath": r,
                       "TestHostBundleIdentifier": bundle_id(r), "TestBundlePath": "__TESTHOST__/PlugIns/" + b,
                       "UITargetAppPath": os.path.realpath(a["app"]), "ProductModuleName": b[:-7]})
        if not ts:
            die("%s has no PlugIns/*.xctest" % a["runner"])
        return os.path.dirname(r), ts
    if a["app"]:
        app = os.path.realpath(a["app"])
        plugins = os.path.join(app, "PlugIns")
        bundles = [os.path.realpath(b) for b in a["bundles"]]
        if not bundles and os.path.isdir(plugins):
            bundles = [os.path.join(plugins, x) for x in sorted(os.listdir(plugins)) if x.endswith(".xctest")]
        if not bundles:
            die("%s has no PlugIns/*.xctest: give --test-bundle" % a["app"])
        for b in bundles:
            inside = b.startswith(app + "/")
            ts.append({"_name": os.path.basename(b)[:-7], "IsAppHostedTestBundle": True, "TestHostPath": app,
                       "TestHostBundleIdentifier": bundle_id(app),
                       "TestBundlePath": "__TESTHOST__" + b[len(app):] if inside else b,
                       "ProductModuleName": bundle_module(b)})
        return os.path.dirname(app), ts
    for b in a["bundles"]:
        b = os.path.realpath(b)
        ts.append({"_name": os.path.basename(b)[:-7], "TestBundlePath": b, "ProductModuleName": bundle_module(b)})
    return os.path.dirname(os.path.realpath(a["bundles"][0])), ts


def spm_targets(a, plat):
    """--spm DIR: build the package's tests for the simulator with the Swift SDK of `x86sim dev setup` (xtool's
    darwin SDK) and run the resulting logic bundle. SwiftPM writes the macOS bundle layout (Contents/MacOS) without an
    Info.plist on Linux: a flat iOS-style copy with one is made next to it."""
    d = os.path.realpath(a["spm"])
    triple = TRIPLES[plat]
    desc = spm_describe(d)
    a["spm_project"] = desc.get("name") or os.path.basename(d)   # the result bundle's project, as Xcode names it
    cmd = ["nice", "swift", "build", "--build-system", "native", "--build-tests", "--swift-sdk", "darwin",
           "--triple", triple, "--package-path", d, "-j", os.environ.get("X86SIM_BUILD_JOBS", "3")]
    if a["coverage"]:
        cmd.append("--enable-code-coverage")
    if plat == "xros":
        # SwiftPM links the test bundle for XCTest's minimum OS, which it can't read here for visionOS and takes as
        # 12.0 (clang: "invalid version number in '--target=arm64-apple-xros12.0-simulator'"): the package's own
        # visionOS version instead, else XCTest's (1.0 in the visionOS 27.2 SDK)
        v = next((p.get("version") for p in desc.get("platforms") or [] if p.get("name") == "visionos"), None) or "1.0"
        cmd += ["-Xswiftc", "-target", "-Xswiftc", "arm64-apple-xros%s-simulator" % v]
    print("x86sim test: " + " ".join(cmd[1:]), file=sys.stderr)
    if subprocess.run(cmd, stdout=sys.stderr).returncode:
        die("swift build --build-tests failed", 1)
    out = subprocess.run(cmd[1:] + ["--show-bin-path"], capture_output=True, text=True).stdout.strip().splitlines()
    bindir = out[-1] if out else os.path.join(d, ".build", triple, "debug")
    found = [os.path.join(bindir, x) for x in sorted(os.listdir(bindir)) if x.endswith(".xctest")]
    if not found:
        die("no .xctest in %s" % bindir)
    # SwiftPM puts every test target in one <Package>PackageTests bundle, Xcode makes a bundle per target: selections
    # name the test target (CalcKitTests/meanOfNothing()), and Swift Testing finds a selected or skipped test only
    # when the configuration's productModuleName is that target's module (it matches "<module>.<test>"), while a run
    # with no run list runs every module's tests in the bundle. So the bundle is linked again once per test target
    # from the objects SwiftPM built (spm_split), and each target runs alone as in Xcode: every test once, each
    # selection and skip matched in its own module. The package bundle's name still selects a lone test target.
    tests = [t for t in desc.get("targets") or [] if t.get("type") == "test" and t.get("c99name")]
    modules = [t["c99name"] for t in tests]
    split = spm_split(bindir, found[0], tests) if modules and len(found) == 1 else None
    if split:
        pkg = os.path.splitext(os.path.basename(found[0]))[0]
        return os.path.dirname(split[0][1]), [{"_name": m, "TestBundlePath": b, "ProductModuleName": m,
                                               "_modules": [pkg] if len(split) == 1 else []} for m, b in split]
    if len(modules) > 1:
        print("x86sim test: %s holds %d test targets and could not be linked per target: it runs as one bundle, "
              "where Swift Testing skips match no test target" % (os.path.basename(found[0]), len(modules)),
              file=sys.stderr)
    a["bundles"] = [ios_layout(b) for b in found]
    root, ts = explicit_targets(a)
    out = []
    for t in ts:
        if len(modules) == 1:
            out.append(dict(t, _name=modules[0], ProductModuleName=modules[0], _modules=[t["_name"]]))
        else:
            out.append(dict(t, _modules=modules))
    return root, out


def link_command(manifest, exe):
    """The arguments of the llbuild manifest's (SwiftPM's <config>.yaml) command whose output is exe, or None"""
    try:
        lines = open(manifest).read().splitlines()
    except OSError:
        return None
    hit = False
    for l in lines:
        s = l.strip()
        if s.startswith('"') and s.endswith(":"):   # the next command
            hit = False
        elif s.startswith("outputs: "):
            try:
                hit = exe in json.loads(s[len("outputs: "):])
            except ValueError:
                hit = False
        elif hit and s.startswith("args: "):
            try:
                return json.loads(s[len("args: "):])
            except ValueError:
                return None
    return None


def split_link_args(args, objects, exe, ast_skip):
    """SwiftPM's link of the package's test bundle, made into the link of one test target's bundle: its own object
    list file (objects), output exe, the rpath one level less deep (the flat iOS layout: <bundle>/<exe>, the products
    two up) and no AST paths of the other test targets' modules"""
    out, i = [], 0
    while i < len(args):
        x = args[i]
        if x == "-o" and i + 1 < len(args):
            out += ["-o", exe]
            i += 2
            continue
        if x.startswith("@") and x.endswith(".LinkFileList"):
            out.append("@" + objects)
        elif x == "@loader_path/../../../":
            out.append("@loader_path/../../")
        elif x == "-add_ast_path" and i + 2 < len(args) and args[i + 1] == "-Xlinker" \
                and os.path.basename(args[i + 2]) in ast_skip and out[-1:] == ["-Xlinker"]:
            out.pop()
            i += 3
            continue
        else:
            out.append(x)
        i += 1
    # Xcode links every test bundle with XCTest and its Swift support (that runs Swift Testing's tests): a target
    # with Swift Testing tests only (no `import XCTest`) otherwise runs no test
    return out + ["-framework", "XCTest", "-lXCTestSwiftSupport"]


def target_objects(objs, bindir, tests, module, pkg):
    """The package bundle's objects (its LinkFileList) without those of the test targets other than module and
    without SwiftPM's own runner.swift of the bundle (<pkg>.build/: its XCTest observer for `swift test`, which
    Xcode's bundles don't have). SwiftPM builds a target's objects in <bindir>/<Target>.build/."""
    def owner(o):
        top = os.path.relpath(o.strip('"'), bindir).split(os.sep)[0]
        if top == pkg + ".build":
            return pkg
        return next((t["c99name"] for t in tests if top in (t["c99name"] + ".build", t.get("name", "") + ".build")), None)
    return [o for o in objs if owner(o) in (None, module)]


def spm_split(bindir, bundle, tests):
    """One bundle per test target, as Xcode builds a package's tests: SwiftPM's link of the package bundle again,
    each time without the other test targets' objects, into <bindir>/x86sim-ios/<Target>.xctest (the flat iOS layout,
    with the package bundle's resources and an Info.plist). [(module, bundle)], or None when SwiftPM's link can't be
    found or a link fails (the package bundle then runs as one target)."""
    name = os.path.splitext(os.path.basename(bundle))[0]
    exe = os.path.join(bundle, "Contents", "MacOS", name)
    manifest = os.path.join(os.path.dirname(os.path.dirname(bindir)), os.path.basename(bindir) + ".yaml")
    args = link_command(manifest, exe)
    lfl = next((x[1:] for x in args or [] if x.startswith("@") and x.endswith(".LinkFileList")), None)
    if not args or not lfl:
        return None
    try:
        objs = [l for l in open(lfl).read().splitlines() if l.strip()]
    except OSError:
        return None
    modules = [t["c99name"] for t in tests]
    shutil.rmtree(os.path.join(bindir, "x86sim-ios"), ignore_errors=True)   # ours only: no stale bundles (coverage)
    out = []
    for m in modules:
        flat = os.path.join(bindir, "x86sim-ios", m + ".xctest")
        shutil.rmtree(flat, ignore_errors=True)
        os.makedirs(flat)
        mine = os.path.join(bindir, "x86sim-ios", m + ".LinkFileList")
        with open(mine, "w") as f:
            f.write("".join(o + "\n" for o in target_objects(objs, bindir, tests, m, name)))
        cmd = split_link_args(args, mine, os.path.join(flat, m), {o + ".swiftmodule" for o in modules if o != m})
        r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        if r.returncode:
            print("x86sim test: linking %s alone failed:\n%s" % (m, r.stdout[-2000:]), file=sys.stderr)
            return None
        res = os.path.join(bundle, "Contents", "Resources")
        if os.path.isdir(res):
            for x in os.listdir(res):
                (shutil.copytree if os.path.isdir(os.path.join(res, x)) else shutil.copy2)(os.path.join(res, x),
                                                                                       os.path.join(flat, x))
        plistlib.dump(bundle_info(m, os.path.join(bundle, "Contents", "Info.plist")), open(os.path.join(flat, "Info.plist"), "wb"))
        out.append((m, flat))
    return out


def spm_describe(d):
    """swift package describe of the package in d ({} when it can't say)"""
    r = subprocess.run(["swift", "package", "--package-path", d, "describe", "--type", "json"], capture_output=True,
                       text=True)
    try:
        v = json.loads(r.stdout)
        return v if isinstance(v, dict) else {}
    except ValueError:
        return {}


def ios_layout(b):
    exe_dir = os.path.join(b, "Contents", "MacOS")
    if not os.path.isdir(exe_dir):
        return b
    name = os.path.splitext(os.path.basename(b))[0]
    flat = os.path.join(os.path.dirname(b), "x86sim-ios", os.path.basename(b))
    shutil.rmtree(flat, ignore_errors=True)
    os.makedirs(flat)
    shutil.copy2(os.path.join(exe_dir, name), os.path.join(flat, name))
    res = os.path.join(b, "Contents", "Resources")
    if os.path.isdir(res):
        for x in os.listdir(res):
            (shutil.copytree if os.path.isdir(os.path.join(res, x)) else shutil.copy2)(os.path.join(res, x), os.path.join(flat, x))
    plistlib.dump(bundle_info(name, os.path.join(b, "Contents", "Info.plist")), open(os.path.join(flat, "Info.plist"), "wb"))
    return flat


def bundle_info(name, old):
    """The Info.plist of a flat test bundle whose executable is name (SwiftPM writes none on Linux; old: its own)"""
    info = {"CFBundleExecutable": name, "CFBundleIdentifier": "x86sim.spm." + re.sub(r"[^A-Za-z0-9.-]", "-", name),
            "CFBundleName": name, "CFBundlePackageType": "BNDL", "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleVersion": "1", "CFBundleShortVersionString": "1.0", "CFBundleSupportedPlatforms": ["iPhoneSimulator"]}
    if os.path.exists(old):
        info.update(plistlib.load(open(old, "rb")))
        info["CFBundleExecutable"] = name
    return info


def selection(t, only, skip, names):
    """(identifiers to run, identifiers to skip) of a target from -only-testing / -skip-testing (Target, Target/Class,
    Target/Class/test, Target/Suite/test(), or Class[/test] for every target) and the .xctestrun's own lists; None:
    the target is left out. An empty run list means every test. A Swift package's bundle also answers to the names
    of the test targets it holds (_modules)."""
    mine_names = {t["_name"]} | set(t.get("_modules", ()))

    def mine(ids):
        """('all' | list of Class[/test] for this target | None when none is about this target)"""
        out = []
        for i in ids:
            p = [x for x in i.split("/") if x]
            if not p:
                continue
            if p[0] in names:
                if p[0] not in mine_names:
                    continue
                if len(p) == 1:
                    return "all"
                out.append("/".join(p[1:]))
            else:
                out.append("/".join(p))
        return out or None

    o, s = mine(only), mine(skip)
    if s == "all" or (only and o is None):
        return None
    o = [] if o in (None, "all") else o
    s = s or []
    o += list(t.get("OnlyTestIdentifiers", []))
    s += list(t.get("SkipTestIdentifiers", []))
    return o, s


# ---------------------------------------------------------------------------------------------------------------------
# XCTest's log -> results

R_CASE_START = re.compile(r"^Test Case '-\[(\S+) (\S+)\]' started\.")
R_CASE_END = re.compile(r"^Test Case '-\[(\S+) (\S+)\]' (passed|failed|skipped) \((\d+(?:\.\d+)?) seconds\)\.")
R_FAIL = re.compile(r"^(.*?):(\d+): error: -\[(\S+) (\S+)\] : (.*)$")
R_SKIP = re.compile(r"^(.*?):(\d+): -\[(\S+) (\S+)\] : Test skipped(?: - (.*))?$")
# XCTExpectFailure's match: "File.swift:45: Expected failure in -[Mod.Class test]: <message>Reason: (<reason>)" (the
# file without its directory; libsimswt's hook gives the path)
# an expected failure; one without a location (a thrown error's) has no "<file>:<line>: "
R_XFAIL = re.compile(r"^(?:(.*?):(\d+): )?Expected failure in -\[(\S+) (\S+)\]: (.*)$")
R_XFAIL_REASON = re.compile(r"^(.*)Reason: \((.*)\)$", re.S)
R_SUITE = re.compile(r"^Test Suite '(.+)' (started|passed|failed) at ")
R_EXEC = re.compile(r"^\s+Executed (\d+) tests?")
# Swift Testing (run by XCTest's interop): "✔ Test add() passed after 0.001 seconds.", "✘ Test x() recorded an issue at
# File.swift:12:5: Expectation failed: ...", "➜ Test y() skipped: ...", "✔ Test f(x:) with 2 test cases passed after ..."
R_ST_END = re.compile(r"^\S+ Test (.+?)(?: with \d+ test cases?)? (passed|failed|skipped)(?: after (\d+(?:\.\d+)?) seconds)?")
R_ST_ISSUE = re.compile(r"^\S+ Test (.+?) recorded an issue(?: with \d+ arguments? .+?)? at (.+?):(\d+)(?::\d+)?: (.*)$")
R_ST_KNOWN = re.compile(r"^\S+ Test (.+?) recorded a known issue(?: with \d+ arguments? .+?)? at (.+?):(\d+)(?::\d+)?: (.*)$")
R_LOGLINE = re.compile(r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d|arm64host:|simshim:|objc\[|\s+t = )")


# Swift Testing's events (libsimswt, host/simswt): one JSON line per event, {"event": Event.Snapshot, "test":
# Test.Snapshot, "time"}, in Swift Testing's own Codable encoding

ST_ISSUE_KINDS = {"unconditional": "Issue recorded", "knownIssueNotRecorded": "Known issue was not recorded",
                  "apiMisused": "An API was misused", "system": "A system failure occurred",
                  "valueAttachmentFailed": "Caught error while saving attachment"}


def st_source(e):
    """An expression's source code (__Expression's kind: generic(String) / stringLiteral(sourceCode:...))"""
    k = e.get("kind") or {}
    for v in k.values():
        if isinstance(v, dict):
            for key in ("_0", "sourceCode"):
                if isinstance(v.get(key), str):
                    return v[key]
    return e.get("sourceCode") or ""


def st_issue_message(issue):
    """An issue's text as Xcode 27.2 shows it (XCTest's interop; checked against Apple's .xcresult): "Expectation
    failed: <expression>", then "<subexpression> → <value>" for each subexpression whose value is not its own source
    ("<not evaluated>" without a value), "Caught error: <error>", "Issue recorded", then ": <comment>"."""
    kind = issue.get("kind") or {}
    k = next(iter(kind), "")
    v = kind.get(k) or {}
    if k == "expectationFailed":
        x = v.get("expectation") or {}
        e = x.get("evaluatedExpression") or {}
        src = st_source(e)
        mism = x.get("mismatchedErrorDescription")
        lines = ["Expectation failed: " + ("%s: %s" % (mism, src) if mism else src)]
        for sub in e.get("_subexpressions") or []:
            s = st_source(sub)
            rv = sub.get("runtimeValue")
            if rv is None:
                lines.append("%s → <not evaluated>" % s)
            elif rv.get("description") != s:
                lines.append("%s → %s" % (s, rv.get("description")))
        msg = "\n".join(lines)
    elif k == "errorCaught":
        err = v.get("error") or v.get("_0") or {}
        msg = "Caught error: " + (err.get("description") or "")
    elif k == "timeLimitExceeded":
        msg = "Time limit was exceeded"
    elif k == "confirmationMiscounted":
        msg = "Confirmation was confirmed %s time(s), but expected %s" % (v.get("actual"), v.get("expected"))
    else:
        msg = ST_ISSUE_KINDS.get(k, k or "Issue recorded")
    comments = [c for c in issue.get("comments") or [] if c]
    if comments:
        msg += ": " + "\n".join(comments)
    return msg


def st_value(rv):
    """A runtime value (Swift Testing's __Expression.Value) as Xcode keeps it (TestValue): with its "label" (a
    struct's property, an optional's "some", a dictionary pair's "key" / "value") and "children" (an array's
    elements, a dictionary's pairs, an optional's wrapped value, a struct's properties: Swift Testing's Mirror of it),
    each the same, when Swift Testing has them."""
    if rv is None:
        return None
    ti = rv.get("typeInfo") or {}
    v = {"description": rv.get("description") or "", "debug": rv.get("debugDescription"),
         "type": ti.get("unqualifiedName"), "full_type": ti.get("fullyQualifiedName"),
         "is_collection": bool(rv.get("isCollection"))}
    if rv.get("label") is not None:
        v["label"] = rv["label"]
    if rv.get("children") is not None:
        v["children"] = [st_value(c) for c in rv["children"]]
    return v


def st_expression(issue):
    """A failed expectation's expression as Xcode keeps it (ActionTestFailureSummary.expression, TestExpression;
    test-details' "Results" node with a "<subexpression> : <value>" per subexpression): its source code, value and
    subexpressions, each the same (an expression without subexpressions of its own: its value's labelled children,
    "x : 1" under "Point(...)"). None for any other issue."""
    kind = issue.get("kind") or {}
    e = ((kind.get("expectationFailed") or {}).get("expectation") or {}).get("evaluatedExpression")
    if not e:
        return None

    def conv(x):
        subs = x.get("_subexpressions") or []
        if not subs:
            # as Swift Testing's __Expression.subexpressions (Xcode's): without subexpressions of its own, one per
            # child of its value that has a label (a struct's properties, an optional's "some"), named by it
            subs = [{"kind": {"generic": {"_0": c["label"]}}, "runtimeValue": c}
                    for c in (x.get("runtimeValue") or {}).get("children") or [] if c.get("label") is not None]
        return {"source": st_source(x), "value": st_value(x.get("runtimeValue")), "subexpressions": [conv(c) for c in subs]}
    return conv(e)


def st_seconds(instant, clock="suspending"):
    c = (instant or {}).get(clock) or {}
    return c.get("seconds", 0) + c.get("attoseconds", 0) / 1e18


def swift_testing_results(lines):
    """results.json tests from libsimswt's event lines: one per test function, identified as Xcode does (its suite
    path: "Second/Inner/same()", "shared()" for one outside a suite), class "<Module>.<Suite>..." with "suite" the
    path; result, duration (test started -> ended), failures with file:line, skip messages, known issues
    (withKnownIssue) as "expected_failures" with their comment as "reason" (result "expected_failure" when nothing
    else failed, as Xcode's "Expected Failure"); a parameterized test also lists its "arguments" (each test case:
    argument description, id = Xcode's ?args= hash, result, failures). None when no event could be read."""
    tests, order, read = {}, [], False
    for ln in lines:
        try:
            d = json.loads(ln)
            ev = d["event"]
        except (ValueError, KeyError, TypeError):
            continue   # a line cut short (the process was killed while writing)
        read = True
        tid = ev.get("testID") or {}
        comps = tid.get("nameComponents") or []
        snap = d.get("test") or {}
        kind = ev.get("kind") or {}
        k = next(iter(kind), "")
        # test functions only (a suite's Test.Snapshot has no parameters)
        if not comps or ("parameters" not in snap and not comps[-1].endswith(")")):
            continue
        key = (tid.get("moduleName"), tuple(comps))
        t = tests.get(key)
        if t is None:
            if k not in ("testStarted", "testSkipped", "testCaseStarted", "issueRecorded"):
                continue
            mod, suite = tid.get("moduleName") or "", list(comps[:-1])
            loc = snap.get("sourceLocation") or tid.get("sourceLocation") or {}
            t = tests[key] = {"class": ".".join([mod] + suite), "name": comps[-1], "identifier": "/".join(comps),
                              "suite": suite, "swift_testing": True, "result": "running", "duration": None,
                              "failures": [], "source": {"file": loc.get("filePath"), "line": loc.get("line")},
                              "_cases": {}, "_args": {}}
            for c in snap.get("testCases") or []:
                ids = (c.get("id") or {}).get("argIDs")
                if ids:
                    h = "".join("%02x" % b for a in ids for b in (a.get("bytes") or []))
                    args = []
                    for a in c.get("arguments") or []:
                        pv, vv = a.get("parameter") or {}, a.get("value") or {}
                        pt, vt = pv.get("typeInfo") or {}, vv.get("typeInfo") or {}
                        args.append({"label": pv.get("firstName") or "", "type": pt.get("unqualifiedName") or "",
                                     "full_type": pt.get("fullyQualifiedName") or "",
                                     "value": vv.get("description") or "", "debug": vv.get("debugDescription") or "",
                                     "value_type": vt.get("unqualifiedName") or "",
                                     "value_full_type": vt.get("fullyQualifiedName") or "",
                                     "is_collection": bool(vv.get("isCollection"))})
                    t["_args"][h] = args
            order.append(key)
        ids = (ev.get("testCaseID") or {}).get("argIDs")
        case = None
        if ids:
            h = "".join("%02x" % b for a in ids for b in (a.get("bytes") or []))
            args = t["_args"].get(h) or []
            case = t["_cases"].setdefault(h, {"name": ", ".join(a["value"] for a in args), "id": h, "args": args,
                                              "result": "running", "duration": None, "failures": [], "_start": None})
        now = st_seconds(ev.get("instant"))
        if k == "testStarted":
            t["_start"] = now
            if d.get("time"):
                t["started"] = round(d["time"], 6)   # wall clock (libsimswt's), where the result bundle places it
        elif k == "testCaseStarted" and case is not None:
            case["_start"] = now
            if d.get("time"):
                case["started"] = round(d["time"], 6)
        elif k == "testCaseEnded" and case is not None:
            case["result"] = "failed" if case["failures"] else "expected_failure" if case.get("expected_failures") else "passed"
            if case["_start"] is not None:
                case["duration"] = round(now - case["_start"], 6)
        elif k == "issueRecorded":
            issue = kind[k].get("_0") or {}
            if (issue.get("severity") or {}).keys() - {"error"}:
                continue   # a warning: shown, does not fail the test
            loc = (issue.get("sourceContext") or {}).get("sourceLocation") or {}
            f = {"file": loc.get("filePath"), "line": loc.get("line"), "message": st_issue_message(issue)}
            if not loc:
                f["file"] = f["line"] = None
            x = st_expression(issue)
            if x:
                f["expression"] = x
            if d.get("frames"):
                f["_frames"] = d["frames"]   # its backtrace (libsimswt): Parser.swift_testing symbolicates it
            if issue.get("isKnown"):
                f["reason"] = d.get("known") or None   # withKnownIssue's comment (libsimswt)
                t.setdefault("expected_failures", []).append(f)
                if case is not None:
                    case.setdefault("expected_failures", []).append(f)
                continue
            t["failures"].append(f)
            if case is not None:
                case["failures"].append(f)
        elif k == "testSkipped":
            t["result"] = "skipped"
            info = kind[k].get("_0") or {}
            t["skip_message"] = info.get("comment") or ""
            loc = (info.get("sourceContext") or {}).get("sourceLocation") or {}
            t["skip_file"], t["skip_line"] = loc.get("filePath"), loc.get("line")
            t["duration"] = 0.0
        elif k == "testEnded":
            if t["result"] != "skipped":
                t["result"] = "failed" if t["failures"] else "expected_failure" if t.get("expected_failures") else "passed"
            if t.get("_start") is not None:
                t["duration"] = round(now - t["_start"], 6)
    if not read:
        return None
    out = []
    for key in order:
        t = tests[key]
        cases = list(t.pop("_cases").values())
        t.pop("_args")
        t.pop("_start", None)
        for c in cases:
            c.pop("_start")
        if cases:
            t["arguments"] = cases
        out.append(t)
    return out


_DSYMS = {}   # binary -> its DWARF for llvm-symbolizer (None: none), made once per run


def dsym_of(binary):
    """The test bundle's debug info: its dSYM (beside the bundle, or inside it as SwiftPM puts it), else one dsymutil
    makes from the objects the image's debug map names (a Debug build keeps them: Xcode's DEBUG_INFORMATION_FORMAT
    dwarf), kept in a temporary dir until x86sim test ends."""
    if binary in _DSYMS:
        return _DSYMS[binary]
    b = binary
    while b != os.path.dirname(b) and not b.endswith((".xctest", ".app", ".framework")):
        b = os.path.dirname(b)
    d = next((c for c in (b + ".dSYM", binary + ".dSYM") if os.path.isdir(c)), None)
    ds = None if d else llvm_tool("dsymutil")
    if ds:
        try:
            tmp = tempfile.mkdtemp(prefix="dsym-", dir=x86config.get("X86SIM_SCRATCH"))
            atexit.register(shutil.rmtree, tmp, True)
            d = os.path.join(tmp, os.path.basename(b) + ".dSYM")
            if subprocess.run([ds, binary, "-o", d], capture_output=True, timeout=120).returncode:
                d = None
        except (OSError, subprocess.SubprocessError):
            d = None
    _DSYMS[binary] = d
    return d


def source_frame(chain):
    """(file, line, function) of a symbolicated address as Xcode shows it: llvm-symbolizer's inlining chain (innermost
    first), the first entry whose line is in a source file. A line in a macro's expansion (#expect's
    "@__swiftmacro_..." buffer, @Test's thunks) or the compiler's own code ("<compiler-generated>", line 0) is not
    one: Xcode leaves such frames out (an #expect inlined into its test function: the function's line)."""
    for e in chain or []:
        f, ln = e.get("FileName") or "", e.get("Line") or 0
        if f and ln and "<compiler-generated>" not in f and not os.path.basename(f).startswith("@__swiftmacro_"):
            return f, int(ln), e.get("FunctionName")
    return None


def symbolize(binary, offsets):
    """[(file, line, function) | None] for offsets into a Mach-O image (llvm-symbolizer with the image's DWARF,
    dsym_of()): the frame Xcode shows for each (source_frame()), function as its linkage name."""
    tool = llvm_tool("llvm-symbolizer")
    if not tool or not offsets:
        return [None] * len(offsets)
    args = [tool, "--obj", binary, "--output-style=JSON", "--functions=linkage"]
    d = dsym_of(binary)
    if d:
        args += ["--dsym-hint", d]
    try:
        r = subprocess.run(args + ["0x%x" % o for o in offsets], capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return [None] * len(offsets)
    entries = []
    for ln in r.stdout.splitlines():
        try:
            v = json.loads(ln)
        except ValueError:
            continue
        entries += v if isinstance(v, list) else [v]
    out = [source_frame((e or {}).get("Symbol")) for e in entries[:len(offsets)]]
    return out + [None] * (len(offsets) - len(out))


def text_vmaddr(binary):
    """The arm64 slice's __TEXT address (0x100000000 for an executable, 0 for a bundle / dylib): llvm-symbolizer takes
    addresses, libsimswt's frames are offsets from the image's Mach-O header."""
    try:
        with open(binary, "rb") as f:
            b = f.read(1 << 16)
        base = 0
        if b[:4] == b"\xca\xfe\xba\xbe":   # universal: the arm64 slice
            n = struct.unpack(">I", b[4:8])[0]
            for i in range(n):
                cpu, _, off, _, _ = struct.unpack(">iiIII", b[8 + 20 * i:28 + 20 * i])
                if cpu == 0x0100000c:
                    base = off
            with open(binary, "rb") as f:
                f.seek(base)
                b = f.read(1 << 16)
        if b[:4] != b"\xcf\xfa\xed\xfe":
            return 0
        ncmds = struct.unpack("<I", b[16:20])[0]
        p = 32
        for _ in range(ncmds):
            cmd, size = struct.unpack("<II", b[p:p + 8])
            if cmd == 0x19 and b[p + 8:p + 24].rstrip(b"\0") == b"__TEXT":   # LC_SEGMENT_64
                return struct.unpack("<Q", b[p + 24:p + 32])[0]
            p += size
    except (OSError, struct.error):
        pass
    return 0


def symbol_names(binary, offsets):
    """[function name | None] for offsets into an image without debug info, from its symbol table (llvm-symbolizer:
    "_XCTRunnerRunTests", "main"), as Xcode names a frame of such an image it symbolicates (the UI test runner's).
    None where the image has line information there (symbolize() decides) or no symbol covers it."""
    tool = llvm_tool("llvm-symbolizer")
    if not tool or not offsets:
        return [None] * len(offsets)
    base = text_vmaddr(binary)
    try:
        r = subprocess.run([tool, "--obj", binary, "--default-arch=arm64", "--output-style=JSON", "--functions=linkage"]
                           + ["0x%x" % (base + o) for o in offsets], capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return [None] * len(offsets)
    out = []
    for ln in r.stdout.splitlines():
        try:
            v = json.loads(ln)
        except ValueError:
            continue
        for e in v if isinstance(v, list) else [v]:
            ch = (e or {}).get("Symbol") or [{}]
            out.append(ch[0].get("FunctionName") or None if not ch[0].get("FileName") else None)
    out = out[:len(offsets)]
    return out + [None] * (len(offsets) - len(out))


def demangle(names):
    """{linkage name: Xcode's name for it} (swift-demangle --simplified: "closure #1 in Known.thrown()"); names it
    does not demangle (C, ObjC) stay as they are."""
    names = sorted({n for n in names if n})
    tool = llvm_tool("swift-demangle")
    if not tool or not names:
        return {n: n for n in names}
    try:
        r = subprocess.run([tool, "--simplified"], input="\n".join(names) + "\n", capture_output=True, text=True,
                           timeout=60)
    except (OSError, subprocess.SubprocessError):
        return {n: n for n in names}
    out = r.stdout.split("\n")
    return {n: (out[i] if i < len(out) and out[i] else n) for i, n in enumerate(names)}


def frame_parts(fr):
    """(image path on this host, offset, address) of a libsimswt frame ([path, offset] or [path, offset, address])"""
    p, off = fr[0], fr[1]
    return (undv(p) if p else None), off, (fr[2] if len(fr) > 2 else None)


def in_test_bundle(path):
    return bool(path) and ".xctest/" in path and os.path.isfile(path)


def absolute(f, products):
    return os.path.normpath(os.path.join(products, f)) if f and not os.path.isabs(f) and products else f


def app_executable(app):
    """The main executable of an .app (its Info.plist's CFBundleExecutable, else the bundle's name)"""
    try:
        with open(os.path.join(app, "Info.plist"), "rb") as f:
            exe = plistlib.load(f).get("CFBundleExecutable")
    except (OSError, ValueError, plistlib.InvalidFileException):
        exe = None
    return os.path.join(app, exe or os.path.splitext(os.path.basename(app))[0])


def call_stacks(stacks, products=None):
    """Issues' call stacks as Xcode keeps them (ActionTestFailureSummary.sourceCodeContext.callStack): per frame of
    libsimswt's ([image path, offset, address]), {"address"} (Xcode's addressString: the frame's address, one before
    it for a return address), and for a frame in the test bundle that symbolicates to a source line (source_frame())
    also "image" (the image's name), "symbol" (its function, swift-demangle --simplified) and "file", "line". stacks:
    a list of frame lists, symbolicated together (one llvm-symbolizer run per image).
    The test host's executable (the app whose PlugIns/ holds the test bundle: a UI test's XCTRunner) is one of the
    images Xcode has XCTest symbolicate too (XCTestConfiguration.symbolicationImageNames: the scheme's products,
    by image name; XCTestDemo.debug.dylib or the xctest agent are not): a frame there with a source line is named as
    a test bundle's, one in an image without debug info (XCTRunner: "_XCTRunnerRunTests", "main") by its symbol
    alone, {"image", "symbol"} without file and line (symbol_names())."""
    want, apps = {}, set()
    for frames in stacks:
        for fr in frames or []:
            path = frame_parts(fr)[0]
            if in_test_bundle(path) and "/PlugIns/" in path:
                apps.add(path.rsplit("/PlugIns/", 1)[0])
    hosts = {app_executable(a) for a in apps}
    for frames in stacks:
        for i, fr in enumerate(frames or []):
            path, off, _ = frame_parts(fr)
            if in_test_bundle(path) or (path in hosts and os.path.isfile(path)):
                want.setdefault(path, set()).add(off - (1 if i else 0))
    syms, bare = {}, {}
    for path, offs in want.items():
        offs = sorted(offs)
        for o, s in zip(offs, symbolize(path, offs)):
            syms[(path, o)] = s
        if path in hosts:
            rest = [o for o in offs if not syms[(path, o)]]
            for o, n in zip(rest, symbol_names(path, rest)):
                bare[(path, o)] = n
    names = demangle([s[2] for s in syms.values() if s] + [n for n in bare.values() if n])
    out = []
    for frames in stacks:
        st = []
        for i, fr in enumerate(frames or []):
            path, off, addr = frame_parts(fr)
            f = {"address": None if addr is None else addr - (1 if i and addr else 0)}
            k = (path, off - (1 if i else 0))
            s = syms.get(k) if path else None
            if s:
                f.update(image=os.path.basename(path), symbol=names.get(s[2], s[2]) or "", file=absolute(s[0], products),
                         line=s[1])
            elif path and bare.get(k):
                f.update(image=os.path.basename(path), symbol=names.get(bare[k], bare[k]))
            st.append(f)
        out.append(st)
    return out


def symbolicated(stack):
    """a stack with a frame symbolicated to a source line (frames named by symbol alone do not count)"""
    return any("file" in x for x in stack or [])


def throw_location(frames, products=None):
    """(file, line) of a thrown error's issue, as Xcode reports it: the first frame of its call stack (captured where
    the error was thrown) in the test bundle (the image inside a .xctest) that symbolicates to a source line.
    The image's path is the device's view of it (/Volumes/SystemRoot/...); its caller is the frame's return address,
    so the offset looked up is one before it. None when nothing symbolicates."""
    mine = [(i, p, off) for i, (p, off, _) in enumerate(frame_parts(fr) for fr in frames) if in_test_bundle(p)]
    if not mine:
        return None
    binary = mine[0][1]
    mine = [x for x in mine if x[1] == binary]
    syms = symbolize(binary, [off - (1 if i else 0) for i, _, off in mine])
    for (i, _, _), s in zip(mine, syms):
        if s and s[0] and s[1]:
            return absolute(s[0], products), int(s[1])
    return None


class Recorder:
    """UI tests' screen recordings, as Xcode makes them (the .xctestrun's PreferredScreenCaptureFormat screenRecording,
    Xcode's default for UI tests): the device's screen while each test case runs (x86sim io recordVideo, the
    device's frame ring), kept as the test's "Screen Recording" attachment when the attachment lifetime says so
    (deleteOnSuccess: failed tests only)."""

    def __init__(self, dev, folder):
        self.dev, self.folder = dev, folder
        self.proc = self.test = self.path = None
        self.done = []   # (test, mp4)
        os.makedirs(folder, exist_ok=True)

    def start(self, t):
        self.stop()
        self.test = t
        self.path = os.path.join(self.folder, "%d.mp4" % len(self.done))
        self.proc = subprocess.Popen([os.path.join(ROOT, "cli/x86sim"), "--phone", self.dev, "io", "recordVideo",
                                      "--fps", "30", self.path], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, start_new_session=True)

    def stop(self, t=None):
        if not self.proc:
            return
        self.proc.send_signal(signal.SIGTERM)   # simctl.record finishes the mp4 on SIGTERM
        try:
            self.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        if os.path.getsize(self.path) if os.path.exists(self.path) else 0:
            self.done.append((self.test, self.path))
        self.proc = self.test = None

    def keep(self, base, lifetime):
        """Move the recordings to keep into attachments/<Class>.<test>/ and list them on their tests"""
        self.stop()
        for t, mp4 in self.done:
            keep = lifetime == "keepAlways" or (lifetime != "keepNever" and t["result"] == "failed")
            if not keep:
                continue
            dst = os.path.join(base, "%s.%s" % (t["class"].split(".")[-1], t["name"]), "Screen_Recording.mp4")
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.move(mp4, dst)
            t.setdefault("attachments", []).append({"name": "Screen Recording", "path": dst, "type": "public.mpeg-4",
                                                     "from": "x86sim"})
        shutil.rmtree(self.folder, ignore_errors=True)


class Parser:
    def __init__(self, target):
        self.target = target
        self.tests = []
        self.by_key = {}
        self.current = None
        self.top = None
        self.done = False
        self.last_fail = None
        self.on_start = self.on_end = None   # UI tests' screen recording: called with the test at its start / end

    def test(self, cls, name):
        k = (cls, name)
        if k not in self.by_key:
            t = {"class": cls, "name": name, "identifier": "%s/%s" % (cls.split(".")[-1], name), "result": "running",
                 "duration": None, "failures": []}
            self.by_key[k] = t
            self.tests.append(t)
        return self.by_key[k]

    def line(self, l):
        """Feed one line; True when it is one of XCTest's own (shown by default)."""
        m = R_CASE_START.match(l)
        if m:
            self.current = self.test(m.group(1), m.group(2))
            self.last_fail = None
            if self.on_start:
                self.on_start(self.current)
            return True
        m = R_CASE_END.match(l)
        if m:
            t = self.test(m.group(1), m.group(2))
            t["result"], t["duration"] = m.group(3), float(m.group(4))
            if t["result"] == "passed" and t.get("expected_failures"):
                t["result"] = "expected_failure"   # XCTest says passed; Xcode: Expected Failure
            self.current = self.last_fail = None
            if self.on_end:
                self.on_end(t)
            return True
        m = R_FAIL.match(l)
        if m:
            t = self.test(m.group(3), m.group(4))
            f = {"file": m.group(1), "line": int(m.group(2)), "message": m.group(5)}
            if f["file"] == "<unknown>":
                f["file"], f["line"] = None, None
            t["failures"].append(f)
            self.last_fail = f
            return True
        m = R_XFAIL.match(l)
        if m:
            t = self.test(m.group(3), m.group(4))
            f = {"file": m.group(1), "line": int(m.group(2)) if m.group(2) else None, "message": m.group(5),
                 "reason": None}
            t.setdefault("expected_failures", []).append(f)
            self.last_fail = f
            return True
        m = R_SKIP.match(l)
        if m:
            t = self.test(m.group(3), m.group(4))
            t["skip_message"] = m.group(5) or ""
            self.last_fail = None
            return True
        m = R_SUITE.match(l)
        if m:
            if self.top is None and m.group(2) == "started":
                self.top = m.group(1)
            elif m.group(1) == self.top and m.group(2) != "started":
                self.done = True
            self.last_fail = None
            return True
        if R_EXEC.match(l):
            return True
        m = R_ST_ISSUE.match(l)
        if m:
            t = self.test("SwiftTesting", m.group(1))
            f = {"file": m.group(2), "line": int(m.group(3)), "message": m.group(4)}
            t["failures"].append(f)
            self.last_fail = f
            return True
        m = R_ST_KNOWN.match(l)
        if m:
            t = self.test("SwiftTesting", m.group(1))
            f = {"file": m.group(2), "line": int(m.group(3)), "message": m.group(4), "reason": None}
            t.setdefault("expected_failures", []).append(f)
            self.last_fail = f
            return True
        m = R_ST_END.match(l)
        if m and m.group(1).startswith("run with "):   # Swift Testing's closing line ("Test run with N tests ...")
            return True
        if m and not l.startswith("Test "):
            t = self.test("SwiftTesting", m.group(1))
            t["result"] = m.group(2)
            if t["result"] == "passed" and t.get("expected_failures"):
                t["result"] = "expected_failure"
            if m.group(3):
                t["duration"] = float(m.group(3))
            return True
        if l.lstrip().startswith("t = "):
            return True
        # a failure message that goes on over more lines (XCTAssert descriptions, thrown errors' descriptions)
        if self.last_fail is not None and l.strip() and not R_LOGLINE.match(l) and len(self.last_fail["message"]) < 4000:
            self.last_fail["message"] += "\n" + l
            return True
        return False

    def swift_testing(self, path, keep=None, products=None):
        """Swift Testing's results from libsimswt's events (path), in place of what its console lines told: each test
        by its suite path, as Xcode names it, its issues with their call stacks (call_stacks()). Nothing changes when
        no event came (no libsimswt: console lines kept)."""
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                lines = f.read().splitlines()
        except OSError:
            return
        if not lines:
            return
        if keep:
            shutil.copy(path, keep)
        tests = swift_testing_results(lines)
        if tests is None:
            return
        issues, seen = [], set()
        for t in tests:
            for c in [t] + (t.get("arguments") or []):
                for f in (c.get("failures") or []) + (c.get("expected_failures") or []):
                    if id(f) not in seen:   # a parameterized test's issue is its case's too
                        seen.add(id(f))
                        issues.append(f)
        # without a backtrace (a known issue that was not recorded) no frames, as Xcode has none; frames of which none
        # symbolicates (no debug info here): left out, the .xcresult then has the test's own frame (xcresult.py)
        for f, st in zip(issues, call_stacks([f.pop("_frames", None) for f in issues], products)):
            if not st or symbolicated(st):
                f["stack"] = st
        self.tests = [t for t in self.tests if t["class"] != "SwiftTesting"] + tests
        self.by_key = {k: v for k, v in self.by_key.items() if k[0] != "SwiftTesting"}

    @staticmethod
    def split_reasons(tests):
        """XCTest's console expected failures: "<message>Reason: (<reason>)" -> message, reason."""
        for t in tests:
            for f in t.get("expected_failures") or []:
                if f.get("reason") is None and not t.get("swift_testing"):
                    m = R_XFAIL_REASON.match(f["message"])
                    if m:
                        f["message"], f["reason"] = m.group(1), m.group(2)

    def issue_stacks(self, path, keep=None, products=None):
        """XCTest issues' call stacks (libsimswt writes each issue's), symbolicated here (call_stacks()) and kept with
        the failure they belong to (the test's, by message and location) as its "stack". A failure without a location
        (a thrown error) gets the one Xcode gives it from the stack XCTest captured when the error was thrown: the
        first frame in the test bundle with a source line (left empty when none symbolicates). XCTExpectFailure's
        matches ("expected" lines): in place of the console's, with their path, message, reason and stack."""
        self.split_reasons(self.tests)
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                lines = f.read().splitlines()
        except OSError:
            return
        if not lines:
            return
        if keep:
            shutil.copy(path, keep)
        recs = []
        for ln in lines:
            try:
                e = json.loads(ln)
            except ValueError:
                continue
            m = re.match(r"-\[(\S+) (\S+)\]$", e.get("test") or "")
            if m:
                recs.append((e, m))
        stacks = call_stacks([e.get("frames") or [] for e, _ in recs], products)
        taken = set()
        for (e, m), st in zip(recs, stacks):
            # XCTest names it -[Class test] here (no module): the test of that class name and method
            t = next((x for k, x in self.by_key.items()
                      if k[1] == m.group(2) and k[0].split(".")[-1] == m.group(1).split(".")[-1]), None)
            if t is None:
                continue
            if e.get("expected"):
                if not t.get("_xfail_hook"):   # the hook's list replaces the console's (the same, in order)
                    t["_xfail_hook"], t["expected_failures"] = True, []
                f = {"file": e.get("file"), "line": e.get("line"), "message": e.get("message") or "",
                     "reason": e.get("reason")}
                if not f["file"]:
                    loc = throw_location(e.get("frames") or [], products)
                    f["file"], f["line"] = loc if loc else (None, None)
                if symbolicated(st):
                    f["stack"] = st
                t["expected_failures"].append(f)
                continue
            first = (e.get("message") or "").split("\n")[0]
            f = next((x for x in t.get("failures", []) if id(x) not in taken
                      and x.get("message", "").split("\n")[0] == first
                      and (x.get("file") is None if not e.get("file") else
                           (os.path.basename(x.get("file") or "") == os.path.basename(e["file"])
                            and x.get("line") == e.get("line")))), None)
            if f is None:
                continue
            taken.add(id(f))
            if f.get("file") is None:
                loc = throw_location(e.get("frames") or [], products)
                if loc:
                    f["file"], f["line"] = loc
            if symbolicated(st):
                f["stack"] = st
        for t in self.tests:
            if t.pop("_xfail_hook", None) and t["result"] == "passed" and t["expected_failures"]:
                t["result"] = "expected_failure"

    def finish(self, why=None):
        """The process ended (or was stopped): a test still running failed with `why`."""
        for t in self.tests:
            if t["result"] == "running":
                t["result"] = "failed"
                t["failures"].append({"file": None, "line": None, "message": why or "the test process ended during this test"})


# ---------------------------------------------------------------------------------------------------------------------
# Running

class Runner:
    def __init__(self, a):
        self.a = a
        self.dev = x86config.device()
        self.data = os.path.join(self.dev, "data")
        self.plat, self.rt = device_platform(self.dev)
        self.pname = PLATFORMS[self.plat]
        self.rtroot = runtime_root(self.rt)
        import simctl
        self.simctl = simctl
        self.phone = (open(os.path.join(self.dev, "name")).read().strip()
                      if os.path.exists(os.path.join(self.dev, "name")) else os.path.basename(self.dev))
        self.work = os.path.join(self.data, "tmp", "x86sim-xctest", uuid.uuid4().hex[:8])
        os.makedirs(self.work, exist_ok=True)
        self.profraw = os.path.join(self.work, "profraw")
        self.out = a["out"] or os.path.join(x86config.get("X86SIM_SCRATCH"), "xctest",
                                            "%s-%s" % (self.phone, time.strftime("%Y%m%d-%H%M%S")))
        os.makedirs(self.out, exist_ok=True)
        self.installed = {}
        self.insert = ""
        try:
            for l in open(os.path.join(self.dev, "sim.env")):
                if l.startswith("DYLD_INSERT_LIBRARIES="):
                    self.insert = l.split("=", 1)[1].strip()
        except OSError:
            pass

    def say(self, msg):
        if not self.a["quiet"]:
            print(msg, flush=True)

    def setup_platform(self):
        src = find_platform(self.pname, self.a["platform"])
        self.platforms = thin_platform(src, self.pname)
        self.pdev = os.path.join(self.platforms, self.pname + ".platform", "Developer")
        self.swt = None if os.environ.get("X86SIM_XCTEST_NO_SWT") else simswt(src, self.plat)

    def install(self, app):
        app = os.path.realpath(app)
        if app in self.installed:
            return self.installed[app]
        bid = bundle_id(app)
        self.say("Installing %s (%s)" % (os.path.basename(app), bid))
        r = subprocess.run([os.path.join(HERE, "appinstall.sh"), "install", app], capture_output=True, text=True,
                           env=dict(os.environ, X86SIM_DEVICE=self.dev))
        if r.returncode:
            raise RuntimeError("install %s failed: %s" % (app, (r.stderr or r.stdout).strip()[-500:]))
        path = self.simctl.app_info(bid)["Path"]
        self.installed[app] = (bid, path)
        return bid, path

    def scheme_info(self, targets):
        """What Xcode puts in every test process's XCTestConfiguration about the scheme (configuration()): the project's
        name (baseTestsURL), its products (DependentProductPaths of the .xctestrun, else each target's bundle and apps)
        by image name (symbolicationImageNames) and bundle id (bundleIDsForCrashReportEmphasis: the test bundles and
        apps, not UI test runners), its apps' bundle infos for UI tests (ApplicationBundleInfos: localizations and the
        supported orientations), XCTAutomationSupport's path."""
        paths = []
        for t in targets:
            deps = [self.local(x) for x in t.get("DependentProductPaths") or []]
            if not deps:
                host = self.local(t["TestHostPath"]) if t.get("TestHostPath") and kind_of(t) != "logic" else None
                b = t.get("TestBundlePath") or ""
                if host and b.startswith("__TESTHOST__/"):
                    b = os.path.join(host, b[len("__TESTHOST__/"):])
                else:
                    b = self.local(b) if b else None
                app = self.local(t["UITargetAppPath"]) if t.get("UITargetAppPath") else None
                deps = [x for x in (app, host, b) if x]
            paths += [x for x in deps if x not in paths and os.path.isdir(x)]
        names, bids, infos = [], [], {}
        for x in paths:
            try:
                with open(os.path.join(x, "Info.plist"), "rb") as f:
                    info = plistlib.load(f)
            except (OSError, ValueError, plistlib.InvalidFileException):
                info = {}
            exe = info.get("CFBundleExecutable") or os.path.splitext(os.path.basename(x))[0]
            if exe not in names:
                names.append(exe)
            bid = info.get("CFBundleIdentifier")
            if not bid:
                continue
            if not bid.endswith(".xctrunner") and bid not in bids:
                bids.append(bid)
            if x.endswith(".app"):
                locs = sorted(d[:-6] for d in os.listdir(x) if d.endswith(".lproj") and d != "Base.lproj")
                infos[bid] = Obj(["XCTApplicationBundleInfo", "NSObject"], {
                    "localizations": set(locs or [info.get("CFBundleDevelopmentRegion") or "en"]),
                    "info": {k: v for k, v in info.items() if k.startswith("UISupportedInterfaceOrientations")},
                    "infoUnprocessed": True})
        auto = os.path.join(self.rtroot, "Developer/Library/PrivateFrameworks/XCTAutomationSupport.framework")
        project = self.a.get("spm_project") or next((t["_container"] for t in targets if t.get("_container")), None)
        if not project:
            import xcresult
            project = xcresult.guess_project([{"name": t["_name"]} for t in targets])
        # NSSets, as Xcode archives them (XCTest's initWithCoder: refuses an NSArray there)
        return {"project": project, "image_names": set(names) or None, "bundle_ids": set(bids) or None,
                "bundle_infos": infos or None, "automation": dv(auto) if os.path.isdir(auto) else None}

    def subst(self, s, host=None):
        s = s.replace("__TESTROOT__", dv(self.root)).replace("__PLATFORMS__", dv(self.platforms))
        if host:
            s = s.replace("__TESTHOST__", host)
        return s

    def local(self, s):
        """A product path of the .xctestrun on this machine."""
        return os.path.realpath(s.replace("__TESTROOT__", self.root).replace("__PLATFORMS__", self.platforms))

    def exists_in_device(self, p):
        if p.startswith(DV + "/"):
            return os.path.exists(undv(p))
        return os.path.exists(self.rtroot + p) or os.path.exists(p)

    def environment(self, t, host, products, test_env=True):
        """The test process's environment, as Xcode builds it from the .xctestrun (EnvironmentVariables +
        TestingEnvironmentVariables), with this machine's paths. DYLD_* also go as X86SIM_A64_GENV_* (the translator
        hands them to the guest as is): launchd_sim's own DYLD_INSERT_LIBRARIES (libsimshim) otherwise replaces the
        one FrontBoard passes for an app, and libXCTestBundleInject.dylib was never loaded."""
        env = {}
        env.update(t.get("EnvironmentVariables", {}))
        if test_env:
            env.update(t.get("TestingEnvironmentVariables", {}))
        env = {k: self.subst(str(v), host) for k, v in env.items()}
        pf, pl = dv(os.path.join(self.pdev, "Library")), dv(os.path.join(self.pdev, "usr/lib"))
        fw = [x for x in env.get("DYLD_FRAMEWORK_PATH", "").split(":") if x]
        lib = [x for x in env.get("DYLD_LIBRARY_PATH", "").split(":") if x]
        for x in (dv(products), pf + "/Frameworks", pf + "/PrivateFrameworks"):
            if x not in fw:
                fw.append(x)
        for x in (dv(products), pl):
            if x not in lib:
                lib.append(x)
        env["DYLD_FRAMEWORK_PATH"], env["DYLD_LIBRARY_PATH"] = ":".join(fw), ":".join(lib)
        ins = []
        for x in [y for y in env.get("DYLD_INSERT_LIBRARIES", "").split(":") if y]:
            if x.endswith("/libXCTestBundleInject.dylib") and not self.exists_in_device(x):
                x = pl + "/libXCTestBundleInject.dylib"   # Xcode copies it into the host app first
            if self.exists_in_device(x):
                ins.append(x)
        if kind_of(t) == "hosted" and not any(x.endswith("/libXCTestBundleInject.dylib") for x in ins):
            ins.insert(0, pl + "/libXCTestBundleInject.dylib")
            env.setdefault("XCInjectBundleInto", "unused")
        ins = [x for x in self.insert.split(":") if x] + [x for x in ins if x not in self.insert.split(":")]
        if self.swt:   # Swift Testing's events (simswt): loads the platform's Testing.framework (DYLD_FRAMEWORK_PATH)
            ins.append(dv(self.swt))
        if ins:
            env["DYLD_INSERT_LIBRARIES"] = ":".join(ins)
        env.setdefault("OS_ACTIVITY_DT_MODE", "YES")
        env.update(self.profile_env(t["_name"]))
        for k in [k for k in env if k.startswith("DYLD_")]:
            env["X86SIM_A64_GENV_" + k] = env[k]
        return env

    def profile_env(self, name):
        """--coverage: where a process's LLVM profile goes, as Xcode sets it (<dir>/<id>-%p%c.profraw: one file per
        process, in continuous mode, so a process that is killed at the end still leaves its counters)"""
        if not self.a["coverage"]:
            return {}
        os.makedirs(self.profraw, exist_ok=True)
        f = "%s/%s-%%p%%c.profraw" % (dv(self.profraw), re.sub(r"[^A-Za-z0-9._-]", "_", name))
        return {"LLVM_PROFILE_FILE": f, "__XPC_LLVM_PROFILE_FILE": f}

    def write_config(self, name, data):
        p = os.path.join(self.work, name + ".xctestconfiguration")
        with open(p, "wb") as f:
            f.write(data)
        return dv(p)

    def wait(self, parser, outf, timeout, alive, raw):
        """Follow the test process's output file until XCTest's top suite ended and the process is gone, the process
        ended, or the time is up. Returns 'done', 'exited' or 'timeout'."""
        pos, buf = 0, b""
        end = time.monotonic() + timeout
        last_alive, gone_since, done_at = 0.0, None, None
        verbose, quiet = self.a["verbose"], self.a["quiet"]
        while True:
            try:
                with open(outf, "rb") as f:
                    f.seek(pos)
                    b = f.read()
            except OSError:
                b = b""
            if b:
                pos += len(b)
                raw.write(b)
                buf += b
                *lines, buf = buf.split(b"\n")
                for l in lines:
                    s = l.decode(errors="replace").rstrip("\r")
                    own = parser.line(s)
                    if not quiet and (verbose or own):
                        print(s, flush=True)
                if parser.done and done_at is None:
                    done_at = time.monotonic()
            now = time.monotonic()
            if now - last_alive > 2:
                last_alive = now
                if not alive():
                    gone_since = gone_since or now
                    if now - gone_since > 1.5:   # its last writes are in the file
                        if not b:
                            return "done" if parser.done else "exited"
                else:
                    gone_since = None
            if done_at and now - done_at > 20:
                return "done"   # finished but still running (an app that stays up): stopped below
            if now > end:
                return "timeout"
            time.sleep(0.2)

    def run_target(self, t):
        name, kind = t["_name"], kind_of(t)
        sel = selection(t, self.a["only"], self.a["skip"], self.names)
        res = {"name": name, "kind": kind, "result": "failed", "tests": [], "duration": 0.0, "log": None}
        if sel is None:
            return None
        only, skip = sel
        t0 = time.monotonic()
        res["started"] = round(time.time(), 3)   # the target's real start, for its result bundle's times
        parser = Parser(name)
        rec = None
        if kind == "ui" and t.get("PreferredScreenCaptureFormat", "screenRecording") == "screenRecording" \
                and not self.a["no_recording"] and shutil.which("ffmpeg"):
            rec = Recorder(self.dev, os.path.join(self.work, "rec-" + name))
            parser.on_start, parser.on_end = rec.start, rec.stop
        log = os.path.join(self.out, name + ".log")
        res["log"] = log
        # issue #50: libsimshim (simshim_xct.c) writes every XCTest attachment of the test process here
        att = os.path.join(self.out, "attachments", ".raw-" + name)
        shutil.rmtree(att, ignore_errors=True)
        os.makedirs(att, exist_ok=True)
        outf = os.path.join(self.work, name + ".out")
        open(outf, "w").close()
        swt_events = os.path.join(self.work, name + ".swt.jsonl")   # Swift Testing's events (simswt)
        open(swt_events, "w").close()
        swt_issues = os.path.join(self.work, name + ".issues.jsonl")   # call stacks of unlocated XCTest issues (simswt)
        open(swt_issues, "w").close()
        module = t.get("ProductModuleName") or name
        interop = INTEROP_MODES.get(str(t.get("TestInteropMode", "complete")).lower(), 2)
        products = self.root
        self.say("\n=== %s (%s tests)" % (name, kind))
        try:
            if kind == "logic":
                bundle = self.subst(t["TestBundlePath"])
                bundle = bundle if bundle.startswith(DV) else dv(bundle)
                agent = os.path.join(self.pdev, "Library/Xcode/Agents/xctest")
                agent = dv(agent) if os.path.exists(agent) else dv(os.path.join(self.rtroot, "Developer/usr/bin/xctest"))
                env = self.environment(t, None, products)
                env["X86SIM_XCTEST_ATTACHMENTS"] = dv(att)
                env["X86SIM_SWT_EVENTS"] = dv(swt_events)
                env["X86SIM_SWT_ISSUES"] = dv(swt_issues)
                cfg = configuration(bundle, module, only=only or None, skip=skip, interop=interop, t=t, scheme=self.scheme)
                env["XCTestConfigurationFilePath"] = self.write_config(name, cfg)
                env["XCTestBundlePath"] = bundle
                rel = os.path.relpath(outf, self.data)
                r = self.simctl.check(self.simctl.request("spawn", path=agent, args=[bundle] + t.get("CommandLineArguments", []),
                                                          env=env, stdout=rel), "spawn")
                exitf = outf + ".exit"
                alive = lambda: not os.path.exists(exitf)   # noqa: E731
                bid = None
                pid = r.get("pid")
            else:
                host_local = self.local(t["TestHostPath"])
                if kind == "ui":
                    if not os.path.exists(os.path.join(self.dev, "LaunchDaemons", "com.apple.testmanagerd.plist")):
                        raise RuntimeError(
                            "UI tests need testmanagerd, which this device was not booted with: turn on its xctest "
                            "feature (x86sim-aot --device %s features set xctest on, or MCP feature_set id=xctest) and "
                            "restart it" % self.phone)
                    tapp = self.local(t["UITargetAppPath"])
                    tbid, tpath = self.install(tapp)
                bid, host = self.install(host_local)
                bundle = self.subst(t["TestBundlePath"], host)
                if not bundle.startswith(DV):
                    bundle = dv(bundle)
                env = self.environment(t, host, products)
                env["X86SIM_XCTEST_ATTACHMENTS"] = dv(att)
                env["X86SIM_SWT_EVENTS"] = dv(swt_events)
                env["X86SIM_SWT_ISSUES"] = dv(swt_issues)
                if kind == "ui":
                    tenv = {k: self.subst(str(v), host) for k, v in t.get("UITargetAppEnvironmentVariables", {}).items()}
                    tenv.update(self.profile_env(t["_name"] + "-app"))
                    for k in [k for k in tenv if k.startswith("DYLD_")]:
                        tenv["X86SIM_A64_GENV_" + k] = tenv[k]
                    cfg = configuration(bundle, module, target_path=tpath, target_bid=tbid,
                                        target_args=[self.subst(x, host) for x in t.get("UITargetAppCommandLineArguments", [])],
                                        target_env=tenv, ui=True, only=only or None, skip=skip, interop=interop, t=t,
                                        scheme=self.scheme)
                else:
                    cfg = configuration(bundle, module, only=only or None, skip=skip, interop=interop, t=t,
                                        scheme=self.scheme)
                env["XCTestConfigurationFilePath"] = self.write_config(name, cfg)
                env["XCTestBundlePath"] = bundle
                rel = os.path.relpath(outf, self.data)
                args = list(t.get("CommandLineArguments", []))
                if rec:
                    # XCTest takes no diagnostic screenshots while a screen recording is on (XCUIAutomation's
                    # XCUIDiagnosticScreenCapturePolicy.allowsScreenshot(withScreenRecordingActive:): only with the
                    # screenshots capture format): Apple's failed UI test has its recording and a UI snapshot, no
                    # screenshots. x86sim records the screen itself (Recorder), so XCTest's recording never starts;
                    # its DisableDiagnosticScreenshots default (the same policy's screenshotsDisabled) does the same.
                    args += ["-DisableDiagnosticScreenshots", "YES"]
                r = self.simctl.check(self.simctl.request("launch", timeout=50, bundle=bid, env=env,
                                                          args=args, stdout=rel, stderr=rel,
                                                          terminate=True), "launch")
                pid = r.get("pid")
                alive = lambda: self.simctl.pidof(bid) is not None   # noqa: E731
                time.sleep(1)
            with open(log, "wb") as raw:
                how = self.wait(parser, outf, self.a["timeout"], alive, raw)
            parser.swift_testing(swt_events, os.path.join(self.out, name + ".swt.jsonl"), self.root)
            parser.issue_stacks(swt_issues, os.path.join(self.out, name + ".issues.jsonl"), self.root)
            if how == "timeout":
                parser.finish("timed out after %d s" % self.a["timeout"])
            elif how == "exited":
                parser.finish("the test process exited before XCTest finished (crash? see x86sim logs %s)" % (bid or "xctest"))
            if bid and how != "exited":
                try:
                    self.simctl.request("terminate", bundle=bid)
                except SystemExit:
                    pass
            if kind == "ui":
                try:
                    self.simctl.request("terminate", bundle=tbid)
                except SystemExit:
                    pass
            if not parser.tests and how == "done":
                res["note"] = "no test matched (-only-testing: Target/Class/test, the target first)"
                print("x86sim test: %s: %s" % (name, res["note"]), file=sys.stderr)
            if not parser.tests and how != "done":
                res["error"] = {"timeout": "no test started in %d s" % self.a["timeout"],
                                "exited": "the test process ended before any test ran (see %s)" % log}.get(how, how)
            res["pid"] = pid
        except (RuntimeError, OSError, subprocess.CalledProcessError) as e:
            res["error"] = str(e)
        except SystemExit as e:   # simctl's die(): no answer from the phone
            res["error"] = "the device did not answer (%s)" % e
        res["tests"] = parser.tests
        res["attachments"] = self.attachments(att, parser.tests)
        if rec:
            rec.keep(os.path.join(self.out, "attachments"), t.get("SystemAttachmentLifetime", "deleteOnSuccess"))
        res["duration"] = round(time.monotonic() - t0, 3)
        failed = any(x["result"] == "failed" for x in parser.tests) or "error" in res
        res["result"] = "failed" if failed else "passed"
        if "error" in res:
            print("x86sim test: %s: %s" % (name, res["error"]), file=sys.stderr)
        return res

    def attachments(self, raw, tests):
        """Issue #50: the attachments libsimshim saved during a target's run, moved to attachments/<Class>.<test>/ and
        listed in each test's "attachments" (deleteOnSuccess ones of passed tests are dropped, as Xcode does). Those
        added outside a test (class setUp) go to attachments/_run/ and are returned for the target."""
        base = os.path.dirname(raw)
        loose = []
        try:
            lines = open(os.path.join(raw, "index.jsonl")).read().splitlines()
        except OSError:
            lines = []
        self.activities(raw, tests)
        self.start_times(raw, tests)
        used = set()
        for ln in lines:
            try:
                e = json.loads(ln)
            except ValueError:
                continue
            if not e.get("file"):
                continue
            src = os.path.join(raw, e["file"])
            m = re.match(r"-\[(\S+) (\S+)\]$", e.get("test") or "")
            t = None
            if m:
                cls, tn = m.group(1).split(".")[-1], m.group(2)
                t = next((x for x in tests if x["name"] == tn and x["class"].split(".")[-1] == cls), None)
            if t and t["result"] in ("passed", "expected_failure") and e.get("lifetime") == 1:
                continue
            folder = "%s.%s" % (t["class"].split(".")[-1], t["name"]) if t else "_run"
            stem = re.sub(r"[^A-Za-z0-9._-]+", "_", e.get("name") or "") or "attachment-%d" % e.get("seq", 0)
            ext = os.path.splitext(e["file"])[1]
            dst = os.path.join(base, folder, stem + ext)
            k = 1
            while dst in used or os.path.exists(dst):
                k += 1
                dst = os.path.join(base, folder, "%s-%d%s" % (stem, k, ext))
            used.add(dst)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            try:
                shutil.move(src, dst)
            except OSError:
                continue
            item = {"name": e.get("name"), "path": dst, "type": e.get("uti"), "from": e.get("from")}
            if e.get("time"):
                item["time"] = e["time"]   # XCTAttachment.timestamp (libsimshim's): when the test made it
            if e.get("activity"):
                item["activity"] = e["activity"]   # the uuid of its activity (its test's "activities")
            (t.setdefault("attachments", []) if t else loose).append(item)
        shutil.rmtree(raw, ignore_errors=True)
        try:
            os.rmdir(base)   # nothing was kept
        except OSError:
            pass
        return loose

    @staticmethod
    def start_times(raw, tests):
        """When each XCTest test really ran (libsimshim's tests.jsonl: -[XCTestCase performTest:]'s start and end, Unix
        times): the test's "started", where its result bundle entry and activities are placed, as Xcode's are."""
        try:
            lines = open(os.path.join(raw, "tests.jsonl")).read().splitlines()
        except OSError:
            return
        for ln in lines:
            try:
                e = json.loads(ln)
            except ValueError:
                continue
            m = re.match(r"-\[(\S+) (\S+)\]$", e.get("test") or "")
            if not m or not e.get("start"):
                continue
            cls, tn = m.group(1).split(".")[-1], m.group(2)
            t = next((x for x in tests if not x.get("swift_testing") and x["name"] == tn
                      and x["class"].split(".")[-1] == cls and "started" not in x), None)
            if t is not None:
                t["started"] = round(e["start"], 6)

    @staticmethod
    def activities(raw, tests):
        """XCTest's activity tree of each test (libsimshim's activities.jsonl: every activity when it finished), as
        Xcode keeps it (ActionTestSummary.activitySummaries): a test's "activities", each {"uuid", "title", "type",
        "start", "finish", "children"}, in the order they started; an attachment's "activity" names its own."""
        try:
            lines = open(os.path.join(raw, "activities.jsonl")).read().splitlines()
        except OSError:
            return
        per = {}
        for ln in lines:
            try:
                e = json.loads(ln)
            except ValueError:
                continue
            m = re.match(r"-\[(\S+) (\S+)\]$", e.get("test") or "")
            if not m or not e.get("uuid"):
                continue
            cls, tn = m.group(1).split(".")[-1], m.group(2)
            t = next((x for x in tests if x["name"] == tn and x["class"].split(".")[-1] == cls), None)
            if t is not None:
                per.setdefault(id(t), (t, {}))[1][e["uuid"]] = {
                    "uuid": e["uuid"], "parent": e.get("parent"), "title": e.get("title") or "", "type": e.get("type"),
                    "start": e.get("start"), "finish": e.get("finish"), "children": []}
        for t, nodes in per.values():
            roots = []
            for n in sorted(nodes.values(), key=lambda n: (n["start"] or 0, n["finish"] or 0)):
                p = nodes.get(n.pop("parent"))
                (p["children"] if p else roots).append(n)
            t["activities"] = roots

    def run(self, root, targets):
        self.root = root
        self.setup_platform()
        if self.a["targets"]:
            targets = [t for t in targets if t["_name"] in self.a["targets"]]
            if not targets:
                die("no test target named %s" % ", ".join(self.a["targets"]))
        self.names = {n for t in targets for n in [t["_name"]] + list(t.get("_modules", ()))}
        self.scheme = self.scheme_info(targets)
        t0 = time.time()
        results = [r for r in (self.run_target(t) for t in targets) if r]
        cov = coverage(self.profraw, self.root, os.path.join(self.out, "coverage"), self.say) if self.a["coverage"] else None
        try:
            shutil.rmtree(self.work)
        except OSError:
            pass
        s = summary(self, results, t0)
        if cov is not None:
            s["coverage"] = cov
        return s


# ---------------------------------------------------------------------------------------------------------------------
# code coverage (--coverage): what Xcode does with -enableCodeCoverage YES, with LLVM's tools for its xccov


def llvm_tool(name):
    """llvm-profdata / llvm-cov of the Swift toolchain that built the products (x86sim dev's), else sdks/ or PATH"""
    cands = []
    sw = shutil.which("swift")
    if sw:
        cands.append(os.path.join(os.path.dirname(os.path.realpath(sw)), name))
    cands += sorted(glob_dirs(os.path.join(ROOT, "sdks"), "swift-*/usr/bin/" + name), reverse=True)
    cands.append(shutil.which(name) or "")
    return next((c for c in cands if c and os.access(c, os.X_OK)), None)


def glob_dirs(base, pattern):
    import glob
    return glob.glob(os.path.join(base, pattern))


def covered_binaries(root):
    """Mach-O files under the products with coverage mapping (__llvm_covmap), by product: {product: [path]}"""
    out = {}
    objdump = llvm_tool("llvm-objdump")
    for d, dirs, files in os.walk(root):
        dirs[:] = [x for x in dirs if not x.endswith((".dSYM", ".swiftmodule", "_CodeSignature"))]
        for f in files:
            p = os.path.join(d, f)
            try:
                with open(p, "rb") as h:
                    if h.read(4) not in (b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe"):
                        continue
            except OSError:
                continue
            r = subprocess.run([objdump, "--macho", "--section-headers", p], capture_output=True, text=True) if objdump \
                else None
            if r is None or "__llvm_covmap" not in r.stdout:
                continue
            rel = os.path.relpath(p, root).split(os.sep)
            bundle = next((x for x in rel if x.endswith((".app", ".xctest", ".framework", ".appex"))), rel[-1])
            # a test bundle inside its app is its own product, as in xccov
            inner = [x for x in rel[:-1] if x.endswith((".app", ".xctest", ".framework", ".appex"))]
            out.setdefault(inner[-1] if inner else bundle, []).append(p)
    return out


def coverage(profraw, root, out, say):
    """Merge the test processes' profiles and report line coverage per product and file, as xccov view --report"""
    os.makedirs(out, exist_ok=True)
    raws = sorted(glob_dirs(profraw, "*.profraw")) if os.path.isdir(profraw) else []
    profdata, cov = llvm_tool("llvm-profdata"), llvm_tool("llvm-cov")
    res = {"profdata": None, "products": [], "lines": {"covered": 0, "executable": 0, "percent": 0.0}}
    if not raws:
        say("\ncoverage: no profiles were written (built without coverage? CLANG_COVERAGE_MAPPING=YES)")
        res["error"] = "no profiles"
        return res
    if not (profdata and cov):
        res["error"] = "llvm-profdata / llvm-cov not found"
        say("\ncoverage: " + res["error"])
        return res
    pd = os.path.join(out, "Coverage.profdata")
    r = subprocess.run([profdata, "merge", "-sparse", "-o", pd] + raws, capture_output=True, text=True)
    if r.returncode:
        res["error"] = "llvm-profdata merge: " + (r.stderr.strip().splitlines() or ["failed"])[-1]
        say("\ncoverage: " + res["error"])
        return res
    res["profdata"] = pd
    lcov = []
    tot_c = tot_n = 0
    for product, bins in sorted(covered_binaries(root).items()):
        objs = [x for b in bins for x in ("-object", b)][1:]
        r = subprocess.run([cov, "export", "-format=text", "-summary-only", "-instr-profile", pd] + objs,
                           capture_output=True, text=True)
        try:
            data = json.loads(r.stdout)["data"][0]
        except (ValueError, KeyError, IndexError):
            continue
        files = []
        for f in data.get("files", []):
            ln = f["summary"]["lines"]
            files.append({"file": f["filename"], "covered": ln["covered"], "executable": ln["count"],
                          "percent": round(ln["percent"], 2)})
        ln = data["totals"]["lines"]
        res["products"].append({"name": product, "binaries": bins, "covered": ln["covered"], "executable": ln["count"],
                                "percent": round(ln["percent"], 2), "files": sorted(files, key=lambda x: x["file"])})
        tot_c, tot_n = tot_c + ln["covered"], tot_n + ln["count"]
        r = subprocess.run([cov, "export", "-format=lcov", "-instr-profile", pd] + objs, capture_output=True, text=True)
        lcov.append(r.stdout)
    res["lines"] = {"covered": tot_c, "executable": tot_n, "percent": round(100.0 * tot_c / tot_n, 2) if tot_n else 0.0}
    with open(os.path.join(out, "lcov.info"), "w") as f:
        f.write("".join(lcov))
    with open(os.path.join(out, "coverage.json"), "w") as f:
        json.dump(res, f, indent=1)
    lines = ["%-70s %s" % ("Name", "Coverage"), "%s %s" % ("-" * 70, "-" * 15)]
    for p in res["products"]:
        lines.append("%-70s %.2f%% (%d/%d)" % (p["name"], p["percent"], p["covered"], p["executable"]))
        for f in p["files"]:
            lines.append("    %-66s %.2f%% (%d/%d)" % (f["file"], f["percent"], f["covered"], f["executable"]))
    with open(os.path.join(out, "report.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")
    say("\nCode coverage (%s):\n" % out + "\n".join(lines))
    return res


def summary(run, results, t0):
    tests = [x for r in results for x in r["tests"]]
    counts = {k: sum(1 for x in tests if x["result"] == k) for k in ("passed", "failed", "skipped")}
    counts["expected_failures"] = sum(1 for x in tests if x["result"] == "expected_failure")
    counts["total"] = len(tests)
    ok = results and all(r["result"] == "passed" for r in results)
    return {"phone": run.phone, "device": run.dev, "runtime": run.rt, "platform": run.plat,
            "started": time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(t0)), "started_unix": round(t0, 3),
            "duration": round(time.time() - t0, 3),
            "result": "passed" if ok else "failed", "counts": counts, "out": run.out, "targets": results}


def build_failed(a, blog):
    """x86sim test --build-log of a build that failed: no test runs; results.json and the result bundle say what Xcode's
    say then (the build's errors, "Testing cancelled because the build failed.")"""
    try:
        dev = x86config.device()
        plat, rt = device_platform(dev)
    except SystemExit:
        dev, plat, rt = None, "ios", None
    log = blog.get("log") or {}
    t0 = float(log.get("startTime") or time.time())
    out = a["out"] or os.path.join(x86config.get("X86SIM_SCRATCH"), "xctest", "build-" + time.strftime("%Y%m%d-%H%M%S"))
    os.makedirs(out, exist_ok=True)
    s = {"phone": os.path.basename(dev or "").replace("device-a64-", "", 1) or None, "device": dev, "runtime": rt,
         "platform": plat, "started": time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(t0)), "started_unix": round(t0, 3),
         "duration": round(time.time() - t0, 3), "result": "failed",
         "counts": {"passed": 0, "failed": 0, "skipped": 0, "expected_failures": 0, "total": 0}, "out": out,
         "targets": [], "build": "failed", "build_log": log, "build_issues": blog.get("issues") or [],
         "error": "the build failed"}
    for k in ("project", "scheme"):
        if blog.get(k):
            s[k] = blog[k]
    with open(os.path.join(out, "results.json"), "w") as f:
        json.dump(s, f, indent=1)
    if a["json"] == "-":
        json.dump(s, sys.stdout, indent=1)
    elif a["json"]:
        with open(a["json"], "w") as f:
            json.dump(s, f, indent=1)
    if a["result_bundle"]:
        import xcresult
        try:
            xcresult.write(s, a["result_bundle"], None)
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as e:
            print("x86sim test: result bundle not written: %s" % e, file=sys.stderr)
    for i in s["build_issues"]:
        if i.get("severity") == "error" and not a["quiet"]:
            print("x86sim test: build error: %s%s" % ("%s:%s: " % (i["file"], i.get("line")) if i.get("file") else "",
                                                     i.get("message")), file=sys.stderr)
    return 1


def junit(s):
    root = ET.Element("testsuites", name="x86sim test", tests=str(s["counts"]["total"]),
                      failures=str(s["counts"]["failed"]), skipped=str(s["counts"]["skipped"]), time="%.3f" % s["duration"])
    for r in s["targets"]:
        by_class = {}
        for t in r["tests"]:
            by_class.setdefault(t["class"], []).append(t)
        if "error" in r and not r["tests"]:
            ts = ET.SubElement(root, "testsuite", name=r["name"], tests="1", failures="0", errors="1", skipped="0", time="%.3f" % r["duration"])
            tc = ET.SubElement(ts, "testcase", classname=r["name"], name="(setup)", time="0")
            ET.SubElement(tc, "error", message=r["error"]).text = r["error"]
            continue
        for cls, ts_ in by_class.items():
            ts = ET.SubElement(root, "testsuite", name="%s.%s" % (r["name"], cls.split(".")[-1]), tests=str(len(ts_)),
                               failures=str(sum(1 for t in ts_ if t["result"] == "failed")), errors="0",
                               skipped=str(sum(1 for t in ts_ if t["result"] == "skipped")),
                               time="%.3f" % sum(t["duration"] or 0 for t in ts_))
            for t in ts_:
                tc = ET.SubElement(ts, "testcase", classname=cls, name=t["name"], time="%.3f" % (t["duration"] or 0))
                if t["result"] == "failed":
                    f0 = t["failures"][0] if t["failures"] else {"message": "failed", "file": None, "line": None}
                    e = ET.SubElement(tc, "failure", message=f0["message"].split("\n")[0])
                    if f0.get("file"):
                        e.set("file", f0["file"])
                        e.set("line", str(f0["line"]))
                    e.text = "\n".join(("%s:%s: %s" % (f["file"], f["line"], f["message"])) if f.get("file") else f["message"]
                                       for f in t["failures"])
                elif t["result"] == "skipped":
                    ET.SubElement(tc, "skipped", message=t.get("skip_message", ""))
                elif t["result"] == "expected_failure":   # not a failure (Xcode: Expected Failure): said in its output
                    ET.SubElement(tc, "system-out").text = "\n".join(
                        "Expected failure%s: %s%s" % (" (%s)" % f["reason"] if f.get("reason") else "",
                                                      "%s:%s: " % (f["file"], f["line"]) if f.get("file") else "",
                                                      f["message"]) for f in t.get("expected_failures") or [])
    ET.indent(root)
    return ET.tostring(root, encoding="unicode", xml_declaration=True) + "\n"


def report(s, quiet):
    """xcodebuild's closing lines: the failing tests and TEST SUCCEEDED / FAILED."""
    c = s["counts"]
    p = (lambda *x: print(*x, file=sys.stderr)) if quiet == "json" else print
    p("\nTest session results: %s" % s["out"])
    xf = c.get("expected_failures") or 0
    p("\t%d tests: %d passed, %d failed, %d skipped%s in %.1f s on %s" % (
        c["total"], c["passed"], c["failed"], c["skipped"], ", %d expected failure%s" % (xf, "" if xf == 1 else "s") if xf else "",
        s["duration"], s["phone"]))
    fails = [(r, t) for r in s["targets"] for t in r["tests"] if t["result"] == "failed"]
    errs = [r for r in s["targets"] if "error" in r]
    if fails or errs:
        p("\nFailing tests:")
        for r, t in fails:
            if t.get("swift_testing"):   # Swift Testing: as Xcode names it (Suite/test())
                p("\t%s: %s" % (r["name"], t["identifier"]))
            else:
                n = t["name"] if t["name"].endswith(")") else t["name"] + "()"   # "positive(x:)" keeps its own
                p("\t%s: %s.%s" % (r["name"], t["class"].split(".")[-1], n))
            for f in t["failures"]:
                where = "%s:%s: " % (f["file"], f["line"]) if f.get("file") else ""
                p("\t\t%s%s" % (where, f["message"].split("\n")[0]))
        for r in errs:
            p("\t%s: %s" % (r["name"], r["error"]))
    p("\n** TEST %s **" % ("SUCCEEDED" if s["result"] == "passed" else "FAILED"))


# ---------------------------------------------------------------------------------------------------------------------

def fetch_platform(args):
    """Copy what x86sim test needs of <Platform>.platform from the Darwin SDK `x86sim dev setup sdk --xip Xcode.xip`
    builds on Linux (or --platform DIR, a copy of Xcode's Contents/Developer/Platforms) into sdks/."""
    plat = next((x for x in args if not x.startswith("-")), "ios")
    if plat not in PLATFORMS:
        die("fetch-platform ios|tvos|watchos|xros [--platform DIR]")
    name = PLATFORMS[plat]
    given = args[args.index("--platform") + 1] if "--platform" in args[:-1] else None
    dst = os.path.join(ROOT, "sdks")
    sdk_plat = os.path.join(dst, name + ".platform")
    src = find_platform(name, given)
    if os.path.realpath(src) == os.path.realpath(sdk_plat):
        print(sdk_plat)
        return
    print("x86sim test: copying %s's XCTest files from %s into %s" % (name, src, dst), file=sys.stderr)
    for part in PLATFORM_PARTS + ["usr/lib/XCTest.swiftmodule"]:
        s, d = os.path.join(src, "Developer", part), os.path.join(sdk_plat, "Developer", part)
        if not os.path.exists(s):
            continue
        os.makedirs(os.path.dirname(d), exist_ok=True)
        if os.path.isdir(s):
            shutil.rmtree(d, ignore_errors=True)
            shutil.copytree(s, d, symlinks=True)
        else:
            shutil.copy2(s, d)
    print(sdk_plat)


def parse(argv):
    a = {"xctestrun": None, "products": None, "app": None, "bundles": [], "runner": None, "spm": None, "only": [],
         "skip": [], "targets": [], "platform": None, "out": None, "json": None, "junit": None, "timeout": 1800.0,
         "verbose": False, "quiet": False, "coverage": False, "no_recording": False, "result_bundle": None,
         "built": False, "build_log": None}
    it = iter(argv)
    for x in it:
        k, _, v = x.partition("=") if x.startswith("--") else (x, "", "")
        if x.startswith("-only-testing:") or x.startswith("-skip-testing:"):
            a["only" if x[1] == "o" else "skip"].append(x.split(":", 1)[1])
            continue

        def val():
            return v if v else next(it, None) or die("%s needs a value" % k)
        if k == "--xctestrun":
            a["xctestrun"] = val()
        elif k == "--products":
            a["products"] = val()
        elif k == "--app":
            a["app"] = val()
        elif k == "--test-bundle":
            a["bundles"].append(val())
        elif k == "--ui-test-runner":
            a["runner"] = val()
        elif k == "--spm":
            a["spm"] = val()
        elif k == "--only-testing":
            a["only"].append(val())
        elif k == "--skip-testing":
            a["skip"].append(val())
        elif k == "--target":
            a["targets"].append(val())
        elif k == "--platform":
            a["platform"] = val()
        elif k == "--out":
            a["out"] = val()
        elif k == "--json":
            a["json"] = val()
        elif k == "--junit":
            a["junit"] = val()
        elif k == "--built":
            a["built"] = True
        elif k == "--build-log":
            a["build_log"] = val()
        elif k == "--result-bundle":
            a["result_bundle"] = val()
        elif k == "--timeout":
            a["timeout"] = float(val())
        elif k == "--verbose":
            a["verbose"] = True
        elif k == "--coverage":
            a["coverage"] = True
        elif k == "--no-screen-recording":
            a["no_recording"] = True
        elif k == "--quiet":
            a["quiet"] = True
        elif k in ("-h", "--help", "help"):
            print(USAGE)
            sys.exit(0)
        elif x.endswith(".xctestrun") and not a["xctestrun"]:
            a["xctestrun"] = x
        else:
            die("unknown argument %s\n%s" % (x, USAGE))
    if a["json"] == "-":
        a["quiet"] = "json"
    return a


def main(argv):
    if argv and argv[0] == "fetch-platform":
        return fetch_platform(argv[1:])
    a = parse(argv)
    blog = None
    if a["build_log"]:
        try:
            with open(a["build_log"]) as f:
                blog = json.load(f)
        except (OSError, ValueError) as e:
            die("--build-log %s: %s" % (a["build_log"], e))
        if blog.get("status") == "failed":
            return build_failed(a, blog)
    if not (a["xctestrun"] or a["app"] or a["bundles"] or a["runner"] or a["spm"]):
        die("nothing to run\n" + USAGE)
    run = Runner(a)
    try:
        if a["xctestrun"]:
            root, targets = load_xctestrun(a["xctestrun"], a["products"])
        elif a["spm"]:
            root, targets = spm_targets(a, run.plat)
        else:
            root, targets = explicit_targets(a)
    except (RuntimeError, OSError, plistlib.InvalidFileException) as e:
        die(str(e))
    s = run.run(root, targets)
    if a["built"] or blog:
        s["build"] = "succeeded"   # the result bundle's build action (xcodebuild test built the products)
    if blog:
        s["build_log"], s["build_issues"] = blog.get("log"), blog.get("issues") or []
    if a.get("spm_project"):   # the package: the .xcresult's project and scheme, as Xcode names them
        s.setdefault("project", a["spm_project"])
    with open(os.path.join(run.out, "results.json"), "w") as f:
        json.dump(s, f, indent=1)
    x = junit(s)
    with open(os.path.join(run.out, "junit.xml"), "w") as f:
        f.write(x)
    if a["junit"]:
        with open(a["junit"], "w") as f:
            f.write(x)
    if a["result_bundle"]:
        import xcresult
        try:
            xcresult.write(s, a["result_bundle"], root)
            s["result_bundle"] = os.path.abspath(a["result_bundle"])
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as e:
            print("x86sim test: result bundle not written: %s" % e, file=sys.stderr)
    if a["json"] == "-":
        json.dump(s, sys.stdout, indent=1)
        print()
    elif a["json"]:
        with open(a["json"], "w") as f:
            json.dump(s, f, indent=1)
    report(s, a["quiet"])
    if not s["targets"]:
        return 2
    return 0 if s["result"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
