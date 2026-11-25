#!/bin/bash
# x86Sim webcov: the web-platform coverage suite (tests/web/webcov) in Safari on a running x86Sim phone, or with
# --ref-mac in Safari on Apple's simulator on the Mac ($X86SIM_MAC: a temporary device of the same model and runtime,
# deleted after). Results: OUT/<run>.jsonl (one JSON event per line: start / result / prompt / log / done), report.md.
# Usage: tests/web/webcov/run.sh PHONE [--only PREFIX[,...]] [--skip PREFIX[,...]] [--nogesture]
#        tests/web/webcov/run.sh --ref-mac [ios|ipad] [--prompts X86.jsonl] [--only ...] [--skip ...]
# How a run goes: serve.py serves the suite on 127.0.0.1:PORT (the phone reaches the host's loopback; on the Mac the
# server runs on the Mac), Safari opens http://127.0.0.1:PORT/?run=RUN, the page POSTs every check's start / result.
# This script watches those events:
#   - a gesture check (user activation: unmuted audio, PiP, clipboard, permission requests) shows a full-page "tap: ID"
#     button: the runner taps it (x86Sim: the button's element from `x86sim snapshot`; Apple: the middle of the screen)
#   - a system or website prompt (Allow / Allow While Using App / OK...) is answered as a user would (allowed) and
#     recorded as a "prompt" event with the check that raised it and the button's place; the Mac run taps the same
#     places for the same checks (--prompts: the x86Sim run's jsonl; same model + runtime = same layout)
#   - no event for STALL s (a hung WebContent process, a sheet in the way): the page is opened again; the harness
#     records the check that was running as CRASH and goes on after it
# Location: the simulated location is set to Apple Park (37.3349,-122.0090) and Safari's location access granted
# (x86sim privacy / simctl privacy), as sdkcov's run.sh does for its app; the website-level prompt is still answered.
# Env: PORT (default 7420; Mac: 7420 there), SECS (overall limit, default 1500), STALL (default 75),
#      OUT (default $X86SIM_SCRATCH/web/webcov-<phone>-<time>), MEDIA (generated fixtures, default $X86SIM_SCRATCH/web/media)
set -u
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../../.." && pwd)}
. "$X86SIM_ROOT/cli/lib/config.sh"
W=$(cd "$(dirname "$0")" && pwd)   # the suite next to this script (X86SIM_ROOT may be another checkout)
ref=0 plat=ios phone= only= skip= nog= prompts=
[ "${1:-}" = --ref-mac ] && { ref=1; shift; case ${1:-} in ios|ipad) plat=$1; shift ;; esac; } || { phone=${1:?usage: run.sh PHONE | --ref-mac [ios|ipad]}; shift; }
while [ $# -gt 0 ]; do case $1 in --only) only=$2; shift ;; --skip) skip=$2; shift ;; --nogesture) nog=1 ;;
	--prompts) prompts=$2; shift ;; *) echo "webcov: unknown option $1" >&2; exit 2 ;; esac; shift; done
PORT=${PORT:-7420} SECS=${SECS:-1500} STALL=${STALL:-75}
MEDIA=${MEDIA:-$X86SIM_SCRATCH/web/media}
RUN=wc$(date +%m%d%H%M%S)
OUT=${OUT:-$X86SIM_SCRATCH/web/webcov-$([ $ref = 1 ] && echo "ref-$plat" || echo "$phone")-$(date +%Y%m%d-%H%M%S)}; mkdir -p "$OUT"
QS="run=$RUN${only:+&only=$only}${skip:+&skip=$skip}${nog:+&nogesture=1}"
"$W/gen-media.sh" "$MEDIA" || { echo "webcov: gen-media failed" >&2; exit 1; }

if [ $ref = 1 ]; then
	MAC=${X86SIM_MAC:?set X86SIM_MAC (ssh host of a Mac with Xcode) in x86sim.env}
	case $plat in ios) dtype=com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro ;; ipad) dtype=com.apple.CoreSimulator.SimDeviceType.iPad-Air-11-inch-M3 ;; esac
	# prompts seen on x86Sim: "check-id nx ny" lines (fractions of the screen), tapped on Apple for the same check
	PL=$OUT/prompts.txt; : > "$PL"
	[ -n "$prompts" ] && python3 -I -c 'import json, sys
for l in open(sys.argv[1]):
	e = json.loads(l)
	if e.get("ev") == "prompt" and e.get("nx") is not None: print(e["id"], e["nx"], e["ny"])' "$prompts" > "$PL"
	read -r -d '' REMOTE <<'MAC'
