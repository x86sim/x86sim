#!/bin/bash
# x86Sim: boot a phone (iOS 27 arm64 runtime, every process under libarm64jit) for SECS seconds and summarise. Each
# phone has its own Darling prefix (A64_DPREFIX, default $X86SIM_SCRATCH/a64prefix: only that container is shut down and
# restarted), device (var/device-a64), state dir (var/run-a64) and frame ring (/dev/shm/x86sim-a64-simfb.ring).
# DNS: own linux/mdnsd server per boot (Speed 26, below). mtlvk is started here only when X86SIM_METAL!=0, on this boot's own
# socket ($ST/mtlvk.sock) and pidfile — never pkill -x mtlvk (would hit a frozen x86 stack's mtlvk).
# Runs in the systemd scope of cli/libexec/limited.sh (40 MB/s IO, memory cap X86SIM_MEM_MAX).
#
# Memory: Stage 2's 6G default was based on cgroup memory.current that missed every guest (darlingserver was
# started by a darling shell *outside* the scope — the TZ link — so guests inherited that foreign cgroup). Real
# Shared boot sum VmRSS at home is ~8.4–9.4 GB (PSS/cgroup ~4.6 GB). Default 12G so MemoryMax does not
# OOM on the sum-RSS guests (6–8G is too low for that accounting). Override with X86SIM_MEM_MAX.
# Usage: SECS=120 cli/libexec/arm64/boot-a64.sh [extra jobs...]   (A64_JOBS=min|slim|std|full: the job list)
#        X86SIM_METAL=0|1|backboardd|all  (default all; 0 = software; needs host/metalhostd/arm64/metalhostd + mtlvk)
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../../.." && pwd)}
export X86SIM_ARCH=arm64
# Safari 1: absolute, as the launcher passes it. launchd_sim gets "/Volumes/SystemRoot$D/data/var/run/launchd_bootstrap.plist"
# and the phone's processes are matched by A64_DEVICE=$D: a relative A64_DEVICE from a shell (A64_DEVICE=var/device-a64)
# aborted launchd_sim ("missing or invalid configuration plist", brk in launchd_sim) at every boot.
A64_DEVICE=$(realpath -m "${A64_DEVICE:-$X86SIM_ROOT/var/device-a64}")
export A64_DEVICE X86SIM_DEVICE=$A64_DEVICE
# erase / clone (devctl.py) hold <phone>/.lifecycle.lock while they work: no boot of a phone being erased. Checked after
# this process exists with A64_DEVICE, as they look for it after taking the lock (whichever comes second sees the other).
if [ -d "$A64_DEVICE" ] && ! flock -n "$A64_DEVICE/.lifecycle.lock" true 2>/dev/null; then
	echo "boot-a64: $A64_DEVICE is being erased or cloned" >&2; exit 1
fi
# Speed 24: a device's iOS version is its <device>/runtime (a runtimes/ dir name, device-a64.py create --ios); A64_RUNTIME
# overrides. Each runtime has its own JIT/AOT cache dir ($X86SIM_SCRATCH/jc-<build>; 27.0: jitcache) and placement table.
A64_RT_DIR=$(cat "$X86SIM_DEVICE/runtime" 2>/dev/null)
export X86SIM_RUNTIME=${A64_RUNTIME:-$X86SIM_ROOT/runtimes/${A64_RT_DIR:-ios-27.2-24B8117-arm64}/RuntimeRoot}
# a device whose runtime refuses its model (the runtime profile's unsupportedDeviceTypes / restrictedToDeviceTypes:
# iOS 27.2 24B8117 and the iPhone Duo) is "unavailable" in CoreSimulator, which won't boot it (A64_RUNTIME: no check)
# (stopping is always allowed: A64_STOP_ONLY ends a device that is running, unavailable or not)
if [ -z "${A64_RUNTIME:-}" ] && [ "${A64_STOP_ONLY:-0}" != 1 ] && ! python3 "$X86SIM_ROOT/cli/libexec/arm64/device-a64.py" check "$X86SIM_DEVICE"; then exit 1; fi
A64_BUILD=$(basename "$(dirname "$X86SIM_RUNTIME")" | sed -n 's/^\(ios\|tvos\|watchos\|xros\)-[0-9.]*-\([0-9A-Za-z]*\)-arm64$/\2/p')
# Platforms 1: the runtime's platform (ios, tvos, watchos): its runtimes/ dir name, else its SystemVersion.plist
A64_PLATFORM=$(basename "$(dirname "$X86SIM_RUNTIME")" | sed -n 's/^\(ios\|tvos\|watchos\|xros\)-.*/\1/p')
if [ -z "$A64_PLATFORM" ]; then
	case $(sed -n '/ProductName/{n;s/.*<string>\(.*\)<\/string>.*/\1/p}' "$X86SIM_RUNTIME/System/Library/CoreServices/SystemVersion.plist" 2>/dev/null) in
	"Apple TVOS") A64_PLATFORM=tvos ;; "Watch OS") A64_PLATFORM=watchos ;; xrOS|visionOS) A64_PLATFORM=xros ;; *) A64_PLATFORM=ios ;;
	esac
fi
# our in-simulator binaries are built per platform: host/<name>/arm64 (iOS), arm64-tvos, arm64-watchos
A64_HOSTDIR=arm64; [ "$A64_PLATFORM" = ios ] || A64_HOSTDIR=arm64-$A64_PLATFORM
export A64_PLATFORM
# A device other than var/device-a64 gets its own state dir and prefix by default, named as the launcher names them
# (tools/launcher/core/src/instance.rs: device-a64<sfx> -> var/run-a64<sfx>, a64prefix<sfx>). These defaulted to
# device-a64's, so a script booting another device by A64_DEVICE alone wrote into the user's phone (2026-11-19).
A64_SFX=$(basename "$A64_DEVICE" | sed -n 's/^device-a64//p')
[ "$(basename "$A64_DEVICE")" = device-a64 ] || [ -n "$A64_SFX" ] || A64_SFX=-$(basename "$A64_DEVICE")
export X86SIM_STATE=${A64_STATE:-$X86SIM_ROOT/var/run-a64$A64_SFX}
. "$X86SIM_ROOT/cli/lib/config.sh"
if [ -z "${X86SIM_A64_JITCACHE_DIR:-}" ] && [ -n "$A64_BUILD" ] && [ "$A64_BUILD" != 24A434 ]; then
	export X86SIM_A64_JITCACHE_DIR=$X86SIM_SCRATCH/jc-$A64_BUILD
