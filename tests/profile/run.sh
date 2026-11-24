#!/bin/bash
# x86Sim: x86sim profile end to end (the sampler in libarm64jit prof.cpp, profile.rs's symbolication and outputs).
#   tests/profile/run.sh PHONE [--no-build] [--settings]     (the phone must be running)
#   tests/profile/run.sh --standalone [--no-build]           (no phone: profhot.sim through tests/arm64/run.sh)
# 1. profapp (profapp.m: a "prof.worker" thread in prof_round -> prof_hot -> prof_leaf, a CADisplayLink animation on
#    the main thread) profiled for 5 s: prof_leaf has the most self time, prof_hot / prof_round / prof_worker are
#    on its stacks, the worker thread is named, the main thread shows UIKit / QuartzCore work; speedscope and gecko
#    files are well-formed (same samples as the text summary)
# 2. (--settings) Settings scrolled while profiled: UIKitCore frames, the main thread on top
# Every check prints "X86TEST profile <check> PASS|FAIL"; exit status 1 if one failed. Output in $X86SIM_SCRATCH/profile-test[-<platform>]/.
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}; . "$X86SIM_ROOT/cli/lib/config.sh"
build=1 settings=0 standalone=0 PHONE=
for a; do case $a in --no-build) build=0 ;; --settings) settings=1 ;; --standalone) standalone=1 ;; -*) ;; *) PHONE=$a ;; esac; done
[ -n "$PHONE" ] || [ $standalone = 1 ] || { echo "usage: $0 PHONE [--no-build] [--settings] | --standalone" >&2; exit 2; }
X=$X86SIM_ROOT/cli/x86sim
SERVE=${X86SIM_SERVE:-$X86SIM_ROOT/tools/launcher/target/release/x86sim-serve}
. "$(dirname "$0")/../apps/platform.sh"
plat=ios; [ -n "$PHONE" ] && plat=$(phone_platform "$PHONE")
X86SIM_PLATFORM=$plat . "$(dirname "$0")/../apps/platform.sh"  # PSUF: the phone's build of profapp (tvOS, watchOS, visionOS)
OUT=$X86SIM_SCRATCH/profile-test$PSUF; mkdir -p "$OUT"
[ $build = 1 ] && { X86SIM_PLATFORM=$plat "$(dirname "$0")/build.sh" >&2 || exit 1; }
fail=0
check() {  # check NAME FILE REGEX...: every regex is found in FILE
	local name=$1 f=$2; shift 2
	for re; do grep -qE -- "$re" "$f" || { echo "X86TEST profile $name FAIL (no /$re/ in $f)"; fail=1; return; }; done
	echo "X86TEST profile $name PASS"
}
# the text summary's first "Top ... by self time" row is FUNC
top_self() {  # top_self NAME FILE FUNC
	local got
	got=$(awk '/^Top .* by self time/{getline; print; exit}' "$2")
	case $got in *" $3  ["*) echo "X86TEST profile $1 PASS ($got)" ;; *) echo "X86TEST profile $1 FAIL (top self: $got)"; fail=1 ;; esac
}
# speedscope / gecko from the same raw samples: valid JSON with the summary's sample count and the expected frames
formats() {  # formats RAW TAG FUNC
	local raw=$1 tag=$2 fn=$3 n
	n=$(sed -n 's/.* \([0-9]*\) samples of .*/\1/p' "$OUT/$tag.txt" | head -1)
	"$SERVE" profile --raw "$raw" --format speedscope --out "$OUT/$tag.speedscope.json" > /dev/null &&
	"$SERVE" profile --raw "$raw" --format gecko --out "$OUT/$tag.gecko.json" > /dev/null || { echo "X86TEST profile $tag-formats FAIL (conversion)"; fail=1; return; }
	if python3 -I - "$OUT/$tag.speedscope.json" "$OUT/$tag.gecko.json" "$n" "$fn" <<'PY'
import json, sys
ss, gk, n, fn = json.load(open(sys.argv[1])), json.load(open(sys.argv[2])), int(sys.argv[3]), sys.argv[4]
names = [f["name"] for f in ss["shared"]["frames"]]
assert sum(len(p["samples"]) for p in ss["profiles"]) == n, "speedscope sample count"
assert all(len(p["samples"]) == len(p["weights"]) and p["type"] == "sampled" for p in ss["profiles"])
assert all(0 <= i < len(names) for p in ss["profiles"] for s in p["samples"] for i in s), "speedscope frame index"
assert fn in names, "speedscope: no " + fn
assert sum(len(t["samples"]["data"]) for t in gk["threads"]) == n, "gecko sample count"
for t in gk["threads"]:
    st, fr, strs = t["stackTable"]["data"], t["frameTable"]["data"], t["stringTable"]
    assert all((p is None or p < i) and f < len(fr) for i, (p, f) in enumerate(st)), "gecko stack table"
    assert all(s[0] is None or s[0] < len(st) for s in t["samples"]["data"]), "gecko sample stack"
    assert all(f[0] < len(strs) for f in fr), "gecko string index"
assert any(s.startswith(fn + " ") for t in gk["threads"] for s in t["stringTable"]), "gecko: no " + fn
PY
	then echo "X86TEST profile $tag-formats PASS"; else echo "X86TEST profile $tag-formats FAIL"; fail=1; fi
}
raw_of() { sed -n 's/^raw samples: //p' "$1"; }

