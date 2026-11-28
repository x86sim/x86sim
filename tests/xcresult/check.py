#!/usr/bin/env python3
"""x86Sim: check cli/libexec/xcresult.py (x86sim test's results.json -> Xcode .xcresult) with Apple's xcresulttool.

Builds a bundle from each results.json given (and from a synthetic one made here: an ObjC test, a failure without a
location, a skipped test, non-ASCII text, a user attachment), checks it here (every object's id is its hash, every
reference resolves), copies it to the Mac ($X86SIM_MAC, default bobby@192.168.1.217) and runs xcresulttool on it there:
get test-results summary / tests / test-details / activities, export attachments, get object --legacy, get log.

    tests/xcresult/check.py [RUN_DIR_OR_RESULTS_JSON ...] [--apple RUN=MAC_PATH.xcresult ...] [--keep]

--apple: compare the bundle made from RUN with an .xcresult Xcode wrote on the Mac for the same suite: the same
counts, test identifiers, results, failure messages and failure lines (files compared by name: paths differ), skip
messages, and every test's test-details (issues with their expressions' values and call stack frames: function,
file:line); durations, dates, device, addresses and paths are not compared. Default fixtures, when they are there: the XCTestDemo
runs in ~/scratch (cov1: iPhone, full suite, Apple's /tmp/xtdc/r.xcresult on the Mac; rec1: UI tests with a screen
recording; the watchOS full run) and spm-multi's full run (spmall: tests/apps/XCTestDemo/spm-multi on an iPhone, its
known issues and expected failures; Apple's ~/x86sim-spm-multi/apple-all.xcresult on the Mac, xcodebuild test of the
same package). The Mac is only used to run Apple's xcresulttool; nothing comes back into the tree.
"""
import base64, hashlib, json, os, plistlib, re, shlex, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(ROOT, "cli", "libexec"))
import xcresult  # noqa: E402

MAC = os.environ.get("X86SIM_MAC") or "bobby@192.168.1.217"
SCRATCH = os.path.expanduser(os.environ.get("X86SIM_SCRATCH") or "~/scratch")
DEFAULTS = [os.path.join(SCRATCH, "xcuitest/s/cov1"), os.path.join(SCRATCH, "xcuitest/s/rec1"),
            os.path.join(SCRATCH, "x86sim/xctest/XCTestDemo-20261127-194747"), os.path.join(SCRATCH, "xcuitest/c/full/spmall")]
DEFAULT_APPLE = {"cov1": "/tmp/xtdc/r.xcresult", "spmall": "x86sim-spm-multi/apple-all.xcresult"}
PRODUCTS = os.path.join(SCRATCH, "xcuitest/cov/dd/Build/Products")

fails = []


def check(ok, what):
    print(("  ok    " if ok else "  FAIL  ") + what)
    if not ok:
        fails.append(what)
    return ok


def ssh(cmd, inp=None, binary=False):
    p = subprocess.run(["ssh", "-o", "BatchMode=yes", MAC, cmd], input=inp, capture_output=True)
    out = p.stdout if binary else p.stdout.decode("utf-8", "replace")
    return p.returncode, out, p.stderr.decode("utf-8", "replace")


# ---------------------------------------------------------------------------------------------------------------------
# local checks: the store

def derlen(r, i):
    c = r[i]
    i += 1
    if c < 0x80:
        return c, i
    n = c & 0x7f
    return int.from_bytes(r[i:i + n], "big"), i + n


def local_check(bundle):
    data = os.path.join(bundle, "Data")
    info = plistlib.load(open(os.path.join(bundle, "Info.plist"), "rb"))
    ids = {n[5:] for n in os.listdir(data) if n.startswith("data.")}
    bad_hash, missing = [], []
    for oid in ids:
        stored = open(os.path.join(data, "data." + oid), "rb").read()
        r = open(os.path.join(data, "refs." + oid), "rb").read()
        n, i = derlen(r, 0)
        ents = []
        for _ in range(n):
            ln, i = derlen(r, i)
            ents.append(r[i:i + ln])
            i += ln
        if hashlib.blake2b(b"".join(ents) + stored).digest() != base64.urlsafe_b64decode(oid[2:]) or i != len(r):
            bad_hash.append(oid)
        for e in ents:
            ref = "0~" + base64.urlsafe_b64encode(e[1:]).decode()
            if ref not in ids:
                missing.append(ref)
    check(info["rootId"]["hash"] in ids, "root object is in Data/")
    check(not bad_hash, "every object's id is BLAKE2b(refs + data) (%d objects)" % len(ids))
    check(not missing, "every reference resolves")


