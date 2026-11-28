"""cli/libexec/xctest.py: -only-testing / -skip-testing as Xcode 27.2 encodes them in the XCTestConfiguration it hands a
test process (read on a Mac from XCTestConfiguration.activeTestConfiguration inside the test, 2026-11-28), and which
target a selection is about. Offline: nothing runs on a phone.

Rows (expected values are Xcode's, copied from the captured configurations):
  1  "DumpXCTests"                -> run: o3 oc ocm a=nil, o1 oc a=nil, o0 a=nil (all c=[DumpXCTests])
  2  "DumpXCTests/testDump"       -> run: o2 ocm a=[] c=[DumpXCTests,testDump], o1 oc a=nil same c
  3  "DumpXCTests/testDump()"     -> run: o6 ocm os a=[] c=[DumpXCTests,testDump], o0 a=nil c=[DumpXCTests,testDump()]
  4  "dumpConfig()"               -> run: o0 a=nil c=[dumpConfig()]
  5  "DumpSuite/inSuite()"        -> as row 3 (a Swift Testing test in a suite)
  6  skip "CalcKitXCTests/testAdd" -> o2, o6, o1;  skip "meanOfNothing()" -> o0 only
  7  "f(x:)" (labelled arguments) -> o5 oc os (initWithSwiftTestingStringRepresentation's rule, disassembled)
  8  selection(): Target/Class/test() keeps its parentheses and the rest of the path; a SwiftPM bundle answers to its
     test targets' names (_modules); another target's selection leaves it out
  9  a Swift package with several test targets: SwiftPM's link of its one bundle (llbuild manifest) found and made
     into one test target's link (own object list, output, rpath for the flat layout, no other targets' AST paths,
     XCTest + libXCTestSwiftSupport as Xcode links every test bundle); its objects: the target's own and the
     non-test targets', not the other test targets' nor SwiftPM's runner.swift
 10  Swift Testing's log of a parameterized test (lines copied from a run): one test named f(x:), not
     "f(x:) with 2 test cases", its issue with its file:line
 11  Swift Testing's events (libsimswt, the shape of a run's GammaTests.swt.jsonl): the same function in two suites
     and a nested one -> one test each, identified as Xcode does (First/same(), Second/same(), Second/Inner/same(),
     top-level shared()); class Module.Suite..., results, failure file:line, skip comment + location, durations; a
     parameterized test's cases (Xcode's ?args= hash, argument, result); a line cut short ignored; console lines
     about those tests replaced, XCTest's kept
 12  issue texts as Xcode 27.2's .xcresult has them (Apple's, 2026-11-28): expectation + subexpressions whose value
     is not their own source, "<not evaluated>", ": comment", "Caught error: Oops()", "Issue recorded: a note"
 13  a thrown error's location: the first frame of its stack in the test bundle (.xctest), looked up one byte
     before its return address; the test found by -[Class test] without the module; nothing when nothing symbolicates
 14  known issues as Xcode 27.2 reports them (spm-multi's DeltaTests on Apple's simulator, 2026-11-28): withKnownIssue
     -> "expected_failure" with the issue and its comment as reason (libsimswt's "known"), a failed test keeps its
     known issues, a parameterized test's case too; "Known issue was not recorded" fails; XCTExpectFailure's console
     line (file name only, "Reason: (...)") and libsimswt's hook line (the path) -> expected_failure; Swift Testing's
     console "recorded a known issue"; counts' expected_failures; junit: not a failure
 15  issues' call stacks and expressions as Xcode 27.2 shows them in test-details (spm-multi on Apple's simulator,
     2026-11-28): a frame's line is the first of its inlining chain in a source file (an #expect inlined into its
     test function -> the function's line; macro expansions and compiler-generated code -> no frame); frames in the
     test bundle symbolicated (image name, swift-demangle'd function, file:line), every frame's address (one before a
     return address); located XCTest failures get their stack by message + location; an XCTExpectFailure matched by
     a thrown error (no location in the console line: "Expected failure in -[C t]: ...") -> expected_failure at the
     throw; a failed #expect's expression (source, value, subexpressions) -> results.json -> TestExpression
 16  a UI test's runner frames as Xcode 27.2 names them (XCTestDemo's testDeliberateUIFailure on Apple's simulator,
     2026-11-28): the test host's executable (the app whose PlugIns/ has the test bundle) is symbolicated too
     (XCTestConfiguration.symbolicationImageNames), a frame there without debug info by its symbol alone
     (_XCTRunnerRunTests, main: image + symbol, no file); other images (XCTestCore, the host's debug dylib) not;
     symbol-only frames do not make a stack "symbolicated"
 17  a UI test's activities and attachments as Xcode 27.2 keeps them (the same test): XCTest's activity tree
     (libsimshim's activities.jsonl: parent uuid, start order) on the test, each attachment in its own activity
     (index + activity uuid in its filename, "App UI hierarchy for com.x_0_<uuid>.y", "Snapshot_1_<uuid>"), the screen
     recording in "Start Test at"; the runner told not to take diagnostic screenshots while x86sim records
 18  a failed #expect's structured values as Xcode 27.2 keeps them (spm-multi's GammaTests Values on Apple's
     simulator, 2026-11-28): TestValue label + children (an array's elements, a dictionary's key/value pairs, an
     optional's "some", a struct's properties), and for an expression without subexpressions of its own one per
     labelled child of its value ("x", "y" under Point(...), "some" under v; none under an array or dictionary)
Mutants (each must fail its row; checked by hand when the test was written):
  M1  both XCTest method forms in a run list (the old double run)          -> rows 2, 3, 5
  M2  a=[] on the class form (made Swift Testing run every test)            -> row 1
  M3  test_identifiers strips "()" before splitting (the old encoding)      -> rows 3, 4, 5, 6
  M4  selection() strips "()" (the old selection)                           -> row 8
  M5  R_ST_END without " with N test cases" / R_ST_ISSUE without "with N argument(s)" -> row 10
  M6  SwiftPM's runner.swift object kept / no XCTest in a target's link     -> row 9
  M7  Swift Testing's tests keyed by function name (the old collapse)        -> row 11
  M8  every subexpression listed (literals too) / comment before the lines   -> row 12
  M9  the return address itself looked up / the first frame of any image     -> row 13
  M10 a known issue's test left "passed" (the old result) / its comment dropped -> row 14
  M11 the innermost inlined entry kept (macro buffer lines) / the return address itself / R_XFAIL needing a
      "<file>:<line>: " -> row 15
  M12 the host's frames left unnamed (the old stack) / every image named by symbol -> row 16
  M13 an attachment outside its activity (the old one activity per attachment) / no tree -> row 17
  M14 a value's children dropped / subexpressions synthesized for unlabelled children too -> row 18
"""
import importlib.util
import json
import os
import sys
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path[:0] = [os.path.join(ROOT, "cli/libexec"), os.path.join(ROOT, "cli/lib")]
spec = importlib.util.spec_from_file_location("xctest_under_test", os.path.join(ROOT, "cli/libexec/xctest.py"))
X = importlib.util.module_from_spec(spec)
spec.loader.exec_module(X)