fi
export DPREFIX=${A64_DPREFIX:-$X86SIM_SCRATCH/a64prefix$A64_SFX}
export X86SIM_DPREFIX=$DPREFIX
# Metal for every process by default, as the launcher boots (instance.rs BootSettings) and as Apple's simulator always
# has a device; it falls back to software rendering below when mtlvk does not start. The default was 0 (software) for
# boots started here, and with no MTLDevice iconservicesagent crashed in CoreUI (sharedCIContext is nil without one),
# so Settings showed gray row icons (a report against x86Sim 0.5.3), and WebGL had no context. X86SIM_METAL=0 still
# turns it off. visionOS needs it (wakeboardd's Metal compositor; refused below without it).
export X86SIM_METAL=${X86SIM_METAL-all}
export X86SIM_MEM_MAX=${X86SIM_MEM_MAX-12G}
X=$X86SIM_ROOT; cd "$X" || exit 1
D=$X86SIM_DEVICE; ST=$X86SIM_STATE; SECS=${SECS:-90}
mkdir -p "$D" "$ST"
# Speed 22: the booted device, for simview (input socket, logs) and the launcher
[ -n "${A64_STOP_ONLY:-}" ] || echo "$D" >"$ST/device"
rm -rf "$D"/data/var/log/*.log
# Speed 1: start this boot's mtlvk when Metal is requested. Own socket + pidfile under var/run-a64.
# Speed 8: A64_MTLVK_SOCKET=<path of a running mtlvk's socket> shares that mtlvk (one per host for several devices; it
# keeps an object table per connection): this boot neither starts nor stops it.
# Shared mtlvk: its lifetime is its clients'. Other phones' darlingservers whose environment names this boot's socket
# (X86SIM_MTLVK_SOCKET; this prefix's own, possibly still running, is not counted). Killing an mtlvk another phone
# renders through crashed that phone's metalhostd -> backboardd -> SpringBoard and every app (a black screen with a
# spinner for ~30-60 s, open apps lost; software rendering if no mtlvk came back): a CLI boot, a launcher phone with
# sharing off, and every non-KEEP boot's exit did it.
# A phone still booting has no darlingserver yet: its boot-a64.sh (A64_MTLVK_SOCKET from the launcher) counts too, or
# stopping the mtlvk's owner in that window took the booting phone's Metal away.
# RAM 14: socket paths compare resolved (realpath): a phone booted through another root naming the same state dir (a
# worktree's mirror of symlinks) had a textually different path, was not counted, and its mtlvk was stopped under it.
a64_mtlvk_users() {
	local want n=0 p v
	want=$(realpath -m "$1" 2>/dev/null || echo "$1")
	for p in /proc/[0-9]*; do
		case "$(cat "$p/comm" 2>/dev/null)" in
		darlingserver)
			tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -q "darlingserver $DPREFIX " && continue
			v=$(tr '\0' '\n' 2>/dev/null < "$p/environ" | sed -n 's/^X86SIM_MTLVK_SOCKET=//p' | head -1)
			[ -n "$v" ] && [ "$(realpath -m "$v" 2>/dev/null || echo "$v")" = "$want" ] && n=$((n+1)) ;;
		boot-a64.sh)
			tr '\0' '\n' 2>/dev/null < "$p/environ" | grep -qxF -e "A64_STOP_ONLY=1" -e "A64_DPREFIX=$DPREFIX" && continue
			v=$(tr '\0' '\n' 2>/dev/null < "$p/environ" | sed -n 's/^A64_MTLVK_SOCKET=//p' | head -1)
			[ -n "$v" ] && [ "$(realpath -m "$v" 2>/dev/null || echo "$v")" = "$want" ] && n=$((n+1)) ;;
		esac
	done
	echo $n
}
# The GPU this boot's mtlvk serves on: MTLVK_DEVICE_ID (the launcher's per-device GPU setting, BootSettings::gpu: auto,
# cpu, or a GPU's "<deviceUUID>,<vendor>:<device>"; linux/mtlvk/src/gpus.rs), else MTLVK_DEVICE=<n> (the n-th, in the
# Vulkan loader's order). Each choice has its own mtlvk in this state dir, $ST/mtlvk<key>.{sock,pid,log,gpu} (auto:
# plain mtlvk.sock, as before), so a phone switched to another GPU never reuses (or stops) the helper other phones
# still render through on the old one, and phones share a helper only when they want the same GPU.
a64_gpu_key() {
	local w=${MTLVK_DEVICE_ID:-auto}
	if [ "$w" = auto ]; then
		case ${MTLVK_DEVICE:-} in ''|*[!0-9]*) echo "" ;; *) echo "-i$MTLVK_DEVICE" ;; esac
		return
	fi
	[ "$w" = cpu ] && { echo -cpu; return; }
	w=${w%%,*}; w=$(printf %s "$w" | tr -cd '0-9a-fA-F:' | tr 'A-F:' 'a-f-'); echo "-${w:0:8}"
}
M=$ST/mtlvk$(a64_gpu_key)
# $1: an mtlvk's base path ($ST/mtlvk<key>)
a64_mtlvk_live() {
	local pid b=${1:-$M}
	pid=$(cat "$b.pid" 2>/dev/null || true)
	[ -n "$pid" ] && [ "$(cat "/proc/$pid/comm" 2>/dev/null)" = mtlvk ] && [ -S "$b.sock" ]
}
stop_a64_mtlvk() {
	[ -n "${A64_MTLVK_SOCKET:-}" ] && [ -z "${1:-}" ] && return 0
	local old users b=${1:-$M}
	if a64_mtlvk_live "$b"; then
		users=$(a64_mtlvk_users "$b.sock")
		if [ "$users" -gt 0 ]; then
			echo "boot-a64: keeping mtlvk (pid $(cat "$b.pid")): $users other phone(s) render through it" >&2
			return 0
		fi
	fi
	old=$(cat "$b.pid" 2>/dev/null || true)
	if [ -n "$old" ] && [ "$(cat "/proc/$old/comm" 2>/dev/null)" = mtlvk ]; then  # not a process that reused its pid
		kill "$old" 2>/dev/null || true
		sleep 0.2
		[ -d "/proc/$old" ] && kill -9 "$old" 2>/dev/null || true
	fi
	rm -f "$b.pid" "$b.sock" "$b.gpu" "$b.sock.state"
}
# a stop: every mtlvk this state dir started (one per GPU it was booted on), each unless another phone uses it
stop_a64_mtlvks() {
	local f
	for f in "$ST"/mtlvk*.pid; do [ -e "$f" ] && stop_a64_mtlvk "${f%.pid}"; done
	return 0
}
# A stop (A64_STOP_ONLY) starts no mtlvk: it used to start one for a phone whose own helper was not up (one that
# shared another phone's, or on another GPU), which then idled until MTLVK_IDLE_EXIT; it also wrote over the stopped phone's
# mtlvk.log / mtlvk.pid, and with A64_KEEP_MTLVK=1 (the launcher's restart) ended the helper it was to keep
if [ "${A64_STOP_ONLY:-0}" = 1 ]; then A64_NO_MTLVK_START=1; fi
if [ -n "$X86SIM_METAL" ] && [ "$X86SIM_METAL" != 0 ] && [ -z "${A64_NO_MTLVK_START:-}" ]; then
	# On-disk Metal binary so xpc_bundle can find MTLCompilerService.xpc (see ensure-metal-framework.sh).
	"$X/cli/libexec/arm64/ensure-metal-framework.sh" || {
		echo "boot-a64: ensure-metal-framework failed; pipelines will not compile" >&2
	}
	MTLVK_BIN=${MTLVK:-$X86SIM_MTLVK}
	if [ -n "${A64_MTLVK_SOCKET:-}" ]; then
		if [ -S "$A64_MTLVK_SOCKET" ]; then
			export X86SIM_MTLVK_SOCKET=$A64_MTLVK_SOCKET
			echo "boot-a64: sharing mtlvk at $A64_MTLVK_SOCKET, Metal for: $X86SIM_METAL" >&2
		else
			echo "boot-a64: A64_MTLVK_SOCKET $A64_MTLVK_SOCKET is not a socket: software rendering" >&2
			export X86SIM_METAL=0
		fi
	fi
fi
if [ -n "$X86SIM_METAL" ] && [ "$X86SIM_METAL" != 0 ] && [ -z "${A64_MTLVK_SOCKET:-}" ] && [ -z "${A64_NO_MTLVK_START:-}" ]; then
	export X86SIM_MTLVK_SOCKET=$M.sock
	if a64_mtlvk_live && [ "$(a64_mtlvk_users "$M.sock")" -gt 0 ]; then
		# another phone renders through this boot's earlier mtlvk (same GPU: same key): use it too (a fresh one would
		# take theirs down)
		echo "boot-a64: reusing mtlvk (pid $(cat "$M.pid")), other phones use it" >&2
	elif stop_a64_mtlvk && [ -x "$MTLVK_BIN" ]; then
		# started from a subshell that exits at once, so mtlvk is not a child of this script: stop_prefix ends an earlier
		# boot's script of this device with its children (pkill -P), and while this script ran (its first ~40 s) a stop
		# or a re-boot of this phone SIGTERMed the mtlvk that other phones had started sharing (their apps then got no
		# MTLDevice until rebooted)
		(setsid "$MTLVK_BIN" "$M.sock" > "$M.log" 2>&1 < /dev/null & echo $! > "$M.pid")
		for i in $(seq 1 50); do [ -S "$M.sock" ] && break; sleep 0.1; done
	fi
	mpid=$(cat "$M.pid" 2>/dev/null || true)
	if [ -S "$M.sock" ] && [ -n "$mpid" ] && [ -d "/proc/$mpid" ]; then
		echo "boot-a64: mtlvk up (pid $mpid, GPU ${MTLVK_DEVICE_ID:-auto}), Metal for: $X86SIM_METAL" >&2
	else
		echo "boot-a64: mtlvk did not start ($MTLVK_BIN, see $M.log): software rendering" >&2
		export X86SIM_METAL=0
		stop_a64_mtlvk
	fi
fi
if [ "$A64_PLATFORM" = xros ] && { [ -z "$X86SIM_METAL" ] || [ "$X86SIM_METAL" = 0 ]; }; then
	echo "boot-a64: visionOS needs Metal (X86SIM_METAL=all and a working mtlvk); not booting without it" >&2
	exit 1
fi
MIN="com.apple.cfprefsd.xpc.daemon com.apple.cfprefsd.xpc.daemon.system com.apple.notifyd com.apple.configd_sim
	com.apple.runningboardd com.apple.lsd com.apple.containermanagerd com.apple.containermanagerd.system
	com.apple.mobilegestalt.xpc com.apple.UserEventAgent-System com.apple.securityd com.apple.trustd com.apple.syslogd
	com.apple.mobile.installd com.apple.systemstatusd com.apple.distnoted.xpc.daemon com.apple.tccd
	com.apple.fontservicesd com.apple.iconservices.iconservicesagent com.apple.pluginkit.pkd
	com.apple.cloudd com.apple.diagnosticd com.apple.nsurlsessiond com.apple.appprotectiond com.apple.logd com.apple.logd_helper"
# x86Sim: lean UI set (every arm64 process costs 150-450 MB under the JIT). No chronod (widget extensions, +~0.8 GB);
# A64_JOBS=full adds it (A64_KEEP_WIDGETS=1 keeps the home widgets).
# A64_JOBS=slim drops usernotificationsd (~400-450 MB RSS under the JIT); home + one app does not need it. It keeps
# sleepd (~20-45 MB RSS measured 2026-11-25): without it SpringBoard retried its lookup without pause, ~0.6 core of
# SpringBoard + ~0.35 of darlingserver at rest on the home screen (with it: 0.02 + 0.01). A64_SLIM=1 (default)
# parks Spotlight / WidgetRenderer before dyld (X86SIM_A64_SLIM_PARK). Without cloudd+diagnosticd
# in the job set, securityd busy-retries those Mach lookups (~1.9 cores idle). Those two stay
# runnable (not parked): a parked MachServices daemon accepts the lookup then hangs clients forever.
# nsurlsessiond: without it cloudd busy-retries getActiveSessionIdentifiers (~70% of a core + huge log).
# logd + logd_helper (on demand): the log store (data/var/db/diagnostics, uuidtext). Installed apps log into their
# firehose buffer as on Apple's simulator (simshim keeps os_log on for them only) and read it back with OSLogStore
# (scope .currentProcessIdentifier: OSLog's OSLogService reads logd's store). logd asks logd_helper to harvest each
# sender's UUID text (format strings; the dyld cache's at boot): without it every harvest failed ("Error connecting to
# logd_helper") and stored entries came back without their messages ("<compose failure [shared UUID]>"). ~15 MB each.
# appprotectiond (Speed 5): without it SpringBoard's ServerBackedClientPolicyStore retries com.apple.appprotectiond.read
# ~5x/s for the whole boot (on-demand, EnablePressuredExit: idle cost only once looked up).
# openurl: com.x86sim.openurl (arm64 build) so simtouch.py app/openurl works on the a64 device.
# InputUI (Speed 12): the keyboard service (LaunchAngel, on demand: started at SpringBoard's first osservice request).
# Without the job SpringBoard's bootstrap of osservice<com.apple.InputUI> fails ~60 times per boot and text fields get no
# keyboard. assistantd (Apple's plist since Speed 26) serves dictation and settings: without them each failed dictation availability
# check posts "availability changed", the keyboard dock (mic key) re-checks, and the app's main thread spins at 100%
# from the first text field tap, so InputUI's keyboard arbiter times the app out and no keyboard appears.
# assetsd (on demand): the photo library; without it Photos throws in its library view's viewDidLoad.
# geod + navd (Speed 15, on demand): Maps' GeoServices and navigation services. Without them Maps retried both lookups
# without backoff (~85% CPU backgrounded); Speed 14 no-op patched the retry instead, which turned the features off.
# mapssyncd (on demand): Maps' synced places (com.apple.Maps.MapsSync.store, a Core Data store over XPC). Without it
# navd's store lookups failed every 3 s ("Connection init failed at lookup with error 3 - No such process").
# campo (Speed 16): the Siri UI (Campo.app LaunchAngel, launched by RunningBoard as osservice<com.apple.campo> on the
# Siri tap; mksimdevice puts angels where launchd_sim classifies them as angels, see there).
# PosterBoard (Speed 17, on-demand angel): the wallpaper. On a device without a poster store, SpringBoard asks for it and
# it builds the store and selects MercuryPoster for the lock/home screen, as a real sim's first boot does (checked
# against the Mac's iOS 27 devices). Once a poster is chosen SpringBoard hosts that one MercuryPoster extension itself
# (~167 MB PSS before Speed 17's AOT training). FULLUI adds chronod (widgets).
# financed + passd (Speed 20, feature "wallet"): Wallet's FinanceKit opens its Core Data store over XPC from financed;
# without it Wallet trapped ("Could not create CoreDataStore"). healthd/healthappd/finhealthd (feature "health"): on demand.
# medialibraryd (Speed 22, on demand): the music library (com.apple.medialibraryd.xpc), which creates and migrates
# Media/iTunes_Control/iTunes/MediaLibrary.sqlitedb. Without it the file stayed an empty database and Settings > General >
# About aborted counting Songs (ML3DatabaseException "no such table: _MLDatabaseProperties").
# StatusKitAgent (Speed 25, on demand): Focus/availability status. Without it opening a Messages conversation spun the
# main thread at 100% forever: IMCore asks StatusKit per chat item, each sync XPC call failed its lookup, and asked again.
# mobileassetd + assetsubscriptiond (Speed 26, on demand): Siri's UAF assets (Understanding, TTS, ASR: ~870 MB, as a Mac's
# iOS 27 simulator device downloads them). Without them assistantd finds no asset set and Siri says "Data for using Siri
# is downloading" forever; with them it downloads once into the device's data/private/var/MobileAsset.
# corespeechd, sirittsd, siriinferenced, siriknowledged, siriactionsd (Speed 26, on demand): Siri's speech capture,
# speech synthesis, inference, knowledge and actions services, which a Mac's simulator has on demand. Without corespeechd
# assistantd's recording failed at once ("Unable to prepareAudioProvider", kAFAssistantErrorDomain 221).
# searchd + corespotlightservice (Speed 26, on demand): the Spotlight index (com.apple.spotlight.SearchAgent / IndexAgent /
# IndexDelegateAgent) and Spotlight's resources (com.apple.MobileAsset.SpotlightResources). Without them Files' Recents
# showed "Content Unavailable ... unknown error" and apps' CoreSpotlight indexing failed its lookups.
# assistant_cdmd (Speed 27, on demand): Siri's dialog manager (com.apple.assistant.cdm), also the embedding service of
# Spotlight's query parser. Without it the parser's model load failed ("QU model load failed ... com.apple.assistant.cdm
# ... lookup"), its token stayed nil and Spotlight crashed on typing (SPSearchEntity / _CSSuggestionToken).
# nanoregistryd (Speed 27, on demand): the paired-device (Watch) registry, which a Mac's simulator runs. Without it
# NanoRegistry's sync query failed its lookup, NSXPCConnection ran the error handler inline and that dispatch_sync'd onto
# the queue the thread was on: a deadlock inside +[NRPairedDeviceRegistry sharedInstance]'s dispatch_once, which Maps'
# main thread waits on (GEO observer callback): Maps froze on a bare globe, no tiles, no input.
# installcoordinationd (apps feature, on demand): app installs (`x86sim install`, as simctl install: IXAppInstallCoordinator
# -> installcoordinationd -> installd -> LaunchServices). Without it every install failed at once ("Failed to create
# IXPlaceholder ... com.apple.installcoordinationd ... lookup error 3"). Idle cost none until an install looks it up.
# mobiletimerd (on demand): AlarmKit (com.apple.alarmkitservices) and the timer/alarm servers, which a Mac's simulator
# runs. Without it AlarmManager.requestAuthorization() never answered (Cadence's "Allow" did nothing).
# nanotimekitcompaniond (on demand: its Mach services start it) renders the Watch app's faces: without it Face Gallery's
# previews stayed empty (com.apple.nanotimekit.facesnapshotserver: no such process).
# CoreAuthentication.daemon, storekitd, quicklook.ThumbnailsAgent, midiserver-sim (sdkcov, on demand: no RunAtLoad /
# KeepAlive, so no idle cost): LocalAuthentication, StoreKit, QuickLook thumbnails and CoreMIDI, which a Mac's
# simulator serves. Without them LAContext.canEvaluatePolicy, Product.products(for:), QLThumbnailGenerator and
# MIDIClientCreate failed at once (lookup error 3 / serviceConnectionFailed / -10833), where Apple's simulator answers
# (tests/apps/sdkcov, checked against the Mac's iOS 27.2 24B8117). pasteboard.pasted, wcd, voiced and
# accessibility.axassetsd (on demand too): UIPasteboard (a string set was read back as nil), WatchConnectivity
# (WCSession.activate() never called back) and the speech voices (AVSpeechSynthesisVoice.speechVoices() was empty, so
# AVSpeechSynthesizer said nothing). locationd: feature "location" (SpringBoard looks it up at boot, ~94 MB); without it
# CMPedometer.isStepCountingAvailable() never returned. liveactivitiesd (on demand: com.apple.sessionservices): Live
# Activities; without it ActivityAuthorizationInfo().areActivitiesEnabled was false where Apple's simulator says true.
# sharingd + sharinguiservice: the share sheet. UIActivityViewController asks sharingd (com.apple.sharing.sharesheet) for
# the activities and shows them in SharingUIService's remote view (angel, on demand). Without them the sheet came up as an
# empty card (no header, app row or actions) where Apple's simulator shows the link, Reminders / More, Copy and Add to
# Reading List. sharingd is EAGER (mksimdevice.py): its extension cache must be primed before the first share.
# gamecontrollerd (on demand) on iOS and visionOS too (Inputs 2): Apple's iOS / visionOS SimulatorHID add the same
# virtual "Gamepad" as tvOS's, so GCController.controllers() lists it there as well. LocalAuthenticationUIService (angel,
# on demand): the Face ID / Optic ID sheet; without it LAContext.evaluatePolicy failed at once ("unable to establish
# connection to UI", -1000) where Apple's simulator waits for a (simulated) match. pointeruid (on demand): an iPad's
# pointer (simhid's mouse, pointer capture); without it the mouse moved nothing visible. DragUI.druid (on demand): drag
# and drop (com.apple.DragUI.druid.source / .destination); without it a long press on a UIDragInteraction view asked
# for its items and nothing lifted: no drag preview, no drop session reached any target (sdkcov func:UIKit.ipad-drag-drop).
# devicecheckd (all four platforms) and AuthenticationServicesAgent (iOS, visionOS; on demand, sdkcov P8): DCDevice /
# DCAppAttestService ask devicecheckd whether the client is supported; without it generateToken / generateKey failed
# with DCError 0 (unknownSystemFailure, "Failed to connect to daemon") where Apple's simulator answers featureUnsupported.
# ASAuthorizationController's requests go to AuthenticationServicesAgent (a Cryptex daemon); without it they failed at
# once with NSCocoaErrorDomain 4099, where Apple's shows its passkey UI / answers ASAuthorizationError 1004.
# contactsd, calaccessd, remindd (sdkcov P4, on demand on iOS / watchOS / visionOS: Apple ships contactsd and calaccessd
# on demand; remindd's RunAtLoad becomes on demand in mksimdevice): the Contacts, Calendar and Reminders stores, which a
# Mac's simulator serves. Without them CNContactStore.execute failed (CNErrorDomain 1 "Communication Error") and EventKit
# answered notDetermined despite a TCC grant, where Apple's simulator saves and fetches (tests/apps/sdkcov P4_People.swift).
# gamed (all four platforms) and gamesaved (iOS, visionOS; on demand, sdkcov P2): Game Center and GameSave, which a Mac's
# simulator serves. Without gamed GKLocalPlayer's authenticateHandler failed at once (GKError 6) and the leaderboard /
# achievement loads with GKError 3 (communicationsFailure), where Apple's simulator never calls the handler back without
# an account and answers the loads with notAuthenticated (6) (tests/apps/sdkcov P2_Games.swift).
# homed (iOS, tvOS, visionOS, on demand, sdkcov P5; the watch runtime has it only for Nano alt accounts): HomeKit's
# daemon. Without it HMHomeManager never reported its homes (no homeManagerDidUpdateHomes in 20 s); with it the answers
# are Apple's simulator's: 0 homes, addHome refused with HMError 2040 (tests/apps/sdkcov P5_Home.swift).
# linkd (all four platforms, on demand, sdkcov P7): App Intents' daemon. Without it IntentDonationManager.donate
# failed (NSCocoaErrorDomain 4099, no com.apple.linkd.transcript), where Apple's simulator takes the donation and
# deletes it by identifier.
# mediaanalysisd + mediaanalysisd.service (iOS, visionOS; on demand, sdkcov P7): VisionKit's ImageAnalyzer (Live Text)
# runs in it; without it analyze() failed 4099 (com.apple.mediaanalysisd.service.public), Apple's reads the text.
# siriactionsd on visionOS too (on demand, sdkcov P7): com.apple.siri.VoiceShortcuts.xpc; without it
# INVoiceShortcutCenter.getAllVoiceShortcuts answered IntentsErrorDomain 7001, Apple's visionOS simulator 0 shortcuts.
# translationd (iOS, on demand, sdkcov P7): LanguageAvailability / TranslationSession; without it supportedLanguages
# stayed empty for 15 s, Apple's lists 47.
# callservicesd (iOS, watchOS, visionOS), UsageTrackingAgent, ScreenTimeAgent (iOS; on demand, sdkcov P3): CallKit's
# call host and DeviceActivity's / ScreenTime's agents, all in Apple's simulators. Without them
# CXProvider.reportNewIncomingCall failed (com.apple.CallKit.error 0) and CXCallController requests 4099,
# DeviceActivityCenter.startMonitoring and STWebHistory.fetchAllHistory 4099, where Apple's iOS 27.2 simulator takes the
# calls, answers unauthorized and 0 URLs (P3_Services.swift, P3_Family.swift); on watchOS / visionOS callservicesd
# refuses both calls as Apple's does (incoming error 0, transaction unentitled 1).
# managedconfiguration.profiled (iOS, tvOS; on demand): ManagedConfiguration's restrictions. Without it ATTrackingManager's
# trackingAuthorizationStatus was restricted (1: MC's check-in with com.apple.managedconfiguration.profiled.public failed,
# "No XPC connection in Simulator"), where Apple's simulator answers notDetermined (0); on tvOS too (sdkcov P4 data:
# func:AppTrackingTransparency.not-determined-on-fresh-install, Apple's tvOS 27.2 simulator 0).
# videosubscriptionsd (iOS, tvOS, visionOS; on demand, sdkcov P9): VideoSubscriberAccount's daemon
# (com.apple.VideoSubscriberAccount.videosubscriptionsd), in Apple's simulators. Without it VSUserAccountManager's
# updateUserAccount failed at lookup (NSCocoaErrorDomain 4099), where Apple's simulator connects (4097).
# managedappdistributiond (iOS, on demand, sdkcov P9): MarketplaceKit's daemon (com.apple.managedappdistributiond.xpc).
# Without it AppDistributor.current failed at once (error Unknown) and eligibilityRegion was nil, where Apple's
# simulator never answers either for a simctl-installed app (P4_Commerce.swift func:MarketplaceKit). Once an app install
# (applicationRegistered) or a client wakes it, it exits 0 at once and launchd starts it again every ~10 s (~0.15 s CPU
# each), as in Apple's iOS 27.2 simulator (runs 3 after an install, 15 three minutes later). videosubscriptionsd starts
# at an install too and stays (Apple's as well).
# dmd (iOS, on demand): the device management daemon (com.apple.dmd), running in Apple's iOS 27.2 simulator. ScreenTimeAgent's
# Catalyst task client connects to it at start and retries at once on every failed connect: without dmd that loop
# kept ScreenTimeAgent at ~90% CPU for good ("Client did interrupt ... Transport could not connect", 4097 ~1500/s).
# shazamd (iOS, watchOS, visionOS; on demand, sdkcov P11): ShazamKit's library service (com.apple.shazamd). Without it
# SHLibrary.default.addItems failed at once (com.apple.ShazamKit.ShazamLibrary 301), where Apple's 27.2 simulators
# never answer it for a simctl-installed app.
# merchantd (iOS, on demand, sdkcov P27): ProximityReader's daemon (com.apple.merchantd.*: Tap to Pay, store and
# forward, mobile documents). Without it PaymentCardReader.fetchPaymentCardReaderStore() threw StoreError 0 and
# prepareStoreAndForward() serviceConnectionError, where Apple's 27.2 simulator answers StoreError 1 and notAllowed
# (no Tap to Pay entitlement) (P27_Data.swift func:ProximityReader.p27-requests-reader-answers).
# itunescloudd (iOS, visionOS, on demand, sdkcov P27): MusicKit's cloud library / token service. Without it
# MusicDeveloperTokenProvider answered MusicTokenRequestError.unknown and a library add never answered, where Apple's 27.2
# simulator answers .developerTokenRequestFailed and MPErrorDomain 2 (P27_Services.swift func:MusicKit.p27-requests).
# sensorkitd (iOS, on demand, sdkcov P27): SensorKit's reader/writer service. Without it SRSensorReader.fetchDevices failed
# NSCocoaErrorDomain 4099, where Apple answers SRErrorDomain 1 (no authorization) (func:SensorKit.p27-sensors-readers).
# biomed (iOS and visionOS, on demand, sdkcov P28): Biome's daemon, also CascadeSets' com.apple.cascade.SetChangeRelayService. Without it
# AppIntents' RelevantEntities.updateEntities failed com.apple.CascadeSets.Set 3, where Apple's 27.2 simulator takes them
# (P28_Intents.swift func:AppIntents.p28-shortcuts-schemas). Mach services only (no RunAtLoad / KeepAlive, pressured exit);
# idle it costs ~100 MB RSS (44 MB anonymous) and no CPU.
# webbookmarksd (iOS, on demand, sdkcov P27; on visionOS since r24, below): SafariServices asks it for a Safari
# extension's state. Without it SFSafariExtensionManager.stateOfExtension(withIdentifier:) never answered, where Apple's
# 27.2 simulator answers SFErrorDomain 1 (no such extension) (P27_Services.swift
# func:SafariServices.p27-blockers-extensions-config). Its notifyd launch events start it once SpringBoard is up
# (Apple's simulator runs it too); idle ~90 MB RSS (11 MB anonymous), no CPU, pressured exit.
UI="com.apple.installcoordinationd com.apple.mobiletimerd com.apple.backboardd com.apple.SpringBoard com.apple.sleepd com.apple.TextInput.kbd com.apple.FileCoordination
	com.apple.usernotificationsd com.apple.InputUI com.apple.assistantd com.apple.assetsd com.apple.geod com.apple.navd com.apple.Maps.mapssyncd
	com.apple.campo com.apple.FileProvider com.apple.DesktopServicesHelper com.apple.revisiond com.apple.PosterBoard com.apple.financed
	com.apple.passd com.apple.finhealthd com.apple.healthd com.apple.healthappd com.apple.medialibraryd
	com.apple.StatusKitAgent com.apple.mobileassetd com.apple.assetsubscriptiond com.apple.corespeechd com.apple.sirittsd
	com.apple.siriinferenced com.apple.siriknowledged com.apple.siriactionsd com.apple.searchd com.apple.corespotlightservice
	com.apple.assistant_cdmd com.apple.nanoregistryd com.apple.nanotimekitcompaniond
	com.apple.CoreAuthentication.daemon com.apple.storekitd com.apple.quicklook.ThumbnailsAgent com.apple.midiserver-sim
	com.apple.locationd com.apple.pasteboard.pasted com.apple.wcd com.apple.voiced com.apple.accessibility.axassetsd com.apple.liveactivitiesd
	com.apple.sharingd com.apple.sharinguiservice com.apple.GameController.gamecontrollerd com.apple.LocalAuthenticationUIService
	com.apple.PointerUI.pointeruid com.apple.DragUI.druid
	com.apple.ctkd com.apple.syncdefaultsd com.apple.accountsd com.apple.amsaccountsd com.apple.devicemanagementclient.managedappsd
	com.apple.devicecheckd com.apple.AuthenticationServicesCore.AuthenticationServicesAgent
	com.apple.contactsd com.apple.calaccessd com.apple.remindd com.apple.managedconfiguration.profiled com.apple.mediaremoted
	com.apple.gamed com.apple.gamesaved
	com.apple.homed
	com.apple.linkd com.apple.mediaanalysisd com.apple.mediaanalysisd.service com.apple.translationd
	com.apple.telephonyutilities.callservicesd com.apple.UsageTrackingAgent com.apple.ScreenTimeAgent com.apple.dmd
	com.apple.videosubscriptionsd com.apple.managedappdistributiond com.apple.shazamd com.apple.backgroundassets.user
	com.apple.merchantd com.apple.itunescloudd com.apple.sensorkitd com.apple.biomed com.apple.WebBookmarks.webbookmarksd"
UI_SLIM="com.apple.backboardd com.apple.SpringBoard com.apple.TextInput.kbd com.apple.FileCoordination com.apple.InputUI
	com.apple.assistantd com.apple.sleepd"
FULLUI="com.apple.chronod"
# FileProvider (Speed 16, on demand: started when Files opens): fileproviderd + the sim2host stand-in (host/fpsim2host) give Files its Browse locations (iCloud Drive, On My iPhone, Recently Deleted). Without them Files only
# showed Recents. DesktopServicesHelper (Speed 17, on demand): Files lists On My iPhone (the LocalStorage domain, no
# extension in iOS 27) through DesktopServices, whose com.apple.DesktopServicesHelper.FileService does the file
# operations. Without it the folder stayed "Loading" and New Folder did nothing. revisiond (GenerationalStorage, on
# demand: started when FileProviderResolver / GSLibrary resolves a "did=" bookmark): tracks document IDs -> file IDs;
# without it bookmark resolution for Files / UIDocumentPicker ("On My iPhone") fails FP -1005 / Resolver -1012.
# sdkcov P5: nisimhost (on demand: an NISession's first lookup) stands in for the Mac's CoreSimulator Nearby Interaction
# service (host/nisimhost): Apple's simulators have no nearbyd, an NISession checks its discovery token in with
# com.apple.CoreSimulator.NearbyInteraction on the host. Without it NISession.discoveryToken stayed nil where Apple's
# iOS / watchOS 27.2 simulators give one (P5_Sensors.swift func:NearbyInteraction). No NearbyInteraction on tvOS.
NI_JOB="com.x86sim.nisimhost=host/nisimhost/$A64_HOSTDIR/nisimhost|mach:com.apple.CoreSimulator.NearbyInteraction"
# fseventsd (on demand: the first FSEvents client's lookup) stands in for the Mac's fseventsd, com.apple.FSEvents
# (host/fseventsd), which Apple's simulators use: without it every FSEventStream failed and fileproviderd disabled a
# replicated FileProvider domain ("Unable to retrieve FSEvent stream UUID", POSIX 19). All four platforms.
FSE_JOB="com.x86sim.fseventsd=host/fseventsd/$A64_HOSTDIR/fseventsd|mach:com.apple.FSEvents"
# FoundationModels (iOS, visionOS), only with X86SIM_FM_RELAY (1: $X86SIM_MAC, or user@host): the system model runs on
# a Mac with Apple Intelligence, as Apple's simulators run it on theirs. modelmanagerd, modelcatalogd and
# generativeexperiencesd as in Apple's simulators; HostInferenceProviderService.appex (modelmanagerd's extension) asks
# com.apple.modelmanager.simulator, the Mac's service: mmhost (host/mmhost) serves it and linux/fmrelay/fmrelay.py relays
# it to the Mac. dasd: generativeexperiencesd learns that the host model is there only in its background system task
# AvailabilityUpdateTask.Boot, which dasd runs (Apple's simulator: ~1 min after boot); without it the model stayed
# modelNotReady. Without the relay none of these run (they start at boot, ~250 MB together, for a model that would only
# report modelNotReady, as Apple's simulator without its host service).
case ${X86SIM_FM_RELAY:-} in ""|0) FM_MAC= ;; 1|on|mac) FM_MAC=$X86SIM_MAC ;; *) FM_MAC=$X86SIM_FM_RELAY ;; esac
MM_JOB=
[ -n "$FM_MAC" ] && MM_JOB="com.apple.modelmanagerd com.apple.modelcatalogd com.apple.generativeexperiencesd com.apple.dasd
	com.x86sim.mmhost=host/mmhost/$A64_HOSTDIR/mmhost|mach:com.apple.modelmanager.simulator|env:X86SIM_FMRELAY_SOCK=/var/run/x86sim-fmrelay.sock"
OUR_JOBS="$MM_JOB com.x86sim.openurl=host/openurl/$A64_HOSTDIR/openurl
	com.x86sim.fpsim2host=host/fpsim2host/$A64_HOSTDIR/fpsim2host|mach:com.apple.FileProvider.sim2host
	$NI_JOB $FSE_JOB"
# Platforms 1: tvOS. PineBoard is tvOS's SpringBoard (the home screen, "Top Shelf" and the app switcher); TVSystemUIService
# (angel) draws the system overlays (volume, Control Center). No sleepd, InputUI or nanoregistryd on tvOS.
# geod + mediaremoted (tvOS sdkcov round, on demand, both in Apple's tvOS simulator): without geod an app's MapKit
# retried com.apple.geod ~15000 times in one sdkcov run (and its SwiftUI Map built 19 views, Apple's 21); without
# mediaremoted NowPlaying's MediaSession.requestToBecomeApplicationPrimary() failed (internalFailure: no
# com.apple.mediaremoted.xpc). Both checked against Apple's tvOS 27.2 simulator (tests/apps/sdkcov/ref-mac.sh tvos).
# gamecontrollerd (on demand: an app's GameController looks it up): it turns host/simhid's virtual gamepad (the
# simulator's "Gamepad", as Apple's tvOS SimulatorHID adds) into a GCController. Without it GCController.controllers()
# stayed empty where Apple's tvOS simulator lists that one controller.
# sdkcov P1: mediaremoted (on demand) on iOS, watchOS and visionOS too, as on tvOS above: NowPlaying's
# MediaSession.requestToBecomeApplicationPrimary() threw internalFailure without it, where Apple's iOS / watchOS / visionOS
# 27.2 simulators return (sdkcov func:NowPlaying.media-session-primary).
# sdkcov P5: healthd, geod and navd (on demand) on visionOS too, as on iOS / watchOS: HKHealthStore calls failed with
# NSCocoaErrorDomain 4099 (no com.apple.healthd.server) where Apple's visionOS 27.2 simulator answers HKError 5 / 4, and
# MapKit search / directions / geocoding / Look Around and CLGeocoder failed with GEOError -15 / kCLErrorDomain 2 (no geod)
# where Apple's answer (P5_Health.swift, P5_Location.swift).
# sdkcov P5: locationd (feature "location") on tvOS too, as on iOS / visionOS: CLLocationManager.locationServicesEnabled()
# was false and the app's authorization stayed notDetermined (x86sim privacy had no locationd to tell), where Apple's
# tvOS 27.2 simulator answers true / authorizedWhenInUse and delivers the simulated location (P5_Location.swift).
# sdkcov P1: assetsd (on demand, feature "photos") on tvOS and visionOS too, as in Apple's simulators: without it
# PHPhotoLibrary.performChanges failed with PHPhotosErrorDomain 3301 (operation interrupted) on tvOS, where Apple's
# tvOS 27.2 simulator creates and fetches the assets (sdkcov func:Photos.library-create-fetch-request).
# backgroundassets.user (on demand; tvOS since sdkcov P3, iOS and visionOS since r25: a diff of the runtimes' launchd
# jobs against these lists, by the Mach services public frameworks name): Background Assets' per-user daemon. With BAInitialDownloadRestrictions
# in the app (build.sh) BADownloadManager.fetchCurrentDownloads failed (Foundation._GenericObjCError 0) without it, where
# Apple's tvOS 27.2 simulator answers 0 downloads (More_tvOS.swift func:BackgroundAssets.asset-pack-manager-downloads).
# iOS / visionOS the same: fetchCurrentDownloads / scheduleDownload / cancel failed (_GenericObjCError 0, NSCocoaErrorDomain
# 4099) where Apple's iOS 27.2 simulator schedules, lists and cancels (P21_Gaps.swift download-manager-schedule-cancel).
TV_UI="com.apple.installcoordinationd com.apple.backboardd com.apple.Pineboard com.apple.TextInput.kbd
	com.apple.homed com.apple.locationd
	com.apple.FileCoordination com.apple.usernotificationsd com.apple.assistantd com.apple.TVSystemUIService
	com.apple.PosterBoard com.apple.campo com.apple.splashboardd com.apple.MediaRemoteUI com.apple.assetsubscriptiond
	com.apple.mobileassetd com.apple.tvremoted com.apple.CoreAuthentication.daemon com.apple.storekitd
	com.apple.voiced com.apple.accessibility.axassetsd com.apple.geod com.apple.mediaremoted
	com.apple.GameController.gamecontrollerd com.apple.assetsd
	com.apple.ctkd com.apple.syncdefaultsd com.apple.accountsd com.apple.amsaccountsd com.apple.devicecheckd com.apple.gamed
	com.apple.linkd com.apple.backgroundassets.user com.apple.videosubscriptionsd com.apple.managedconfiguration.profiled"
TV_UI_SLIM="com.apple.backboardd com.apple.Pineboard com.apple.TextInput.kbd com.apple.FileCoordination"
# corespotlightd (watchOS, on demand, sdkcov P7): the watch's Spotlight index (com.apple.spotlight.IndexAgent; no
# searchd on watchOS). Intents keeps donated interactions there: without it INInteraction.delete(with:) /
# deleteAll failed (IntentsErrorDomain 1904 / 1902) and INRelevantShortcutStore.setRelevantShortcuts with
# CSIndexErrorDomain -1003, where Apple's watchOS 27.2 simulator deletes and stores them (P7 Intents checks).
# Inputs 2: watchgesturesd (on demand): the hand gestures (double tap, wrist flick) simhid's "gesture" hands to its
# WatchGesturesSimulatorServer, as Apple's watchOS SimulatorHID does; without it they did nothing.
# Platforms 1: watchOS. Carousel is watchOS's SpringBoard (the watch face, the app grid, notifications); the watch face
# itself comes from nanotimekitd. nanoregistryd (+ its launch helper) holds the pairing state, unpaired for a watch booted
# on its own. No PosterBoard on watchOS.
# Both: assetsubscriptiond (+ mobileassetd) as on iOS: it makes data/private/var/db/assetsubscriptiond, whose
# UAFAssetSubscriptions.db assistantd opened 100-400 times a boot without it (sqlite "unable to open database file").
# Watch: geod / navd / mapssyncd (on demand) as on iOS: without them the watch's Maps drew its empty grid, no tiles.
# All four platforms (iOS / tvOS since sdkcov P8, checked on Apple's iPad and tvOS 27.2 simulators; iOS also
# devicemanagementclient.managedappsd, as visionOS below): ctkd, syncdefaultsd, accountsd, amsaccountsd (on demand, ~20-50 MB each once looked up), all in
# Apple's simulator (ref-mac.sh watchos / xros, 27.2): without them TKTokenWatcher listed no tokens (Apple's: ctkd's
# com.apple.setoken + setoken:aks), NSUbiquitousKeyValueStore.synchronize() was false (com.apple.kvsd) and StoreKit's
# Storefront.current was nil (Apple's: USA 143441, from the simulator's local App Store sandbox account in accountsd).
# visionOS also: locationd (feature "location": CLLocationManager.locationServicesEnabled() was false, Apple's true),
# FileProvider (on demand: NSFileProviderManager answered NSCocoaErrorDomain 4099, no fileproviderd; Apple's
# NSFileProviderErrorDomain -2001) and devicemanagementclient.managedappsd (on demand: ManagedApp's configuration stream
# yielded nil at once for lack of it; Apple's yields nothing for an unmanaged app). With these ATTrackingManager's
# status read notDetermined (0) as on Apple's, not restricted (1).
# itunescloudd (on demand, pressured exit): watchOS MusicKit asks its cloud service status monitor
# (com.apple.itunescloudd.xpc) for the Apple Music privacy acknowledgement before anything else; without it
# MusicSubscription.current failed .permissionDenied (ICError -7010, no listener endpoint) where Apple's watchOS 27.2
# simulator answers .privacyAcknowledgementRequired (P1_Library.swift).
WATCH_UI="com.apple.installcoordinationd com.apple.backboardd com.apple.Carousel com.apple.sleepd com.apple.TextInput.kbd
	com.apple.FileCoordination com.apple.usernotificationsd com.apple.assistantd com.apple.campo com.apple.splashboardd
	com.apple.nanoregistryd com.apple.nanoregistrylaunchd com.apple.nanotimekitd com.apple.carouselecomoded
	com.apple.assetsubscriptiond com.apple.mobileassetd com.apple.geod com.apple.navd com.apple.Maps.mapssyncd
	com.apple.CoreAuthentication.daemon com.apple.storekitd com.apple.quicklook.ThumbnailsAgent com.apple.voiced
	com.apple.accessibility.axassetsd com.apple.wcd com.apple.locationd com.apple.healthd
	com.apple.ctkd com.apple.syncdefaultsd com.apple.accountsd com.apple.amsaccountsd com.apple.watchgesturesd com.apple.devicecheckd
	com.apple.contactsd com.apple.calaccessd com.apple.remindd com.apple.mediaremoted com.apple.gamed
	com.apple.linkd com.apple.telephonyutilities.callservicesd com.apple.corespotlightd com.apple.itunescloudd com.apple.shazamd"
WATCH_UI_SLIM="com.apple.backboardd com.apple.Carousel com.apple.TextInput.kbd com.apple.FileCoordination
	com.apple.nanoregistryd com.apple.nanotimekitd"
# Platforms 2: visionOS. SurfBoard is its SpringBoard; vhmdd (the simulator's virtual head-mounted display,
# com.apple.vhmdd-sim) and wakeboardd (the hmd presentable / post-process server) put the room on the screen;
# pointeruid is the look-and-pinch pointer. RealityEnvironment (a LaunchAngel) is the room itself and hosts
# com.apple.surfboard.environmentservice: without it SurfBoard aborted in -[SFBSEnvironmentService
# addEnvironmentsObserver:] ("Value for 'endpoint' was unexpectedly nil"). The rest as on iOS, including the on-demand
# services sdkcov needs (searchd + corespotlightservice, CoreAuthentication.daemon ... axassetsd, as above). chronod
# (WidgetCenter) stays the "widgets" feature as on iOS: on visionOS SurfBoard's RealityWidgets starts it at boot and it
# runs the room's widget extensions (Calendar, News, ...): ~350 MB.
# sdkcov gaps round: managedassetsd (on demand): visionOS locationd asks it for the app's in-use state
# (#MAIUA addInUseObserverWithAuditToken, Framework-InUseArbiter); without it every app stayed InUseLevel 0 and
# requestLocation / CLLocationUpdate.liveUpdates failed kCLErrorDenied although authorized when in use, where Apple's
# visionOS 27.2 simulator delivers the simulated location (P5_Location.swift). eligibilityd (runs at boot until it has
# written /var/db/eligibilityd/eligibility.plist, as in Apple's simulators: answers forced "Simulator"): without that file
# GMAvailabilityWrapper.isDeviceEligible was false, kbd's DataDeliveryServices skipped "prewarm-embedding-assets" (no
# UAF com.apple.linguisticdata subscription, no mBERT asset) and NLContextualEmbedding.hasAvailableAssets stayed false
# where Apple's visionOS simulator downloads the asset and answers true (P7_ML.swift).
# webbookmarksd (on demand): SSReadingList's WebBookmarksXPCConnection to com.apple.WebBookmarks.webbookmarksd was
# invalidated without it, and the invalidation handler clearing the connection raced -addReadingListItemWithURL:,
# which then built its message from a nil connection: libxpc "XPC API Misuse" killed the app in about half the runs
# (func:SafariServices.reading-list-add-xros; Apple's visionOS simulator runs the daemon).
# medialibraryd (on demand, as on iOS above): without it an app's MPMediaQuery opened the never-created music library
# database and died (ML3DatabaseException "no such table: _MLDatabaseProperties"; func:MediaPlayer.library-authorized-queries).
# biomed (on demand, as on iOS above): without it RelevantEntities.updateEntities failed com.apple.CascadeSets.Set 3 here
# too, where Apple's visionOS simulator takes them (idle ~115 MB RSS, 44 MB anonymous, no CPU).
# RAM 14: togetherd (visionOS only, on demand, as in Apple's simulator): SharePlay's daemon. Without it callservicesd's
# CopresenceUtilities XPCSession to com.apple.togetherd was cancelled at once and re-activated, forever: ~100% CPU and
# ~300 MB of file pages (a busy process never gets the idle file trim) on an idle Vision Pro. (FaceTimeSupportService,
# which callservicesd also asks for, ran the same loop on togetherd and leaked ~40 MB/s.)
XR_UI="com.apple.togetherd com.apple.installcoordinationd com.apple.backboardd com.apple.SurfBoard com.apple.vhmdd com.apple.wakeboardd
	com.apple.homed com.apple.healthd com.apple.geod com.apple.navd
	com.apple.RealityEnvironment
	com.apple.PointerUI.pointeruid com.apple.sleepd com.apple.TextInput.kbd com.apple.FileCoordination
	com.apple.usernotificationsd com.apple.assistantd com.apple.splashboardd com.apple.assetsubscriptiond
	com.apple.mobileassetd com.apple.searchd com.apple.corespotlightservice
	com.apple.CoreAuthentication.daemon com.apple.storekitd com.apple.quicklook.ThumbnailsAgent com.apple.midiserver-sim
	com.apple.pasteboard.pasted com.apple.wcd com.apple.voiced com.apple.accessibility.axassetsd
	com.apple.ctkd com.apple.syncdefaultsd com.apple.accountsd com.apple.amsaccountsd
	com.apple.locationd com.apple.FileProvider com.apple.revisiond com.apple.devicemanagementclient.managedappsd
	com.apple.GameController.gamecontrollerd com.apple.LocalAuthenticationUIService
	com.apple.devicecheckd com.apple.AuthenticationServicesCore.AuthenticationServicesAgent
	com.apple.contactsd com.apple.calaccessd com.apple.remindd com.apple.assetsd com.apple.mediaremoted
	com.apple.gamed com.apple.gamesaved
	com.apple.linkd com.apple.mediaanalysisd com.apple.mediaanalysisd.service com.apple.siriactionsd
	com.apple.managedassetsd com.apple.eligibilityd com.apple.telephonyutilities.callservicesd com.apple.videosubscriptionsd com.apple.shazamd
	com.apple.WebBookmarks.webbookmarksd com.apple.medialibraryd com.apple.backgroundassets.user com.apple.itunescloudd com.apple.biomed"
XR_UI_SLIM="com.apple.backboardd com.apple.SurfBoard com.apple.vhmdd com.apple.wakeboardd com.apple.RealityEnvironment com.apple.TextInput.kbd
	com.apple.FileCoordination"
# No fpsim2host on tvOS / watchOS: their FileProvider has no FPSimulatorSupportInterface (no Files app), so it exited 1
# at every boot. visionOS's has it, and fileproviderd needs the sim2host service there too: without it every sim2host
# call failed and an app's replicated FileProvider domain could not even be added ("cannotSetXattr" for its
# com.apple.file-provider-domain-id, POSIX 22; tests/apps/sdkcov func:FileProvider.replicated-extension-domain).
case $A64_PLATFORM in
	tvos) UI=$TV_UI; UI_SLIM=$TV_UI_SLIM; FULLUI=; OUR_JOBS="com.x86sim.openurl=host/openurl/$A64_HOSTDIR/openurl $FSE_JOB" ;;
	watchos) UI=$WATCH_UI; UI_SLIM=$WATCH_UI_SLIM; FULLUI=; OUR_JOBS="com.x86sim.openurl=host/openurl/$A64_HOSTDIR/openurl $NI_JOB $FSE_JOB" ;;
	xros) UI=$XR_UI; UI_SLIM=$XR_UI_SLIM; FULLUI=; OUR_JOBS="$MM_JOB com.x86sim.openurl=host/openurl/$A64_HOSTDIR/openurl
		com.x86sim.fpsim2host=host/fpsim2host/$A64_HOSTDIR/fpsim2host|mach:com.apple.FileProvider.sim2host $NI_JOB $FSE_JOB" ;;
esac
case "${A64_JOBS:-std}" in
	min) JOBS="$MIN $OUR_JOBS" ;;
	slim) JOBS="$MIN $UI_SLIM $OUR_JOBS" ;;
	full) JOBS="$MIN $UI $FULLUI $OUR_JOBS" ;;
	*) JOBS="$MIN $UI $OUR_JOBS" ;;
esac
# Speed 18: the device's feature toggles ($D/features.json; definitions, costs and presets in features-def.json, edited
# by tools/launcher: x86sim-aot features, the launcher GUI, x86sim-mcp). A job is left out only when every feature
# listing it is off; boot-critical jobs are never listed. Applied at boot, so changes take effect at the next boot.
eval "$(python3 - "$D/features.json" "$X/cli/libexec/arm64/features-def.json" "$D" <<'PY'
import glob, json, os, plistlib, sys
try:
	prof = json.load(open(sys.argv[1])).get("features", {})
except Exception:
	prof = {}
d = json.load(open(sys.argv[2]))["features"]
on = {f["id"]: prof.get(f["id"], f.get("default", False)) for f in d}
# RAM 15: with the wallpaper off SpringBoard shows the poster's saved snapshot (RuntimeSnapshot-<hardware id>-home), which
# PosterKit keys by the screen. A phone made from another model's data (every new phone copies the main phone's) has
# none for its own screen and its home was black (iPhone 18 Pro Max from an iPhone 17's data). Such a boot runs the
# wallpaper's jobs once: PosterBoard renders and saves this screen's snapshot, later boots find it and stay without.
note = ""
if not on.get("wallpaper"):
	env = dict(l.rstrip("\n").split("=", 1) for l in open(os.path.join(sys.argv[3], "sim.env")) if "=" in l)
	try:
		want = {(int(float(env["SIMULATOR_MAINSCREEN_WIDTH"])), int(float(env["SIMULATOR_MAINSCREEN_HEIGHT"])))}
	except (KeyError, ValueError):
		want = set()
	want |= {(h, w) for w, h in want}
	have = set()
	for m in glob.glob(os.path.join(sys.argv[3], "data/Library/Application Support/PRBPosterExtensionDataStore/*/Extensions/*/configurations/*/versions/*/RuntimeSnapshotMetadata-*-home.plist")):
		try:
			md = plistlib.load(open(m, "rb"))
			w, h = (float(x) for x in md["PUIPosterSnapshotBundleInfoKeyAssetSize"].strip("{}").split(","))
			sc = float(md.get("PUIPosterSnapshotBundleInfoKeyScale", 1))
			have.add((round(w * sc), round(h * sc)))
		except Exception:
			pass
	if want and not want & have:
		on["wallpaper"] = True
		note = "no wallpaper snapshot for this screen: PosterBoard runs this boot to make one"
keep = {j for f in d if on[f["id"]] for j in f.get("jobs", [])}
drop = sorted({j for f in d if not on[f["id"]] for j in f.get("jobs", [])} - keep)
add = [j for f in d if on[f["id"]] for j in f.get("add_jobs", [])]
park = [p for f in d if not on[f["id"]] for p in f.get("park", [])]
unpark = [p for f in d if on[f["id"]] for p in f.get("unpark", [])]
keepw = any(on[f["id"]] and f.get("keep_widgets") for f in d)
off = [f["id"] for f in d if on[f["id"]] != f.get("default", False) and not (note and f["id"] == "wallpaper")]
print(f'FEAT_NOTE="{note}"')
print(f'FEAT_DROP="{" ".join(drop)}" FEAT_ADD="{" ".join(add)}" FEAT_PARK="{",".join(park)}" FEAT_UNPARK="{" ".join(unpark)}" FEAT_KEEPW={int(keepw)} FEAT_CHANGED="{" ".join(off)}"')
PY
)"
[ -n "$FEAT_CHANGED" ] && echo "  features changed from the defaults: $FEAT_CHANGED"
[ -n "$FEAT_NOTE" ] && echo "  $FEAT_NOTE"
JOBS="$JOBS $FEAT_ADD"
[ "$FEAT_KEEPW" = 1 ] && A64_KEEP_WIDGETS=${A64_KEEP_WIDGETS:-1}
[ -n "$FEAT_PARK" ] && A64_PARK_EXTRA=${A64_PARK_EXTRA:+$A64_PARK_EXTRA,}$FEAT_PARK
# drop_job LABEL: leave a job out of JOBS, as a label or a label=executable entry. (A sed over the multi-line lists
# missed every job at the end of a line: the wallet, siri and maps features' financed, sirittsd and mapssyncd stayed.)
drop_job() {
	local j out=
	for j in $JOBS; do
		case $j in "$1"|"$1"=*) ;; *) out="$out $j" ;; esac
	done
	JOBS=$out
}
# A64_DROP="label ...": leave these jobs out (experiments)
for j in ${A64_DROP:-} $FEAT_DROP; do drop_job "$j"; done
# Platforms 1: the shared lists name iOS jobs a tvOS / watchOS runtime doesn't ship (cfprefsd's system daemon,
# containermanagerd.system, fontservicesd, appprotectiond, ...): those are left out there
if [ "$A64_PLATFORM" != ios ]; then
	left=
	for j in $JOBS; do
		case $j in *=*) continue ;; esac
		[ -f "$X86SIM_RUNTIME/System/Library/LaunchDaemons/$j.plist" ] || [ -f "$X86SIM_RUNTIME/System/Library/LaunchAngels/$j.plist" ] ||
			[ -f "$X86SIM_RUNTIME/Developer/Library/LaunchDaemons/$j.plist" ] && continue
		drop_job "$j"; left="$left $j"
	done
	[ -n "$left" ] && echo "  not in this $A64_PLATFORM runtime:$left"
fi
# Speed 10: A64_CLONE_FROM=<device dir> starts a new device (no data/ yet) from a copy of that device's data/, like
# simctl clone. A fresh device's first boot otherwise builds the LaunchServices database (lsd) while SpringBoard's icon
# requests wait on it: ~80 s, and SpringBoard was killed and relaunched at ~90 s (home after ~5 min). The store refers
# to the apps' data containers, so the whole data/ is copied (reflinks), without logs, run state, tmp and sockets.
if [ -n "${A64_CLONE_FROM:-}" ] && [ ! -d "$D/data" ] && [ -d "$A64_CLONE_FROM/data/var/db/lsd" ]; then
	mkdir -p "$D"
	cp -a --reflink=auto "$A64_CLONE_FROM/data" "$D/data" || exit 1
	rm -rf "$D/data/var/log" "$D/data/var/run" "$D/data/tmp" "$D/data/simhid.sock" "$D/data/framebuffer.bgra"
	mkdir -p "$D/data/var/log" "$D/data/var/run" "$D/data/tmp"
	echo "  cloned data/ from $A64_CLONE_FROM"
fi
# Speed 21: os_log DT mode (formatted text of every log call on the job's stderr log, and a stream to diagnosticd) is
# off by default: it was ~25% of SpringBoard's CPU per tap (0.84 -> 0.61 CPU-s; tap response 0.57 -> 0.45 s). Real iOS
# never formats on the send path. X86SIM_NODT= (empty) brings the per-job os_log text back, X86SIM_NODT=all,-<label>
# for one job; x86sim-stderr-progs and X86SIM_OSLOG still work per process.
X86SIM_NODT=${X86SIM_NODT-all} X86SIM_JOBLOG=1 python3 cli/libexec/mksimdevice.py "$D" $JOBS "$@" > /dev/null || exit 1
# Speed 8: the container gets a private /dev/shm (X86SIM_PRIVATE_SHM, darlingserver): its remount no longer stacks a
# tmpfs over the host's /dev/shm, and two devices' shm objects stay apart. The frame ring is written to
# /run/user/<uid> (sim.env SIMFB_RING); $SIMFB_RING_LINUX (default /dev/shm/x86sim-a64-simfb.ring) links to it.
export X86SIM_PRIVATE_SHM=1
# Speed 28: the AOT image's huge-page copy (libarm64jit, /tmp/x86sim-a64-img): a boot retries a copy the last one
# dropped for lack of 2 MB pages.
rm -f /tmp/x86sim-a64-img/*.small 2>/dev/null
RING=$(sed -n 's#^SIMFB_RING=/Volumes/SystemRoot##p' "$D/sim.env")
# One /dev/shm link per phone, named after its ring (the launcher's SIMFB_RING_LINUX). A SIMFB_RING_LINUX naming another
# phone's link (e.g. the generic default inherited from the environment) is left alone: re-pointing it made the main
# phone's viewers and the gate read a second phone's screen.
if [ -n "$RING" ]; then
	ln -sfn "$RING" "/dev/shm/$(basename "$RING")"
	if [ -n "${SIMFB_RING_LINUX:-}" ] && [ "$RING" != "$SIMFB_RING_LINUX" ] && [ "$(basename "$SIMFB_RING_LINUX")" = "$(basename "$RING")" ]; then
		ln -sfn "$RING" "$SIMFB_RING_LINUX"
	fi
fi
# Strip home-screen / Today widgets so SpringBoard does not demand WidgetRenderer (even with park).
# A64_KEEP_WIDGETS=1 leaves IconState alone (full profile / wallpaper experiments).
# Platforms 1: SpringBoard's (iOS) only
if [ "${A64_KEEP_WIDGETS:-0}" != 1 ] && [ "$A64_PLATFORM" = ios ]; then
	python3 - "$D/data/Library/SpringBoard" "$(sed -n 's/^SIMULATOR_LEGACY_ASSET_SUFFIX=//p' "$D/sim.env")" \
		"$X86SIM_RUNTIME/System/Library/CoreServices/SpringBoard.app" <<'PY'
import os, sys, plistlib
d = sys.argv[1]
suffix, sbapp = sys.argv[2], sys.argv[3]
os.makedirs(d, exist_ok=True)

def strip(obj):
	if isinstance(obj, list):
		out = []
		for x in obj:
			# RAM 2: also the Shortcuts widget (elementType shortcutsSingle / shortcutsStack, no widgetIdentifier),
			# and any other custom grid element that is not a folder or a widget stack (all are widgets)
			if isinstance(x, dict) and (x.get("elementType") in ("widget", "shortcutsSingle", "shortcutsStack")
					or "widgetIdentifier" in x
					or (x.get("iconType") == "custom" and x.get("elementType") not in (None, "folder", "stack"))):
				continue
			# drop emptied widget stacks / custom placeholders
			if isinstance(x, dict) and x.get("iconType") == "custom" and not x.get("elements") and "widgetIdentifier" not in x and x.get("elementType") != "folder":
				# keep if it still has meaningful keys beyond stack chrome
				keys = set(x) - {"allowsExternalSuggestions","allowsSuggestions","displayIdentifier","elements","gridSize","iconType","uniqueIdentifier"}
				if not keys:
					continue
			sx = strip(x)
			if isinstance(sx, dict) and sx.get("elements") == [] and sx.get("iconType") == "custom":
				continue
			out.append(sx)
		return out
	if isinstance(obj, dict):
		return {k: strip(v) for k, v in obj.items()}
	return obj

# Speed 9: a fresh device has no IconState yet, so SpringBoard wrote its default layout on the first boot, with the
# Maps and Calendar widgets (two blank white tiles: no chronod / WidgetRenderer). Seed the same layout without widgets.
seed = os.path.join(d, "IconState.plist")
if not os.path.exists(seed):
	import uuid
	u = lambda: str(uuid.uuid4()).upper()
	utilities = {"defaultDisplayName": "Utilities", "displayName": "Utilities", "listType": "folder",
		"iconLists": [["com.apple.shortcuts", "com.apple.Passwords"]], "listUniqueIdentifiers": [u()], "uniqueIdentifier": u()}
	layout = {"buttonBar": ["com.apple.mobilesafari", "com.apple.MobileSMS"], "defaultDisplayName": "Folder", "displayName": "Folder",
		"iconLists": [["com.apple.mobilecal", "com.apple.mobileslideshow", "com.apple.Maps", "com.apple.news", "com.apple.Health",
			"com.apple.Passbook", "com.apple.SiriApp", "com.apple.Preferences"],
			["com.apple.Fitness", "com.apple.Bridge", "com.apple.reminders", "com.apple.DocumentsApp", "com.apple.Preview", utilities,
			"com.apple.MobileAddressBook"]],
		"ignored": ["com.apple.TVRemoteApp"], "listUniqueIdentifiers": [u(), u()], "today": [], "uniqueIdentifier": u()}
	# iPad: SpringBoard's iPad default (DefaultIconState~ipad.plist: Files, Reminders, Maps, News, Settings on the first
	# page, Messages / Safari / Calendar / Photos / Siri in the dock, as Apple's iPad simulator shows them), without its
	# widgets ("today", and the "small" / "medium" / "large" slots of its first page). The iPhone layout above on an iPad
	# put iPhone's first page and a two-app dock on the iPad's home screen.
	ipad = os.path.join(sbapp, "DefaultIconState~ipad.plist")
	if suffix == "ipad" and os.path.exists(ipad):
		with open(ipad, "rb") as f:
			dflt = plistlib.load(f)
		# apps and folders only: widget slots are names ("small" ...) or custom elements (stacks, the Files suggestion)
		lists = [[x for x in l if (isinstance(x, str) and x not in ("small", "medium", "large", "extraLarge"))
			or (isinstance(x, dict) and x.get("listType") == "folder")] for l in dflt.get("iconLists", [])]
		layout = {"buttonBar": dflt.get("buttonBar", []), "defaultDisplayName": "Folder", "displayName": "Folder",
			"iconLists": [l for l in lists if l], "ignored": dflt.get("ignored", []), "today": [], "uniqueIdentifier": u()}
		layout["listUniqueIdentifiers"] = [u() for _ in layout["iconLists"]]
	with open(seed, "wb") as f:
		plistlib.dump(layout, f)
	print("  seeded a widget-free IconState.plist (fresh %s)" % (suffix or "device"), flush=True)

for name in ("IconState.plist", "DesiredIconState.plist"):
	path = os.path.join(d, name)
	if not os.path.exists(path):
		continue
	with open(path, "rb") as f:
		data = plistlib.load(f)
	with open(path, "wb") as f:
		plistlib.dump(strip(data), f)
	print(f"  stripped widgets from {name}", flush=True)
PY
fi
# Accessibility off at boot, as on a fresh simulator. host/simax turns automation and app accessibility on the first
# time a process is asked for its tree (_AXSSetAutomationEnabled, _AXSApplicationAccessibilitySetEnabled), and those
# are the device's preferences: they stayed on across boots, so every app of a phone inspected once exposed its
# accessibility elements as if VoiceOver ran (sdkcov func:SwiftUI.p27-accessibility-elements: 4 elements, Apple's
# simulator 0). simax turns them on again when the phone is inspected.
python3 - "$D/data/Library/Preferences/com.apple.Accessibility.plist" <<'PY'
import os, plistlib, sys
path = sys.argv[1]
if os.path.exists(path):
	with open(path, "rb") as f:
		prefs = plistlib.load(f)
	on = [k for k in ("AutomationEnabled", "ApplicationAccessibilityEnabled") if prefs.get(k)]
	if on:
		for k in on:
			prefs[k] = 0
		with open(path + ".new", "wb") as f:
			plistlib.dump(prefs, f, fmt=plistlib.FMT_BINARY)
		os.replace(path + ".new", path)
		print("  accessibility automation off (left on by an inspection: %s)" % ", ".join(on), flush=True)
PY
UDID=$(sed -n 's/^SIMULATOR_UDID=//p' "$D/sim.env")
L=$DPREFIX/private/var/tmp/com.apple.CoreSimulator.SimDevice.$UDID/launchd.log
rm -f "$L" "$L".1 "$L".2
R="/Volumes/SystemRoot$X86SIM_RUNTIME"
# Default slim park list: path substrings matched in arm64host before dyld. A64_SLIM=0 clears it.
# Do not park MachServices daemons (cloudd, diagnosticd): lookup succeeds then clients block forever
# on the unreplied port. Those are in MIN so securityd's lookups resolve to real (lean) processes.
# Speed 15: Spotlight is no longer parked (the home screen's Search pill showed nothing). It costs ~320 MB PSS.
# A64_PARK overrides the list (old default: Spotlight.app,WidgetRenderer).
PARK_LIST=${A64_PARK-WidgetRenderer}${A64_PARK_EXTRA:+,$A64_PARK_EXTRA}
for u in $FEAT_UNPARK; do PARK_LIST=$(echo ",$PARK_LIST," | sed "s/,$u,/,/g; s/^,//; s/,$//"); done
if [ "${A64_SLIM:-1}" = 1 ] && [ "${A64_JOBS:-std}" != full ]; then
	if grep -q '^X86SIM_A64_SLIM_PARK=' "$D/sim.env" 2>/dev/null; then
		sed -i "s|^X86SIM_A64_SLIM_PARK=.*|X86SIM_A64_SLIM_PARK=$PARK_LIST|" "$D/sim.env"
	else
		echo "X86SIM_A64_SLIM_PARK=$PARK_LIST" >>"$D/sim.env"
	fi
	# File fallback: launchd does not pass sim.env into job environ; arm64host reads this path.
	echo "$PARK_LIST" > "$X86SIM_SCRATCH/a64-slim-park"
else
	rm -f "$X86SIM_SCRATCH/a64-slim-park"
fi
ENVS=$(sed 's/^/"/; s/$/"/' "$D/sim.env" | tr '\n' ' ')
ENVS="$ENVS ${A64_ENV:-}"   # A64_ENV: extra K=V for launchd_sim (e.g. X86SIM_A64_TRACE=1)
# stop only this prefix's container. Its launchers (darling shell, and the `timeout` that holds the mDNSResponder
# link) are found by their command line (this device's path, this state dir, or tests/arm64/run.sh's a64home), so
# the x86_64 container's are never touched. Then darling shutdown, then a sweep of what it misses: darling shutdown
# SIGKILLs darlingserver and the first entry of /proc/<darlingserver>/task/<main>/children only, and launchd is often
# not a child of the main thread (or the entry is another process), so launchd -- pid 1 of the container's pid
# namespace -- survived, reparented to the session's subreaper, with every daemon and app of the old boot under it.
# All of these run as the user: plain kill works where sudo is not available.
# A namespace init is this prefix's when its mount table has the prefix overlay mounted at exactly $DPREFIX.
prefix_procs() {
	local d p
	for d in /proc/[0-9]*; do
		p=${d#/proc/}
		case "$(cat "$d/comm" 2>/dev/null)" in
		darlingserver)
			tr '\0' ' ' < "$d/cmdline" 2>/dev/null | grep -q "^darlingserver $DPREFIX " && echo "$p" ;;
		mldr)  # Darling processes run under mldr: launchd is comm mldr
			[ "$(awk '/^NSpid:/{print $NF}' "$d/status" 2>/dev/null)" = 1 ] || continue
			awk -v m="$DPREFIX" '$5 == m { f = 1 } END { exit !f }' "$d/mountinfo" 2>/dev/null && echo "$p" ;;
		esac
	done
}
stop_prefix() {
	local pids="" d p i left
	for d in /proc/[0-9]*; do
		p=${d#/proc/}
		[ -r "$d/cmdline" ] || continue
		tr '\0' '\n' < "$d/cmdline" 2>/dev/null | grep -qxF "$X86SIM_DARLING" || continue
		tr '\0' '\n' < "$d/cmdline" 2>/dev/null | grep -q -e "$D/" -e "$ST/" -e "/a64home" && pids="$pids $p"
	done
	if [ -n "$pids" ]; then
		kill -9 $pids 2>/dev/null
		# the setuid darling launchers (real uid 0) refuse the user's signal; they exit with their container
		x86sim_sudo -n kill -9 $pids 2>/dev/null </dev/null
	fi
	# an earlier boot's helper subshells (the 90 s AOT status check, the 60 s prefetch snapshot) outlived the stop,
	# and the launcher showed the phone "starting" while one ran: end those of this device (not this script's)
	for d in /proc/[0-9]*; do
		p=${d#/proc/}
		[ "$p" = $$ ] && continue
		[ "$(cat "$d/comm" 2>/dev/null)" = boot-a64.sh ] || continue
		tr '\0' '\n' 2>/dev/null < "$d/environ" | grep -qxF "A64_DEVICE=$D" || continue
		tr '\0' '\n' 2>/dev/null < "$d/environ" | grep -qx 'A64_STOP_ONLY=1' && continue
		# its children but an mtlvk (one an older boot-a64.sh started as its child): other phones may render through it,
		# stop_a64_mtlvk / the launcher end it when none does
		for c in $(pgrep -P "$p"); do [ "$(cat "/proc/$c/comm" 2>/dev/null)" = mtlvk ] || kill "$c" 2>/dev/null; done
		kill "$p" 2>/dev/null
	done
	timeout 30 "$X86SIM_DARLING" shutdown 2>/dev/null
	for i in $(seq 1 50); do
		left=$(prefix_procs)
		[ -z "$left" ] && break
		[ "$i" = 1 ] || [ "$i" = 20 ] && echo "boot-a64: ending leftover container processes of $DPREFIX:" $left >&2
		kill -9 $left 2>/dev/null
		sleep 0.1
	done
	local ip; ip=$(cat "$DPREFIX/.init.pid" 2>/dev/null)
	if [ -z "$ip" ] || [ ! -d "/proc/$ip" ] || grep -q "^State:.*Z" "/proc/$ip/status" 2>/dev/null; then
		rm -f "$DPREFIX/.init.pid" "$DPREFIX/.darlingserver.sock"
	fi
}
stop_prefix
# Speed 23: jetsam stand-in (cli/libexec/jetsam.py): reclaims extensions and older
# background apps that RunningBoard keeps suspended, as a device's memorystatus does. Without it every poster
# extension PosterBoard updated at boot (12 of them, ~470 MB PSS) and every app opened stayed resident.
# X86SIM_JETSAM=0: off. Scoped to this prefix's container (jetsam.container_ns).
# The boot's host helpers (jetsam, mDNS, simaudio): stopped before a boot and when a timed boot ends.
stop_helpers() {
	[ -f "$ST/jetsam.pid" ] && kill "$(cat "$ST/jetsam.pid")" 2>/dev/null
	rm -f "$ST/jetsam.pid"
	[ -f "$ST/mdns.pid" ] && kill "$(cat "$ST/mdns.pid")" 2>/dev/null
	# and one whose pidfile a later boot overwrote (two from the day before were still serving var/run-a64); matched by
	# the state dir's name so one started with a relative path (var/run-a64-<name>) is found too
	pkill -f "mdnsresponder.py .*/$(basename "$ST")/mDNSResponder\$" 2>/dev/null
	rm -f "$ST/mdns.pid" "$ST/mDNSResponder"
	[ -f "$ST/simaudio.pid" ] && kill "$(cat "$ST/simaudio.pid")" 2>/dev/null
	rm -f "$ST/simaudio.pid" "$ST/simaudio-tap.sock"
	[ -f "$ST/fmrelay.pid" ] && kill "$(cat "$ST/fmrelay.pid")" 2>/dev/null
	rm -f "$ST/fmrelay.pid"
}
stop_helpers
# Speed 10: A64_STOP_ONLY=1 stops this device: the container, and its mtlvk unless another phone renders through it
# (A64_KEEP_MTLVK=1: the container only -- the launcher's restart keeps the GPU helper, its stop ends it itself)
if [ "${A64_STOP_ONLY:-0}" = 1 ]; then
	[ "${A64_KEEP_MTLVK:-0}" = 1 ] || stop_a64_mtlvks
	# RAM 15: the frame rings live in /run/user (tmpfs, RAM): a stopped phone's stayed resident (4 frames each, 50 MB
	# for an iPhone 17, 140 MB for the Duo's two screens) until its next boot replaced them. The next boot makes new ones.
	R=$(sed -n 's#^SIMFB_RING=/Volumes/SystemRoot##p' "$D/sim.env" 2>/dev/null)
	[ -n "$R" ] && rm -f "$R" "${R%.ring}"-[0-9].ring
	# a device path too long for a Unix socket: simhid's socket is in /run/user too, data/simhid.sock links to it
	[ -L "$D/data/simhid.sock" ] && rm -f "$(readlink "$D/data/simhid.sock")" "$D/data/simhid.sock"
	exit 0
