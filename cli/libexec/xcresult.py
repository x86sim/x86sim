#!/usr/bin/env python3
"""x86Sim: write an `x86sim test` run (its results.json) as an Xcode .xcresult bundle, the format Xcode 27.2 writes
(legacy commands format 3.60, storage "fileBacked2"), so that a Mac's `xcrun xcresulttool` reads it like one Xcode made:
`get test-results summary|tests|test-details|activities`, `export attachments`, `get object --legacy`, `get log`.

    python3 cli/libexec/xcresult.py results.json OUT.xcresult [--products DIR]
    xcresult.write(summary, bundle_path, products_root=None)

The bundle is Info.plist + Data/. Format facts (measured on bundles Xcode 27.2 wrote, checked with its xcresulttool):

- Every object is a pair of files: Data/data.<id> (the stored bytes) and Data/refs.<id> (the ids it references).
  id = "0~" + base64url(BLAKE2b-512(R + stored)), with padding, where R is the concatenation of the refs file's entries
  (each 0x00 + the referenced object's 64-byte digest) and `stored` is the file's bytes as written (compressed or
  not): an object's id covers the ids it points to, so the graph is a Merkle tree written leaves first.
- refs.<id>: a count, then per reference a length (65) and 0x00 + digest. Counts and lengths are DER-style: below
  0x80 one byte, else 0x80|n followed by n big-endian bytes (0x81 0xce = 206 refs). No refs: the single byte 0x00.
  Typed objects list their refs sorted by id string (no duplicates); directory objects in entry order.
- data.<id> is zstd (one frame, content size in the header) or raw: Xcode stores incompressible payloads (mp4, tiny
  objects) as they are; xcresulttool tells them apart by the zstd magic 28 b5 2f fd.
- Typed objects ("[T" ...): `[` type fields `]`. type: `T` + typedef the first time a type is used as an object's
  type in that data blob, `S<len>:Name` after. typedef: `[K2:_nV<len>:Name]`, with `K2:_s` + the supertype's typedef
  before the `]` (ActionTestSummary <- ActionTestSummaryIdentifiableObject <- ActionAbstractTestSummary); a type
  defined only as a supertype is not "seen" (it is spelled out again in every subtype's T). Fields: `K<len>:key`
  value, keys sorted. Values are objects; leaf values (String, Int, Double, Bool, Date) are `[<type>K2:_vV<len>:text]`
  (Bool "true"/"false", Date "2026-11-27T19:58:17.688-0700"); Array is the type then its elements, nothing else; a
  missing optional is left out. Every <len> is the UTF-8 byte length.
- Directories (diagnostics, coverage archives) are JSON data objects: [{"name": ..., "type": 1 file | 2 dir}], the
  referenced objects in the same order as the entries.
- xcresulttool's test-results commands are computed from the legacy graph (no database in the bundle): root
  ActionsInvocationRecord -> actions[ActionRecord].actionResult.testsRef -> ActionTestPlanRunSummaries ->
  testableSummaries (one per target) -> ActionTestSummaryGroup "All tests" -> "<Target>.xctest" -> class ->
  ActionTestMetadata.summaryRef -> ActionTestSummary (failureSummaries, skipNoticeSummary, activitySummaries with the
  attachments' payloadRef). Its "Unit test bundle" / "UI test bundle" node comes from testKind ("app hosted",
  "xctest-tool hosted", "UI"); test ids are "Class/test()" for Swift tests ("()" from Xcode's naming, not XCTest's log).
- Expected failures (XCTExpectFailure, Swift Testing's withKnownIssue; results.json's result "expected_failure" and
  "expected_failures"): testStatus "Expected Failure", ActionTestSummary.expectedFailures = ActionTestExpectedFailure
  {failureReason: the reason / known issue's comment (left out without one), failureSummary: the issue's
  ActionTestFailureSummary with isTopLevelFailure false, isTopLevelFailure true, uuid}; a failed test keeps its
  expected failures too. They are not in the issues' testFailureSummaries, nor in ResultMetrics (xcresulttool's
  summary counts them, "expectedFailures", from the statuses); test-results tests names each by its reason, else
  "<file name>:<line>: <message>". A known issue that did not occur is a failure of issueType "Unmatched Expected
  Failure" ("Expected failure 'why' but none recorded", "Known issue was not recorded").
- Issues' call stacks (sourceCodeContext.callStack): every frame of the stack XCTest / Swift Testing captured, as
  SourceCodeFrame {addressString: the address in decimal, one before it for a return address}; only frames in the
  test bundle that symbolicate to a source line have symbolInfo {imageName: the image's name, symbolName: swift-demangle
  --simplified ("closure #1 in Known.thrown()"), location}: macro expansions (#expect, @Test's thunks) and
  compiler-generated code have none. The UI test runner's frames (an image Xcode lists in symbolicationImageNames,
  without debug info) have symbolInfo without a location: "_XCTRunnerRunTests", the runTestsInBackground: block,
  "main"; test-details does not show them. test-details shows the frames with a location as "Source Code Reference"s; a
  known issue that was not recorded has no stack (test-details: one reference named "" at the issue's location).
- A failed #expect / #require: ActionTestFailureSummary.expression = TestExpression {sourceCode, value: TestValue
  {description, debugDescription, typeName, fullyQualifiedTypeName, isCollection, label, children: TestValue}: an
  array's elements and a dictionary's (key: value) pairs (unlabelled, each pair's children labelled "key" / "value"),
  an optional's "some", a struct's properties by name; subexpressions: the same, nested, and for an expression with
  none of its own one per labelled child of its value (source = the label: "x", "y" under "Point(x: 1, y: 2)")}
  (test-details: "Results" with "<subexpression> : <value>", "_" for one whose value is its source, a collection's
  "<source> : 3 elements" with "0 : 1" ... under it). XCTest's assertions have none.
- Line numbers: ActionTestFailureSummary.lineNumber is 1-based; the TestFailureIssueSummary's document URL
  (#StartingLineNumber=) is 0-based, one less.
- The screen recording's attachment is named "kXCTAttachmentScreenRecording" (xcresulttool shows "Screen Recording
  <date>.mp4"); XCTest's automatic screenshots keep their kXCTAttachmentLegacyScreenImageData name.
- A UI test's activities (results.json's test "activities", XCTest's tree from libsimshim): ActionTestActivitySummary
  {title, activityType, uuid (XCTest's), start, finish, attachments, subactivities}, every activity XCTest ran
  ("Start Test at ...", "Set Up", "Open ...", ..., "Tear Down"), each attachment in the activity it was added to
  (inActivityIdentifier = its index there, filename "<name>_<index>_<activity uuid>.<ext>", the extension after a
  "." in the name: "App UI hierarchy for com.x_0_<uuid>.y"; an element snapshot "Snapshot_<index>_<uuid>"), the
  screen recording in "Start Test at". A failed UI test of Apple's has no failure screenshot while the screen is
  recorded: its "Collecting debug information to assist test failure triage" activity holds "App UI hierarchy for
  <bundle id>" (text) and kXCTAttachmentLegacySnapshot (xcresulttool: "UI Snapshot <date>").
- The summary's environmentDescription is "<scheme> · Built with macOS <localComputerRecord.operatingSystemVersion>",
  "macOS" whatever the platform record says: the Linux host's record leaves that version empty (just "<scheme>").
- Code coverage (CodeCoverageInfo): reportRef = an NSKeyedArchiver XCTHCodeCoverage (buildableCoverageObjects:
  XCTHCodeCoverageTarget per product: name, productPath, buildableIdentifier, sourceFiles: XCTHCodeCoverageFile per
  file: name, documentLocation (the path), executable/covered lines, functions); functions is an NSData: u32 count,
  per function <u32 covered lines, u32 executable lines, u32 first line, u32 lines spanned, u64 execution count>, then
  the names (swift-demangle --simplified), NUL-terminated, in llvm-cov's order. archiveRef = a directory (xccov
  extracts it as action.xccovarchive and needs all three files, even for --report): Metadata.plist {formatVersion 1},
  Index (u32 file count; per file u64 offset of its path in Index, u64 offset of its data in Coverage; the paths,
  each u16 length + UTF-8), Coverage (per file: u32 run count; per run a u64: bit 63 executable, bits 48-62 length-1,
  the low bits the line's execution count; then u32 subrange count and per subrange u32 line, u32 column, u32
  length (0: to the end of the line), u64 count). The subranges are the column stretches between llvm-cov's
  segments whose count is not the line's (see subranges()). From the run's Coverage.profdata and products (llvm-cov
  export), xccov view --report / --functions-for-file / --archive --file print what they print for Xcode's bundle.

Left out, as x86Sim has no such thing: the build log and build result's log, the Mac's machine record (the local
computer record describes this Linux host), crash/diagnostic files other than the targets' logs (Diagnostics:
<Target>/StandardOutputAndStandardError.txt); without XCTest's activity tree (a results.json without "activities")
each attachment gets an activity of its own.
"""
import base64, datetime, hashlib, json, os, platform, plistlib, re, shutil, struct, subprocess, sys, time, uuid
import urllib.parse