# ---------------------------------------------------------------------------------------------------------------------
# a synthetic run: what the XCTestDemo runs do not have

def synthetic(dirpath):
    os.makedirs(os.path.join(dirpath, "attachments/Odd.testNote"), exist_ok=True)
    att = os.path.join(dirpath, "attachments/Odd.testNote/note.txt")
    with open(att, "w") as f:
        f.write("café → ↳ note\n")
    log = os.path.join(dirpath, "Odd.log")
    with open(log, "w") as f:
        f.write("/src/Odd.m:12: -[OddTests testSkip] : Test skipped - not here überall\n")
    s = {"phone": "synthetic", "runtime": "ios-27.2-24B8117-arm64", "platform": "ios", "started": "2026-11-27T12:00:00",
         "duration": 3.5, "result": "failed", "counts": {"passed": 1, "failed": 1, "skipped": 1, "total": 3},
         "project": "Odd", "scheme": "Odd",
         "targets": [{"name": "OddTests", "kind": "logic", "result": "failed", "duration": 3.5, "log": log,
                      "attachments": [{"name": "class setUp", "path": att, "type": "public.plain-text", "from": "test"}],
                      "tests": [
                          {"class": "OddTests", "name": "testObjC", "identifier": "OddTests/testObjC", "result": "passed",
                           "duration": 0.01, "failures": []},
                          {"class": "Odd.Swifty", "name": "testNote", "identifier": "Swifty/testNote", "result": "failed",
                           "duration": 0.2, "failures": [
                               {"file": None, "line": None, "message": "caught «é» → boom\nline 2"},
                               {"file": "/src/Swifty.swift", "line": 7, "message": "XCTAssertTrue failed - ☃"}],
                           "attachments": [{"name": "Note ☃", "path": att, "type": "public.plain-text", "from": "test"}]},
                          {"class": "OddTests", "name": "testSkip", "identifier": "OddTests/testSkip", "result": "skipped",
                           "duration": 0.0, "failures": [], "skip_message": "not here überall"}]}]}
    p = os.path.join(dirpath, "results.json")
    with open(p, "w") as f:
        json.dump(s, f)
    return p


# ---------------------------------------------------------------------------------------------------------------------
# xcresulttool on the Mac

def nodes(tree, qualified=False, urls=None):
    """{identifier: (result, [(message, file name, line)], [skip messages], [(argument, result, messages)])} from get
    test-results tests (messages: failures' and, marked "expected: ", expected failures') (a parameterized Swift Testing test: its "Arguments" children). qualified: keyed
    "<test bundle>:<identifier>" (the same identifier in two targets: shared(), Common/t()). urls: a list that gets
    every test's nodeIdentifierURL (what test-details takes when an identifier is not unique)."""
    out = {}

    def msgs_of(n):
        return sorted(((("expected: " if c["nodeType"] == "Expected Failure" else "") + c["name"],
                       os.path.basename((c.get("sourceLocation") or {}).get("filePath") or ""),
                       (c.get("sourceLocation") or {}).get("lineNumber")) for c in n.get("children", [])
                      if c.get("nodeType") in ("Failure Message", "Expected Failure")), key=str)

    def walk(n, bundle):
        if n.get("nodeType", "").endswith("test bundle"):
            bundle = n.get("name")
        if n.get("nodeType") == "Test Case":
            skips = [c["name"] for c in n.get("children", []) if c.get("nodeType") == "Skip Message"]
            args = sorted((c.get("name"), c.get("result"), msgs_of(c)) for c in n.get("children", [])
                          if c.get("nodeType") == "Arguments")
            out["%s:%s" % (bundle, n["nodeIdentifier"]) if qualified else n["nodeIdentifier"]] = (
                n.get("result"), msgs_of(n), skips, args)
            if urls is not None and n.get("nodeIdentifierURL"):
                urls.append(n["nodeIdentifierURL"])
        for c in n.get("children", []):
            walk(c, bundle)
    for n in tree.get("testNodes", []):
        walk(n, None)
    return out


