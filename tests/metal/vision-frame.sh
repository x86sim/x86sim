#!/bin/bash
# x86Sim: Vision Pro display regression check. Boots a Vision Pro phone (off before), waits, and fails if the display
# frame is black (mean below MIN, default 20 of 255) or if mtlvk refused a render pipeline with "DrawInput is not
# built" (the 2026-11 runtime port refused every [[amplification_count]] shader: all frames black). Stops the phone.
# Usage: tests/metal/vision-frame.sh <phone> [seconds, default 120]
# Env: MTLVK=<mtlvk binary> runs the phone on that mtlvk (A/B; default: the one the boot starts), MIN, OUT (shot dir).
set -u
X=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
PH=${1:?usage: tests/metal/vision-frame.sh <phone> [seconds]}; SECS=${2:-120}; MIN=${MIN:-20}
OUT=${OUT:-${X86SIM_SCRATCH:-$HOME/scratch/x86sim}/vision-frame}; mkdir -p "$OUT"
ST=$X/var/run-a64-$PH; FAILED=$X/var/metal/failed; mkdir -p "$ST" "$FAILED"
mcp() {  # mcp TOOL JSON: one x86sim-mcp call, its text printed
	X86SIM_ROOT=$X python3 - "$X/tools/launcher/target/release/x86sim-mcp" "$@" <<'EOF'
import json, subprocess, sys
p = subprocess.Popen([sys.argv[1]], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def send(m): p.stdin.write(json.dumps(m) + "\n"); p.stdin.flush()
def recv(i):
    while True:
        line = p.stdout.readline()
        if not line: sys.exit("x86sim-mcp closed")
        m = json.loads(line)
        if m.get("id") == i: return m
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "vision-frame", "version": "1"}}})
recv(1); send({"jsonrpc": "2.0", "method": "notifications/initialized"})
send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": sys.argv[2], "arguments": json.loads(sys.argv[3])}})
for c in recv(2).get("result", {}).get("content", []):
    if c.get("type") == "text": print(c["text"])
p.stdin.close(); p.wait()
EOF
}
mpid=
if [ -n "${MTLVK:-}" ]; then  # a phone whose state dir holds a live mtlvk pid uses it (boot-a64.sh); sim_stop ends it
	rm -f "$ST/mtlvk.sock"
	(cd "$X" && MTLVK_IDLE_EXIT=0 exec setsid "$MTLVK" "$ST/mtlvk.sock" > "$OUT/mtlvk.log" 2>&1 < /dev/null) &
	mpid=$!; echo $mpid > "$ST/mtlvk.pid"
	for _ in $(seq 50); do [ -S "$ST/mtlvk.sock" ] && break; sleep 0.1; done
fi
stamp=$OUT/.start; touch "$stamp"
mcp sim_start "{\"phone\":\"$PH\"}" > "$OUT/start.txt"
sleep "$SECS"
mcp sim_screenshot "{\"phone\":\"$PH\",\"path\":\"$OUT/frame.png\"}" > /dev/null
mcp sim_stop "{\"phone\":\"$PH\"}" > /dev/null
[ -n "$mpid" ] && [ "$(cat /proc/$mpid/comm 2>/dev/null)" = mtlvk ] && kill "$mpid"
mean=$(python3 -c "
from PIL import Image
im = Image.open('$OUT/frame.png').convert('L').resize((64, 36))
print(int(sum(im.histogram()[i] * i for i in range(256)) / (64 * 36)))" 2>/dev/null || echo -1)
refused=$(find "$FAILED" -name '*.txt' -newer "$stamp" -exec grep -l 'DrawInput is not built' {} + 2>/dev/null | wc -l)
echo "vision-frame: $PH frame mean $mean (min $MIN), DrawInput refusals $refused, shot $OUT/frame.png"
[ "$mean" -ge "$MIN" ] && [ "$refused" -eq 0 ] && { echo PASS; exit 0; }
echo FAIL; exit 1