try:
    from compression import zstd as _zstd   # Python 3.14
except ImportError:   # pragma: no cover
    _zstd = None

# supertype chains, as Xcode's typedefs spell them
SUPER = {
    "ActionTestMetadata": ["ActionTestSummaryIdentifiableObject", "ActionAbstractTestSummary"],
    "ActionTestSummary": ["ActionTestSummaryIdentifiableObject", "ActionAbstractTestSummary"],
    "ActionTestSummaryGroup": ["ActionTestSummaryIdentifiableObject", "ActionAbstractTestSummary"],
    "ActionTestPlanRunSummary": ["ActionAbstractTestSummary"],
    "ActionTestableSummary": ["ActionAbstractTestSummary"],
    "ActivityLogCommandInvocationSection": ["ActivityLogSection"],
    "ActivityLogUnitTestSection": ["ActivityLogSection"],
    "TestFailureIssueSummary": ["IssueSummary"],
}

PLATFORMS = {   # x86sim platform -> (platform identifier, description, sdk prefix, OS name)
    "ios": ("com.apple.platform.iphonesimulator", "iOS Simulator", "iphonesimulator", "iOS"),
    "tvos": ("com.apple.platform.appletvsimulator", "tvOS Simulator", "appletvsimulator", "tvOS"),
    "watchos": ("com.apple.platform.watchsimulator", "watchOS Simulator", "watchsimulator", "watchOS"),
    "xros": ("com.apple.platform.xrsimulator", "visionOS Simulator", "xrsimulator", "visionOS"),
}
KINDS = {"logic": "xctest-tool hosted", "hosted": "app hosted", "ui": "UI"}
RUN_NAME = "Test Scheme Action"
LIB = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "lib")


# ---------------------------------------------------------------------------------------------------------------------
# values

class V:
    """A leaf value: String, Int, Double, Bool, Date."""
    __slots__ = ("t", "v")

    def __init__(self, t, v):
        self.t, self.v = t, v


class O:
    """A typed object; fields that are None are left out."""
    __slots__ = ("t", "f")

    def __init__(self, t, **f):
        self.t, self.f = t, f


class Ref:
    __slots__ = ("id", "target")

    def __init__(self, oid, target=None):
        self.id, self.target = oid, target


def S(s):
    return None if s is None else V("String", str(s))


def I(n):
    return None if n is None else V("Int", str(int(n)))


def D(x):
    return None if x is None else V("Double", repr(float(x)))


def B(b):
    return V("Bool", "true" if b else "false")