def ids(s, skip=False):
    out = []
    for o in X.test_identifiers(s, skip):
        d = o.fields
        out.append((d["o"], tuple(d["c"]), d["a"], tuple(k for k in ("oc", "ocm", "os") if d.get(k))))
    return out


class Identifiers(unittest.TestCase):
    def test_row1_class(self):
        c = ("DumpXCTests",)
        self.assertEqual(ids("DumpXCTests"), [(3, c, None, ("oc", "ocm")), (1, c, None, ("oc",)), (0, c, None, ())])

    def test_row2_method(self):
        c = ("DumpXCTests", "testDump")
        self.assertEqual(ids("DumpXCTests/testDump"), [(2, c, [], ("ocm",)), (1, c, None, ("oc",))])

    def test_row3_swift_method(self):
        self.assertEqual(ids("DumpXCTests/testDump()"), [(6, ("DumpXCTests", "testDump"), [], ("ocm", "os")),
                                                         (0, ("DumpXCTests", "testDump()"), None, ())])

    def test_row4_free_function(self):
        self.assertEqual(ids("dumpConfig()"), [(0, ("dumpConfig()",), None, ())])

    def test_row5_suite_test(self):
        self.assertEqual(ids("DumpSuite/inSuite()"), [(6, ("DumpSuite", "inSuite"), [], ("ocm", "os")),
                                                      (0, ("DumpSuite", "inSuite()"), None, ())])

    def test_row6_skips(self):
        c = ("CalcKitXCTests", "testAdd")
        self.assertEqual(ids("CalcKitXCTests/testAdd", skip=True),
                         [(2, c, [], ("ocm",)), (6, c, [], ("ocm", "os")), (1, c, None, ("oc",))])
        self.assertEqual(ids("meanOfNothing()", skip=True), [(0, ("meanOfNothing()",), None, ())])

    def test_row7_labelled_arguments(self):
        self.assertEqual(ids("f(x:)"), [(5, ("f(x:)",), None, ("oc", "os"))])


class Selection(unittest.TestCase):
    def test_row8_targets(self):
        t = {"_name": "CalcKitPackageTests", "_modules": ["CalcKitTests"]}
        names = {"CalcKitPackageTests", "CalcKitTests", "Other"}
        self.assertEqual(X.selection(t, ["CalcKitTests/Suite/test()"], [], names), (["Suite/test()"], []))
        self.assertEqual(X.selection(t, ["CalcKitPackageTests/C/m"], ["CalcKitTests/f()"], names), (["C/m"], ["f()"]))
        self.assertIsNone(X.selection(t, ["Other/C/m"], [], names))
        self.assertEqual(X.selection(t, [], ["CalcKitTests"], names), None)   # the whole target skipped


MANIFEST = """client:
  name: basic
commands:
  "C.Core-debug.module":
    tool: shell
    outputs: ["/b/Core.build/Core.swift.o"]
    args: ["/usr/bin/swiftc","-module-name","Core"]

  "C.MultiPackageTests-debug.test":
    tool: shell
    inputs: ["/b/AlphaTests.build/AlphaTests.swift.o"]
    outputs: ["/b/MultiPackageTests.xctest/Contents/MacOS/MultiPackageTests"]
    description: "Linking"
    args: ["/usr/bin/swiftc","-L","/b","-o","/b/MultiPackageTests.xctest/Contents/MacOS/MultiPackageTests","-Xlinker","-bundle","-Xlinker","-rpath","-Xlinker","@loader_path/../../../","@/b/MultiPackageTests.product/Objects.LinkFileList","-Xlinker","-add_ast_path","-Xlinker","/b/Modules/AlphaTests.swiftmodule","-Xlinker","-add_ast_path","-Xlinker","/b/Modules/BetaTests.swiftmodule","-Xlinker","-add_ast_path","-Xlinker","/b/Modules/Core.swiftmodule","-g"]

  "PackageStructure":
    tool: package-structure-tool
"""


class Split(unittest.TestCase):
    def test_row9_link(self):
        import tempfile
        with tempfile.NamedTemporaryFile("w", suffix=".yaml", delete=False) as f:
            f.write(MANIFEST)
        try:
            args = X.link_command(f.name, "/b/MultiPackageTests.xctest/Contents/MacOS/MultiPackageTests")
            self.assertIsNone(X.link_command(f.name, "/b/Other.xctest/Contents/MacOS/Other"))
        finally:
            os.unlink(f.name)
        self.assertEqual(args[:2], ["/usr/bin/swiftc", "-L"])
        got = X.split_link_args(args, "/b/x86sim-ios/AlphaTests.LinkFileList", "/b/x86sim-ios/AlphaTests.xctest/AlphaTests",
                                {"BetaTests.swiftmodule"})
        self.assertEqual(got, [
            "/usr/bin/swiftc", "-L", "/b", "-o", "/b/x86sim-ios/AlphaTests.xctest/AlphaTests", "-Xlinker", "-bundle",
            "-Xlinker", "-rpath", "-Xlinker", "@loader_path/../../", "@/b/x86sim-ios/AlphaTests.LinkFileList",
            "-Xlinker", "-add_ast_path", "-Xlinker", "/b/Modules/AlphaTests.swiftmodule",
            "-Xlinker", "-add_ast_path", "-Xlinker", "/b/Modules/Core.swiftmodule", "-g",
            "-framework", "XCTest", "-lXCTestSwiftSupport"])
        tests = [{"name": "AlphaTests", "c99name": "AlphaTests"}, {"name": "my-tests", "c99name": "my_tests"}]
        objs = ["/b/AlphaTests.build/AlphaTests.swift.o", "/b/AlphaTests.build/resource_bundle_accessor.swift.o",
                "/b/my-tests.build/T.swift.o", "/b/Core.build/Core.swift.o", "/b/MultiPackageTests.build/runner.swift.o"]
        self.assertEqual(X.target_objects(objs, "/b", tests, "my_tests", "MultiPackageTests"), objs[2:4])
        self.assertEqual(X.target_objects(objs, "/b", tests, "AlphaTests", "MultiPackageTests"), objs[:2] + objs[3:4])