def bundles_of(tree):
    out = {}

    def walk(n):
        if n.get("nodeType", "").endswith("test bundle"):
            out[n["name"]] = n["nodeType"]
        for c in n.get("children", []):
            walk(c)
    for n in tree.get("testNodes", []):
        walk(n)
    return out


def mac_tool(remote, name, run):
    """Run xcresulttool on the Mac's copy; returns what the comparison needs."""
    def sh(cmd):
        return ssh("cd %s && %s" % (shlex.quote(remote), cmd))

    def get(args):
        return sh("xcrun xcresulttool " + args)

    b = shlex.quote(name)
    res = {}
    rc, out, err = get("get test-results summary --path %s" % b)
    if not check(rc == 0, "%s: get test-results summary%s" % (name, "" if rc == 0 else ": " + err.strip()[:300])):
        return None
    res["summary"] = json.loads(out)
    rc, out, err = get("get test-results tests --path %s" % b)
    check(rc == 0, "%s: get test-results tests" % name)
    res["tests"] = json.loads(out) if rc == 0 else {}
    rc, out, err = get("get object --legacy --format json --path %s" % b)
    check(rc == 0 and json.loads(out).get("_type", {}).get("_name") == "ActionsInvocationRecord",
          "%s: get object --legacy (root ActionsInvocationRecord)" % name)
    if rc == 0:
        root = json.loads(out)
        ref = root["actions"]["_values"][0]["actionResult"]["testsRef"]["id"]["_value"]
        rc, out, err = get("get object --legacy --format json --path %s --id %s" % (b, shlex.quote(ref)))
        check(rc == 0 and "ActionTestPlanRunSummaries" in out, "%s: get object --legacy --id <testsRef>" % name)
    rc, out, err = get("get log --type action --path %s" % b)
    check(rc == 0 and out.strip().startswith("{"), "%s: get log --type action" % name)
    res["action_log"] = json.loads(out) if rc == 0 and out.strip().startswith("{") else None
    # the build: its status and issues; its log when the run built the products (the xcodebuild stand-in's test)
    rc, out, err = get("get build-results --path %s" % b)
    check(rc == 0 and out.strip().startswith("{"), "%s: get build-results" % name)
    res["build"] = json.loads(out) if rc == 0 else {}
    if res["build"].get("status") not in (None, "notRequested"):
        rc, out, err = get("get log --type build --path %s" % b)
        check(rc == 0 and out.strip().startswith("{"), "%s: get log --type build" % name)
    res["legacy_issues"] = legacy_issues(remote + "/" + name)
    # every test's details and activities
    ids = []
    nodes(res["tests"], urls=ids)   # by URL: an identifier can be in two targets
    bad = []
    res["details"] = {}
    for tid in ids:
        rc1, o1, e1 = get("get test-results test-details --path %s --test-id %s" % (b, shlex.quote(tid)))
        rc2, o2, e2 = get("get test-results activities --path %s --test-id %s" % (b, shlex.quote(tid)))
        if rc1 or rc2:
            bad.append(tid)
        else:
            res["details"][tid] = json.loads(o1)
    # a run whose build failed has no tests (Xcode's too)
    check((ids or not res["summary"].get("totalTestCount")) and not bad,
          "%s: test-details + activities for all %d tests%s" % (name, len(ids), " (bad: %s)" % bad if bad else ""))
    # attachments: every one in results.json comes out of export attachments, byte for byte
    want = {}
    for t in run.get("targets", []):
        for x in t.get("tests", []):
            for a in x.get("attachments") or []:
                if a.get("path") and os.path.isfile(a["path"]):
                    want.setdefault(hashlib.sha256(open(a["path"], "rb").read()).hexdigest(), a["path"])
    e = shlex.quote("exp-" + name)
    rc, out, err = sh("rm -rf %s && mkdir %s && xcrun xcresulttool export attachments --path %s --output-path %s "
                      ">/dev/null && cd %s && cat manifest.json && echo @@@ && { shasum -a 256 * | grep -v manifest.json || true; }"
                      % (e, e, b, e, e))
    if check(rc == 0, "%s: export attachments" % name):
        man, sums = out.split("@@@")
        got = {ln.split()[0] for ln in sums.strip().splitlines() if ln.strip()}
        n = sum(len(e["attachments"]) for e in json.loads(man))
        check(set(want) <= got, "%s: %d attachment files exported (%d entries), every one of results.json's (%d) byte-equal"
              % (name, len(got), n, len(want)))
        res["manifest"] = json.loads(man)
    sh("rm -rf %s" % e)
    if run.get("coverage"):
        res["coverage"] = xccov(sh, b, name)
    return res