fi
# the marks of the last boot (Darling pids are reused)
# (x86sim-iopol: the dataless-materialization policy apps were spawned with, Darling's iopolicysys.c)
rm -rf "$DPREFIX/private/var/run/x86sim-suspended" "$DPREFIX/private/var/run/x86sim-jetsam" \
	"$DPREFIX/private/var/run/x86sim-iopol" "$DPREFIX/private/var/run/x86sim-unsupported.log"
# libarm64jit's per-process logs (X86SIM_A64_LOG, <exe>.<pid>.log): the last boot's are kept as x86sim-a64.prev, older
# ones go. They were never removed: ~100 files a boot, 7000 files / 253 MB in a day on an sdkcov device.
if [ -d "$DPREFIX/private/var/tmp/x86sim-a64" ]; then
	rm -rf "$DPREFIX/private/var/tmp/x86sim-a64.prev"
	mv "$DPREFIX/private/var/tmp/x86sim-a64" "$DPREFIX/private/var/tmp/x86sim-a64.prev"
	mkdir -p "$DPREFIX/private/var/tmp/x86sim-a64"
fi
# the Darling pieces our build fixed (libsystem_kernel ...), also into a recreated prefix: cli/libexec/arm64/prefix-heal.sh
[ "${A64_PREFIX_HEAL:-1}" != 0 ] && sh "$X/cli/libexec/arm64/prefix-heal.sh" "$DPREFIX"
# RAM 14: the container's own macOS daemons (Darling's launchd jobs, x86_64) that the simulator never needs at load.
# Mach-service daemons start on demand instead (a lookup from the guest still starts them); eventmonitor (launchd's
# WatchPaths for the container's own jobs) and settingsd (the Linux desktop's appearance for macOS apps) not at all.
# ~35 MB per phone. The copies in the prefix's upper layer are made from the installed ones at every boot.
# A64_HOST_DAEMONS=all: every job as installed.
if [ "${A64_HOST_DAEMONS:-}" != all ]; then
	python3 -I - "${X86SIM_DARLING_ROOT:-$X/darling-prefix/libexec/darling}/System/Library/LaunchDaemons" "$DPREFIX/System/Library/LaunchDaemons" <<'PY' || true
