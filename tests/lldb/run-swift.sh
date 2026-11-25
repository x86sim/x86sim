#!/bin/bash
# x86Sim: Swift debugging end to end (x86sim debug with the Swift toolchain's lldb, cli/libexec/swiftlldb.py). The phone
# must be running.
#   tests/lldb/run-swift.sh PHONE [--no-build]
# swiftdbg (tests/lldb/swiftdbg.swift, -g with a dSYM by build-swift-app.sh), attached while it runs: break on a line of
# swiftTick, bt (demangled Swift frames), frame variable of a struct / array / String / Dictionary / Optional / enum,
# po of a struct and of a class instance, expression of Swift expressions; detach. Then the same through the MCP-style
# background session (--start / --cmd / --stop), which runs under lldb's own Python.
# Every check prints "X86TEST lldb-swift <check> PASS|FAIL"; exit status 1 if one failed. Output in
# $X86SIM_SCRATCH/lldb-test/.
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}; . "$X86SIM_ROOT/cli/lib/config.sh"
PHONE=$1; shift || { echo "usage: $0 PHONE [--no-build]" >&2; exit 2; }
build=1
for a; do case $a in --no-build) build=0 ;; esac; done
X=$X86SIM_ROOT/cli/x86sim
OUT=$X86SIM_SCRATCH/lldb-test; mkdir -p "$OUT"
APP=$X86SIM_SCRATCH/apps/swiftdbg.app DS=$X86SIM_SCRATCH/apps/swiftdbg.app.dSYM
[ $build = 1 ] && { "$X86SIM_ROOT/tests/lldb/build-swift-app.sh" >&2 || exit 1; }
P() { "$X" --phone "$PHONE" "$@"; }
python3 "$X86SIM_ROOT/cli/libexec/swiftlldb.py" setup > "$OUT/swift-setup.txt" 2>&1 ||
	{ echo "X86TEST lldb-swift setup FAIL (see $OUT/swift-setup.txt)"; exit 1; }
echo "X86TEST lldb-swift setup PASS"
P install "$APP" >&2 || exit 1
P launch --terminate-running-process com.x86sim.test.swiftdbg > /dev/null 2>&1; sleep 5
fail=0
check() {  # check NAME FILE REGEX...: every regex is found in FILE
	local name=$1 f=$2; shift 2
	for re; do grep -qE -- "$re" "$f" || { echo "X86TEST lldb-swift $name FAIL (no /$re/ in $f)"; fail=1; return; }; done
	echo "X86TEST lldb-swift $name PASS"
}
L=$(grep -n '_ = (greeting' "$X86SIM_ROOT/tests/lldb/swiftdbg.swift" | cut -d: -f1)
timeout 400 "$X" --phone "$PHONE" debug com.x86sim.test.swiftdbg --symbols "$DS" --batch \
	-o "version" -o "breakpoint set -f swiftdbg.swift -l $L" -o "continue" -o "bt 4" \
	-o "frame variable point numbers greeting table maybe mode" -o "po point" -o "po counter" \
	-o "expression point.x + 41" -o "expression greeting.count" -o "expression counter.history.count > 0" \
	-o "breakpoint delete -f" -o "detach" > "$OUT/swift.txt" 2>&1
check swift-lldb "$OUT/swift.txt" "Swift version"
check breakpoint "$OUT/swift.txt" "stop reason = breakpoint 1\.1" "swiftdbg\`swiftTick\(n=[0-9]+\) at swiftdbg\.swift:[0-9]+"
check bt-demangled "$OUT/swift.txt" "closure #1 in SceneDelegate\.scene"
check frame-variable "$OUT/swift.txt" "\(swiftdbg\.Point\) point = \(x = [0-9]+, y = [0-9.]+, label = \"p[0-9]+\"\)" \
	"\(String\) greeting = \"hello swift [0-9]+\"" "\[0\] = 1" "\(\[String : Int\]\) table = 2 key/value pairs" \
	"\(swiftdbg\.Mode\) mode = running"
check po-struct "$OUT/swift.txt" "▿ Point|x : [0-9]+"
check po-object "$OUT/swift.txt" "Counter\(ticks, hits: [0-9]+\)"
check expression "$OUT/swift.txt" "\(Int\) \\\$R[0-9]+ = [0-9]+" "\(Bool\) \\\$R[0-9]+ = true"
check detach "$OUT/swift.txt" "Process [0-9]+ detached"
sleep 2
P pidof com.x86sim.test.swiftdbg > /dev/null && echo "X86TEST lldb-swift runs-after-detach PASS" || { echo "X86TEST lldb-swift runs-after-detach FAIL"; fail=1; }
# the background session (MCP debug_start / debug_command / debug_stop)
S() { timeout 120 "$X" --phone "$PHONE" debug "$@"; }
{ S --start com.x86sim.test.swiftdbg --symbols "$DS"; S --cmd "breakpoint set -n swiftTick"; S --cmd continue --timeout 20
  S --cmd "frame variable n"; S --cmd "finish" --timeout 20; S --cmd "po counter"; S --cmd "breakpoint delete -f"; S --stop; } > "$OUT/swift-session.txt" 2>&1
check session "$OUT/swift-session.txt" '"swift": true' 'swiftTick' '\(Int\) n = [0-9]+' 'Counter\(ticks, hits: [0-9]+\)' '"state": "detached"'
exit $fail