if [ $standalone = 1 ]; then
	# profhot in its own Darling container (tests/arm64/run.sh), outside any phone: SIMULATOR_UDID none
	PROF_SECS=20 "$X86SIM_ROOT/tests/arm64/run.sh" "$X86SIM_SCRATCH/apps/profhot.sim" > "$OUT/profhot.out" 2>&1 &
	RUNNER=$!
	for i in $(seq 1 60); do grep -q "profhot: pid" "$OUT/profhot.out" && break; sleep 1; done
	G=$(sed -n 's/^profhot: pid //p' "$OUT/profhot.out" | head -1)
	sleep 2
	"$SERVE" profile --udid none "$G" --secs 4 --format text --out "$OUT/profhot.txt" > "$OUT/profhot.cli" 2>&1
	top_self standalone-top "$OUT/profhot.txt" prof_leaf
	check standalone-stack "$OUT/profhot.txt" "prof_hot " "prof_round " "prof\.worker \(tid"
	formats "$(raw_of "$OUT/profhot.cli")" profhot prof_leaf
	wait $RUNNER
	exit $fail
fi

P() { "$X" --phone "$PHONE" "$@"; }
P install "$X86SIM_SCRATCH/apps/profapp$PSUF.app" >&2 || exit 1
P launch --terminate-running-process com.x86sim.test.profapp >&2 || exit 1
sleep 6
X86SIM_SERVE=$SERVE timeout 120 "$X" profile "$PHONE" com.x86sim.test.profapp --secs 5 --format text --out "$OUT/profapp.txt" > "$OUT/profapp.cli" 2>&1
top_self app-top "$OUT/profapp.txt" prof_leaf
check app-stack "$OUT/profapp.txt" "prof_hot " "prof_round " "prof_worker " "prof\.worker \(tid"
check app-main "$OUT/profapp.txt" "main \(tid" "\[(UIKitCore|QuartzCore|CoreFoundation|Foundation)\]"
formats "$(raw_of "$OUT/profapp.cli")" profapp prof_leaf
P terminate com.x86sim.test.profapp >/dev/null 2>&1

if [ $settings = 1 ]; then
	P launch --terminate-running-process com.apple.Preferences >&2
	sleep 8
	# raw drags in device pixels (simtouch.py: no accessibility snapshot per swipe), up and down the list
	( sleep 0.5; for k in 1 2 3 4 5; do P swipe 600 2000 600 800 250 >/dev/null 2>&1; P swipe 600 800 600 2000 250 >/dev/null 2>&1; done ) &
	SW=$!
	X86SIM_SERVE=$SERVE timeout 120 "$X" profile "$PHONE" com.apple.Preferences --secs 6 --format text --out "$OUT/settings.txt" > "$OUT/settings.cli" 2>&1
	wait $SW
	check settings "$OUT/settings.txt" "Preferences \(pid" "\[UIKitCore\]" "main \(tid"
	awk '/^Threads:/{getline; getline; print; exit}' "$OUT/settings.txt" | grep -q "^  main (tid" &&
		echo "X86TEST profile settings-main PASS" || { echo "X86TEST profile settings-main FAIL (main is not the busiest thread)"; fail=1; }
fi
exit $fail
