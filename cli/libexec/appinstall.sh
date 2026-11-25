#!/bin/bash
# x86Sim: `x86sim install|uninstall`, as `xcrun simctl install|uninstall` on a Mac (`x86sim launch`: simctl.py).
#   install <path.app|.ipa|.xcarchive>
#                                  copy the bundle into the device and have installd install it (through
#                                  LaunchServices -> InstallCoordination -> installd, in host/openurl inside the device).
#                                  A device build (.ipa, .xcarchive, device .app) is made a simulator .app first by
#                                  x86sim-devapp (tools/launcher core/src/devapp.rs: Mach-O platform retag, Info.plist,
#                                  ad-hoc re-sign keeping entitlements); FairPlay-encrypted App Store apps are refused
#   uninstall <bundle id>
# An optional first argument names the device, as simctl's <device>: a device dir, or an arm64 phone's name / folder
# (var/device-a64-<name>, as `x86sim device list` shows; "booted" = the default). Else X86SIM_DEVICE.
# Each waits for the helper's answer in <device>/data/var/log/x86sim-openurl.log and prints it.
. "$(dirname "$0")/../lib/config.sh"
cmd=$1; shift
. "$(dirname "$0")/../lib/pickdevice.sh"
# install <device> <app> / uninstall <device> <bundle id>: a leading device argument when one more follows
if [ $# -ge 2 ] && pick_device "$1"; then shift; fi
export X86SIM_DEVICE
D=$X86SIM_DEVICE/data
LOG=$D/var/log/x86sim-openurl.log
Q=$D/tmp/x86sim-openurl
request() {  # request LINE PATTERN TIMEOUT
	local n; n=$(wc -l < "$LOG" 2>/dev/null || echo 0)
	# as simctl.py request(): a staging file of our own; waiting for the queue to empty and the rename under its lock
	printf '%s\n' "$1" > "$Q.new.$$" && { flock 9; for _ in $(seq 100); do [ -e "$Q" ] || break; sleep 0.1; done
		mv "$Q.new.$$" "$Q"; } 9>>"$Q.lock"
	for _ in $(seq $(( $3 * 2 ))); do
		sleep 0.5
		r=$(tail -n +$((n + 1)) "$LOG" 2>/dev/null | grep -m1 -E "$2") && { echo "$r"; [[ $r != *failed* && $r != *error* ]]; return; }
	done
	echo "$cmd: no answer in $3 s (is the device booted? see $LOG)" >&2; return 1
}
case $cmd in
install)
	app=${1%/}
	[ -n "$app" ] && [ -e "$app" ] || { echo "usage: x86sim install <path.app|path.ipa|path.xcarchive>" >&2; exit 2; }
	# device builds: converted into a work dir under the scratch dir (never /tmp: RAM), removed when this ends.
	# X86SIM_NO_CONVERT: the caller (the launcher's apps::install) already did it
	if [ -z "${X86SIM_NO_CONVERT:-}" ]; then
		conv=$X86SIM_ROOT/tools/launcher/target/release/x86sim-devapp
		if [ -x "$conv" ]; then
			W=$X86SIM_SCRATCH/devapp/install-$$; trap 'rm -rf "$W"' EXIT
			app=$("$conv" convert "$app" --work "$W") || exit 1
		elif [ ! -f "$app/Info.plist" ]; then
			echo "x86sim install: $app needs x86sim-devapp (cd tools/launcher && cargo build --release)" >&2; exit 1
		fi
	fi
	[ -f "$app/Info.plist" ] || { echo "usage: x86sim install <path.app|path.ipa|path.xcarchive>" >&2; exit 2; }
	# staging copy inside the device's data directory (visible inside Darling); installd copies it again
	S=$D/tmp/x86sim-install; mkdir -p "$S"; rm -rf "$S/$(basename "$app")"
	cp -a "$app" "$S/" || exit 1
	request "install tmp/x86sim-install/$(basename "$app")" "^install .*$(basename "$app")" 120 || exit 1
	# Speed 31 (X86SIM_APP_AOT): translate the app ahead of time. AOT cache 1: one transaction in x86sim-aot, under the
	# cache dir's lock (concurrent installs on phones of one runtime wait for each other): the app's Mach-Os get slots
	# in a staged copy of placement.txt (placeapp.py: stale app rows pruned, freed ranges reused, least recently used
	# apps evicted when the window is full; system slots never move), its chunks are built, the image for the staged
	# table is assembled, and only then is the table committed. On any failure the last good table and image stay
	# (the app runs live); a chunk that fails twice is left out of an image that is otherwise complete.
	if [ "${X86SIM_APP_AOT:-1}" = 1 ]; then
		J=$(sed -n 's/^X86SIM_A64_JITCACHE_DIR=//p' "$X86SIM_DEVICE/sim.env")
		inst=$(ls -td "$D"/Containers/Bundle/Application/*/"$(basename "$app")" 2>/dev/null | head -1)
		if [ -n "$J" ] && [ -n "$inst" ]; then
			"$X86SIM_ROOT/tools/launcher/target/release/x86sim-aot" --jc "$J" --device "$X86SIM_DEVICE" app install "$inst" | tail -1
			[ "${PIPESTATUS[0]}" = 0 ] || echo "appinstall: no AOT image for $(basename "$app"): it runs live (placement and image unchanged)" >&2
		fi
	fi ;;
uninstall)
	[ -n "$1" ] || { echo "usage: x86sim uninstall <bundle id>" >&2; exit 2; }
	request "uninstall $1" "^uninstall $1:" 60 || exit 1
	# Speed 35 / AOT cache 1: the uninstalled app's placement rows and chunks go too, as one transaction (x86sim-aot)
	if [ "${X86SIM_APP_AOT:-1}" = 1 ]; then
		J=$(sed -n 's/^X86SIM_A64_JITCACHE_DIR=//p' "$X86SIM_DEVICE/sim.env")
		if [ -n "$J" ] && [ -f "$J/placement.txt" ]; then
			"$X86SIM_ROOT/tools/launcher/target/release/x86sim-aot" --jc "$J" --device "$X86SIM_DEVICE" app prune "$D" | tail -1
			[ "${PIPESTATUS[0]}" = 0 ] || echo "appinstall: AOT update after uninstall failed; placement and image unchanged" >&2
		fi
	fi ;;
*) echo "appinstall.sh install|uninstall" >&2; exit 2 ;;
esac