import os, plistlib, sys
src, dst = sys.argv[1:3]
ondemand = ["com.apple.usernoted", "com.apple.locationd", "com.apple.coreservices.launchservicesd", "com.apple.securityd",
            "com.apple.aslmanager", "com.apple.diskarbitrationd"]
off = ["org.darlinghq.eventmonitor", "com.darlinghq.settingsd"]
# launchd skips a job whose plist (or a folder above it) others can write, as on macOS, and the system bootstrap then
# never started shellspawn: no phone booted where the user's umask is 002 (Ubuntu's default for login users).
os.umask(0o022)
os.makedirs(dst, exist_ok=True)
for d in (os.path.dirname(os.path.dirname(dst)), os.path.dirname(dst), dst):
    if os.stat(d).st_mode & 0o022:
        os.chmod(d, 0o755)
for name in ondemand + off:
    try:
        with open(os.path.join(src, name + ".plist"), "rb") as f:
            p = plistlib.load(f)
    except OSError:
        continue
    if name in off:
        p["Disabled"] = True
    elif p.get("MachServices"):
        p.pop("RunAtLoad", None)
        p.pop("KeepAlive", None)
    else:
        continue
    with open(os.path.join(dst, name + ".plist"), "wb") as f:
        plistlib.dump(p, f)
    os.chmod(os.path.join(dst, name + ".plist"), 0o644)  # a copy from an earlier boot keeps its mode