class Log(unittest.TestCase):
    def test_row10_parameterized(self):
        p = X.Parser("GammaTests")
        for l in ["\u25c7 Test positive(x:) started.",
                  "\u2718 Test positive(x:) recorded an issue with 1 argument x \u2192 0 at GammaTests.swift:7:50: "
                  "Expectation failed: x > 0",
                  "\u2718 Test positive(x:) with 2 test cases failed after 0.008 seconds with 1 issue.",
                  "\u2714 Test labelled(x:) with 2 test cases passed after 0.005 seconds.",
                  "\u2718 Test run with 2 tests in 0 suites failed after 0.013 seconds with 1 issue."]:
            p.line(l)
        got = [(t["name"], t["result"], [(f["file"], f["line"]) for f in t["failures"]]) for t in p.tests]
        self.assertEqual(got, [("positive(x:)", "failed", [("GammaTests.swift", 7)]), ("labelled(x:)", "passed", [])])


def ev(kind, comps, payload=None, t=0.0, test=None, case=None, module="GammaTests", known=None):
    e = {"instant": {"suspending": {"seconds": int(t), "attoseconds": int(round((t % 1) * 1e18))}},
         "kind": {kind: payload or {}}, "testID": {"moduleName": module, "nameComponents": comps}}
    if case is not None:
        e["testCaseID"] = case
    d = {"event": e, "test": test or {}, "time": t}
    if known is not None:
        d["known"] = known
    return json.dumps(d)


def fn(comps, line, cases=None):
    return {"name": comps[-1], "parameters": [], "id": {"nameComponents": comps},
            "sourceLocation": {"filePath": "/p/Tests/GammaTests/GammaTests.swift", "line": line},
            "testCases": cases or [{"id": {"isStable": True}, "arguments": []}]}


LOC = lambda line, col=5: {"sourceContext": {"sourceLocation": {"filePath": "/p/Tests/GammaTests/GammaTests.swift",  # noqa: E731
                                                                "fileID": "GammaTests/GammaTests.swift", "line": line, "column": col},
                                             "backtrace": [1, 2]}}


def expectation(src, subs, comments=()):
    return dict(LOC(18, 26), comments=list(comments), isKnown=False, severity={"error": {}}, kind={"expectationFailed": {
        "expectation": {"evaluatedExpression": {"kind": {"generic": {"_0": src}}, "runtimeValue": {"description": "false"},
                                                "_subexpressions": [dict({"kind": {"generic": {"_0": a}}, "_subexpressions": []},
                                                                         **({"runtimeValue": {"description": b}} if b is not None else {}))
                                                                    for a, b in subs]}}}})


ARG1 = {"bytes": list(bytes.fromhex("6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b"))}
ARG0 = {"bytes": list(bytes.fromhex("5feceb66ffc86f38d952786c6d696c79c2dbc239dd4e91b46729d73a27fb57e9"))}


def arg_case(a, v):
    return {"id": {"argIDs": [a], "discriminator": 0, "isStable": True},
            "arguments": [{"parameter": {"firstName": "x", "typeInfo": {"unqualifiedName": "Int", "fullyQualifiedName": "Swift.Int"}},
                           "value": {"description": v, "debugDescription": v, "isCollection": False,
                                     "typeInfo": {"unqualifiedName": "Int", "fullyQualifiedName": "Swift.Int"}}}]}


class SwiftTestingEvents(unittest.TestCase):
    def lines(self):
        pos = fn(["positive(x:)"], 7, [arg_case(ARG1, "1"), arg_case(ARG0, "0")])
        c1, c0 = {"argIDs": [ARG1], "discriminator": 0}, {"argIDs": [ARG0], "discriminator": 0}
        out = [ev("testDiscovered", ["First"], test={"name": "First", "id": {"nameComponents": ["First"]}}),
               ev("runStarted", [])]
        for comps, line, t in ((["First", "same()"], 12, 1.0), (["Second", "same()"], 17, 2.0),
                               (["Second", "Inner", "same()"], 22, 3.0), (["shared()"], 6, 4.0)):
            out += [ev("testStarted", comps, t=t, test=fn(comps, line)), ev("testEnded", comps, t=t + 0.25, test=fn(comps, line))]
        out += [ev("testStarted", ["Second"], test={"name": "Second", "id": {"nameComponents": ["Second"]}}),
                ev("testStarted", ["Second", "check()"], t=5.0, test=fn(["Second", "check()"], 18)),
                ev("issueRecorded", ["Second", "check()"], {"_0": expectation("Core.twice(1) == 3", [("Core.twice(1)", "2"), ("3", "3")])},
                   t=5.1, test=fn(["Second", "check()"], 18)),
                ev("testEnded", ["Second", "check()"], t=5.5, test=fn(["Second", "check()"], 18)),
                ev("testSkipped", ["Second", "off()"], {"_0": dict(LOC(19, 40), comment="off on purpose")}, t=6.0,
                   test=fn(["Second", "off()"], 19)),
                ev("testStarted", ["positive(x:)"], t=7.0, test=pos),
                ev("testCaseStarted", ["positive(x:)"], t=7.0, test=pos, case=c1),
                ev("testCaseEnded", ["positive(x:)"], t=7.1, test=pos, case=c1),
                ev("testCaseStarted", ["positive(x:)"], t=7.1, test=pos, case=c0),
                ev("issueRecorded", ["positive(x:)"], {"_0": expectation("x > 0", [("x", "0"), ("0", "0")])}, t=7.2, test=pos, case=c0),
                ev("testCaseEnded", ["positive(x:)"], t=7.3, test=pos, case=c0),
                ev("testEnded", ["positive(x:)"], t=7.5, test=pos),
                ev("runEnded", []), '{"event": {"kind": {"testStar']
        return out

    def test_row11_suites_apart(self):
        got = {t["identifier"]: t for t in X.swift_testing_results(self.lines())}
        self.assertEqual(list(got), ["First/same()", "Second/same()", "Second/Inner/same()", "shared()", "Second/check()",
                                     "Second/off()", "positive(x:)"])
        self.assertEqual([(got[i]["class"], got[i]["name"], got[i]["suite"], got[i]["result"]) for i in got][:4],
                         [("GammaTests.First", "same()", ["First"], "passed"), ("GammaTests.Second", "same()", ["Second"], "passed"),
                          ("GammaTests.Second.Inner", "same()", ["Second", "Inner"], "passed"), ("GammaTests", "shared()", [], "passed")])
        self.assertEqual(got["First/same()"]["duration"], 0.25)
        c = got["Second/check()"]
        self.assertEqual((c["result"], [(f["file"], f["line"], f["message"]) for f in c["failures"]]),
                         ("failed", [("/p/Tests/GammaTests/GammaTests.swift", 18, "Expectation failed: Core.twice(1) == 3\nCore.twice(1) → 2")]))
        o = got["Second/off()"]
        self.assertEqual((o["result"], o["skip_message"], o["skip_line"]), ("skipped", "off on purpose", 19))
        p = got["positive(x:)"]
        self.assertEqual((p["result"], [(a["name"], a["id"][:8], a["result"], len(a["failures"])) for a in p["arguments"]]),
                         ("failed", [("1", "6b86b273", "passed", 0), ("0", "5feceb66", "failed", 1)]))
        self.assertEqual(p["arguments"][0]["args"][0]["label"], "x")
        self.assertIsNone(X.swift_testing_results(['{"cut sh']))

    def test_row11_parser_replaces_console(self):
        p = X.Parser("GammaTests")
        for l in ["Test Case '-[GammaTests.XT testA]' started.", "Test Case '-[GammaTests.XT testA]' passed (0.001 seconds).",
                  "✔ Test same() passed after 0.001 seconds.", "✔ Test same() passed after 0.001 seconds."]:
            p.line(l)
        self.assertEqual([t["identifier"] for t in p.tests], ["XT/testA", "SwiftTesting/same()"])
        with tempfile.TemporaryDirectory() as d:
            f = os.path.join(d, "e.jsonl")
            with open(f, "w") as fh:
                fh.write("\n".join(self.lines()) + "\n")
            p.swift_testing(f)
            empty = os.path.join(d, "none.jsonl")
            open(empty, "w").close()
            p.swift_testing(empty)   # no events (no libsimswt): nothing changes
        self.assertEqual([t["identifier"] for t in p.tests][:4], ["XT/testA", "First/same()", "Second/same()", "Second/Inner/same()"])