dtype=$1 port=$2 qs=$3 secs=$4 stall=$5 run=$6
set -u
T=$(mktemp -d /tmp/webcov-ref.XXXXXX); tar xf - -C "$T"; exec 3>&1 1>&2
rt=$(xcrun simctl list runtimes -j | python3 -c "import json,sys; r=[x for x in json.load(sys.stdin)['runtimes'] if x['platform']=='iOS' and x['isAvailable']]; print(sorted(r, key=lambda x: [int(p) for p in x['version'].split('.')])[-1]['identifier'])")
dev=$(xcrun simctl create webcov-ref "$dtype" "$rt") || exit 1
python3 "$T/webcov/serve.py" --port "$port" --root "$T/webcov" --media "$T/media" --out "$T/out" > "$T/serve.log" 2>&1 & spid=$!
cleanup() { kill $spid 2>/dev/null; xcrun simctl shutdown "$dev" >/dev/null 2>&1; xcrun simctl delete "$dev" >/dev/null 2>&1; rm -rf "$T"; }
trap cleanup EXIT
echo "webcov ref: $rt on $dev"
xcrun simctl boot "$dev" && xcrun simctl bootstatus "$dev" -b >/dev/null
clang -fobjc-arc -o "$T/indigo" "$T/indigo.m" -framework Foundation -framework AppKit -framework IOKit || exit 1
xcrun simctl privacy "$dev" grant location com.apple.mobilesafari >/dev/null 2>&1 || echo "webcov ref: location not pre-granted"
xcrun simctl location "$dev" set 37.3349,-122.0090 >/dev/null 2>&1 || echo "webcov ref: no simulated location"
J=$T/out/$run.jsonl
post() { curl -s -m 5 -X POST --data-binary "$1" "http://127.0.0.1:$port/r/$run" >/dev/null; }
# Safari started once first: a fresh device's first openurl can block past simctl's timeout while Safari sets up
xcrun simctl launch "$dev" com.apple.mobilesafari >/dev/null 2>&1; sleep 8
open_() { xcrun simctl openurl "$dev" "http://127.0.0.1:$port/?$qs" >/dev/null 2>&1 & }
open_
t0=$(date +%s); lastn=0; lastt=$t0; cur=; curt=$t0; tapped=; ptaps=0; to=10
while [ $(( $(date +%s) - t0 )) -lt "$secs" ]; do
	sleep 2; now=$(date +%s)
	# nothing posted yet: the page did not open (a lost openurl): again every 30 s
	[ -f "$J" ] || { [ $((now - lastt)) -ge 30 ] && { open_; lastt=$now; }; continue; }
	grep -q '"ev": "done"' "$J" && break
	n=$(wc -l < "$J"); [ "$n" != "$lastn" ] && { lastn=$n; lastt=$now; }
	st=$(grep '"ev": "\(start\|result\)"' "$J" | tail -1)
	id=$(echo "$st" | python3 -c 'import json,sys; e=json.loads(sys.stdin.read() or "{}"); print(e.get("id","-") if e.get("ev")=="start" else "-", 1 if e.get("gesture") else 0, e.get("timeout") or 10000)')
	set -- $id; sid=${1:--}; g=${2:-0}; to=$(( ${3:-10000} / 1000 )); [ "$sid" = - ] && sid=
	[ "$sid" != "$cur" ] && { cur=$sid; curt=$now; tapped=; ptaps=0; }
	if [ -n "$sid" ]; then
		# the tap button covers the page: a tap in the middle; then the x86Sim run's prompt places for this check
		[ "$g" = 1 ] && [ -z "$tapped" ] && [ $((now - curt)) -ge 2 ] && { "$T/indigo" "$dev" tap 0.5 0.5 >/dev/null 2>&1; tapped=1; }
		# (the prompt can come up long after the tap -- ~70 s on a freshly booted Apple device: the place is tapped
		# every 2 s from 4 s on while the check runs; a tap there without a prompt lands on the page's log text)
		if [ "$g" = 1 ] && [ $((now - curt)) -ge 4 ]; then
			grep "^$sid " "$T/prompts.txt" | while read -r _ nx ny; do "$T/indigo" "$dev" press "$nx" "$ny" 250 >/dev/null 2>&1
				[ $ptaps = 0 ] && post "{\"ev\":\"prompt\",\"id\":\"$sid\",\"label\":\"x86Sim place\",\"nx\":$nx,\"ny\":$ny}"; done
			grep -q "^$sid " "$T/prompts.txt" && ptaps=$((ptaps + 1))
		fi
	fi
	if [ $((now - lastt)) -ge $(( stall > to + 20 ? stall : to + 20 )) ]; then
		post "{\"ev\":\"log\",\"msg\":\"stall: page opened again\",\"id\":\"$sid\"}"
		xcrun simctl io "$dev" screenshot "$T/out/stall-$now.png" >/dev/null 2>&1
		open_; lastt=$now
	fi