PY
else
	rm -f "$DPREFIX"/System/Library/LaunchDaemons/*.plist
fi
# Speed 27: frameworks a Mac host has in /System/Library/Frameworks that the simulator opens there by absolute path (not
# under its runtime root): CoreSpotlight reads its localized strings
# from [NSBundle bundleWithPath:@"/System/Library/Frameworks/CoreSpotlight.framework"]. Without it every CoreSpotlight string was nil (PHOTO_TOKEN) and
# Spotlight crashed on the first typed letter ("attempt to insert nil object" in -[_CSSuggestionToken init...]). The
# booted runtime's own copy (its real path) is linked in the prefix's upper layer at every boot.
# _ManagedAppDistribution_SwiftUI (visionOS) likewise takes its strings from [NSBundle bundleWithURL:/System/Library/
# Frameworks/_ManagedAppDistribution_SwiftUI.framework]: without it ManagedContentView's not-installed offer button had no
# accessibility label (Apple's visionOS 27.2 simulator: "Redownload").
for fw in CoreSpotlight _ManagedAppDistribution_SwiftUI; do
	[ -d "$X86SIM_RUNTIME/System/Library/Frameworks/$fw.framework" ] || continue
	mkdir -p "$DPREFIX/System/Library/Frameworks"
	ln -sfn "/Volumes/SystemRoot$(realpath "$X86SIM_RUNTIME")/System/Library/Frameworks/$fw.framework" \
		"$DPREFIX/System/Library/Frameworks/$fw.framework"
done
# The same for the LinguisticData catalog: DataDeliveryServices (kbd) reads the UAF locales of the linguistic assets from
# /System/Library/LinguisticData/Info.plist by absolute path (DDSUAFSupportedLocalesForLinguisticDataAssets), on a Mac
# the host's copy. Without it no locale was supported: kbd's "prewarm-embedding-assets" and TextInput assertions made no
# UAF com.apple.linguisticdata subscription ("No asset set usages created from query"), the mBERT asset was never
# downloaded and NLContextualEmbedding.hasAvailableAssets stayed false where Apple's visionOS 27.2 simulator answers true
# once mobileassetd has it (P7_ML.swift).
if [ -d "$X86SIM_RUNTIME/System/Library/LinguisticData" ]; then
	mkdir -p "$DPREFIX/System/Library"
	ln -sfn "/Volumes/SystemRoot$(realpath "$X86SIM_RUNTIME")/System/Library/LinguisticData" "$DPREFIX/System/Library/LinguisticData"
fi
# Private frameworks too: TextToSpeech opens [NSBundle bundleWithPath:@"/System/Library/PrivateFrameworks/
# TextToSpeechMauiSupport.framework"] (the built-in voices, TTSResources): on a Mac that is the host's copy. Without it
# AVSpeechSynthesisVoice.speechVoices() was empty (axassetsd's VoiceDB: OnDiskVoiceError) and AVSpeechSynthesizer silent.
for fw in TextToSpeechMauiSupport; do
	mkdir -p "$DPREFIX/System/Library/PrivateFrameworks"
	ln -sfn "/Volumes/SystemRoot$(realpath "$X86SIM_RUNTIME")/System/Library/PrivateFrameworks/$fw.framework" \
		"$DPREFIX/System/Library/PrivateFrameworks/$fw.framework"
done
# The SF Symbols catalogs: CoreLocationUI's button drawing (and IconServices, SocialLayer) opens [NSBundle bundleWithPath:
# @"/System/Library/CoreServices/CoreGlyphs.bundle"], on a Mac the host's copy. Without it -[CLLocationButtonDrawing
# _computeImageMetrics] bailed before setting its size: CLLocationButton.sizeThatFits was 1x1 (Apple: 204x51).
for b in CoreGlyphs CoreGlyphsPriority CoreGlyphsPrivate; do
	[ -d "$X86SIM_RUNTIME/System/Library/CoreServices/$b.bundle" ] || continue
	mkdir -p "$DPREFIX/System/Library/CoreServices"
	ln -sfn "/Volumes/SystemRoot$(realpath "$X86SIM_RUNTIME")/System/Library/CoreServices/$b.bundle" \
		"$DPREFIX/System/Library/CoreServices/$b.bundle"
done
# Bundles Darling's own tree also has (a macOS stub, no resources): the Sign in with Apple button drawing takes its
# Apple logo PDF and strings from +[AKUIBundleProvider bundle] = [NSBundle bundleWithPath:@"/System/Library/
# PrivateFrameworks/AuthKitUI.framework"], on a Mac the host's copy. Here it was Darling's stub: no logo, and every app
# showing an ASAuthorizationAppleIDButton / SignInWithAppleButton aborted (assertion "page" in _DrawLogoPDFIntoContext).
# Darling's stub stays (its x86_64 binary): the runtime copy's entries it lacks are linked into a real directory in the
# prefix's upper layer, which the overlay merges with Darling's.
DLOWER=${X86SIM_DARLING_ROOT:-$X/darling-prefix/libexec/darling}
for fw in PrivateFrameworks/AuthKitUI; do
	src="$(realpath "$X86SIM_RUNTIME")/System/Library/$fw.framework" dst="$DPREFIX/System/Library/$fw.framework"
	[ -d "$src" ] || continue
	[ -L "$dst" ] && rm -f "$dst"
	mkdir -p "$dst"
	for e in "$src"/*; do
		[ -e "$DLOWER/System/Library/$fw.framework/${e##*/}" ] || ln -sfn "/Volumes/SystemRoot$e" "$dst/${e##*/}"
	done
done
# Host frameworks no runtime has: ARKit's errors (ns_error_create_with_code_and_userinfo) take their descriptions from
# ARKitCoreBundle() = [NSBundle bundleWithPath:@"/System/Library/PrivateFrameworks/ARKitCore.framework"], on a Mac the
# host's macOS framework (Info.plist only: the description is the key, e.g. "Adding an object failed."). With no such
# bundle every description was nil: visionOS WorldTrackingProvider.addAnchor failed "com.apple.arkit error 200" where
# Apple's visionOS 27.2 simulator says "Adding an object failed." (P2_Reality.swift). A bundle with the host's identifier.
fw="$DPREFIX/System/Library/PrivateFrameworks/ARKitCore.framework"
[ -e "$DLOWER/System/Library/PrivateFrameworks/ARKitCore.framework" ] || [ -f "$fw/Info.plist" ] || {
	mkdir -p "$fw" && python3 -c 'import plistlib,sys; plistlib.dump({"CFBundleIdentifier": "com.apple.ARKitCore",
		"CFBundleName": "ARKitCore", "CFBundlePackageType": "FMWK", "CFBundleDevelopmentRegion": "en",
		"CFBundleInfoDictionaryVersion": "6.0"}, open(sys.argv[1], "wb"))' "$fw/Info.plist"; }