class IssueText(unittest.TestCase):
    def test_row12_apple_texts(self):
        m = X.st_issue_message
        self.assertEqual(m(expectation("k() == 2", [("k()", "1"), ("2", "2")], ["with a comment"])),
                         "Expectation failed: k() == 2\nk() → 1: with a comment")
        self.assertEqual(m(expectation("flag", [])), "Expectation failed: flag")
        e = expectation("Oops.self", [("Oops", None), ("self", None)])
        e["kind"]["expectationFailed"]["expectation"]["mismatchedErrorDescription"] = "an error was expected but none was thrown"
        self.assertEqual(m(e), "Expectation failed: an error was expected but none was thrown: Oops.self\n"
                               "Oops → <not evaluated>\nself → <not evaluated>")
        self.assertEqual(m({"kind": {"errorCaught": {"error": {"description": "Oops()"}}}, "comments": []}), "Caught error: Oops()")
        self.assertEqual(m({"kind": {"unconditional": {}}, "comments": ["a note"]}), "Issue recorded: a note")
        self.assertEqual(m({"kind": {"unconditional": {}}, "comments": []}), "Issue recorded")


class ThrowLocation(unittest.TestCase):
    def test_row13_frame(self):
        with tempfile.TemporaryDirectory() as d:
            b = os.path.join(d, "App.app/PlugIns/T.xctest/T")
            os.makedirs(os.path.dirname(b))
            open(b, "w").close()
            asked = []

            def sym(binary, offsets):
                asked.append((binary, offsets))
                return [None, ("/src/T.swift", 35, "f")]
            old = X.symbolize
            X.symbolize = sym
            try:
                other = [os.path.join(d, n) for n in ("XCTestCore", "libswiftCore.dylib", "CoreFoundation")]
                for o in other:   # images that are there too: only the test bundle's frames count
                    open(o, "w").close()
                frames = [[X.dv(other[0]), 100], [X.dv(other[1]), 200], [X.dv(b), 0x269b], [X.dv(b), 0x2753],
                          [X.dv(other[2]), 5]]
                self.assertEqual(X.throw_location(frames), ("/src/T.swift", 35))
                self.assertEqual(asked, [(b, [0x269a, 0x2752])])
                p = X.Parser("T")
                p.line("Test Case '-[T.HostTests testThrow]' started.")
                p.line('<unknown>:0: error: -[T.HostTests testThrow] : failed: caught error: "Oops()"')
                f = os.path.join(d, "i.jsonl")
                with open(f, "w") as fh:
                    fh.write(json.dumps({"test": "-[HostTests testThrow]", "message": 'failed: caught error: "Oops()"',
                                         "frames": frames}) + "\n")
                p.issue_stacks(f)
                self.assertEqual([(x["file"], x["line"]) for x in p.tests[0]["failures"]], [("/src/T.swift", 35)])
                X.symbolize = lambda binary, offsets: [None] * len(offsets)
                self.assertIsNone(X.throw_location(frames))
                self.assertIsNone(X.throw_location([[X.dv(other[0]), 100]]))
            finally:
                X.symbolize = old