done
xcrun simctl io "$dev" screenshot "$T/out/screen.png" >/dev/null 2>&1
cp "$T/serve.log" "$T/out/"
cd "$T/out" && tar cf - . >&3
MAC
	tar cf - -C "$W/.." webcov -C "$(dirname "$MEDIA")" "$(basename "$MEDIA")" -C "$W/../../apps/sdkcov/mac" indigo.m -C "$OUT" prompts.txt \
		--transform "s,^$(basename "$MEDIA"),media," 2>/dev/null |
		ssh -o BatchMode=yes "$MAC" "bash -c $(printf '%q' "$REMOTE") ref $dtype $PORT '$QS' $SECS $STALL $RUN" > "$OUT/ref.tar"
	(cd "$OUT" && tar xf ref.tar && rm ref.tar)
	J=$OUT/$RUN.jsonl
	[ -f "$J" ] || { echo "webcov: no results came back from the Mac" >&2; exit 1; }
	python3 "$W/report.py" "$J" | tee "$OUT/report.md" | head -20
	echo "results: $J"; exit 0
fi

# x86Sim: the phone must be running
xs() { "$X86SIM_ROOT/cli/x86sim" --phone "$phone" "$@"; }
SRV=$OUT/srv; mkdir -p "$SRV"
# the server in a memory-capped scope of its own (it holds nothing big; a runaway is killed alone)
systemd-run --user --scope -q -p MemoryMax=512M -p MemorySwapMax=0 -- nice python3 -I "$W/serve.py" --port "$PORT" --root "$W" \
	--media "$MEDIA" --out "$SRV" > "$OUT/serve.log" 2>&1 & spid=$!
trap 'kill $spid 2>/dev/null' EXIT
sleep 1; kill -0 $spid 2>/dev/null || { echo "webcov: serve.py did not start: $(tail -2 "$OUT/serve.log")" >&2; exit 1; }
J=$SRV/$RUN.jsonl
post() { curl -s -m 5 -X POST --data-binary "$1" "http://127.0.0.1:$PORT/r/$RUN" >/dev/null; }
xs privacy grant location com.apple.mobilesafari >/dev/null 2>&1 || echo "  note: location not pre-granted (x86sim privacy)"
xs location set 37.3349,-122.0090 >/dev/null 2>&1 || echo "  note: no simulated location (x86sim location)"
# the screen in points (prompt places are recorded as fractions of it, for the Mac run)
read -r PW PH < <(xs snapshot --json 2>/dev/null | python3 -I -c 'import json,sys
try:
	d = json.load(sys.stdin); s = d.get("screen") or {}; print(s.get("width", 0), s.get("height", 0))
except Exception: print(0, 0)')
[ "${PW:-0}" = 0 ] && { PW=402; PH=874; }
# the phone's Metal: Safari's WebGL is ANGLE on Metal in the WebKit GPU process. A phone booted with software rendering
# has no MTLDevice and every WebGL check fails (createShader null, getSupportedExtensions null, black texture reads) --
# boot-a64.sh started by hand defaults iPhone/iPad to X86SIM_METAL=0 (the launcher / MCP sim_start boot with "all").
# Read from the phone's darlingserver (cmdline "darlingserver <its prefix> ...", environment from boot-a64.sh).
metal=$(for p in /proc/[0-9]*; do
	[ "$(cat "$p/comm" 2>/dev/null)" = darlingserver ] || continue
	tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -q "darlingserver $X86SIM_SCRATCH/a64prefix-$phone " || continue
	tr '\0' '\n' < "$p/environ" 2>/dev/null | sed -n 's/^X86SIM_METAL=//p'; break
done)
echo "$phone: X86SIM_METAL=${metal:-?}" > "$OUT/metal.txt"
case ${metal:-?} in
0) echo "webcov: WARNING: $phone renders in software (X86SIM_METAL=0): WebGL checks will fail; boot it" \
	"with X86SIM_METAL=all (the launcher's default)" | tee -a "$OUT/metal.txt" >&2 ;;
