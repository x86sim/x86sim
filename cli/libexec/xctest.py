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
Results: per-test lines while it runs, a summary as xcodebuild prints it, results.json and junit.xml in the output dir;
exit status 0 when every test passed (or was skipped), 1 when one failed, 2 when nothing could run.
"""
import json, os, plistlib, re, shutil, subprocess, sys, time, uuid
import xml.etree.ElementTree as ET
from plistlib import UID

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "lib"))
import x86config  # noqa: E402

USAGE = """usage: x86sim [--phone NAME] test [options]
       x86sim test fetch-platform [ios|tvos|watchos|xros]

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
  --json FILE, --junit FILE
                          also write the JSON summary / JUnit XML there ("-": JSON to stdout, nothing else printed)
  --timeout S             per test target (default 1800)
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
        if isinstance(v, (bool, int, float)):
            return v
        i = len(objs)
        objs.append(None)
        if isinstance(v, str):
            objs[i] = v
        elif isinstance(v, URL):
            objs[i] = {"NS.base": UID(0), "NS.relative": enc("file://" + v.path), "$class": cls(["NSURL", "NSObject"])}
        elif isinstance(v, NSUUID):
            objs[i] = {"NS.uuidbytes": uuid.UUID(v.u).bytes, "$class": cls(["NSUUID", "NSObject"])}
        elif isinstance(v, (list, tuple)):
            objs[i] = {"NS.objects": [enc(x) for x in v], "$class": cls(["NSArray", "NSObject"])}
        elif isinstance(v, (set, frozenset)):
            objs[i] = {"NS.objects": [enc(x) for x in sorted(v)], "$class": cls(["NSSet", "NSObject"])}
        elif isinstance(v, dict):
            ks = list(v)
            objs[i] = {"NS.keys": [enc(k) for k in ks], "NS.objects": [enc(v[k]) for k in ks],
                       "$class": cls(["NSDictionary", "NSObject"])}
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


def test_identifiers(s):
    """As -[XCTTestIdentifier initWithStringRepresentation:] archives "Class" (o 3), "Class/method" (o 2: an
    Objective-C style name) and "Class/method()" (o 6, os: a Swift one). A method goes in both forms: the run list
    matches either, the skip list only the form of the test (Swift tests: the second)."""
    parts = [p for p in s.replace("()", "").split("/") if p]
    if len(parts) > 1:
        return [Obj(["XCTTestIdentifier", "NSObject"], {"a": [], "c": parts[:2], "o": 2, "ocm": True}),
                Obj(["XCTTestIdentifier", "NSObject"], {"a": [], "c": parts[:2], "o": 6, "ocm": True, "os": True})]
    return [Obj(["XCTTestIdentifier", "NSObject"], {"a": None, "c": parts, "o": 3, "oc": True, "ocm": True})]


def identifier_set(ids):
    return Obj(["XCTTestIdentifierSet", "NSObject"], {"identifiers": [i for x in ids for i in test_identifiers(x)]})