def xccov(sh, b, name):
    """xccov's report (JSON, functions' ranges) and every file's line coverage, paths cut to <dir>/<file>."""
    rc, out, err = sh("xcrun xccov view --report --json %s" % b)
    if not check(rc == 0, "%s: xccov view --report%s" % (name, "" if rc == 0 else ": " + err.strip()[:300])):
        return None
    rep = json.loads(out)
    res = {"report": {}, "lines": {}, "ranges": {}}
    for t in rep["targets"]:
        res["report"][t["name"]] = (t["executableLines"], t["coveredLines"], os.path.basename(t["buildProductPath"]))
        for f in t["files"]:
            short = "/".join(f["path"].split("/")[-2:])
            res["report"][(t["name"], short)] = (f["executableLines"], f["coveredLines"])
            for fn in f["functions"]:
                res["report"][(t["name"], short, fn["name"], fn["lineNumber"])] = (
                    fn["executableLines"], fn["coveredLines"], fn["executionCount"])
            rc, out, _ = sh("xcrun xccov view --report --functions-for-file %s %s" % (shlex.quote(f["path"]), b))
            res["ranges"][short] = [ln.split(None, 1)[1].rsplit("%", 1)[0] for ln in out.splitlines() if "{" in ln]
            rc, out, _ = sh("xcrun xccov view --archive --file %s %s" % (shlex.quote(f["path"]), b))
            res["lines"][short] = out if rc == 0 else None
    check(all(v is not None for v in res["lines"].values()),
          "%s: xccov view --archive --file for all %d files" % (name, len(res["lines"])))
    return res


def apple(remote_bundle, coverage=False):
    res = {}
    if coverage:
        res["coverage"] = xccov(lambda c: ssh(c), shlex.quote(remote_bundle), "Apple's")
    for k, a in (("summary", "get test-results summary"), ("tests", "get test-results tests")):
        rc, out, err = ssh("xcrun xcresulttool %s --path %s" % (a, shlex.quote(remote_bundle)))
        if rc:
            return None
        res[k] = json.loads(out)
    res["legacy_issues"] = legacy_issues(remote_bundle)
    rc, out, err = ssh("xcrun xcresulttool get log --type action --path %s" % shlex.quote(remote_bundle))
    res["action_log"] = json.loads(out) if rc == 0 and out.strip().startswith("{") else None
    urls = []
    nodes(res["tests"], urls=urls)
    b = shlex.quote(remote_bundle)
    rc, out, err = ssh("; ".join("echo @@@; xcrun xcresulttool get test-results test-details --path %s --test-id %s"
                                 % (b, shlex.quote(u)) for u in urls))
    parts = out.split("@@@\n")[1:]
    res["details"] = {u: json.loads(p) for u, p in zip(urls, parts) if p.strip().startswith("{")}
    return res


