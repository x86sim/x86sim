#!/bin/bash
# x86Sim: erase / clone refuse a phone that is starting (boot-a64.sh up, no launchd_sim yet), and boot-a64.sh refuses a
# phone being erased (devctl.py Lifecycle: <phone>/.lifecycle.lock). Uses a fake phone var/device-a64-r2s-erasetest
# (a dir with a runtime file and data/) and a fake boot process (sleep with A64_DEVICE set), both removed after.
set -u
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
D=$(realpath -m "$X86SIM_ROOT/var/device-a64-r2s-erasetest")
rm -rf "$D"; mkdir -p "$D/data/Library"; echo ios-27.2-24B8117-arm64 > "$D/runtime"; echo erasetest > "$D/name"; touch "$D/data/keep"
fail=0
A64_DEVICE=$D sleep 60 & fake=$!
out=$(X86SIM_DEVICE=$D python3 "$X86SIM_ROOT/cli/libexec/devctl.py" erase 2>&1); rc=$?
if [ $rc != 0 ] && [ -e "$D/data/keep" ]; then echo "ok   erase refused while starting: $out"; else echo "FAIL erase ran on a starting phone (rc=$rc): $out"; fail=1; fi
out=$(X86SIM_DEVICE=$D python3 "$X86SIM_ROOT/cli/libexec/devctl.py" clone -- r2s-erasetest2 2>&1); rc=$?
if [ $rc != 0 ] && [ ! -e "$X86SIM_ROOT/var/device-a64-r2s-erasetest2" ]; then echo "ok   clone refused while starting"; else echo "FAIL clone ran (rc=$rc): $out"; fail=1; fi
kill "$fake"; wait "$fake" 2>/dev/null
# the other order: the lock held (an erase at work) -> boot-a64.sh refuses at once
exec 7>"$D/.lifecycle.lock"; flock 7
out=$(A64_DEVICE=$D SECS=1 timeout 20 "$X86SIM_ROOT/cli/libexec/arm64/boot-a64.sh" 2>&1); rc=$?
if [ $rc = 1 ] && grep -q "being erased" <<<"$out"; then echo "ok   boot refused while erasing"; else echo "FAIL boot went on (rc=$rc): ${out:0:300}"; fail=1; fi
flock -u 7; exec 7>&-
rm -rf "$D" "$X86SIM_ROOT/var/device-a64-r2s-erasetest2"
exit $fail
