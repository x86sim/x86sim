#!/bin/bash
# x86Sim: LLDB on a phone's app, end to end (x86sim debug, the stub in libarm64jit). The phone must be running.
#   tests/lldb/run.sh PHONE [--no-build] [--settings]
# 1. dbgapp (tests/lldb/dbgapp.m, built with a dSYM by build-app.sh) launched stopped before dyld (--launch):
#    break on dbg_tick, continue, hit, bt, read n and acc, step over lines, finish; detach
# 2. attach to the running dbgapp: thread list, bt all, break on dbg_work (the worker thread), read round; break in
#    UIKitCore (-[UILabel setText:], AOT-translated code); detach
# 3. (--settings) attach to Settings (com.apple.Preferences, launched first): bt all; detach
# On every platform: the phone's (tvOS, watchOS, visionOS) build of dbgapp (build-app.sh X86SIM_PLATFORM); on watchOS
# the AOT breakpoint is CoreFoundation's CFStringCreateWithFormatAndArguments (no UILabel there).
# Every check prints "X86TEST lldb <check> PASS|FAIL"; exit status 1 if one failed. Output in $X86SIM_SCRATCH/lldb-test[-<platform>]/.
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}; . "$X86SIM_ROOT/cli/lib/config.sh"
PHONE=$1; shift || { echo "usage: $0 PHONE [--no-build] [--settings]" >&2; exit 2; }
build=1 settings=0
for a; do case $a in --no-build) build=0 ;; --settings) settings=1 ;; esac; done
X=$X86SIM_ROOT/cli/x86sim
. "$(dirname "$0")/../apps/platform.sh"
plat=$(phone_platform "$PHONE"); X86SIM_PLATFORM=$plat . "$(dirname "$0")/../apps/platform.sh"
OUT=$X86SIM_SCRATCH/lldb-test$PSUF; mkdir -p "$OUT"
APP=$X86SIM_SCRATCH/apps/dbgapp$PSUF.app
[ $build = 1 ] && { X86SIM_PLATFORM=$plat "$(dirname "$0")/build-app.sh" >&2 || exit 1; }
AOTBP="-[UILabel setText:]" AOTRE="UIKitCore\`-\[UILabel setText:\]" MAINRE=UIApplicationMain
[ "$plat" = watchos ] && AOTBP=CFStringCreateWithFormatAndArguments AOTRE="CoreFoundation\`CFStringCreateWithFormatAndArguments" MAINRE=WKApplicationMain
P() { "$X" --phone "$PHONE" "$@"; }
P install "$APP" >&2 || exit 1
fail=0
check() {  # check NAME FILE REGEX...: every regex is found in FILE
	local name=$1 f=$2; shift 2
	for re; do grep -qE -- "$re" "$f" || { echo "X86TEST lldb $name FAIL (no /$re/ in $f)"; fail=1; return; }; done
	echo "X86TEST lldb $name PASS"
}
DS=$APP.dSYM
# 1. launched stopped
timeout 300 "$X" --phone "$PHONE" debug com.x86sim.test.dbgapp --launch --symbols "$DS" --batch \
	-o "breakpoint set -n dbg_tick" -o "continue" -o "bt" -o "frame variable n" -o "next" -o "next" -o "next" \
	-o "frame variable acc" -o "finish" -o "breakpoint delete -f" -o "detach" > "$OUT/launch.txt" 2>&1
check launch-stopped "$OUT/launch.txt" "stop reason = signal SIGSTOP" "_dyld_sim_prepare|dyld"
check breakpoint "$OUT/launch.txt" "stop reason = breakpoint 1\.1" "dbgapp\`dbg_tick\(n=[0-9]+\) at dbgapp\.m"
check backtrace "$OUT/launch.txt" "frame #1: .*dbgapp\`__.*block_invoke|frame #1: .*dbgapp" "UIKitCore|CoreFoundation"
check variable "$OUT/launch.txt" "\(int\) n = [0-9]+" "\(long\) acc = [0-9]+"
check step "$OUT/launch.txt" "stop reason = step over"
check finish "$OUT/launch.txt" "stop reason = step out"
check detach "$OUT/launch.txt" "Process [0-9]+ detached"
sleep 2
P pidof com.x86sim.test.dbgapp > /dev/null && echo "X86TEST lldb runs-after-detach PASS" || { echo "X86TEST lldb runs-after-detach FAIL"; fail=1; }
# 2. attach to the running app
timeout 300 "$X" --phone "$PHONE" debug com.x86sim.test.dbgapp --symbols "$DS" --batch \
	-o "thread list" -o "bt all" -o "breakpoint set -n dbg_work" -o "continue" -o "frame variable round" -o "thread info" \
	-o "breakpoint delete -f" -o "breakpoint set -n '$AOTBP'" -o "continue" -o "bt 4" \
	-o "breakpoint delete -f" -o "detach" > "$OUT/attach.txt" 2>&1
check attach "$OUT/attach.txt" "stop reason = signal SIGSTOP" "thread #2"
check bt-all "$OUT/attach.txt" "$MAINRE" "dbgapp\`worker"
check worker-breakpoint "$OUT/attach.txt" "dbgapp\`dbg_work\(round=[0-9]+\)" "\(long\) round = [0-9]+"
# UIKitCore's (CoreFoundation's) code comes from the AOT image: its blocks must be dropped for the breakpoint (gdbstub.cpp bp_set)
check aot-breakpoint "$OUT/attach.txt" "$AOTRE" "stop reason = breakpoint 2\.1"
check detach2 "$OUT/attach.txt" "Process [0-9]+ detached"
# 3. Settings
if [ $settings = 1 ]; then
	P launch com.apple.Preferences > /dev/null 2>&1; sleep 5
	timeout 300 "$X" --phone "$PHONE" debug com.apple.Preferences --batch -o "thread list" -o "bt all" -o "detach" > "$OUT/settings.txt" 2>&1
	check settings-bt-all "$OUT/settings.txt" "UIApplicationMain" "Process [0-9]+ detached"
fi
exit $fail