# run on the Mac (python3 - BUNDLE): every test's issues in the legacy graph (get object --legacy: its
# ActionTestSummary's failureSummaries and expectedFailures), one JSON line per test summary
LEGACY_ISSUES = r"""
import json, subprocess, sys
b = sys.argv[1]
def get(i=None):
    a = ["xcrun", "xcresulttool", "get", "object", "--legacy", "--format", "json", "--path", b] + (["--id", i] if i else [])
    return json.loads(subprocess.run(a, capture_output=True, text=True).stdout)
def plain(o):
    if isinstance(o, dict):
        if "_value" in o and len([k for k in o if k != "_type"]) == 1: return o["_value"]
        if "_values" in o: return [plain(x) for x in o["_values"]]
        return {k: plain(v) for k, v in o.items() if k != "_type"}
    return o
root = get()
runs = get(root["actions"]["_values"][0]["actionResult"]["testsRef"]["id"]["_value"])
def walk(n):
    if isinstance(n, dict):
        if n.get("_type", {}).get("_name") == "ActionTestMetadata":
            s = get(n["summaryRef"]["id"]["_value"])
            print(json.dumps({"url": n["identifierURL"]["_value"],
                              "issues": {k: plain(s[k]) for k in ("failureSummaries", "expectedFailures", "activitySummaries")
                                         if k in s}}))
            return
        for v in n.values(): walk(v)
    elif isinstance(n, list):
        for v in n: walk(v)
walk(runs)
"""


VARYING = [(re.compile(r"\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d(\.\d+)?"), "<date>"), (re.compile(r"\b(pid|with pid) \d+"), r"\1 <pid>"),
           (re.compile(r"(Terminate \S+):\d+"), r"\1:<pid>"),
           (re.compile(r"[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}"), "<uuid>"),
           (re.compile(r"\d{4}-\d\d-\d\d at \d\d\.\d\d\.\d\d [AP]M"), "<date>")]


def varying(s):
    """a title / name without what differs between runs (dates, pids, uuids)"""
    for r, w in VARYING:
        s = r.sub(w, s)
    return s


def activity(a):
    """an activity of the legacy graph as compared: title, type, its attachments (name, type, filename), the same for
    its subactivities (dates, pids and uuids left out)"""
    atts = a.get("attachments") if isinstance(a.get("attachments"), list) else []
    subs = a.get("subactivities") if isinstance(a.get("subactivities"), list) else []
    return {"title": varying(a.get("title") or ""), "type": a.get("activityType"),
            "attachments": [(varying(x.get("name") or ""), x.get("uniformTypeIdentifier"), varying(x.get("filename") or ""))
                            for x in atts],
            "subactivities": [activity(c) for c in subs]}


def legacy_issues(bundle):
    """{test URL (with ?args=): its issues in the legacy graph, without what is not compared (ids, dates, addresses,
    an error's domain: Apple's names a type's runtime context; paths: file names), the call stack's frames that are not
    symbolicated left out (Xcode keeps every address: their number is the stack's depth on each machine); the named
    ones are compared: located frames and the UI test runner's, named by symbol alone (_XCTRunnerRunTests, main); an
    issue's attachments by name and type; the test's activities (activity(): XCTest's tree, a UI test's failure triage
    "UI Snapshot" and debug descriptions where XCTest added them)}"""
    rc, out, err = ssh("python3 - %s" % shlex.quote(bundle), inp=LEGACY_ISSUES.encode())
    if rc:
        return None

    def norm(x):
        if isinstance(x, dict):
            r = {k: (os.path.basename(v) if k in ("filePath", "fileName") else norm(v)) for k, v in x.items()
                 if k not in ("uuid", "timestamp", "addressString", "associatedError", "attachments", "activitySummaries")}
            if "attachments" in x:
                r["attachments"] = [(varying(a.get("name") or ""), a.get("uniformTypeIdentifier"))
                                    for a in (x["attachments"] if isinstance(x["attachments"], list) else [])]
            if "activitySummaries" in x:
                r["activities"] = [activity(a) for a in (x["activitySummaries"] if isinstance(x["activitySummaries"], list) else [])]
            if isinstance(r.get("callStack"), list):
                r["callStack"] = [f for f in r["callStack"] if f.get("symbolInfo")]
            return r
        return [norm(v) for v in x] if isinstance(x, list) else x
    res = {}
    for ln in out.splitlines():
        d = json.loads(ln)
        res[d["url"]] = norm(d["issues"])
    return res


