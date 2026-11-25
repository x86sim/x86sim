#!/bin/bash
# x86Sim: run XCTestDemo's tests on a phone with `x86sim test` and check the outcome is Apple's (Xcode 27.2, iOS 27.2
# simulator on a Mac, see tests/apps/README.md): every test target, the deliberate failures failing with their
# file:line, the skip skipped. Exit 0 when it matches. On every platform (the phone's: iPhone / iPad, Apple TV, Apple
# Watch, Vision Pro), UI tests included unless --no-ui.
#   tests/apps/XCTestDemo/run.sh <phone> [--no-build] [--no-ui]
# UI tests need the phone's xctest feature (x86sim-aot --device <phone> features set xctest on; restart the phone).
set -uo pipefail
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../../.." && pwd)}
. "$X86SIM_ROOT/cli/lib/config.sh"
phone=${1:?usage: run.sh <phone> [--no-build] [--no-ui]}; shift
build=1 ui=1
for a in "$@"; do case $a in --no-build) build=0 ;; --no-ui) ui=0 ;; esac; done
# the phone's platform picks the build (build.sh X86SIM_PLATFORM): iOS, tvOS, watchOS or visionOS
plat=$(cut -d- -f1 "$X86SIM_ROOT/var/device-a64-$phone/runtime" 2>/dev/null); plat=${plat:-ios}
P=$X86SIM_SCRATCH/apps/XCTestDemo-products$([ "$plat" = ios ] || echo "-$plat")
[ "$build" = 1 ] && { X86SIM_PLATFORM=$plat "$(dirname "$0")/build.sh" "$P" || exit 1; }
OUT=$X86SIM_SCRATCH/xctest/XCTestDemo-$(date +%Y%m%d-%H%M%S)
skip=(); [ "$ui" = 0 ] && skip=(--skip-testing XCTestDemoUITests)
"$X86SIM_ROOT/cli/x86sim" test "$phone" --xctestrun "$(ls "$P"/*.xctestrun | head -1)" --out "$OUT" "${skip[@]}"
echo "exit status $? (1 expected: the deliberate failures)"
python3 - "$OUT/results.json" "$ui" "$plat" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
want = {  # Apple's simulator: target -> {test: result}
    "XCTestDemoTests": {"CalcTests/testAdd": "passed", "CalcTests/testAsyncExpectation": "passed",
                        "CalcTests/testDeliberateFailure": "failed", "CalcTests/testGreeting": "passed",
                        "CalcTests/testSkipped": "skipped", "HostTests/testDeliberateThrow": "failed",
                        "HostTests/testRunsInsideHostApp": "passed"},
    "XCTestDemoLogicTests": {"LogicTests/testDeliberateFailure": "failed", "LogicTests/testNoHostApp": "passed",
                             "LogicTests/testSort": "passed", "LogicTests/testString": "passed"},
    "XCTestDemoUITests": {"XCTestDemoUITests/testDeliberateUIFailure": "failed", "XCTestDemoUITests/testIncrement": "passed",
                          "XCTestDemoUITests/testTypeName": "passed"},
}
# Apple's watchOS and visionOS 27.2 simulators fail testTypeName (watch: "Neither element nor any descendant has
# keyboard focus"; visionOS: typeText leaves the greeting "Hello!"); tvOS matches iOS (2026-11-24, Xcode 27.2)
if sys.argv[3] in ("watchos", "xros"):
    want["XCTestDemoUITests"]["XCTestDemoUITests/testTypeName"] = "failed"
if sys.argv[2] == "0":
    del want["XCTestDemoUITests"]
lines = {"CalcTests/testDeliberateFailure": 15, "LogicTests/testDeliberateFailure": 8, "XCTestDemoUITests/testDeliberateUIFailure": 49}
bad = 0
got = {r["name"]: r for r in s["targets"]}
for tgt, tests in want.items():
    r = got.get(tgt)
    if not r:
        print("MISSING target", tgt); bad += 1; continue
    res = {t["identifier"]: t for t in r["tests"]}
    for k, v in tests.items():
        t = res.get(k)
        ok = t is not None and t["result"] == v
        if ok and k in lines:
            ok = any(f.get("line") == lines[k] for f in t["failures"])
        print("%-4s %s/%s: %s%s" % ("ok" if ok else "BAD", tgt, k, t["result"] if t else "missing", "" if ok else " (want %s)" % v))
        bad += not ok
print("XCTestDemo: %s" % ("matches Apple's simulator" if not bad else "%d differences" % bad))
sys.exit(1 if bad else 0)
PY