def T(ts):
    """Date from a Unix time: local time with its offset, milliseconds (as Xcode writes them)."""
    if ts is None:
        return None
    d = datetime.datetime.fromtimestamp(ts).astimezone()
    return V("Date", d.strftime("%Y-%m-%dT%H:%M:%S.") + "%03d" % (d.microsecond // 1000) + d.strftime("%z"))


class Encoder:
    def __init__(self):
        self.seen, self.refs, self.out = set(), set(), []

    @staticmethod
    def lstr(tag, s):
        b = s.encode("utf-8")
        return tag + str(len(b)).encode() + b":" + b

    def typedef(self, name, sup):
        b = b"[K2:_n" + self.lstr(b"V", name)
        if sup:
            b += b"K2:_s" + self.typedef(sup[0], sup[1:])
        return b + b"]"

    def head(self, name):
        if name in self.seen:
            return b"[" + self.lstr(b"S", name)
        self.seen.add(name)
        return b"[T" + self.typedef(name, SUPER.get(name, []))

    def enc(self, x):
        if isinstance(x, V):
            return self.head(x.t) + b"K2:_v" + self.lstr(b"V", x.v) + b"]"
        if isinstance(x, list):
            return self.head("Array") + b"".join(self.enc(e) for e in x) + b"]"
        if isinstance(x, Ref):
            self.refs.add(x.id)
            return self.enc(O("Reference", id=S(x.id),
                              targetType=O("TypeDefinition", name=S(x.target)) if x.target else None))
        if isinstance(x, O):
            b = self.head(x.t)
            for k in sorted(x.f):
                v = x.f[k]
                if v is not None:
                    b += self.lstr(b"K", k) + self.enc(v)
            return b + b"]"
        raise TypeError("cannot encode %r" % (x,))


def encode(obj):
    """(bytes, [referenced ids]) for a typed object."""
    e = Encoder()
    data = e.enc(obj)
    return data, sorted(e.refs)


# ---------------------------------------------------------------------------------------------------------------------
# storage

def derlen(n):
    if n < 0x80:
        return bytes([n])
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([0x80 | len(b)]) + b


def compress(raw):
    if _zstd is not None:
        return _zstd.compress(raw, level=3)
    if shutil.which("zstd"):
        return subprocess.run(["zstd", "-q", "-3", "-c"], input=raw, capture_output=True, check=True).stdout
    return raw   # no zstd here: stored as is, as Xcode stores incompressible payloads (xcresulttool reads the magic)


class Store:
    def __init__(self, bundle):
        self.dir = os.path.join(bundle, "Data")
        os.makedirs(self.dir, exist_ok=True)
        self.ids = set()

    def put(self, raw, refs=(), squeeze=True):
        stored = raw
        if squeeze and len(raw) > 32:
            z = compress(raw)
            if len(z) < len(raw) * 0.95:
                stored = z
        ents = [b"\x00" + base64.urlsafe_b64decode(r[2:]) for r in refs]
        oid = "0~" + base64.urlsafe_b64encode(hashlib.blake2b(b"".join(ents) + stored).digest()).decode()
        if oid not in self.ids:
            self.ids.add(oid)
            with open(os.path.join(self.dir, "data." + oid), "wb") as f:
                f.write(stored)
            with open(os.path.join(self.dir, "refs." + oid), "wb") as f:
                f.write(derlen(len(ents)) + b"".join(derlen(len(e)) + e for e in ents))
        return oid

    def obj(self, o):
        data, refs = encode(o)
        return self.put(data, refs)

    def file(self, path):
        with open(path, "rb") as f:
            raw = f.read()
        ext = os.path.splitext(path)[1].lower()
        return self.put(raw, squeeze=ext not in (".mp4", ".mov", ".jpg", ".jpeg", ".png", ".heic", ".gz", ".zip")), len(raw)

    def directory(self, entries):
        """entries: [(name, ("file", id) | ("dir", id))], written as Xcode's JSON directory listing."""
        entries = sorted(entries)
        listing = [{"name": n, "type": 1 if k == "file" else 2} for n, (k, _) in entries]
        return self.put(json.dumps(listing, separators=(",", ":")).encode(), [i for _, (_, i) in entries])


# ---------------------------------------------------------------------------------------------------------------------
# what results.json does not say

def parse_time(s):
    try:
        return time.mktime(time.strptime(s, "%Y-%m-%dT%H:%M:%S"))
    except (TypeError, ValueError):
        return time.time()


def xctestrun_info(summary, products_root):
    """ContainerInfo (project, scheme) and the source root from the run's .xctestrun, when there is one."""
    cands = []
    if summary.get("xctestrun"):
        cands.append(summary["xctestrun"])
    if products_root and os.path.isdir(products_root):
        cands += sorted(os.path.join(products_root, n) for n in os.listdir(products_root) if n.endswith(".xctestrun"))
    for p in cands:
        try:
            with open(p, "rb") as f:
                d = plistlib.load(f)
        except (OSError, plistlib.InvalidFileException, ValueError):
            continue
        ci = d.get("ContainerInfo") or {}
        md = d.get("__xctestrun_metadata__") or {}
        plan = d.get("TestPlan") or {}
        return {"project": ci.get("ContainerName"), "scheme": ci.get("SchemeName"), "plan": plan.get("Name"),
                "source": md.get("SweetPadSourceRoot")}
    return {}


def guess_project(targets):
    names = [t["name"] for t in targets if t.get("name")]
    if not names:
        return "Tests"
    p = os.path.commonprefix(names)
    if len(names) == 1 or len(p) < 2:
        p = re.sub(r"(UI)?Tests?$", "", names[0]) or names[0]
    return p


def device_info(summary):
    plat = summary.get("platform") or "ios"
    pid, pdesc, sdk, osname = PLATFORMS.get(plat, PLATFORMS["ios"])
    m = re.match(r"^[a-z]+-([\d.]+)-([0-9A-Za-z]+)-([a-z0-9_]+)$", summary.get("runtime") or "")
    ver, build, arch = (m.group(1), m.group(2), m.group(3)) if m else ("", "", "arm64")
    dev = summary.get("device") or ""
    model = ""
    try:
        model = open(os.path.join(dev, "model")).read().strip()
    except OSError:
        pass
    code = ""
    try:
        with open(os.path.join(LIB, "devicetypes-a64.json")) as f:
            types = json.load(f)
        if not model:
            model = "iPhone 17" if plat == "ios" else ""
        code = (types.get(model) or {}).get("model", "")
    except (OSError, ValueError):
        pass
    udid = str(uuid.uuid5(uuid.NAMESPACE_URL, os.path.abspath(dev))).upper() if dev else \
        str(uuid.uuid5(uuid.NAMESPACE_URL, "x86sim:" + (summary.get("phone") or ""))).upper()
    name = summary.get("phone") or model or "x86Sim"
    return {"platform": pid, "pdesc": pdesc, "sdk": sdk + ver, "sdkname": "Simulator - %s %s" % (osname, ver),
            "version": ver, "build": build, "arch": arch, "model": model or name, "code": code, "udid": udid,
            "name": name}


def device_record(di):
    return O("ActionDeviceRecord", name=S(di["name"]), isConcreteDevice=B(True), operatingSystemVersion=S(di["version"]),
             operatingSystemVersionWithBuildNumber=S("%s (%s)" % (di["version"], di["build"]) if di["build"] else di["version"]),
             nativeArchitecture=S(di["arch"]), modelName=S(di["model"]), modelCode=S(di["code"]), modelUTI=S(""),
             identifier=S(di["udid"]), isWireless=B(False), cpuKind=S(""), cpuCount=I(0), cpuSpeedInMHz=I(0),
             busSpeedInMHz=I(0), ramSizeInMegabytes=I(0), physicalCPUCoresPerPackage=I(0),
             logicalCPUCoresPerPackage=I(0), platformRecord=O("ActionPlatformRecord", identifier=S(di["platform"]),
                                                               userDescription=S(di["pdesc"])))


def host_record():
    """This Linux host, as the run's local computer (Xcode: the Mac)."""
    u = platform.uname()
    ram = 0
    try:
        ram = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") // (1024 * 1024)
    except (ValueError, OSError):
        pass
    ncpu = os.cpu_count() or 1
    # operatingSystemVersion stays empty: xcresulttool's summary says "<scheme> · Built with macOS <it>" for any
    # non-empty one (whatever the platform record says), just "<scheme>" without
    return O("ActionDeviceRecord", name=S("x86Sim host"), isConcreteDevice=B(True), operatingSystemVersion=S(""),
             operatingSystemVersionWithBuildNumber=S("Linux %s" % u.release), nativeArchitecture=S(u.machine),
             modelName=S("Linux"), modelCode=S("Linux"), modelUTI=S(""), identifier=S(""), isWireless=B(False),
             cpuKind=S(u.processor or u.machine), cpuCount=I(1), cpuSpeedInMHz=I(0), busSpeedInMHz=I(0),
             ramSizeInMegabytes=I(ram), physicalCPUCoresPerPackage=I(ncpu), logicalCPUCoresPerPackage=I(ncpu),
             platformRecord=O("ActionPlatformRecord", identifier=S("org.x86sim.platform.linux"), userDescription=S("Linux")))


R_SKIP = re.compile(r"^(.*?):(\d+): -\[(\S+) (\S+)\] : Test skipped")


def skip_locations(log):
    out = {}
    try:
        with open(log, errors="replace") as f:
            for ln in f:
                m = R_SKIP.match(ln)
                if m:
                    out[(m.group(3), m.group(4))] = (m.group(1), int(m.group(2)))
    except (OSError, TypeError):
        pass
    return out


def is_swift(t):
    return "." in (t.get("class") or "") or t.get("class") == "SwiftTesting"


def test_name(t):
    n = t["name"]
    return n if "(" in n or not is_swift(t) else n + "()"


def url_part(s):
    return urllib.parse.quote(s, safe="()-._~!$&'*+,;=:@")


def issue_type(msg):
    if msg.startswith(("failed: caught error", "Caught error: ")):
        return "Thrown Error"
    if re.match(r"(Expected failure( '.*')? but none recorded|Known issue was not recorded)", msg, re.S):
        return "Unmatched Expected Failure"
    # XCTFail("why"): "failed - why" (Apple's: Assertion Failure)
    if msg.startswith(("XCT", "failed - ")) or msg == "failed" or " failed" in msg.split("\n")[0][:60]:
        return "Assertion Failure"
    return "Uncategorized"


def local_ms(ts):
    return time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(ts)) + ".%03d" % int((ts % 1) * 1000)


def uid(*parts):
    return str(uuid.uuid5(uuid.NAMESPACE_URL, "x86sim-xcresult:" + "\x00".join(str(p) for p in parts))).upper()


# ---------------------------------------------------------------------------------------------------------------------
# code coverage: the report (xccov view --report), from the run's profile and products

class KObj:
    def __init__(self, classes, **fields):
        self.classes, self.fields = classes, fields


def keyed_archive(root):
    """NSKeyedArchiver's binary plist for KObj / str / int / list (NSArray; ("mutable", list): NSMutableArray) /
    bytes (NSMutableData)."""
    objs, classes = ["$null"], {}

    def cls(chain):
        if chain not in classes:
            objs.append({"$classes": list(chain), "$classname": chain[0]})
            classes[chain] = plistlib.UID(len(objs) - 1)
        return classes[chain]

    def enc(v):
        if isinstance(v, (bool, int, float)):
            return v
        i = len(objs)
        objs.append(None)
        if isinstance(v, str):
            objs[i] = v
        elif isinstance(v, bytes):
            objs[i] = {"NS.data": v, "$class": cls(("NSMutableData", "NSData", "NSObject"))}
        elif isinstance(v, tuple) and v and v[0] == "mutable":
            objs[i] = {"NS.objects": [enc(x) for x in v[1]], "$class": cls(("NSMutableArray", "NSArray", "NSObject"))}
        elif isinstance(v, list):
            objs[i] = {"NS.objects": [enc(x) for x in v], "$class": cls(("NSArray", "NSObject"))}
        elif isinstance(v, KObj):
            d = {k: enc(x) for k, x in v.fields.items()}
            d["$class"] = cls(tuple(v.classes))
            objs[i] = d
        else:
            raise TypeError(type(v))
        return plistlib.UID(i)

    top = enc(root)
    return plistlib.dumps({"$archiver": "NSKeyedArchiver", "$objects": objs, "$top": {"root": top}, "$version": 100000},
                          fmt=plistlib.FMT_BINARY, sort_keys=False)


def toolchain_tool(name):
    """llvm-cov / swift-demangle of the Swift toolchain (as xctest.py's llvm_tool): PATH's swift, sdks/, PATH."""
    cands = []
    sw = shutil.which("swift")
    if sw:
        cands.append(os.path.join(os.path.dirname(os.path.realpath(sw)), name))
    root = os.path.dirname(os.path.dirname(LIB))
    sdks = os.path.join(root, "sdks")
    if os.path.isdir(sdks):
        cands += [os.path.join(sdks, d, "usr", "bin", name) for d in sorted(os.listdir(sdks), reverse=True)
                  if d.startswith("swift-")]
    cands.append(shutil.which(name) or "")
    return next((c for c in cands if c and os.access(c, os.X_OK)), None)


def demangle(names):
    tool = toolchain_tool("swift-demangle")
    if not tool or not names:
        return {n: n for n in names}
    r = subprocess.run([tool, "--simplified"], input="\n".join(names) + "\n", capture_output=True, text=True)
    out = r.stdout.split("\n")
    return {n: (out[i] if i < len(out) and out[i] else n) for i, n in enumerate(names)}


FUNC_ENTRY = "<IIIIQ"   # per function: covered lines, executable lines, first line, lines spanned, execution count


def functions_blob(funcs):
    """XCTHCodeCoverageFile.functions: u32 count, the entries (FUNC_ENTRY), then the names, each NUL-terminated."""
    b = struct.pack("<I", len(funcs))
    for f in funcs:
        b += struct.pack(FUNC_ENTRY, f["covered"], f["executable"], f["line"], f["span"], f["count"])
    return b + b"".join(f["name"].encode("utf-8") + b"\0" for f in funcs)


def subranges(da, segments):
    """The column ranges xccov shows inside a line, from llvm-cov's segments [line, col, count, hasCount, ...]: per
    line, the stretches between segments (from the count in force at column 1) whose count is not the line's (any
    counted stretch of a line that is not executable itself); length 0 = to the end of the line."""
    out, active, by_line = [], None, {}
    for sg in segments:
        by_line.setdefault(sg[0], []).append(sg)
    last = max(list(da) + list(by_line) or [0])
    for n in range(1, last + 1):
        segs = by_line.get(n, [])
        if not segs and active is None:
            continue
        cols = [(1, active)] + [(sg[1], sg[2] if sg[3] else None) for sg in segs]
        for i, (col, cnt) in enumerate(cols):
            end = cols[i + 1][0] if i + 1 < len(cols) else None
            if end is not None and end <= col:
                continue
            if cnt is not None and (n not in da or cnt != da[n]):
                out.append((n, col, 0 if end is None else end - col, cnt))
        if segs:
            active = cols[-1][1]
    return out


def line_runs(da, subs=()):
    """One source file's part of the archive's Coverage file: its lines 1..last as runs, then the subranges."""
    runs = []
    for n in range(1, max(da) + 1 if da else 1):
        key = (n in da, da.get(n, 0))
        if runs and runs[-1][0] == key and runs[-1][1] < 0x8000:
            runs[-1][1] += 1
        else:
            runs.append([key, 1])
    b = struct.pack("<I", len(runs))
    for (ex, cnt), ln in runs:
        b += struct.pack("<Q", (1 << 63 if ex else 0) | (ln - 1) << 48 | min(cnt, (1 << 48) - 1))
    b += struct.pack("<I", len(subs))
    return b + b"".join(struct.pack("<IIIQ", n, col, ln, cnt) for n, col, ln, cnt in subs)


def coverage_archive(per_file, segs):
    """action.xccovarchive's Coverage and Index files from {path: {line: count}} (executable lines only) and
    {path: llvm-cov segments}."""
    paths = sorted(per_file)
    cov, offs = b"", []
    for p in paths:
        offs.append(len(cov))
        cov += line_runs(per_file[p], subranges(per_file[p], segs.get(p) or []))
    names = [p.encode("utf-8") for p in paths]
    head = 4 + 16 * len(paths)
    idx, at = struct.pack("<I", len(paths)), head
    for n, o in zip(names, offs):
        idx += struct.pack("<QQ", at, o)
        at += 2 + len(n)
    idx += b"".join(struct.pack("<H", len(n)) + n for n in names)
    return cov, idx


def coverage_report(cov):
    """(report bytes, covered, executable) from results.json's "coverage" (profdata + products' binaries), with
    llvm-cov export: per product, file and function, as xccov reports them. None without the tools or data."""
    pd = (cov or {}).get("profdata")
    tool = toolchain_tool("llvm-cov")
    if not pd or not os.path.isfile(pd) or not tool:
        return None
    targets, tot_c, tot_n, per_file, segs = [], 0, 0, {}, {}
    for prod in cov.get("products") or []:
        bins = [b for b in prod.get("binaries") or [] if os.path.isfile(b)]
        if not bins:
            continue
        objs = [x for b in bins for x in ("-object", b)][1:]
        r = subprocess.run([tool, "export", "-format=text", "-skip-expansions", "-instr-profile", pd] + objs,
                           capture_output=True, text=True)
        try:
            data = json.loads(r.stdout)["data"][0]
        except (ValueError, KeyError, IndexError):
            continue
        r = subprocess.run([tool, "export", "-format=lcov", "-instr-profile", pd] + objs, capture_output=True, text=True)
        lines, cur = {}, None
        for ln in r.stdout.splitlines():
            if ln.startswith("SF:"):
                cur = lines.setdefault(ln[3:], {})
            elif ln.startswith("DA:") and cur is not None:
                n, c = ln[3:].split(",")[:2]
                cur[int(n)] = max(cur.get(int(n), 0), int(c))
        names = demangle(sorted({f["name"] for f in data.get("functions", [])}))
        files, pc, pn = [], 0, 0
        for f in sorted(data.get("files", []), key=lambda f: f["filename"]):
            da = lines.get(f["filename"], {})
            segs.setdefault(f["filename"], f.get("segments") or [])
            merged = per_file.setdefault(f["filename"], {})
            for n, c in da.items():
                merged[n] = merged.get(n, 0) + c
            funcs = []
            for fn in data.get("functions", []):
                if f["filename"] not in fn.get("filenames", [])[:1] or not fn.get("regions"):
                    continue
                regs = [x for x in fn["regions"] if x[5] == 0]
                first, last = min(x[0] for x in regs), max(x[2] for x in regs)
                ex = [n for n in da if first <= n <= last]
                funcs.append({"name": names.get(fn["name"], fn["name"]), "line": first, "span": last - first + 1,
                              "count": fn.get("count", 0), "executable": len(ex),
                              "covered": sum(1 for n in ex if da[n] > 0)})
            funcs.sort(key=lambda x: x["line"])   # stable: llvm-cov's order within a line, as xccov's
            ln = f["summary"]["lines"]
            files.append(KObj(("XCTHCodeCoverageFile", "XCTHCoverageDataContainer", "NSObject"),
                              coveredLines=ln["covered"], functions=functions_blob(funcs), documentLocation=f["filename"],
                              name=os.path.basename(f["filename"]), executableLines=ln["count"]))
            pc, pn = pc + ln["covered"], pn + ln["count"]
        main = bins[0][:-len(".debug.dylib")] if bins[0].endswith(".debug.dylib") else bins[0]   # the app's executable
        targets.append(KObj(("XCTHCodeCoverageTarget", "XCTHCoverageDataContainer", "NSObject"),
                            productPath=main, coveredLines=pc, buildableIdentifier="%s:primary" % prod["name"],
                            sourceFiles=("mutable", files), name=prod["name"], executableLines=pn))
        tot_c, tot_n = tot_c + pc, tot_n + pn
    if not targets:
        return None
    root = KObj(("XCTHCodeCoverage", "XCTHCoverageDataContainer", "NSObject"), coveredLines=tot_c, creationErrors=[],
                name=uid("coverage", pd), executableLines=tot_n, buildableCoverageObjects=targets)
    return keyed_archive(root), tot_c, tot_n, coverage_archive(per_file, segs)


# ---------------------------------------------------------------------------------------------------------------------
# the graph

def call_frame(x):
    """A call stack frame (results.json's {"address", "image", "symbol", "file", "line"}): SourceCodeFrame, with
    symbolInfo for a frame symbolicated to a source line (Xcode shows those as the issue's "Source Code Reference"s)"""
    sym = None
    if x.get("symbol") is not None:
        sym = O("SourceCodeSymbolInfo", imageName=S(x.get("image")), symbolName=S(x["symbol"]),
                location=O("SourceCodeLocation", filePath=S(x.get("file")), lineNumber=I(x.get("line")))
                if x.get("file") else None)
    return O("SourceCodeFrame", addressString=S(None if x.get("address") is None else str(x["address"])), symbolInfo=sym)


def test_value(v):
    if not v:
        return None
    return O("TestValue", description=S(v.get("description") or ""), debugDescription=S(v.get("debug")),
             typeName=S(v.get("type")), fullyQualifiedTypeName=S(v.get("full_type")),
             isCollection=B(bool(v.get("is_collection"))), label=S(v.get("label")),
             children=None if v.get("children") is None else [test_value(c) for c in v["children"]])


def expression(x):
    """A failed #expect / #require's expression (results.json's {"source", "value", "subexpressions"}): TestExpression,
    which test-details shows as the issue's "Results" (each subexpression "<source> : <value>")"""
    if not x:
        return None
    return O("TestExpression", sourceCode=S(x.get("source") or ""), value=test_value(x.get("value")),
             subexpressions=[expression(c) for c in x.get("subexpressions") or []])


class Writer:
    def __init__(self, summary, bundle, products_root):
        self.s = summary
        self.st = Store(bundle)
        info = xctestrun_info(summary, products_root)
        self.project = summary.get("project") or info.get("project") or guess_project(summary.get("targets") or [])
        self.scheme = summary.get("scheme") or info.get("scheme") or self.project
        self.plan = summary.get("test_plan") or info.get("plan") or self.scheme
        self.source = summary.get("source_root") or info.get("source")
        self.di = device_info(summary)
        self.t0 = float(summary["started_unix"]) if summary.get("started_unix") else parse_time(summary.get("started"))
        self.issues = []

    def url(self, *parts):
        return "test://com.apple.xcode/" + "/".join(url_part(p) for p in (self.project,) + parts)

    def attachment(self, a, start, end, lifetime, slot=None):
        """(ActionTestAttachment, its time): the file's mtime (when the test added it), kept inside the test's run.
        slot: (its index in its activity, the activity's uuid), for its inActivityIdentifier and filename as Xcode
        writes them ("<name>_<index>_<activity uuid>.<ext>", the extension the name's own after a "." in it, an element
        snapshot's "Snapshot_<index>_<activity uuid>")."""
        path = a.get("path")
        if not path or not os.path.isfile(path):
            return None
        oid, size = self.st.file(path)
        name = a.get("name") or os.path.basename(path)
        info = None
        lt = a.get("lifetime")
        if lt in (0, 1, "0", "1"):
            lifetime = "keepAlways" if str(lt) == "0" else "deleteOnSuccess"
        elif isinstance(lt, str) and lt:
            lifetime = lt
        if a.get("from") == "x86sim" and (a.get("type") or "").startswith("public.mpeg-4"):
            name = "kXCTAttachmentScreenRecording"   # xctest.py's recording of the test: XCTest's own name for it
            when = start
        else:
            try:   # XCTAttachment.timestamp (x86sim test's "time"), else when its file was written
                when = min(max(float(a.get("time") or os.path.getmtime(path)), start), end)
            except (OSError, ValueError):
                when = end
        u = uid("attachment", path)
        ext = os.path.splitext(path)[1]
        idx, au = slot or (0, u)
        if a.get("type") == "com.apple.dt.xctest.element-snapshot":
            fn = "Snapshot_%d_%s" % (idx, au)
        elif slot and "." in name:
            stem, _, sfx = name.rpartition(".")
            fn = "%s_%d_%s.%s" % (stem, idx, au, sfx)
        else:
            fn = "%s_%d_%s%s" % (name, idx, au, ext)
        return O("ActionTestAttachment", uniformTypeIdentifier=S(a.get("type") or "public.data"), name=S(name),
                 uuid=S(u), timestamp=T(when), userInfo=info, lifetime=S(lifetime), inActivityIdentifier=I(idx),
                 filename=S(fn), payloadRef=Ref(oid), payloadSize=I(size)), when

    @staticmethod
    def activity(a, att, when, start, key):
        """The activity an attachment is shown in."""
        label = a.get("name") or os.path.basename(a["path"])
        if att.f["name"].v == "kXCTAttachmentScreenRecording":
            title, kind = "Start Test at %s" % local_ms(start), "internal"
        elif a.get("from") == "test":   # XCTestCase.add(_:)
            title, kind = "Added attachment named '%s'" % label, "attachmentContainer"
        else:   # an XCTest activity's (automatic screenshots, debug descriptions): its title is not known here
            title, kind = label, "internal"
        return O("ActionTestActivitySummary", title=S(title), activityType=S("com.apple.dt.xctest.activity-type." + kind),
                 uuid=S(uid("activity", *key)), start=T(when), finish=T(when), attachments=[att], subactivities=[],
                 failureSummaryIDs=[], expectedFailureIDs=[], warningSummaryIDs=[])

    def activity_tree(self, nodes, atts, start, end, lifetime):
        """XCTest's activities (results.json's test "activities": {"uuid", "title", "type", "start", "finish",
        "children"}) as Xcode keeps them, each with the attachments added to it (atts: {activity uuid: [attachment]})"""
        out = []
        for n in nodes:
            mine = []
            for i, a in enumerate(atts.get(n["uuid"]) or []):
                r = self.attachment(a, start, end, lifetime, (i, n["uuid"]))
                if r:
                    mine.append(r[0])
            out.append(O("ActionTestActivitySummary", title=S(n.get("title") or ""),
                         activityType=S(n.get("type") or "com.apple.dt.xctest.activity-type.internal"),
                         uuid=S(n["uuid"]), start=T(n.get("start")), finish=T(n.get("finish") or n.get("start")),
                         attachments=mine, subactivities=self.activity_tree(n.get("children") or [], atts, start, end, lifetime),
                         failureSummaryIDs=[], expectedFailureIDs=[], warningSummaryIDs=[]))
        return out

    def test_summary(self, target, t, start, skips, arg=None):
        """A test's ActionTestMetadata (+ its summary). Swift Testing's (x86sim test's "swift_testing" entries) are
        identified by their suite path (Second/Inner/same()), as Xcode does; arg: one test case of a parameterized one
        (Xcode keeps each: the same identifier, ?args=<argument ID hash> in its URL, its arguments)."""
        swift = bool(t.get("swift_testing"))
        if arg:
            t = dict(t, result=arg.get("result"), duration=arg.get("duration"), failures=arg.get("failures") or [],
                     expected_failures=arg.get("expected_failures") or [])
        dur = float(t.get("duration") or 0.0)
        name = test_name(t)
        cls = (t.get("class") or "").split(".")[-1]
        ident = t["identifier"] if swift else "%s/%s" % (cls, name)
        symbol = ".".join(list(t.get("suite") or []) + [name]) if swift else "%s.%s" % (cls, name)
        url = self.url(target["name"], *ident.split("/")) if swift else self.url(target["name"], cls, t["name"])
        if arg:
            url += "?args=" + arg["id"]
        key = ident + ("?" + arg["id"] if arg else "")
        status = {"passed": "Success", "failed": "Failure", "skipped": "Skipped",
                  "expected_failure": "Expected Failure"}.get(t.get("result"), "Failure")
        lifetime = "deleteOnSuccess" if status == "Failure" else "keepAlways"
        fails, issue_atts, acts = [], [], []
        # XCTest's activity tree (libsimshim's): each attachment in its activity, the screen recording (x86sim's) in
        # "Start Test at ...", as XCTest's own recording is; the others as before
        tree = [] if arg else t.get("activities") or []
        uuids, placed, stack = set(), {}, list(tree)
        while stack:
            n = stack.pop()
            uuids.add(n["uuid"])
            stack += n.get("children") or []
        first = tree[0]["uuid"] if tree and tree[0].get("title", "").startswith("Start Test at ") else None
        rest = []
        for a in t.get("attachments") or []:
            u = a.get("activity") if a.get("activity") in uuids else \
                first if a.get("from") == "x86sim" and (a.get("type") or "").startswith("public.mpeg-4") else None
            if u:
                placed.setdefault(u, []).append(a)
            else:
                rest.append(a)
        for i, a in enumerate(rest):
            r = self.attachment(a, start, start + dur, lifetime)
            if not r:
                continue
            att, when = r
            if a.get("from") == "issue" and t.get("failures"):
                issue_atts.append(att)   # the failure's own (its screenshot), shown with the failure
                continue
            acts.append((when, self.activity(a, att, when, start, (target["name"], key, i))))
        if tree:
            for n, o in zip(tree, self.activity_tree(tree, placed, start, start + dur, lifetime)):
                acts.append((n.get("start") or start, o))
        acts = [x for _, x in sorted(acts, key=lambda p: p[0])]
        def failure(f, u, atts, top):
            msg = f.get("message") or ""
            loc = O("SourceCodeLocation", filePath=S(f.get("file")), lineNumber=I(f.get("line"))) if f.get("file") else None
            if "stack" in f:   # the issue's call stack (x86sim test symbolicated it: its frames in the test bundle)
                stack = [call_frame(x) for x in f["stack"]]
            else:   # the test method's frame, as Xcode symbolicates it (its "Source Code Reference")
                stack = [O("SourceCodeFrame", addressString=S("0"), symbolInfo=O(
                    "SourceCodeSymbolInfo", imageName=S(target["name"]), symbolName=S(symbol), location=loc))] if loc else []
            return O("ActionTestFailureSummary", message=S(msg), fileName=S(f.get("file") or ""),
                     lineNumber=I(f.get("line") or 0), isPerformanceFailure=B(False), uuid=S(u),
                     issueType=S(issue_type(msg)), attachments=atts,
                     sourceCodeContext=O("SourceCodeContext", location=loc, callStack=stack),
                     timestamp=T(start + dur), isTopLevelFailure=B(top), expression=expression(f.get("expression")))
        expected = [O("ActionTestExpectedFailure", failureReason=S(f.get("reason") or None),
                      failureSummary=failure(f, uid("expected-issue", target["name"], key, i), [], False),
                      isTopLevelFailure=B(True), uuid=S(uid("expected", target["name"], key, i)))
                    for i, f in enumerate(t.get("expected_failures") or [])]
        for i, f in enumerate(t.get("failures") or []):
            msg = f.get("message") or ""
            fails.append(failure(f, uid("failure", target["name"], key, i), issue_atts if i == 0 else [], True))
            doc = None
            if f.get("file"):
                ln = max(0, int(f.get("line") or 1) - 1)
                doc = O("DocumentLocation", concreteTypeName=S("DVTTextDocumentLocation"),
                        url=S("file://%s#EndingLineNumber=%d&StartingLineNumber=%d" % (urllib.parse.quote(f["file"]), ln, ln)))
            self.issues.append(O("TestFailureIssueSummary", issueType=S("Uncategorized"), message=S(msg),
                                 producingTarget=S(target["name"]), documentLocationInCreatingWorkspace=doc,
                                 testCaseName=S(symbol)))
        skip = None
        if status == "Skipped":
            m = t.get("skip_message")
            fl = skips.get((t.get("class"), t["name"]), ("", 0))
            if swift:   # Swift Testing's skip (.disabled("why")): Apple's text, at the trait
                fl = ((t.get("skip_file") or ""), int(t.get("skip_line") or 0))
            skip = O("ActionTestNoticeSummary", message=S(("Test skipped: %s" if swift else "Test skipped - %s") % m if m else "Test skipped"),
                     fileName=S(fl[0]), lineNumber=I(fl[1]), timestamp=T(start + dur))
        args = [O("TestArgument", description=S(""),
                  parameter=O("TestParameter", label=S(a.get("label")), typeName=S(a.get("type")),
                              fullyQualifiedTypeName=S(a.get("full_type"))),
                  value=O("TestValue", description=S(a.get("value")), debugDescription=S(a.get("debug")),
                          typeName=S(a.get("value_type")), fullyQualifiedTypeName=S(a.get("value_full_type")),
                          isCollection=B(bool(a.get("is_collection")))))
                for a in (arg or {}).get("args") or []]
        summ = O("ActionTestSummary", name=S(name), identifier=S(ident), identifierURL=S(url),
                 testStatus=S(status), duration=D(dur), performanceMetrics=[], failureSummaries=fails, expectedFailures=expected,
                 skipNoticeSummary=skip, activitySummaries=acts, arguments=args, warningSummaries=[], documentation=[],
                 trackedIssues=[], tags=[])
        ref = self.st.obj(summ)
        return O("ActionTestMetadata", name=S(name), identifier=S(ident), identifierURL=S(url),
                 testStatus=S(status), duration=D(dur), summaryRef=Ref(ref, "ActionTestSummary"),
                 performanceMetricsCount=I(0), failureSummariesCount=I(len(fails)), activitySummariesCount=I(len(acts))), dur

    def group(self, target, ident, name, subtests, dur):
        return O("ActionTestSummaryGroup", name=S(name), identifier=S(ident), identifierURL=S(self.url(target, *ident.split("/"))),
                 duration=D(dur), subtests=subtests, failureSummaries=[], warningSummaries=[], expectedFailures=[],
                 activitySummaries=[], documentation=[], trackedIssues=[], tags=[])

    def testable(self, target, start):
        skips = skip_locations(target.get("log"))
        classes, order = {}, []
        tests = target.get("tests") or []
        # each test where it really started (x86sim test's "started": libsimshim's / libsimswt's wall clock); without
        # that, the tests run after the process is up and end when the target's run ends
        clock = max(start, start + float(target.get("duration") or 0) - sum(float(t.get("duration") or 0) for t in tests))

        def when(t):
            return max(float(t["started"]), start) if t.get("started") else clock
        # Swift Testing's tests: beside "All tests", in their suites' groups (nested, as Xcode keeps them)
        swift = {"tests": [], "suites": {}, "order": []}
        for t in tests:
            if not t.get("swift_testing"):
                continue
            node = swift
            for i, s in enumerate(t.get("suite") or []):
                if s not in node["suites"]:
                    node["suites"][s] = {"tests": [], "suites": {}, "order": [], "ident": "/".join(t["suite"][:i + 1])}
                    node["order"].append(s)
                node = node["suites"][s]
            for a in t.get("arguments") or [None]:
                at = when(a or t)
                md, dur = self.test_summary(target, t, at, skips, a)
                clock = at + dur
                node["tests"].append((md, dur))

        def swift_nodes(node):
            out, total = [], 0.0
            for s in node["order"]:
                sub, d = swift_nodes(node["suites"][s])
                out.append(self.group(target["name"], node["suites"][s]["ident"], s, sub, d))
                total += d
            out += [m for m, _ in node["tests"]]
            return out, total + sum(d for _, d in node["tests"])
        for t in tests:
            if t.get("swift_testing"):
                continue
            cls = (t.get("class") or "").split(".")[-1]
            if cls not in classes:
                classes[cls] = []
                order.append(cls)
            at = when(t)
            md, dur = self.test_summary(target, t, at, skips)
            clock = at + dur
            classes[cls].append((md, dur))
        groups, total = [], 0.0
        for cls in order:
            dur = sum(d for _, d in classes[cls])
            total += dur
            groups.append(self.group(target["name"], cls, cls, [m for m, _ in classes[cls]], dur))
        bundle = self.group(target["name"], target["name"] + ".xctest", target["name"] + ".xctest", groups, total)
        swift_tests, _ = swift_nodes(swift)
        # attachments added outside a test (class setUp): the bundle's own activities
        end = start + float(target.get("duration") or 0)
        for i, a in enumerate(target.get("attachments") or []):
            r = self.attachment(a, start, end, "keepAlways")
            if r:
                bundle.f["activitySummaries"].append(self.activity(a, r[0], r[1], start, (target["name"], "_run", i)))
        # Xcode: a target without XCTest tests has an empty "All tests"
        top = self.group(target["name"], "All tests", "All tests",
                         [bundle] if groups or bundle.f["activitySummaries"] or not swift_tests else [], total)
        return O("ActionTestableSummary", name=S(target["name"]), identifierURL=S(self.url(target["name"])),
                 projectRelativePath=S(self.project + ".xcodeproj"), targetName=S(target["name"]),
                 testKind=S(KINDS.get(target.get("kind"), "app hosted")), tests=[top] + swift_tests, diagnosticsDirectoryName=S(""),
                 failureSummaries=[], testLanguage=S(""), testRegion=S(""))

    def build_log(self, log):
        """The build's ActivityLogSection (log: `xcresulttool get log --type build`'s shape, which the xcodebuild
        stand-in writes from Swift Build's tasks): its id"""
        def message(m):
            loc = None
            if (m.get("location") or {}).get("path"):
                lc = m["location"]
                loc = O("DocumentLocation", concreteTypeName=S("DVTTextDocumentLocation"),
                        url=S(text_location(lc["path"], lc.get("line"), lc.get("column"))))
            return O("ActivityLogMessage", type=S(m.get("type") or "notice"), title=S(m.get("title")),
                     shortTitle=S(m.get("shortTitle")), category=S(m.get("category")), location=loc,
                     annotations=[O("ActivityLogMessageAnnotation", title=S(a.get("title")))
                                  for a in m.get("annotations") or []])

        def section(n, top=False):
            f = dict(domainType=S(n.get("domainType")), title=S(n.get("title")), startTime=T(n.get("startTime")),
                     duration=D(n.get("duration") or 0), result=S(n.get("result")),
                     subsections=[section(x) for x in n.get("subsections") or []],
                     messages=[message(m) for m in n.get("messages") or []], attachments=[])
            c = n.get("commandInvocationDetails")
            if c and not top:
                f.update(commandDetails=S(c.get("commandDetails")), emittedOutput=S(c.get("emittedOutput") or ""),
                         exitCode=I(c.get("exitCode") or 0))
                if n.get("location"):
                    f["location"] = O("DocumentLocation", concreteTypeName=S("DVTDocumentLocation"),
                                      url=S("file://" + urllib.parse.quote(n["location"])))
                return O("ActivityLogCommandInvocationSection", **f)
            return O("ActivityLogSection", **f)

        return self.st.obj(section(log, True))

    def log_section(self, target, start):
        """The target's part of the action log: its process's output, as Xcode keeps it."""
        try:
            with open(target.get("log") or "", errors="replace") as f:
                out = f.read()
        except OSError:
            out = ""
        res = "succeeded" if target.get("result") == "passed" else "failed"
        tests = target.get("tests") or []
        passed = sum(1 for t in tests if t.get("result") == "passed")
        worker = O("ActivityLogUnitTestSection", domainType=S("com.apple.dt.IDE.UnitTestLogSection.Worker"),
                   title=S("%s (%s)" % (target["name"], target.get("pid") or 0)), startTime=T(start),
                   duration=D(target.get("duration") or 0), result=S(res), subsections=[], messages=[], attachments=[],
                   emittedOutput=S(out), testsPassedString=S("%d test%s total, %d passed" % (len(tests), "" if len(tests) == 1 else "s", passed)),
                   wasSkipped=B(False))
        return O("ActivityLogUnitTestSection", domainType=S("com.apple.dt.unit.cocoaUnitTest"),
                 title=S("Test target %s" % target["name"]), startTime=T(start), duration=D(target.get("duration") or 0),
                 result=S(res), subsections=[worker], messages=[], attachments=[], wasSkipped=B(False))

    def diagnostics(self):
        ents = []
        for t in self.s.get("targets") or []:
            if t.get("log") and os.path.isfile(t["log"]):
                oid, _ = self.st.file(t["log"])
                d = self.st.directory([("StandardOutputAndStandardError.txt", ("file", oid))])
                ents.append((t["name"], ("dir", d)))
        return self.st.directory(ents) if ents else None

    def write(self):
        s, di = self.s, self.di
        targets = s.get("targets") or []
        clock, testables, logs = self.t0, [], []
        for t in targets:
            at = max(float(t["started"]), self.t0) if t.get("started") else clock   # its real start (x86sim test's)
            testables.append(self.testable(t, at))
            logs.append(self.log_section(t, at))
            clock = at + float(t.get("duration") or 0)
        end = self.t0 + float(s.get("duration") or (clock - self.t0))
        tests_ref = self.st.obj(O("ActionTestPlanRunSummaries", summaries=[
            O("ActionTestPlanRunSummary", name=S(RUN_NAME), testableSummaries=testables)]))
        failed = s.get("result") != "passed"
        log_ref = self.st.obj(O("ActivityLogSection", domainType=S("com.apple.dt.unit.cocoaUnitTest"),
                                title=S("Test %s" % self.scheme), startTime=T(self.t0), duration=D(end - self.t0),
                                result=S("failed" if failed else "succeeded"), subsections=logs, messages=[], attachments=[]))
        diag = self.diagnostics()
        all_tests = [x for t in targets for x in t.get("tests") or []]
        c = s.get("counts") or {}
        n = c.get("total", len(all_tests))
        nf = c.get("failed", sum(1 for x in all_tests if x.get("result") == "failed"))
        ns = c.get("skipped", sum(1 for x in all_tests if x.get("result") == "skipped"))

        # the build's errors and warnings (the xcodebuild stand-in's build log: x86sim test --build-log), and Xcode's
        # own error when the build failed: the test action is cancelled
        b_err = [build_issue(i) for i in s.get("build_issues") or [] if i.get("severity") == "error"]
        b_warn = [build_issue(i) for i in s.get("build_issues") or [] if i.get("severity") == "warning"]
        built_failed = s.get("build") == "failed"
        a_err = [O("IssueSummary", issueType=S("Uncategorized"), message=S("Testing cancelled because the build failed."))] \
            if built_failed else []

        def metrics(tests=True, errors=0, warnings=0):
            return O("ResultMetrics", analyzerWarningCount=I(0), errorCount=I(errors), warningCount=I(warnings),
                     testsCount=I(n if tests else 0), testsFailedCount=I(nf if tests else 0),
                     testsSkippedCount=I(ns if tests else 0))

        def issues(fails, errors=(), warnings=()):
            return O("ResultIssueSummaries", analyzerWarningSummaries=[], errorSummaries=list(errors),
                     testFailureSummaries=fails, warningSummaries=list(warnings), testWarningSummaries=[])

        nocov = O("CodeCoverageInfo", hasCoverageData=B(False))
        cov, pct = nocov, None
        rep = coverage_report(s.get("coverage")) if s.get("coverage") else None
        if rep:
            covfile, index = rep[3]
            arch = self.st.directory([("Coverage", ("file", self.st.put(covfile))), ("Index", ("file", self.st.put(index))),
                                      ("Metadata.plist", ("file", self.st.put(plistlib.dumps({"formatVersion": 1}))))])
            cov = O("CodeCoverageInfo", hasCoverageData=B(True), reportRef=Ref(self.st.put(rep[0])), archiveRef=Ref(arch))
            pct = rep[1] / rep[2] if rep[2] else 0.0
        self.coverage_fraction = pct
        action = O("ActionResult", resultName=S("action"),
                   status=S("cancelled" if built_failed else "failed" if failed else "succeeded"),
                   metrics=metrics(errors=len(a_err)),
                   issues=issues(self.issues, a_err), coverage=cov, logRef=Ref(log_ref, "ActivityLogSection"),
                   testsRef=Ref(tests_ref, "ActionTestPlanRunSummaries"), diagnosticsRef=Ref(diag) if diag else None)
        # Xcode's build action: "notRequested" for test-without-building (x86sim test), "succeeded" / "failed" when the
        # run built the products first (the xcodebuild stand-in's test: x86sim test --built / --build-log), with the
        # build's activity log when there is one
        blog = self.build_log(s.get("build_log")) if s.get("build_log") else None
        build = O("ActionResult", resultName=S("build"), status=S(s.get("build") or "notRequested"),
                  metrics=metrics(False, len(b_err), len(b_warn)), issues=issues([], b_err, b_warn), coverage=nocov,
                  logRef=Ref(blog, "ActivityLogSection") if blog else None)
        dest = O("ActionRunDestinationRecord", displayName=S(di["name"]), targetArchitecture=S(di["arch"]),
                 targetDeviceRecord=device_record(di), localComputerRecord=host_record(),
                 targetSDKRecord=O("ActionSDKRecord", name=S(di["sdkname"]), identifier=S(di["sdk"]),
                                   operatingSystemVersion=S(di["version"]), isInternal=B(False)))
        rec = O("ActionRecord", schemeCommandName=S("Test"), schemeTaskName=S("Action"),
                # test-without-building's action has no title (Xcode 27.2), xcodebuild test's names the project
                title=S("Testing project %s with scheme %s" % (self.project, self.scheme)) if s.get("build") else None,
                startedTime=T(self.t0),
                endedTime=T(end), runDestination=dest, buildResult=build, actionResult=action, testPlanName=S(self.plan))
        ws = os.path.join(self.source, self.project + ".xcodeproj") if self.source else self.project + ".xcodeproj"
        meta = self.st.obj(O("ActionsInvocationMetadata", creatingWorkspaceFilePath=S(ws),
                             uniqueIdentifier=S(uid("run", s.get("out"), s.get("started"))),
                             schemeIdentifier=O("EntityIdentifier", entityName=S(self.scheme),
                                                containerName=S("%s project" % self.project), entityType=S("scheme"),
                                                sharedState=S("shared"))))
        top = metrics(errors=len(b_err) + len(a_err), warnings=len(b_warn))
        top.f["totalCoveragePercentage"] = D(pct)
        root = self.st.obj(O("ActionsInvocationRecord", metadataRef=Ref(meta, "ActionsInvocationMetadata"),
                             metrics=top, issues=issues(self.issues, b_err + a_err, b_warn), actions=[rec]))
        return root


def text_location(path, line=None, column=None, when=None):
    """A DVTTextDocumentLocation URL as Xcode writes one for a build issue (0-based line and column)"""
    frag = []
    if line:
        ln = max(0, int(line) - 1)
        col = max(0, int(column or 1) - 1)
        frag = ["EndingColumnNumber=%d" % col, "EndingLineNumber=%d" % ln, "StartingColumnNumber=%d" % col,
                "StartingLineNumber=%d" % ln]
        if when:
            frag.append("Timestamp=%.6f" % (when - 978307200))   # seconds since 2001 (Apple's reference date)
    return "file://%s%s" % (urllib.parse.quote(path), "#" + "&".join(frag) if frag else "")


def build_issue(i):
    """IssueSummary of a build error / warning (the xcodebuild stand-in's build-log.json "issues")"""
    doc = None
    if i.get("file"):
        doc = O("DocumentLocation", concreteTypeName=S("DVTTextDocumentLocation"),
                url=S(text_location(i["file"], i.get("line"), i.get("column"), i.get("time"))))
    return O("IssueSummary", issueType=S(i.get("type") or "Uncategorized"), message=S(i.get("message") or ""),
             documentLocationInCreatingWorkspace=doc)


def write(summary, bundle_path, products_root=None):
    """Write `summary` (what x86sim test writes as results.json) as the .xcresult bundle `bundle_path` (replaced if it
    is there). products_root: where the run's products and .xctestrun are (the project and scheme names come from its
    ContainerInfo). Returns the root object's id."""
    if os.path.lexists(bundle_path):
        if os.path.isdir(bundle_path) and not os.path.islink(bundle_path):
            if not (bundle_path.rstrip("/").endswith(".xcresult") or os.path.isfile(os.path.join(bundle_path, "Info.plist"))):
                raise OSError("%s is there and is not an .xcresult bundle" % bundle_path)
            shutil.rmtree(bundle_path)
        else:
            os.unlink(bundle_path)
    tmp = bundle_path.rstrip("/") + ".tmp-%d" % os.getpid()
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)
    try:
        root = Writer(summary, tmp, products_root).write()
        info = {"dateCreated": datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None, microsecond=0),
                "directoryImportMode": "concurrent", "externalLocations": [], "rootId": {"hash": root},
                "storage": {"backend": "fileBacked2", "compression": "standard"}, "version": {"major": 3, "minor": 60}}
        with open(os.path.join(tmp, "Info.plist"), "wb") as f:
            plistlib.dump(info, f)
        os.rename(tmp, bundle_path)
    except BaseException:
        shutil.rmtree(tmp, ignore_errors=True)
        raise
    return root


def main(argv):
    args, products = [], None
    i = 0
    while i < len(argv):
        if argv[i] == "--products" and i + 1 < len(argv):
            products = argv[i + 1]
            i += 2
            continue
        if argv[i] in ("-h", "--help"):
            print("usage: xcresult.py results.json OUT.xcresult [--products DIR]")
            return 0
        args.append(argv[i])
        i += 1
    if len(args) != 2:
        print("usage: xcresult.py results.json OUT.xcresult [--products DIR]", file=sys.stderr)
        return 2
    with open(args[0]) as f:
        summary = json.load(f)
    write(summary, args[1], products)
    print(args[1])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