class KnownIssues(unittest.TestCase):
    def test_row14_swift_testing(self):
        def known(src, subs, comment):   # an issue inside withKnownIssue: isKnown, libsimswt's "known" comment
            e = expectation(src, subs)
            e["isKnown"] = True
            return {"_0": e}, comment
        pos = fn(["each(x:)"], 40, [arg_case(ARG1, "1"), arg_case(ARG0, "0")])
        c1, c0 = {"argIDs": [ARG1], "discriminator": 0}, {"argIDs": [ARG0], "discriminator": 0}
        lines = []
        for comps, issues in ((["knownIssue()"], [known("Core.twice(1) == 3", [("Core.twice(1)", "2"), ("3", "3")], "twice is off by one")]),
                              (["knownIssueNotRecorded()"], [({"_0": dict(LOC(14, 20), comments=[], isKnown=False, severity={"error": {}},
                                                                         kind={"knownIssueNotRecorded": True})}, None)]),
                              (["Known", "partly()"], [known("Core.twice(2) == 5", [("Core.twice(2)", "4")], ""),
                                                       ({"_0": expectation("Core.twice(3) == 7", [("Core.twice(3)", "6")])}, None)])):
            lines.append(ev("testStarted", comps, test=fn(comps, 8)))
            lines += [ev("issueRecorded", comps, p, test=fn(comps, 8), known=k) for p, k in issues]
            lines.append(ev("testEnded", comps, test=fn(comps, 8)))
        lines += [ev("testStarted", ["each(x:)"], test=pos),
                  ev("testCaseStarted", ["each(x:)"], test=pos, case=c1), ev("testCaseEnded", ["each(x:)"], test=pos, case=c1),
                  ev("testCaseStarted", ["each(x:)"], test=pos, case=c0),
                  ev("issueRecorded", ["each(x:)"], known("x > 0", [("x", "0")], "zero")[0], test=pos, case=c0, known="zero"),
                  ev("testCaseEnded", ["each(x:)"], test=pos, case=c0), ev("testEnded", ["each(x:)"], test=pos)]
        got = {t["identifier"]: t for t in X.swift_testing_results(lines)}
        k = got["knownIssue()"]
        self.assertEqual((k["result"], k["failures"], [(f["line"], f["message"], f["reason"]) for f in k["expected_failures"]]),
                         ("expected_failure", [], [(18, "Expectation failed: Core.twice(1) == 3\nCore.twice(1) → 2", "twice is off by one")]))
        n = got["knownIssueNotRecorded()"]
        self.assertEqual((n["result"], [(f["line"], f["message"]) for f in n["failures"]], n.get("expected_failures")),
                         ("failed", [(14, "Known issue was not recorded")], None))
        p = got["Known/partly()"]
        self.assertEqual((p["result"], len(p["failures"]), [f["reason"] for f in p["expected_failures"]]), ("failed", 1, [None]))
        e = got["each(x:)"]
        self.assertEqual((e["result"], [a["result"] for a in e["arguments"]]), ("expected_failure", ["passed", "expected_failure"]))

    def test_row14_xctest_and_console(self):
        p = X.Parser("DeltaTests")
        for l in ["Test Case '-[DeltaTests.DeltaXCTests testExpectFailure]' started.",
                  'DeltaTests.swift:45: Expected failure in -[DeltaTests.DeltaXCTests testExpectFailure]: XCTAssertEqual failed: '
                  '("2") is not equal to ("3")Reason: (known bug)',
                  "Test Case '-[DeltaTests.DeltaXCTests testExpectFailure]' passed (0.915 seconds).",
                  "Test Case '-[DeltaTests.DeltaXCTests testBlock]' started.",
                  "DeltaTests.swift:50: Expected failure in -[DeltaTests.DeltaXCTests testBlock]: failed - inside the blockReason: (block form)",
                  "Test Case '-[DeltaTests.DeltaXCTests testBlock]' passed (0.009 seconds).",
                  "━ Test thrown() recorded a known issue at DeltaTests.swift:29:24: Caught error: Oops()",
                  "━ Test thrown() passed after 0.008 seconds with 1 known issue."]:
            self.assertTrue(p.line(l), l)
        with tempfile.TemporaryDirectory() as d:
            f = os.path.join(d, "i.jsonl")
            with open(f, "w") as fh:   # libsimswt's line for the first one only: the second keeps the console's
                fh.write(json.dumps({"test": "-[DeltaXCTests testExpectFailure]", "message": 'XCTAssertEqual failed: ("2") is not equal to ("3")',
                                     "expected": True, "reason": "known bug", "file": "/p/Tests/DeltaTests/DeltaTests.swift",
                                     "line": 45, "frames": []}) + "\n")
            p.issue_stacks(f)
        got = {t["identifier"]: t for t in p.tests}
        self.assertEqual([(i, t["result"], [(x["file"], x["line"], x["message"], x["reason"]) for x in t["expected_failures"]])
                          for i, t in got.items()],
                         [("DeltaXCTests/testExpectFailure", "expected_failure",
                           [("/p/Tests/DeltaTests/DeltaTests.swift", 45, 'XCTAssertEqual failed: ("2") is not equal to ("3")', "known bug")]),
                          ("DeltaXCTests/testBlock", "expected_failure", [("DeltaTests.swift", 50, "failed - inside the block", "block form")]),
                          ("SwiftTesting/thrown()", "expected_failure", [("DeltaTests.swift", 29, "Caught error: Oops()", None)])])
        run = type("R", (), {"phone": "p", "dev": "/d", "rt": "ios-27.2-24B8117-arm64", "plat": "ios", "out": "/o"})()
        s = X.summary(run, [{"name": "DeltaTests", "result": "passed", "duration": 1.0, "tests": p.tests}], 0)
        self.assertEqual((s["result"], s["counts"]), ("passed", {"passed": 0, "failed": 0, "skipped": 0, "expected_failures": 3, "total": 3}))
        j = X.junit(s)
        self.assertIn('failures="0"', j)
        self.assertNotIn("<failure", j)
        self.assertIn("Expected failure (known bug): /p/Tests/DeltaTests/DeltaTests.swift:45: XCTAssertEqual failed", j)