# TextToSpeech's voice database (axassetsd, /var/mobile/Library/Accessibility/voicedb.sqlite) is refilled at every
# start by its loaders, each skipped when com.apple.SpeakSelection's PreferencesLastLoadedOn says it already loaded in
# this boot: the value is kern.boottime, which here is the Linux host's for every device boot. So after the first
# boot the rows were cleared and never reloaded: AVSpeechSynthesisVoice.speechVoices() empty. A device boot is a new
# boot for it: the marker goes.
python3 - "$D/data/Library/Preferences/com.apple.SpeakSelection.plist" <<'PY' 2>/dev/null
import plistlib, sys
try:
	d = plistlib.load(open(sys.argv[1], "rb"))
except Exception:
	sys.exit(0)
if d.pop("PreferencesLastLoadedOn", None) is not None:
	plistlib.dump(d, open(sys.argv[1], "wb"), fmt=plistlib.FMT_BINARY)
PY
# The Network Extension configuration store: NEConfigurationManager (NEVPNManager / NERelayManager /
# NETunnelProviderManager loadFromPreferences) reads /Library/Preferences/com.apple.networkextension.plist by
# absolute path, on a Mac the host's own (a keyed archive: Version, Generation, Index {UserMap, ConfigurationProperties}
# + one entry per configuration). When it can't read it (not root, no file) it asks com.apple.nehelper, which the
# simulator runtime does not have: every load failed "IPC failed" (NEVPNErrorDomain 5, NEConfigurationErrorDomain 11)
# where Apple's tvOS 27.2 simulator answers ok / 0 managers. An empty store, as a Mac without configurations:
if [ ! -s "$DPREFIX/Library/Preferences/com.apple.networkextension.plist" ]; then
	mkdir -p "$DPREFIX/Library/Preferences"
	python3 - "$DPREFIX/Library/Preferences/com.apple.networkextension.plist" <<'PY'