def details_tree(d):
    """test-details without what is not compared (durations, devices, paths: file names): the test's identity and
    result, then its runs' nodes (issues with their "Results" expressions and "Source Code Reference" frames, skip
    messages, arguments, attachments by name without dates), Device / Test Plan Configuration levels left out (Apple's
    device is another)."""
    def norm(n):
        out = []
        if n.get("nodeType") == "Attachment":
            return [{"name": varying(n.get("name") or ""), "nodeType": "Attachment"}]
        if n.get("nodeType") in ("Device", "Test Plan Configuration"):
            for c in n.get("children", []):
                out += norm(c)
            return out
        m = {k: n[k] for k in ("name", "nodeType", "result", "details", "nodeIdentifierURL") if k in n}
        if n.get("sourceLocation"):
            m["at"] = "%s:%s" % (os.path.basename(n["sourceLocation"].get("filePath") or ""), n["sourceLocation"].get("lineNumber"))
        kids = []
        for c in n.get("children", []):
            kids += norm(c)
        if kids:
            m["children"] = kids
        return [m]
    top = {k: d.get(k) for k in ("testIdentifier", "testIdentifierURL", "testName", "testResult", "testDescription")}
    # a parameterized test's argument values: xcresulttool lists them in no fixed order (Apple's bundle too)
    top["arguments"] = sorted(json.dumps(a, sort_keys=True) for a in d.get("arguments") or [])
    runs = []
    for r in d.get("testRuns", []):
        runs += norm(r)
    top["runs"] = runs
    return top


def action_tree(log):
    """The test targets' part of an action log as Xcode nests it (`get log --type action`): per section its path of
    titles (a process's pid left out), result, testDetails keys, tests-passed line, skipped, its failure / skip
    messages and the first line of its summary without times; not "Launch actions" nor the coverage steps"""
    out = {}

    def walk(n, path):
        t = n.get("title") or ""
        if not t.startswith(("Run test", "Writing")) and t.endswith(")"):
            t = t.rsplit(" (", 1)[0]
        path = path + (t,)
        td = n.get("testDetails") or {}
        # Swift Testing runs tests in parallel: Xcode's summary of one is often another test's line
        swift_line = re.match(r"^\s*[\u200b◇✔✘➜↳━]", td.get("summary") or "")
        summ = "" if swift_line else re.sub(r"\(\d+\.\d+ seconds\)|in [\d.]+ \([\d.]+\) seconds|at \d{4}-\d\d-\d\d [\d:.]+|\d+\.\d+s", "", (td.get("summary") or "").strip().split("\n")[-1])
        out["/".join(path)] = (n.get("result"), sorted(k for k in td if k != "emittedOutput"), td.get("testsPassedString"),
                               td.get("wasSkipped"), sorted((m.get("type"), m.get("title")) for m in n.get("messages") or []
                                                            if m.get("type") != "notice"), summ, td.get("runnableUTI"))
        for c in n.get("subsections") or []:
            walk(c, path)
    for n in (log or {}).get("subsections") or []:
        if (n.get("title") or "").startswith("Test target "):
            walk(n, ())
    return out