class CallStacks(unittest.TestCase):
    MACRO = "/tmp/swift-generated-sources/@__swiftmacro_10GammaTests0021GammaTestsswift_IfFDefMX6_49_6expectfMf_.swift"

    def test_row15_source_frame(self):
        sf = X.source_frame
        # llvm-symbolizer's chain for positive(x:)'s #expect (a run's, innermost first)
        self.assertEqual(sf([{"FunctionName": "$s10GammaTests0021GammaTestsswift_IfFDefMX6_49_6expectfMf_", "FileName": self.MACRO, "Line": 1},
                             {"FunctionName": "$s10GammaTests8positive1xySi_tF", "FileName": "/p/GammaTests.swift", "Line": 7}]),
                         ("/p/GammaTests.swift", 7, "$s10GammaTests8positive1xySi_tF"))
        self.assertIsNone(sf([{"FunctionName": "c", "FileName": self.MACRO, "Line": 1}]))   # knownIssue()'s closure
        self.assertIsNone(sf([{"FunctionName": "t", "FileName": "/<compiler-generated>", "Line": 0}]))
        self.assertIsNone(sf([{"FunctionName": "t", "FileName": "/p/x.swift", "Line": 0}]))

    def with_symbols(self, table):
        """X.symbolize / X.demangle replaced: table {(binary name, offset): (file, line, linkage name)}"""
        asked = []

        def sym(binary, offsets):
            asked.append((os.path.basename(binary), list(offsets)))
            return [table.get((os.path.basename(binary), o)) for o in offsets]
        old = X.symbolize, X.demangle
        X.symbolize, X.demangle = sym, lambda names: {n: "dm(%s)" % n for n in names if n}
        self.addCleanup(lambda: setattr(X, "symbolize", old[0]) or setattr(X, "demangle", old[1]))
        return asked

    def test_row15_stacks(self):
        with tempfile.TemporaryDirectory() as d:
            b = os.path.join(d, "DeltaTests.xctest/DeltaTests")
            os.makedirs(os.path.dirname(b))
            open(b, "w").close()
            core = os.path.join(d, "XCTestCore")
            open(core, "w").close()
            asked = self.with_symbols({("DeltaTests", 0x10): ("/p/D.swift", 66, "oops"),
                                       ("DeltaTests", 0x1f): ("/p/D.swift", 62, "thrown")})
            frames = [[X.dv(core), 100, 5000], [X.dv(b), 0x10, 6000], [X.dv(b), 0x20, 7000], [X.dv(b), 0x30, 8000], [None, 9, 9]]
            st = X.call_stacks([frames])[0]
            self.assertEqual(st, [{"address": 5000}, {"address": 5999}, {"address": 6999, "image": "DeltaTests", "symbol": "dm(thrown)",
                                                                       "file": "/p/D.swift", "line": 62},
                                  {"address": 7999}, {"address": 8}])
            self.assertEqual(asked, [("DeltaTests", [0xf, 0x1f, 0x2f])])
            # XCTest: an XCTExpectFailure matched by a thrown error (console line without a location), a located failure
            p = X.Parser("DeltaTests")
            for l in ["Test Case '-[DeltaTests.DeltaXCTests testThrown]' started.",
                      'Expected failure in -[DeltaTests.DeltaXCTests testThrown]: failed: caught error: "Oops()"Reason: (thrown on purpose)',
                      "Test Case '-[DeltaTests.DeltaXCTests testThrown]' passed (0.008 seconds).",
                      "Test Case '-[DeltaTests.DeltaXCTests testFail]' started.",
                      "/p/D.swift:45: error: -[DeltaTests.DeltaXCTests testFail] : XCTAssertEqual failed: (\"2\") is not equal to (\"3\")",
                      "Test Case '-[DeltaTests.DeltaXCTests testFail]' failed (0.008 seconds)."]:
                self.assertTrue(p.line(l), l)
            got = {t["identifier"]: t for t in p.tests}
            X.Parser.split_reasons(p.tests)   # the console's alone (no libsimswt)
            self.assertEqual((got["DeltaXCTests/testThrown"]["result"], [(x["file"], x["message"], x["reason"]) for x in
                                                                          got["DeltaXCTests/testThrown"]["expected_failures"]]),
                             ("expected_failure", [(None, 'failed: caught error: "Oops()"', "thrown on purpose")]))
            th = [[X.dv(core), 100, 5000], [X.dv(b), 0x11, 6001], [X.dv(b), 0x20, 7000]]
            f = os.path.join(d, "i.jsonl")
            with open(f, "w") as fh:
                fh.write(json.dumps({"test": "-[DeltaXCTests testThrown]", "message": 'failed: caught error: "Oops()"', "expected": True,
                                     "reason": "thrown on purpose", "frames": th}) + "\n")
                fh.write(json.dumps({"test": "-[DeltaXCTests testFail]", "message": 'XCTAssertEqual failed: ("2") is not equal to ("3")',
                                     "file": "/p/D.swift", "line": 45, "frames": th}) + "\n")
            p.issue_stacks(f)
            x = got["DeltaXCTests/testThrown"]["expected_failures"]
            self.assertEqual((got["DeltaXCTests/testThrown"]["result"], [(e["file"], e["line"]) for e in x]),
                             ("expected_failure", [("/p/D.swift", 66)]))
            self.assertEqual([(s.get("symbol"), s.get("line")) for s in x[0]["stack"]], [(None, None), ("dm(oops)", 66), ("dm(thrown)", 62)])
            self.assertEqual([s.get("symbol") for s in got["DeltaXCTests/testFail"]["failures"][0]["stack"]], [None, "dm(oops)", "dm(thrown)"])

    def test_row15_swift_testing(self):
        e = expectation("Core.twice(1) == 3", [("Core.twice(1)", "2"), ("3", "3")])
        x = X.st_expression(e)
        self.assertEqual((x["source"], x["value"]["description"], [(c["source"], c["value"]["description"]) for c in x["subexpressions"]]),
                         ("Core.twice(1) == 3", "false", [("Core.twice(1)", "2"), ("3", "3")]))
        self.assertIsNone(X.st_expression({"kind": {"errorCaught": {}}}))
        with tempfile.TemporaryDirectory() as d:
            b = os.path.join(d, "GammaTests.xctest/GammaTests")
            os.makedirs(os.path.dirname(b))
            open(b, "w").close()
            self.with_symbols({("GammaTests", 0x2f): ("/p/G.swift", 18, "check")})
            check = fn(["Second", "check()"], 18)
            lines = [ev("testStarted", ["Second", "check()"], t=1.0, test=check),
                     json.dumps(dict(json.loads(ev("issueRecorded", ["Second", "check()"], {"_0": e}, t=1.1, test=check)),
                                     frames=[[None, 1, 1], [X.dv(b), 0x30, 900]])),
                     ev("testEnded", ["Second", "check()"], t=1.2, test=check)]
            f = os.path.join(d, "e.jsonl")
            with open(f, "w") as fh:
                fh.write("\n".join(lines) + "\n")
            p = X.Parser("GammaTests")
            p.swift_testing(f)
        fail = p.tests[0]["failures"][0]
        self.assertEqual(fail["stack"], [{"address": 1}, {"address": 899, "image": "GammaTests", "symbol": "dm(check)",
                                                          "file": "/p/G.swift", "line": 18}])
        self.assertNotIn("_frames", fail)
        import xcresult
        fr = xcresult.call_frame(fail["stack"][1])
        self.assertEqual((fr.f["addressString"].v, fr.f["symbolInfo"].f["symbolName"].v,
                          fr.f["symbolInfo"].f["location"].f["lineNumber"].v), ("899", "dm(check)", "18"))
        self.assertIsNone(xcresult.call_frame(fail["stack"][0]).f["symbolInfo"])
        xe = xcresult.expression(fail["expression"])
        self.assertEqual((xe.t, xe.f["sourceCode"].v, [c.f["sourceCode"].v for c in xe.f["subexpressions"]],
                          xe.f["subexpressions"][0].f["value"].f["description"].v), ("TestExpression", "Core.twice(1) == 3",
                                                                                     ["Core.twice(1)", "3"], "2"))