import plistlib, sys
U = plistlib.UID
md = {"$classname": "NSMutableDictionary", "$classes": ["NSMutableDictionary", "NSDictionary", "NSObject"]}
empty = {"NS.keys": [], "NS.objects": [], "$class": U(6)}
objs = ["$null", {"NS.keys": [U(2), U(4)], "NS.objects": [U(3), U(5)], "$class": U(6)},
	"UserMap", empty, "ConfigurationProperties", dict(empty), md]
plistlib.dump({"$archiver": "NSKeyedArchiver", "$version": 100000, "$objects": objs,
	"$top": {"Version": 1, "Generation": 1, "Index": U(1)}}, open(sys.argv[1], "wb"), fmt=plistlib.FMT_BINARY)
PY
fi
# Speed 27: geod seeds its resource manifest from /System/Library/GeoServicesData/ResourceManifest.json (a Mac's own
# /System/Library has one, the simulator runtime has none); without it there was no active tile group and Maps drew a
# bare globe grid without ever requesting a tile. The seed only has to name a tile group: geod then fetches Apple's
# current manifest and every resource in it (styles, icons, the region / territory / time zone archives) itself, so
# x86Sim writes it (runtimes/geo-seed; 2611252: a tile group of Apple's manifest, 2026-11-25; geod replaces it).
G=$X/runtimes/geo-seed/GeoServicesData
if [ ! -f "$G/ResourceManifest.json" ]; then
	mkdir -p "$G" && echo '{"tileGroup": [{"identifier": 2611252}], "resource": []}' > "$G/ResourceManifest.json"
fi
ln -sfn "/Volumes/SystemRoot$G" "$DPREFIX/System/Library/GeoServicesData"
# Speed 27: the audio devices (what a Mac's CoreSimulator/coreaudiod provide): host/simaudio/arm64/libsimaudio.dylib in
# each process (mksimdevice, X86SIM_AUDIO) streams PCM to cli/libexec/simaudio.py (PipeWire). Without an input device
# Siri's recording failed at once (corespeechd: "siriInputSource 'sino'. No Input available", AudioQueue -66680 finding
# Default-InputOutput, prepareRecordForStream -11790) and Siri never got further. simaudio.py runs on every boot: its
# tap (simaudio-tap.sock) lets x86sim serve play the phone in a browser; X86SIM_AUDIO=1 also plays it on PipeWire and
# records the host's microphone (else input streams get silence, or a browser's microphone through the tap).
X86SIM_AUDIO_PLAYBACK=$([ "${X86SIM_AUDIO:-0}" != 0 ] && echo 1 || echo 0) \
	setsid python3 "$X/cli/libexec/simaudio.py" "$ST/simaudio.sock" > "$ST/simaudio.log" 2>&1 < /dev/null &
echo $! > "$ST/simaudio.pid"
if [ "${X86SIM_JETSAM:-1}" != 0 ]; then
	setsid python3 "$X/cli/libexec/jetsam.py" > "$ST/jetsam.log" 2>&1 < /dev/null &
	echo $! > "$ST/jetsam.pid"
fi
sleep 1

# Host zone name (no darling yet). The TZ link itself must run *inside* limited.sh so it does not birth
# darlingserver in the caller's cgroup (that was the Stage 2/3 escape: only timeout+darling stayed under MemoryMax).
TZNAME=$(readlink -f /etc/localtime 2>/dev/null | sed -n 's#^.*/zoneinfo/##p')
# the phone's own time zone (x86sim timezone ZONE: <device>/timezone), else the host's (as a Mac's simulator)
if [ -s "$D/timezone" ] && [ -e "/usr/share/zoneinfo/$(cat "$D/timezone")" ]; then TZNAME=$(cat "$D/timezone"); fi
H="/Volumes/SystemRoot$X/host/simhost/simhost"

# Payload for `darling shell sh -c` — raw text so $ENVS quote tokens are only parsed by that inner sh.
A64_TIMEOUT=$([ "${A64_KEEP:-0}" = 1 ] && echo 86400 || echo $((SECS + 5)))
CMDFILE=$ST/simhost.cmd
# Speed 26: DNS. On a Mac the simulator's libsystem_dnssd talks to the host's mDNSResponder at /var/run/mDNSResponder;
# linux/mdnsd/mdnsresponder.py plays it. Without one every lookup failed with -1003
# (mobileassetd could not fetch Siri's assets, News/Spotlight/Settings search had no network). Own server per boot
# ($ST/mDNSResponder), linked into this container's /var/run. X86SIM_MDNS=0: none.
MDNS_LINK=
if [ "${X86SIM_MDNS:-1}" != 0 ]; then
	setsid python3 -I "$X/linux/mdnsd/mdnsresponder.py" "$ST/mDNSResponder" > "$ST/mdnsresponder.log" 2>&1 < /dev/null &
	echo $! > "$ST/mdns.pid"
	MDNS_LINK="ln -sf '/Volumes/SystemRoot$ST/mDNSResponder' /var/run/mDNSResponder;"
fi
# FoundationModels relay (X86SIM_FM_RELAY, above): linux/fmrelay/fmrelay.py on $ST/fmrelay.sock, the container's
# /var/run/x86sim-fmrelay.sock, where mmhost hands it the simulator's connections. generativeexperiencesd keeps the host
# model's availability (vmHostAvailability) in /var/mobile/Library/AppleIntelligencePlatform by absolute path: on a Mac
# the host's, which it may not write ("Failed to create directory /var/mobile"), so Apple's simulator starts each boot
# without it; here it landed in the prefix and outlived the boot (with the relay off, or the Mac away, apps still saw
# the model available and failed with ModelManagerError 1008 instead of modelNotReady).
rm -rf "$DPREFIX/var/mobile/Library/AppleIntelligencePlatform"
if [ -n "$MM_JOB" ]; then
	setsid python3 -I "$X/linux/fmrelay/fmrelay.py" "$ST/fmrelay.sock" "$FM_MAC" > "$ST/fmrelay.log" 2>&1 < /dev/null &
	echo $! > "$ST/fmrelay.pid"
	MDNS_LINK="$MDNS_LINK ln -sf '/Volumes/SystemRoot$ST/fmrelay.sock' /var/run/x86sim-fmrelay.sock;"
else
	# without the relay, as before it was used: no availability cache from a boot with it (generativeexperiencesd's
	# com.apple.gms.availability domain and its com.apple.gms.* keys in the global domain; with them apps saw the model
	# available and failed with ModelManagerError 1008, no modelmanagerd, instead of modelNotReady)
	rm -f "$D/data/Library/Preferences/com.apple.gms.availability.plist"
	python3 -I - "$D/data/Library/Preferences/.GlobalPreferences.plist" <<-'PYEOF' 2>/dev/null
	import plistlib, sys
	try:
	    with open(sys.argv[1], "rb") as f: d = plistlib.load(f)
	except OSError: sys.exit()
	gms = [k for k in d if k.startswith("com.apple.gms.")]
	if gms:
	    for k in gms: del d[k]
	    with open(sys.argv[1], "wb") as f: plistlib.dump(d, f, fmt=plistlib.FMT_BINARY)
	PYEOF