def compare(ours, theirs, name):
    so, sa = ours["summary"], theirs["summary"]
    for k in ("result", "totalTestCount", "passedTests", "failedTests", "skippedTests", "expectedFailures", "title"):
        check(so.get(k) == sa.get(k), "%s vs Apple: summary %s %r == %r" % (name, k, so.get(k), sa.get(k)))
    fo = sorted((f["testIdentifierString"], f["targetName"], f["failureText"], f["testIdentifierURL"]) for f in so["testFailures"])
    fa = sorted((f["testIdentifierString"], f["targetName"], f["failureText"], f["testIdentifierURL"]) for f in sa["testFailures"])
    check(fo == fa, "%s vs Apple: summary testFailures (identifier, target, text, URL) equal (%d)" % (name, len(fa)))
    if fo != fa:
        print("     ours:  %s\n     Apple: %s" % (fo, fa))
    no, na = nodes(ours["tests"], True), nodes(theirs["tests"], True)
    check(sorted(no) == sorted(na), "%s vs Apple: the same %d test identifiers" % (name, len(na)))
    if sorted(no) != sorted(na):
        print("     only ours:  %s\n     only Apple: %s" % (sorted(set(no) - set(na)), sorted(set(na) - set(no))))
    diff = []
    for tid in sorted(set(no) & set(na)):
        ro, mo, ko, ao = no[tid]
        ra, ma, ka, aa = na[tid]
        # a failure Apple located only by symbolicating the stack (a thrown error) has no location in x86sim's log
        ma_cmp = [m if any(m[0] == x[0] and x[1] for x in mo) else (m[0], "", None) for m in ma]
        if ro != ra or mo != sorted(ma_cmp, key=str) or ko != ka or ao != aa:
            diff.append((tid, no[tid], na[tid]))
    check(not diff, "%s vs Apple: per test result, failure messages + file:line, skip messages" % name)
    for d in diff:
        print("     %s\n       ours:  %s\n       Apple: %s" % d)
    check(bundles_of(ours["tests"]) == bundles_of(theirs["tests"]), "%s vs Apple: test bundle kinds %s" % (name, bundles_of(ours["tests"])))
    # test-details: issues, their expressions' values and call stack frames (function, file:line), skips, arguments
    do, da = ours.get("details") or {}, theirs.get("details") or {}
    both = sorted(set(do) & set(da))
    ddiff = [u for u in both if details_tree(do[u]) != details_tree(da[u])]
    check(both and not ddiff, "%s vs Apple: test-details of %d tests equal (issues, expression values, call stack "
          "frames: function + file:line; not durations, devices, paths)" % (name, len(both)))
    for u in ddiff:
        print("     %s\n       ours:  %s\n       Apple: %s" % (u, json.dumps(details_tree(do[u])["runs"], ensure_ascii=False),
                                                         json.dumps(details_tree(da[u])["runs"], ensure_ascii=False)))
    ao, aa = action_tree(ours.get("action_log")), action_tree(theirs.get("action_log"))
    if aa:
        adiff = sorted(k for k in set(ao) | set(aa) if ao.get(k) != aa.get(k))
        check(not adiff, "%s vs Apple: action log of the test targets (%d sections: processes, suites, test cases, "
              "their results, tests-passed lines, failure and skip messages, summaries)" % (name, len(aa)))
        for k in adiff:
            print("     %s\n       ours:  %s\n       Apple: %s" % (k, ao.get(k), aa.get(k)))
    lo, la = ours.get("legacy_issues") or {}, theirs.get("legacy_issues") or {}
    both = sorted(set(lo) & set(la))
    ldiff = [u for u in both if lo[u] != la[u]]
    check(both and not ldiff, "%s vs Apple: legacy issues of %d test summaries equal (messages, issueType, expressions, "
          "located call stack frames)" % (name, len(both)))
    for u in ldiff:
        print("     %s\n       ours:  %s\n       Apple: %s" % (u, json.dumps(lo[u], ensure_ascii=False)[:2000],
                                                         json.dumps(la[u], ensure_ascii=False)[:2000]))
    co, ca = ours.get("coverage"), theirs.get("coverage")
    if co and ca:
        check(co["report"] == ca["report"], "%s vs Apple: xccov report: %d entries (targets, files, functions: lines, "
              "covered, execution counts) equal" % (name, len(ca["report"])))
        for k in sorted(set(co["report"]) | set(ca["report"]), key=str):
            if co["report"].get(k) != ca["report"].get(k):
                print("     %s ours %s Apple %s" % (k, co["report"].get(k), ca["report"].get(k)))
        check(co["ranges"] == ca["ranges"], "%s vs Apple: xccov functions-for-file (order, names, ranges, coverage) equal" % name)
        same = [f for f in ca["lines"] if co["lines"].get(f) == ca["lines"][f]]
        check(len(same) == len(ca["lines"]), "%s vs Apple: xccov line coverage (counts + column ranges) identical "
              "for %d/%d files" % (name, len(same), len(ca["lines"])))
    unlocated = [(t, m[0]) for t, (_, ms, _, _) in na.items() for m in ms
                 if not any(x[0] == m[0] and x[1] for x in no.get(t, (0, [], 0, 0))[1]) and m[1]]
    if unlocated:
        print("  note  Apple has a location x86Sim's log does not give for: %s" % unlocated)