class RunnerFrames(unittest.TestCase):
    def test_row16_runner_frames(self):
        with tempfile.TemporaryDirectory() as d:
            app = os.path.join(d, "UITests-Runner.app")
            b = os.path.join(app, "PlugIns/UITests.xctest/UITests")
            os.makedirs(os.path.dirname(b))
            for f in (b, os.path.join(app, "UITests-Runner"), os.path.join(app, "Other.debug.dylib"), os.path.join(d, "XCTestCore")):
                open(f, "w").close()
            import plistlib
            with open(os.path.join(app, "Info.plist"), "wb") as f:
                plistlib.dump({"CFBundleExecutable": "UITests-Runner"}, f)
            asked = []
            old = X.symbolize, X.demangle, X.symbol_names
            X.symbolize = lambda binary, offs: [("/p/U.swift", 49, "$sU") if os.path.basename(binary) == "UITests" else None for o in offs]
            X.demangle = lambda names: {n: "dm(%s)" % n for n in names if n}

            def names(binary, offs):
                asked.append((os.path.basename(binary), list(offs)))
                return [{0x1df1: "_XCTRunnerRunTests", 0x2115: "main"}.get(o) for o in offs]
            X.symbol_names = names
            self.addCleanup(lambda: (setattr(X, "symbolize", old[0]), setattr(X, "demangle", old[1]), setattr(X, "symbol_names", old[2])))
            frames = [[X.dv(os.path.join(d, "XCTestCore")), 5, 105], [X.dv(b), 0x10, 0x110], [X.dv(os.path.join(app, "Other.debug.dylib")), 7, 207],
                      [X.dv(os.path.join(app, "UITests-Runner")), 0x1df2, 0x101df2], [X.dv(os.path.join(app, "UITests-Runner")), 0x2116, 0x102116]]
            st = X.call_stacks([frames])[0]
        self.assertEqual([(f.get("image"), f.get("symbol"), f.get("line")) for f in st],
                         [(None, None, None), ("UITests", "dm($sU)", 49), (None, None, None),
                          ("UITests-Runner", "dm(_XCTRunnerRunTests)", None), ("UITests-Runner", "dm(main)", None)])
        self.assertNotIn("file", st[3])
        self.assertEqual(asked, [("UITests-Runner", [0x1df1, 0x2115])])
        self.assertFalse(X.symbolicated(st[3:]))
        self.assertTrue(X.symbolicated(st))
        import xcresult
        fr = xcresult.call_frame(st[3])
        self.assertEqual((fr.f["symbolInfo"].f["imageName"].v, fr.f["symbolInfo"].f["symbolName"].v, fr.f["symbolInfo"].f["location"]),
                         ("UITests-Runner", "dm(_XCTRunnerRunTests)", None))


class UIActivities(unittest.TestCase):
    def test_row17_activity_tree(self):
        U = ["%08X-0000-0000-0000-000000000000" % i for i in range(6)]
        with tempfile.TemporaryDirectory() as d:
            raw = os.path.join(d, "attachments", ".raw-UI")
            os.makedirs(raw)
            t = {"class": "UITests.UITests", "name": "testFail", "identifier": "UITests/testFail", "result": "failed",
                 "duration": 1.0, "failures": [{"file": "/p/U.swift", "line": 49, "message": "XCTAssertTrue failed"}]}
            acts = [("Checking existence", U[2], U[1], 10.2), ("Waiting 2.0s", U[1], None, 10.1), ("Start Test at 2026-11-28 01:00:00.000", U[0], None, 10.0),
                    ("Requesting snapshot", U[4], U[3], 10.5), ("Collecting debug information to assist test failure triage", U[3], None, 10.4)]
            with open(os.path.join(raw, "activities.jsonl"), "w") as f:
                for title, u, p, st in acts:
                    f.write(json.dumps({"test": "-[UITests.UITests testFail]", "uuid": u, "parent": p, "title": title,
                                        "type": "com.apple.dt.xctest.activity-type.internal", "start": st, "finish": st + 0.05}) + "\n")
            atts = [("Debug description", "public.plain-text", U[1], "txt"), ("App UI hierarchy for com.x.y", "public.plain-text", U[3], "txt"),
                    ("kXCTAttachmentLegacySnapshot", "com.apple.dt.xctest.element-snapshot", U[3], "bin")]
            with open(os.path.join(raw, "index.jsonl"), "w") as f:
                for i, (n, uti, au, ext) in enumerate(atts):
                    open(os.path.join(raw, "%d.%s" % (i, ext)), "w").write(n)
                    f.write(json.dumps({"test": "-[UITests.UITests testFail]", "name": n, "uti": uti, "lifetime": 1,
                                        "from": "activity", "activity": au, "seq": i, "file": "%d.%s" % (i, ext)}) + "\n")
            X.Runner.attachments(X.Runner.__new__(X.Runner), raw, [t])
            tree = t["activities"]
            self.assertEqual([(n["title"][:12], [c["title"] for c in n["children"]]) for n in tree],
                             [("Start Test a", []), ("Waiting 2.0s", ["Checking existence"]),
                              ("Collecting d", ["Requesting snapshot"])])
            self.assertEqual([a.get("activity") for a in t["attachments"]], [U[1], U[3], U[3]])
        self.assertIn('args += ["-DisableDiagnosticScreenshots", "YES"]', open(X.__file__).read())

    def test_row17_writer(self):
        import xcresult
        with tempfile.TemporaryDirectory() as d:
            files = {}
            for n in ("dd", "ui", "snap", "rec"):
                files[n] = os.path.join(d, n)
                open(files[n], "w").write(n)
            U = ["A%07d-0000-0000-0000-000000000000" % i for i in range(4)]
            t = {"class": "UITests.UITests", "name": "testFail", "identifier": "UITests/testFail", "result": "failed", "duration": 1.0,
                 "failures": [{"file": "/p/U.swift", "line": 49, "message": "XCTAssertTrue failed"}],
                 "activities": [{"uuid": U[0], "title": "Start Test at x", "type": "t", "start": 10.0, "finish": 10.1, "children": []},
                                {"uuid": U[1], "title": "Waiting", "type": "t", "start": 10.2, "finish": 10.5, "children": [
                                    {"uuid": U[2], "title": "Checking", "type": "t", "start": 10.3, "finish": 10.4, "children": []}]},
                                {"uuid": U[3], "title": "Collecting", "type": "t", "start": 10.6, "finish": 10.7, "children": []}],
                 "attachments": [{"name": "Debug description", "path": files["dd"] + ".txt", "type": "public.plain-text", "from": "activity", "activity": U[2]},
                                 {"name": "App UI hierarchy for com.x.y", "path": files["ui"] + ".txt", "type": "public.plain-text", "from": "activity", "activity": U[3]},
                                 {"name": "kXCTAttachmentLegacySnapshot", "path": files["snap"] + ".bin", "type": "com.apple.dt.xctest.element-snapshot", "from": "activity", "activity": U[3]},
                                 {"name": "Screen Recording", "path": files["rec"] + ".mp4", "type": "public.mpeg-4", "from": "x86sim"}]}
            for a in t["attachments"]:
                os.rename(files[os.path.splitext(os.path.basename(a["path"]))[0]], a["path"])
            w = xcresult.Writer.__new__(xcresult.Writer)
            w.st, w.project, w.issues = xcresult.Store(os.path.join(d, "b.xcresult")), "P", []
            objs = []
            put = w.st.obj
            w.st.obj = lambda o: objs.append(o) or put(o)
            w.test_summary({"name": "UITests"}, t, 10.0, {})
        summ = next(o for o in objs if o.t == "ActionTestSummary")

        def tree(acts):
            return [(a.f["title"].v, a.f["uuid"].v, [(x.f["name"].v, x.f["inActivityIdentifier"].v, x.f["filename"].v) for x in a.f["attachments"]],
                     tree(a.f["subactivities"])) for a in acts]
        self.assertEqual(tree(summ.f["activitySummaries"]), [
            ("Start Test at x", U[0], [("kXCTAttachmentScreenRecording", "0", "kXCTAttachmentScreenRecording_0_%s.mp4" % U[0])], []),
            ("Waiting", U[1], [], [("Checking", U[2], [("Debug description", "0", "Debug description_0_%s.txt" % U[2])], [])]),
            ("Collecting", U[3], [("App UI hierarchy for com.x.y", "0", "App UI hierarchy for com.x_0_%s.y" % U[3]),
                                  ("kXCTAttachmentLegacySnapshot", "1", "Snapshot_1_%s" % U[3])], [])])
        self.assertEqual(summ.f["failureSummaries"][0].f["attachments"], [])