fi
# Platforms 1: tvOS / watchOS launchd_sim has only the system domain, which stays in bootstrap mode (every request of its
# jobs deferred) until the host's MIG 700: simhost sends it (host/simhost/simhost.c). A64_BOOTSTRAP_DONE=1: on iOS too
# (experiments; its jobs are in the user domain, which ends bootstrap mode itself).
BDONE=
if [ "$A64_PLATFORM" != ios ] || [ "${A64_BOOTSTRAP_DONE:-0}" = 1 ]; then
	BDONE="SIMHOST_BOOTSTRAP_DONE=1"
fi
# mksimdevice.py seeds Apple's SampleContent (the six stock simulator photos) into $D/data/Media, while assetsd
# and Photos read /var/mobile/Media inside Darling. Link /var/mobile/Media to $D/data/Media before launchd_sim
# starts when missing, empty, or already a symlink (preserving any existing non-empty library in an older prefix).
mkdir -p "$D/data/Media"
MEDIA_LINK='mkdir -p /var/mobile; if [ -L /var/mobile/Media ]; then rm -f /var/mobile/Media; elif [ -d /var/mobile/Media ] && [ -z "$(ls -A /var/mobile/Media 2>/dev/null)" ]; then rmdir /var/mobile/Media; fi; [ -e /var/mobile/Media ] || ln -s "/Volumes/SystemRoot'"$D"'/data/Media" /var/mobile/Media;'
cat > "$CMDFILE" <<CMDEOF
$MEDIA_LINK $MDNS_LINK $BDONE exec "$H" /usr/bin/env $ENVS "$R/sbin/launchd_sim" "/Volumes/SystemRoot$D/data/var/run/launchd_bootstrap.plist"
CMDEOF

# Speed 14: fixed placement of non-cache Mach-Os (libarm64jit; mkplacement.py): the jitcache dir gets the committed
# table when it has none (the table is part of the JIT cache key: the AOT image must be built with the same one)
JC=${X86SIM_A64_JITCACHE_DIR:-$X86SIM_SCRATCH/jitcache}
mkdir -p "$JC"
[ -f "$JC/placement.txt" ] || cp "$X/cli/libexec/arm64/placement-${A64_BUILD:-24A434}.txt" "$JC/placement.txt" 2>/dev/null
# Speed 14: the early set (pages in first-touch order up to ~home) is read in that order here, outside the IO-capped
# scope: ~1-2 s of sequential reads instead of ~10-20 s at 40 MB/s, competing with the boot's own faults. The rest
# (prefetch.hot) stays a capped WILLNEED inside the scope. A64_PREFETCH_EARLY_CAPPED=1: the early set capped too.
PF_INNER="$ST/prefetch.early $ST/prefetch.hot"
if [ "${A64_PREFETCH:-1}" != 0 ] && [ "${A64_PREFETCH_EARLY_CAPPED:-0}" != 1 ] && [ -f "$ST/prefetch.early" ]; then
	(python3 -I "$X/cli/libexec/arm64/prefetch.py" read "$ST/prefetch.early" > "$ST/prefetch-read.log" 2>&1 &)
	PF_INNER="$ST/prefetch.hot"
fi
# The container at the highest OOM score (oom_score_adj 1000, inherited by darlingserver and every guest; a process may
# raise its own): in a host-wide OOM the kernel takes a guest process (an app, a daemon its launchd restarts), not the
# shared mtlvk (this script's score). On 2026-11-15 a host-wide OOM during parallel boots killed the iphone-duo's mtlvk
# (the largest process at the same score), and every app of each phone rendering through it had no MTLDevice until
# that phone was rebooted.
(echo 1000 > /proc/self/oom_score_adj 2>/dev/null
cli/libexec/limited.sh env \
	DPREFIX="$DPREFIX" \
	X86SIM_DARLING="$X86SIM_DARLING" \
	A64_TZNAME="$TZNAME" \
	A64_TIMEOUT="$A64_TIMEOUT" \
	A64_CMDFILE="$CMDFILE" \
	A64_PREFETCH_HOT="$([ "${A64_PREFETCH:-1}" != 0 ] && echo "$PF_INNER")" \
	"$X/cli/libexec/arm64/boot-inner.sh" > "$ST/lsim.out" 2>&1 &)
# Speed 18: 90 s in, check the chunked AOT image against this runtime / placement / codegen (tools/launcher x86sim-aot
# status, low priority) and note in $ST/aot-status.txt how many chunks need rebuilding: a translator rebuild with a new
# CODEGEN_VERSION, a runtime update or a newly placed app no longer silently boots without an image. The launcher
# offers the rebuild; X86SIM_AOT_AUTOUPDATE=1 runs `x86sim-aot update` (2 builders, nice 19, idle IO) right away.
AOTBIN=$X/tools/launcher/target/release/x86sim-aot
if [ -x "$AOTBIN" ]; then
	# Launch 1: and only once the phone is idle (its scope used under half a core over 5 s; at most 5 min more): the
	# check reads the image, its index and every training profile (~7 CPU s, ~12 s, GBs of IO, 1.8 GB RSS) and landed in
	# the middle of any app opened ~1.5 min after boot.
	(sleep 90
	 CGS=/sys/fs/cgroup$(awk -F: '/^0::/{print $NF; exit}' /proc/self/cgroup)/cpu.stat
	 for _ in $(seq 60); do
		u0=$(awk '/^usage_usec/{print $2}' "$CGS" 2>/dev/null) || break; [ -n "$u0" ] || break; sleep 5
		u1=$(awk '/^usage_usec/{print $2}' "$CGS" 2>/dev/null); [ $(( ${u1:-0} - u0 )) -lt 2500000 ] && break
	 done
	 # AOT cache 1: upkeep first (x86sim-aot maintain, skipped while a build / install holds the cache): an image for
	 # the current placement if it has none (assembled from the chunks that are up to date), then the cleanup of
	 # images, chunks and per-process caches nothing uses any more (appcache.rs). No user command needed.
	 # RAM 14: the programs running now (home reached, CPU quiet) are this runtime's residents: x86sim-aot ranks their
	 # code higher for the image (hot.rs RESIDENT_WEIGHT). The union over boots, by executable name as the
	 # translator names its training files.
	 if [ -d "$JC/aot" ]; then
		{ cat "$JC/aot/resident.txt" 2>/dev/null
		  for p in /proc/[0-9]*; do
			grep -qsazF "__mldr_sockpath=$DPREFIX/.darlingserver.sock" "$p/environ" || continue
			a0=$(tr '\0' '\n' < "$p/cmdline" 2>/dev/null | head -1); [ -n "$a0" ] && echo "${a0##*/}"
		  done; } | sort -u > "$JC/aot/resident.txt.$$" && mv -f "$JC/aot/resident.txt.$$" "$JC/aot/resident.txt"
	 fi
	 nice -n 19 ionice -c 3 "$AOTBIN" --jc "$JC" --device "$D" maintain > "$ST/aot-maintain.log" 2>&1
	 nice -n 19 ionice -c 3 "$AOTBIN" --jc "$JC" --device "$D" status > "$ST/aot-status.txt" 2>&1
	 if [ "${X86SIM_AOT_AUTOUPDATE:-0}" = 1 ] && grep -q "need rebuilding" "$ST/aot-status.txt"; then
		"$AOTBIN" --jc "$JC" --device "$D" update --workers 2 > "$ST/aot-update.log" 2>&1
	 fi) < /dev/null > /dev/null 2>&1 &
fi
# Speed 11: 60 s in (home is up), record which ranges of the dyld cache and the AOT image this boot's processes
# have mapped in; the next boot prefetches them (boot-inner.sh). A64_PREFETCH=0: neither.
if [ "${A64_PREFETCH:-1}" != 0 ]; then
	# Speed 14: prefetch.early = the pages present up to A64_PREFETCH_EARLY_SECS (default 24, ~home), snapshotted
	# every 3 s and saved by first-touch time; prefetch.hot = everything mapped 60 s in (read after, capped)
	# (nullglob: before the image's first load there is no .map yet, and a literal "*.map" crashed the recording)
	shopt -s nullglob
	PF_FILES=("$X86SIM_RUNTIME"/System/Library/Caches/com.apple.dyld/dyld_sim_shared_cache_arm64* "$JC"/*.map "$JC"/*.map.idx)
	shopt -u nullglob
	pf_record() { python3 -I "$X/cli/libexec/arm64/prefetch.py" record "$1" "${PF_FILES[@]}"; }
	PF_ES=${A64_PREFETCH_EARLY_SECS:-24}
	# early: also every other mapped file of the runtime, the jitcache dir and the darling prefix (non-cache dylibs,
	# app executables, the .aot itself): what the boot otherwise faults in at the scope's 40 MB/s
	(nice -n 19 python3 -I "$X/cli/libexec/arm64/prefetch.py" record-seq "$ST/prefetch.early" "$PF_ES" 3 "${PF_FILES[@]}" \
		"$X86SIM_RUNTIME/" "$JC/" "$X/darling-prefix/"
	 sleep $((60 - PF_ES)); pf_record "$ST/prefetch.hot") > "$ST/prefetch.log" 2>&1 &
fi
sleep 3
(timeout $((SECS + 2)) tail -n +1 -F "$L" 2>/dev/null | grep --line-buffered -v -E "failed lookup|Last log repeated" > "$ST/launchd.log" &)
# a boot that hangs before home (black, every process idle): diagnostics + one restart (cli/libexec/arm64/bootwatch.sh)
[ "${A64_BOOTWATCH:-0}" = 1 ] && (bash "$X/cli/libexec/arm64/bootwatch.sh" "$D" "$ST" "$DPREFIX" < /dev/null > /dev/null 2>&1 &)
sleep "$SECS"
echo "== spawns/exits"
grep -E "Successfully spawned|exited due|exited with" "$ST/launchd.log" | sed "s/^[0-9-]* [0-9:.]* //; s/\[[0-9]*\]//g; s/ because.*//; s/, ran for.*//; s/<Notice>: //" | sort | uniq -c | cut -c1-140
echo "== lsim.out (tail)"; tail -5 "$ST/lsim.out"
# Report cgroup containment
echo "== cgroup containment"
# This boot's own scope (the one this script runs in); a phone's processes are those of its Darling prefix (as
# boot-inner.sh's join_prefix_cgroup matches them). Before, the scope with the most memory and every process of any
# phone counted: another running phone's processes were reported as OUT of this one.
SCOPE=$(awk -F: '/^0::/{print $NF; exit}' /proc/$$/cgroup 2>/dev/null); SCOPE=${SCOPE##*/}
case $SCOPE in x86sim-*.scope) ;; *) SCOPE="" ;; esac
if [ -n "$SCOPE" ]; then
	CG=$(systemctl --user show "$SCOPE" -p ControlGroup --value 2>/dev/null)
	echo "scope=$SCOPE memory.current=$(cat /sys/fs/cgroup$CG/memory.current 2>/dev/null) memory.max=$(cat /sys/fs/cgroup$CG/memory.max 2>/dev/null)"
	in=0; out=0; in_rss=0; out_rss=0
	for p in /proc/[0-9]*; do
		[ -r "$p/environ" ] || continue
		grep -qsazxF -e "DPREFIX=$DPREFIX" -e "A64_DPREFIX=$DPREFIX" -e "__mldr_sockpath=$DPREFIX/.darlingserver.sock" "$p/environ" 2>/dev/null || continue
		cg=$(awk -F: '/^0::/{print $NF; exit}' "$p/cgroup" 2>/dev/null)
		rss=$(awk '/^VmRSS/{print $2}' "$p/status" 2>/dev/null); rss=${rss:-0}
		case "$cg" in
			*"$SCOPE"*) in=$((in+1)); in_rss=$((in_rss+rss)) ;;
			*) out=$((out+1)); out_rss=$((out_rss+rss)); echo "  OUT pid=${p#/proc/} rss=$((rss/1024))MB cg=$cg" ;;
		esac
	done
	# darlingserver for this prefix
	for p in /proc/[0-9]*; do
		[ -r "$p/cmdline" ] || continue
		tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -qF "darlingserver $DPREFIX " || continue
		cg=$(awk -F: '/^0::/{print $NF; exit}' "$p/cgroup" 2>/dev/null)
		rss=$(awk '/^VmRSS/{print $2}' "$p/status" 2>/dev/null); rss=${rss:-0}
		case "$cg" in
			*"$SCOPE"*) echo "  darlingserver IN rss=$((rss/1024))MB" ;;
			*) echo "  darlingserver OUT rss=$((rss/1024))MB cg=$cg"; out=$((out+1)); out_rss=$((out_rss+rss)) ;;
		esac
	done
	echo "guests IN=$in ($((in_rss/1024)) MB) OUT=$out ($((out_rss/1024)) MB)"
fi
# A64_SLIM=1 (default): report parked helpers (X86SIM_A64_SLIM_PARK). Parking replaced a kill loop (slim-reaper.sh):
# SpringBoard holds RunningBoard assertions and relaunched them within ~1 s, which burned a core in xpcproxy_sim and
# never saved memory.
if [ "${A64_SLIM:-1}" = 1 ]; then
	echo "== slim park status"
	parked=""
	for p in /proc/[0-9]*; do
		pid=${p#/proc/}
		[ -r "$p/comm" ] || continue
		c=$(cat "$p/comm" 2>/dev/null) || continue
		case "$c" in
			Spotlight|WidgetRenderer_*|pkd)
				# launchd does not put SLIM_PARK in job environ; detect via pause + tiny RSS
				w=$(cat "$p/wchan" 2>/dev/null || true)
				r=$(awk '/^VmRSS/{print $2}' "$p/status" 2>/dev/null); r=${r:-0}
				case "$w" in
					*pause*)
						echo "  $c pid=$pid rss_mb=$((r/1024)) (parked)"
						parked="$parked $c"
						;;
					*)
						[ "$r" -lt 51200 ] && echo "  $c pid=$pid rss_mb=$((r/1024)) wchan=$w"
						;;
				esac
				;;
		esac
	done
	echo "  parked:$parked"
fi
# A64_KEEP=1: leave the device running (the container outlives the launcher's timeout); else stop this prefix
if [ "${A64_KEEP:-0}" != 1 ]; then
	stop_prefix
	stop_a64_mtlvks
	stop_helpers   # the mDNS server outlived every timed boot (test devices left one each)
fi