\?) echo "  note: $phone's Metal mode unknown (no darlingserver for $X86SIM_SCRATCH/a64prefix-$phone)" ;;
esac
url="http://127.0.0.1:$PORT/?$QS"
echo "webcov on $phone: $url -> $J"
xs open "$url" --after none >/dev/null 2>&1
ALLOW='^(Allow|Allow While Using App|Allow Once|OK|Continue|Paste|Allow Paste)$'
SNAP=$OUT/snap.txt
# the alert / prompt buttons of the screen: "ref x y label" for the ones a user taps to allow
allow_buttons() { python3 -I -c 'import re, sys
for l in open(sys.argv[1]):
	m = re.match(r"\s*(@e\d+) button \"([^\"]*)\".*\((-?[\d.]+),\s*(-?[\d.]+)\)\s*$", l)
	if m and re.match(sys.argv[2], m.group(2)): print(m.group(1), m.group(3), m.group(4), m.group(2))' "$SNAP" "$ALLOW"; }
t0=$(date +%s); lastn=0; lastt=$t0; cur=; curt=$t0; tapped=
while [ $(( $(date +%s) - t0 )) -lt "$SECS" ]; do
	sleep 2; now=$(date +%s)
	[ -f "$J" ] || { [ $((now - lastt)) -ge "$STALL" ] && { xs open "$url" --after none >/dev/null 2>&1; lastt=$now; }; continue; }
	grep -q '"ev": "done"' "$J" && break
	n=$(wc -l < "$J"); [ "$n" != "$lastn" ] && { lastn=$n; lastt=$now; printf '\r  %s events, %s s ' "$n" $((now - t0)); }
	set -- $(grep '"ev": "\(start\|result\)"' "$J" | tail -1 | python3 -I -c 'import json,sys; e=json.loads(sys.stdin.read() or "{}"); print(e.get("id","") if e.get("ev")=="start" else "-", 1 if e.get("gesture") else 0, e.get("timeout") or 10000)')
	sid=${1:--}; g=${2:-0}; to=$(( ${3:-10000} / 1000 ))
	[ "$sid" != "$cur" ] && { cur=$sid; curt=$now; tapped=; }
	[ "$sid" = - ] && continue
	# a gesture check waiting for its tap, or any check quiet for 4 s (a prompt over the page?): look at the screen
	if { [ "$g" = 1 ] && [ -z "$tapped" ]; } || [ $((now - curt)) -ge 4 ]; then
		xs snapshot > "$SNAP" 2>/dev/null
		allow_buttons | head -1 | while read -r r x y label; do
			xs tap "$r" --after none >/dev/null 2>&1
			post "{\"ev\":\"prompt\",\"id\":\"$sid\",\"label\":\"$label\",\"x\":$x,\"y\":$y,\"nx\":$(python3 -c "print(round($x/$PW,4))"),\"ny\":$(python3 -c "print(round($y/$PH,4))")}"
			echo; echo "  prompt during $sid: \"$label\" allowed"
		done
		# another app's sheet in front of Safari (the share sheet, a picker) with nothing to allow: swiped away
		front=$(sed -n '1s/.* front \([^ ]*\).*/\1/p' "$SNAP")
		if [ -n "$front" ] && [ "$front" != com.apple.mobilesafari ] && [ -z "$(allow_buttons)" ] && [ $((now - curt)) -ge 6 ]; then
			xs swipe down >/dev/null 2>&1; post "{\"ev\":\"log\",\"msg\":\"dismissed $front\",\"id\":\"$sid\"}"; echo; echo "  $front dismissed during $sid"
		fi
		if [ "$g" = 1 ] && [ -z "$tapped" ]; then
			r=$(grep -F "\"tap: $sid\"" "$SNAP" | grep -o '@e[0-9]*' | head -1)
			[ -n "$r" ] && { xs tap "$r" --after none >/dev/null 2>&1; tapped=1; }
		fi
	fi
	if [ $((now - lastt)) -ge $(( STALL > to + 20 ? STALL : to + 20 )) ]; then
		echo; echo "  stall in $sid: page opened again"
		post "{\"ev\":\"log\",\"msg\":\"stall: page opened again\",\"id\":\"$sid\"}"
		xs screenshot "$OUT/stall-$now.png" 2 >/dev/null 2>&1; cp "$SNAP" "$OUT/stall-$now.txt" 2>/dev/null
		xs open "$url" --after none >/dev/null 2>&1; lastt=$now
	fi
done
echo
xs screenshot "$OUT/screen.png" 2 >/dev/null 2>&1
cp "$J" "$OUT/" 2>/dev/null
python3 "$W/report.py" "$J" | tee "$OUT/report.md" | head -20
[ "$metal" = 0 ] && sed -n 2p "$OUT/metal.txt" | tee -a "$OUT/report.md" >&2
echo "results: $OUT/$RUN.jsonl"