class StructuredValues(unittest.TestCase):
    def test_row18_children(self):
        def v(desc, t, label=None, children=None, coll=False):
            r = {"description": desc, "debugDescription": desc, "isCollection": coll, "typeInfo": {"unqualifiedName": t, "fullyQualifiedName": "Swift." + t}}
            if label is not None:
                r["label"] = label
            if children is not None:
                r["children"] = children
            return r

        def issue(src, subs):
            return {"kind": {"expectationFailed": {"expectation": {"evaluatedExpression": {
                "kind": {"generic": {"_0": src}}, "runtimeValue": v("false", "Bool"),
                "_subexpressions": [{"kind": {"generic": {"_0": s}}, "runtimeValue": rv, "_subexpressions": []} for s, rv in subs]}}}}}
        point = v("Point(x: 1, y: 2)", "Point", children=[v("1", "Int", "x"), v("2", "Int", "y")])
        arr = v("[1, 2]", "Array<Int>", children=[v("1", "Int"), v("2", "Int")], coll=True)
        pair = v('(key: "b", value: 2)', "(key: String, value: Int)", children=[v('"b"', "String", "key"), v("2", "Int", "value")])
        dic = v('["b": 2]', "Dictionary<String, Int>", children=[pair], coll=True)
        opt = v("2", "Optional<Int>", children=[v("2", "Int", "some")])
        x = X.st_expression(issue("a == b", [("Point(x: 1, y: Core.twice(1))", point), ("[1, 2]", arr), ('["b": 2]', dic), ("v", opt)]))

        def shape(e):
            return (e["source"], [shape(c) for c in e["subexpressions"]])
        self.assertEqual(shape(x), ("a == b", [("Point(x: 1, y: Core.twice(1))", [("x", []), ("y", [])]), ("[1, 2]", []),
                                               ('["b": 2]', []), ("v", [("some", [])])]))
        pv = x["subexpressions"][0]["value"]
        self.assertEqual([(c["label"], c["description"]) for c in pv["children"]], [("x", "1"), ("y", "2")])
        dv = x["subexpressions"][2]["value"]
        self.assertEqual((dv["is_collection"], "label" in dv["children"][0], [c["label"] for c in dv["children"][0]["children"]]),
                         (True, False, ["key", "value"]))
        self.assertNotIn("children", x["value"])
        self.assertEqual(X.st_issue_message(issue("v == nil", [("v", opt), ("nil", v("nil", "_OptionalNilComparisonType"))])),
                         "Expectation failed: v == nil\nv → 2")
        import xcresult
        tv = xcresult.test_value(dv)
        kid = tv.f["children"][0]
        self.assertEqual((kid.f["label"], [c.f["label"].v for c in kid.f["children"]], xcresult.test_value(x["value"]).f["children"]),
                         (None, ["key", "value"], None))


class Repetitions(unittest.TestCase):
    # xcodebuild -retry-tests-on-failure / -test-iterations 3 on Xcode 27.2: XCTest's lines, Apple's verdicts
    LOG = """Test Case '-[L.FlakyTests testFlaky]' started (Iteration 1 of 3).
/s/L.swift:29: error: -[L.FlakyTests testFlaky] : XCTAssertGreaterThan failed - fails on its first run
Test Case '-[L.FlakyTests testFlaky]' failed (1.000 seconds).
Test Case '-[L.FlakyTests testFlaky]' started (Iteration 2 of 3).
Test Case '-[L.FlakyTests testFlaky]' passed (0.001 seconds).
Test Case '-[L.LogicTests testSort]' started (Iteration 1 of 3).
Test Case '-[L.LogicTests testSort]' passed (0.001 seconds).
"""

    def runs(self, mode):
        p = X.Parser("L")
        for ln in self.LOG.splitlines():
            p.line(ln)
        p.close(mode)
        return {t["name"]: t for t in p.tests}

    def test_row19_retry_and_iterations(self):
        t = self.runs("RunRetryOnFailure")
        self.assertEqual([(r["iteration"], r["result"], len(r["failures"])) for r in t["testFlaky"]["iterations"]],
                         [(1, "failed", 1), (2, "passed", 0)])
        self.assertEqual((t["testFlaky"]["result"], t["testFlaky"]["repetition"]), ("passed", {"mode": "RunRetryOnFailure", "total": 3}))
        self.assertEqual(t["testSort"]["result"], "passed")
        self.assertEqual(self.runs("FixedIterations")["testFlaky"]["result"], "failed")   # any run failed
        cfg = X.configuration("/b/T.xctest", "M", opts={"retry": True})
        self.assertIn(b"XCTRepetitionMode", cfg)
        self.assertEqual(X.repetition({"until_failure": True, "iterations": 5}), (1, 5, "RunUntilFailure"))
        self.assertEqual(X.repetition({"iterations": 2}), (3, 2, "FixedIterations"))


if __name__ == "__main__":
    unittest.main()