def configuration(bundle, module, target_path=None, target_bid=None, target_args=(), target_env=None, ui=False,
                  only=None, skip=()):
    sel = Obj(["XCTTestSelection", "NSObject"], {
        "IdentifiersToRun": identifier_set(only) if only else None, "IdentifiersToSkip": identifier_set(skip),
        "TagsToRun": None,
        "TagsToSkip": Obj(["XCTTagSelection", "NSObject"], {"XCTTagSelectionIncludeXCTests": False,
                                                          "XCTTagSelectionMode": 1, "XCTTagSelectionTagSet": set()})})
    f = {
        "ApplicationBundleInfos": None, "EnablePerformanceTestsDiagnostics": None, "IDECapabilities": None,
        "IsMemoryTaggingAddressSanitizerEnabled": False, "aggregateStatisticsBeforeCrash": None,
        "applicationCrashDetectionPolicy": Obj(["XCTApplicationCrashDetectionPolicy", "NSObject"],
                                               {"XCTApplicationCrashDetectionSeverity": 2}),
        "automationFrameworkPath": None, "baseTestsURL": None, "baselineFileRelativePath": None, "baselineFileURL": None,
        "bundleIDsForCrashReportEmphasis": None, "defaultTestExecutionTimeAllowance": None,
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
        "sessionIdentifier": None, "shouldExtractMultiDeviceRequirements": False, "symbolicationImageNames": None,
        "systemAttachmentLifetime": 0,
        "targetApplicationArguments": list(target_args) if target_path else None,
        "targetApplicationBundleID": target_bid,
        "targetApplicationEnvironment": dict(target_env or {}) if target_path else None,
        "targetApplicationPath": target_path, "testApplicationDependencies": None, "testApplicationUserOverrides": None,
        "testBundleRelativePath": None, "testBundleURL": URL(bundle), "testExecutionOrdering": 0,
        "testInstructionsSet": None, "testInteropMode": 1, "testSelection": sel, "testTimeoutsEnabled": False,
        "testsDrivenByIDE": False, "testsMustRunOnMainThread": False, "traceCollectionEnabled_v2": None,
        "treatMissingBaselinesAsFailures": False, "userAttachmentLifetime": 0,
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
    die("no XCTest frameworks for %s: put Xcode's %s.platform in sdks/ (x86sim test fetch-platform copies it from "
        "$X86SIM_MAC), point --platform / X86SIM_XCTEST_PLATFORMS at a copy of Xcode's Contents/Developer/Platforms, "
        "or set up the xtool SDK from an Xcode .xip (x86sim dev setup sdk --xip Xcode.xip)" % (name, name))


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
    try:
        if open(os.path.join(dst, ".x86sim-stamp")).read() == stamp:
            return dst_root
    except OSError:
        pass
    shutil.rmtree(dst, ignore_errors=True)
    lipo = shutil.which("llvm-lipo") or shutil.which("lipo")
    for part in PLATFORM_PARTS:
        s, d = os.path.join(src, "Developer", part), os.path.join(dst, "Developer", part)
        if not os.path.exists(s):
            continue
        os.makedirs(os.path.dirname(d), exist_ok=True)
        if os.path.isdir(s):
            shutil.copytree(s, d, symlinks=True)
        else:
            shutil.copy2(s, d)
    for dp, _, fs in os.walk(dst):
        for f in fs:
            p = os.path.join(dp, f)
            if not os.path.islink(p) and macho_archs(p):
                if not lipo:
                    die("llvm-lipo is needed to thin %s (install llvm)" % p)
                subprocess.run([lipo, "-thin", "arm64", p, "-output", p + ".thin"], check=True)
                os.replace(p + ".thin", p)
    open(os.path.join(dst, ".x86sim-stamp"), "w").write(stamp)
    return dst_root


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
    cmd = ["nice", "swift", "build", "--build-system", "native", "--build-tests", "--swift-sdk", "darwin",
           "--triple", triple, "--package-path", d, "-j", os.environ.get("X86SIM_BUILD_JOBS", "3")]
    print("x86sim test: " + " ".join(cmd[1:]), file=sys.stderr)
    if subprocess.run(cmd, stdout=sys.stderr).returncode:
        die("swift build --build-tests failed", 1)
    out = subprocess.run(cmd[1:] + ["--show-bin-path"], capture_output=True, text=True).stdout.strip().splitlines()
    bindir = out[-1] if out else os.path.join(d, ".build", triple, "debug")
    found = [os.path.join(bindir, x) for x in sorted(os.listdir(bindir)) if x.endswith(".xctest")]
    if not found:
        die("no .xctest in %s" % bindir)
    a["bundles"] = [ios_layout(b) for b in found]
    return explicit_targets(a)


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
    info = {"CFBundleExecutable": name, "CFBundleIdentifier": "x86sim.spm." + re.sub(r"[^A-Za-z0-9.-]", "-", name),
            "CFBundleName": name, "CFBundlePackageType": "BNDL", "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleVersion": "1", "CFBundleShortVersionString": "1.0", "CFBundleSupportedPlatforms": ["iPhoneSimulator"]}
    old = os.path.join(b, "Contents", "Info.plist")
    if os.path.exists(old):
        info.update(plistlib.load(open(old, "rb")))
    plistlib.dump(info, open(os.path.join(flat, "Info.plist"), "wb"))
    return flat


def selection(t, only, skip, names):
    """(identifiers to run, identifiers to skip) of a target from -only-testing / -skip-testing (Target, Target/Class,
    Target/Class/test, or Class[/test] for every target) and the .xctestrun's own lists; None: the target is left out.
    An empty run list means every test."""
    name = t["_name"]

    def mine(ids):
        """('all' | list of Class[/test] for this target | None when none is about this target)"""
        out = []
        for i in ids:
            p = [x for x in i.replace("()", "").split("/") if x]
            if not p:
                continue
            if p[0] in names:
                if p[0] != name:
                    continue
                if len(p) == 1:
                    return "all"
                out.append("/".join(p[1:3]))
            else:
                out.append("/".join(p[:2]))
        return out or None

    o, s = mine(only), mine(skip)
    if s == "all" or (only and o is None):
        return None
    o = [] if o in (None, "all") else o
    s = s or []
    o += [x.replace("()", "") for x in t.get("OnlyTestIdentifiers", [])]
    s += [x.replace("()", "") for x in t.get("SkipTestIdentifiers", [])]
    return o, s


# ---------------------------------------------------------------------------------------------------------------------
# XCTest's log -> results

R_CASE_START = re.compile(r"^Test Case '-\[(\S+) (\S+)\]' started\.")
R_CASE_END = re.compile(r"^Test Case '-\[(\S+) (\S+)\]' (passed|failed|skipped) \((\d+(?:\.\d+)?) seconds\)\.")
R_FAIL = re.compile(r"^(.*?):(\d+): error: -\[(\S+) (\S+)\] : (.*)$")
R_SKIP = re.compile(r"^(.*?):(\d+): -\[(\S+) (\S+)\] : Test skipped(?: - (.*))?$")
R_SUITE = re.compile(r"^Test Suite '(.+)' (started|passed|failed) at ")
R_EXEC = re.compile(r"^\s+Executed (\d+) tests?")
# Swift Testing (run by XCTest's interop): "✔ Test add() passed after 0.001 seconds.", "✘ Test x() recorded an issue at
# File.swift:12:5: Expectation failed: ...", "➜ Test y() skipped: ..."
R_ST_END = re.compile(r"^\S+ Test (.+?) (passed|failed|skipped)(?: after (\d+(?:\.\d+)?) seconds)?")
R_ST_ISSUE = re.compile(r"^\S+ Test (.+?) recorded an issue at (.+?):(\d+)(?::\d+)?: (.*)$")
R_LOGLINE = re.compile(r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d|arm64host:|simshim:|objc\[|\s+t = )")


class Parser:
    def __init__(self, target):
        self.target = target
        self.tests = []
        self.by_key = {}
        self.current = None
        self.top = None
        self.done = False
        self.last_fail = None

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
            return True
        m = R_CASE_END.match(l)
        if m:
            t = self.test(m.group(1), m.group(2))
            t["result"], t["duration"] = m.group(3), float(m.group(4))
            self.current = self.last_fail = None
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
        m = R_ST_END.match(l)
        if m and m.group(1).startswith("run with "):   # Swift Testing's closing line ("Test run with N tests ...")
            return True
        if m and not l.startswith("Test "):
            t = self.test("SwiftTesting", m.group(1))
            t["result"] = m.group(2)
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
        if ins:
            env["DYLD_INSERT_LIBRARIES"] = ":".join(ins)
        env.setdefault("OS_ACTIVITY_DT_MODE", "YES")
        for k in [k for k in env if k.startswith("DYLD_")]:
            env["X86SIM_A64_GENV_" + k] = env[k]
        return env

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
        parser = Parser(name)
        log = os.path.join(self.out, name + ".log")
        res["log"] = log
        outf = os.path.join(self.work, name + ".out")
        open(outf, "w").close()
        module = t.get("ProductModuleName") or name
        products = self.root
        self.say("\n=== %s (%s tests)" % (name, kind))
        try:
            if kind == "logic":
                bundle = self.subst(t["TestBundlePath"])
                bundle = bundle if bundle.startswith(DV) else dv(bundle)
                agent = os.path.join(self.pdev, "Library/Xcode/Agents/xctest")
                agent = dv(agent) if os.path.exists(agent) else dv(os.path.join(self.rtroot, "Developer/usr/bin/xctest"))
                env = self.environment(t, None, products)
                env["XCTestConfigurationFilePath"] = self.write_config(name, configuration(bundle, module, only=only or None, skip=skip))
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
                if kind == "ui":
                    tenv = {k: self.subst(str(v), host) for k, v in t.get("UITargetAppEnvironmentVariables", {}).items()}
                    for k in [k for k in tenv if k.startswith("DYLD_")]:
                        tenv["X86SIM_A64_GENV_" + k] = tenv[k]
                    cfg = configuration(bundle, module, target_path=tpath, target_bid=tbid,
                                        target_args=[self.subst(x, host) for x in t.get("UITargetAppCommandLineArguments", [])],
                                        target_env=tenv, ui=True, only=only or None, skip=skip)
                else:
                    cfg = configuration(bundle, module, only=only or None, skip=skip)
                env["XCTestConfigurationFilePath"] = self.write_config(name, cfg)
                env["XCTestBundlePath"] = bundle
                rel = os.path.relpath(outf, self.data)
                r = self.simctl.check(self.simctl.request("launch", timeout=50, bundle=bid, env=env,
                                                          args=t.get("CommandLineArguments", []), stdout=rel, stderr=rel,
                                                          terminate=True), "launch")
                pid = r.get("pid")
                alive = lambda: self.simctl.pidof(bid) is not None   # noqa: E731
                time.sleep(1)
            with open(log, "wb") as raw:
                how = self.wait(parser, outf, self.a["timeout"], alive, raw)
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
        res["duration"] = round(time.monotonic() - t0, 3)
        failed = any(x["result"] == "failed" for x in parser.tests) or "error" in res
        res["result"] = "failed" if failed else "passed"
        if "error" in res:
            print("x86sim test: %s: %s" % (name, res["error"]), file=sys.stderr)
        return res

    def run(self, root, targets):
        self.root = root
        self.setup_platform()
        if self.a["targets"]:
            targets = [t for t in targets if t["_name"] in self.a["targets"]]
            if not targets:
                die("no test target named %s" % ", ".join(self.a["targets"]))
        self.names = {t["_name"] for t in targets}
        t0 = time.time()
        results = [r for r in (self.run_target(t) for t in targets) if r]
        try:
            shutil.rmtree(self.work)
        except OSError:
            pass
        return summary(self, results, t0)


def summary(run, results, t0):
    tests = [x for r in results for x in r["tests"]]
    counts = {k: sum(1 for x in tests if x["result"] == k) for k in ("passed", "failed", "skipped")}
    counts["total"] = len(tests)
    ok = results and all(r["result"] == "passed" for r in results)
    return {"phone": run.phone, "device": run.dev, "runtime": run.rt, "platform": run.plat,
            "started": time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(t0)), "duration": round(time.time() - t0, 3),
            "result": "passed" if ok else "failed", "counts": counts, "out": run.out, "targets": results}


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
    ET.indent(root)
    return ET.tostring(root, encoding="unicode", xml_declaration=True) + "\n"


def report(s, quiet):
    """xcodebuild's closing lines: the failing tests and TEST SUCCEEDED / FAILED."""
    c = s["counts"]
    p = (lambda *x: print(*x, file=sys.stderr)) if quiet == "json" else print
    p("\nTest session results: %s" % s["out"])
    p("\t%d tests: %d passed, %d failed, %d skipped in %.1f s on %s" % (c["total"], c["passed"], c["failed"], c["skipped"],
                                                                       s["duration"], s["phone"]))
    fails = [(r, t) for r in s["targets"] for t in r["tests"] if t["result"] == "failed"]
    errs = [r for r in s["targets"] if "error" in r]
    if fails or errs:
        p("\nFailing tests:")
        for r, t in fails:
            p("\t%s: %s.%s()" % (r["name"], t["class"].split(".")[-1], t["name"].replace("()", "")))
            for f in t["failures"]:
                where = "%s:%s: " % (f["file"], f["line"]) if f.get("file") else ""
                p("\t\t%s%s" % (where, f["message"].split("\n")[0]))
        for r in errs:
            p("\t%s: %s" % (r["name"], r["error"]))
    p("\n** TEST %s **" % ("SUCCEEDED" if s["result"] == "passed" else "FAILED"))


# ---------------------------------------------------------------------------------------------------------------------

def fetch_platform(args):
    """Copy what x86sim test needs of Xcode's <Platform>.platform from $X86SIM_MAC into sdks/ (ssh + tar)."""
    plat = args[0] if args else "ios"
    if plat not in PLATFORMS:
        die("fetch-platform ios|tvos|watchos|xros")
    name = PLATFORMS[plat]
    mac = os.environ.get("X86SIM_MAC") or x86config.load().get("X86SIM_MAC")
    if not mac:
        die("set X86SIM_MAC (ssh host of a Mac with Xcode) in x86sim.env, or copy Xcode's "
            "Contents/Developer/Platforms/%s.platform into sdks/ yourself" % name)
    dst = os.path.join(ROOT, "sdks")
    os.makedirs(dst, exist_ok=True)
    parts = " ".join("'%s.platform/Developer/%s'" % (name, p) for p in PLATFORM_PARTS + ["Library/Xcode/Agents/XCTRunner.app", "usr/lib/XCTest.swiftmodule"])
    remote = 'cd "$(xcode-select -p)/Platforms" && COPYFILE_DISABLE=1 tar cf - %s 2>/dev/null' % parts
    print("x86sim test: copying %s.platform's XCTest files from %s into %s" % (name, mac, dst), file=sys.stderr)
    p1 = subprocess.Popen(["ssh", "-o", "BatchMode=yes", mac, remote], stdout=subprocess.PIPE)
    p2 = subprocess.run(["tar", "xf", "-", "-C", dst], stdin=p1.stdout, stderr=subprocess.DEVNULL)
    p1.wait()
    if p2.returncode or not os.path.isdir(os.path.join(dst, name + ".platform/Developer/Library/Frameworks/XCTest.framework")):
        die("copy failed (is Xcode installed on %s?)" % mac, 1)
    print(os.path.join(dst, name + ".platform"))


def parse(argv):
    a = {"xctestrun": None, "products": None, "app": None, "bundles": [], "runner": None, "spm": None, "only": [],
         "skip": [], "targets": [], "platform": None, "out": None, "json": None, "junit": None, "timeout": 1800.0,
         "verbose": False, "quiet": False}
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
        elif k == "--timeout":
            a["timeout"] = float(val())
        elif k == "--verbose":
            a["verbose"] = True
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
    with open(os.path.join(run.out, "results.json"), "w") as f:
        json.dump(s, f, indent=1)
    x = junit(s)
    with open(os.path.join(run.out, "junit.xml"), "w") as f:
        f.write(x)
    if a["junit"]:
        with open(a["junit"], "w") as f:
            f.write(x)
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