def main(argv):
    keep = "--keep" in argv
    runs, apples, i = [], dict(DEFAULT_APPLE), 0
    argv = [a for a in argv if a != "--keep"]
    explicit = False
    while i < len(argv):
        if argv[i] == "--apple":
            k, _, v = argv[i + 1].partition("=")
            apples[k] = v
            i += 2
            continue
        runs.append(argv[i])
        explicit = True
        i += 1
    if not explicit:
        runs = [d for d in DEFAULTS if os.path.isfile(os.path.join(d, "results.json"))]
    base = os.path.join(SCRATCH, "xcuitest", "xcr")
    base = base if os.path.isdir(base) else SCRATCH if os.path.isdir(SCRATCH) else None   # not /tmp: tmpfs
    work = tempfile.mkdtemp(prefix="check-", dir=base)
    items = [("synthetic", synthetic(os.path.join(work, "synthetic")))]
    for r in runs:
        p = os.path.join(r, "results.json") if os.path.isdir(r) else r
        items.append((os.path.basename(os.path.dirname(os.path.abspath(p))), p))
    built = []
    for name, p in items:
        print("%s (%s)" % (name, p))
        run = json.load(open(p))
        b = os.path.join(work, name + ".xcresult")
        # XCTestDemo's products (its .xctestrun names the project and scheme) for runs that do not name their project
        # (a Swift package's results.json does: x86sim test --spm)
        xcresult.write(run, b, PRODUCTS if os.path.isdir(PRODUCTS) and not run.get("project") else None)
        local_check(b)
        built.append((name, b, run))
    rc, out, err = ssh("mktemp -d /tmp/x86sim-xcr.XXXXXX")
    if not check(rc == 0, "the Mac (%s) answers: %s" % (MAC, err.strip()[:200])):
        return 1
    remote = out.strip()
    try:
        tar = subprocess.run(["tar", "-C", work, "-cf", "-"] + [os.path.basename(b) for _, b, _ in built],
                             capture_output=True, check=True).stdout
        rc, _, err = ssh("tar -C %s -xf -" % shlex.quote(remote), inp=tar)
        check(rc == 0, "bundles copied to the Mac (%s)" % remote)
        for name, b, run in built:
            print("%s on the Mac" % name)
            ours = mac_tool(remote, os.path.basename(b), run)
            if ours and name in apples:
                theirs = apple(apples[name], bool(ours.get("coverage")))
                if check(theirs is not None, "%s: Apple's bundle %s reads" % (name, apples[name])):
                    compare(ours, theirs, name)
            if ours and name == "synthetic":
                t = nodes(ours["tests"])
                check(set(t) == {"OddTests/testObjC", "Swifty/testNote()", "OddTests/testSkip"},
                      "synthetic: ObjC ids without (), Swift with: %s" % sorted(t))
                check(t.get("OddTests/testSkip", (0, 0, []))[2] == ["Test skipped - not here überall"],
                      "synthetic: non-ASCII skip message")
                check(any("→ boom" in m[0] for m in t.get("Swifty/testNote()", (0, [], 0))[1]),
                      "synthetic: non-ASCII failure message, no location")
                check(ours["summary"].get("environmentDescription") == "Odd", "synthetic: environmentDescription %r"
                      % ours["summary"].get("environmentDescription"))
    finally:
        ssh("rm -rf %s" % shlex.quote(remote))
        if not keep:
            subprocess.run(["rm", "-rf", work])
        else:
            print("bundles kept in %s" % work)
    print("\n%s" % ("PASS" if not fails else "FAIL: %d check(s)" % len(fails)))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
