#!/bin/bash
# x86Sim: the share server's access rules over loopback, against the built x86sim-serve (a server of its own: its
# state and log in a scratch XDG_RUNTIME_DIR / XDG_STATE_HOME, a port from 7391 up; the user's server is not touched).
#   - a phone-limited control link can't read the main tokens (/api/urls), start a tunnel, list / make / revoke links
#   - a link made with a phone's NAME is limited to that phone (stored as its id)
#   - revoke wants a link id ("" revoked every link)
#   - serve.log is 0600 and holds no full token
#   - an endless header line is cut off, a slow head times out (20 s)
# Usage: tests/share/loopback.sh
set -u
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
. "$X86SIM_ROOT/cli/lib/config.sh"
BIN=$X86SIM_ROOT/tools/launcher/target/release/x86sim-serve
W=$X86SIM_SCRATCH/share-loopback; rm -rf "$W"; mkdir -p "$W/run" "$W/state/x86sim"; chmod 700 "$W/run"
export XDG_RUNTIME_DIR=$W/run XDG_STATE_HOME=$W/state
# an old 0644 log, as earlier versions left it: share::start narrows it to 0600
touch "$W/state/x86sim/serve.log"; chmod 644 "$W/state/x86sim/serve.log"
"$BIN" --bg --listen 127.0.0.1:7391 > "$W/start.out" 2>&1 || { cat "$W/start.out"; exit 1; }
pid=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["pid"])' "$W/run/x86sim-serve.json")
trap 'kill $pid 2>/dev/null; for _ in $(seq 50); do kill -0 $pid 2>/dev/null || break; sleep 0.1; done; rm -rf "$W"' EXIT
python3 -I - "$W/run/x86sim-serve.json" "$W/state/x86sim/serve.log" <<'PY'
import json, os, socket, stat, sys, time, urllib.request
st = json.load(open(sys.argv[1])); port, main = st["port"], st["control_token"]
fail = []
def call(method, path, tok=None, body=None):
    req = urllib.request.Request("http://127.0.0.1:%d%s" % (port, path), method=method, data=json.dumps(body).encode() if body is not None else None,
                                 headers=dict({"X-X86sim": "1", "Content-Type": "application/json"}, **({"Authorization": "Bearer " + tok} if tok else {})))
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok: fail.append(what)
code, phones = call("GET", "/api/phones", main)
check(code == 200, "owner lists phones (%d)" % len(phones))
ph = phones[0] if phones else None
# a link made by NAME, control, expiring
code, l = call("POST", "/api/links", main, {"phone": ph["name"] if ph else None, "role": "control", "ttl_s": 600})
check(code == 200, "owner makes a control link")
tok = l["token"]
if ph:
    check(l["phone"] == ph["id"], "a link made with the name %r is stored as the id %r" % (ph["name"], l["phone"]))
    code, seen = call("GET", "/api/phones", tok)
    check(code == 200 and [p["id"] for p in seen] == [ph["id"]], "the link sees its phone only")
for m, p, b in [("GET", "/api/urls", None), ("GET", "/api/urls?role=view", None), ("GET", "/api/status", None), ("GET", "/api/links", None),
                ("POST", "/api/tunnel", {"kind": "funnel"}), ("POST", "/api/links", {"role": "control"}), ("POST", "/api/links/revoke", {"id": ""}),
                ("POST", "/api/links/revoke", {"id": "all"})]:
    code, v = call(m, p, tok, b)
    check(code == 403 and main not in json.dumps(v), "the link: %s %s -> %d" % (m, p, code))
code, _ = call("POST", "/api/links/revoke", main, {"id": ""})
check(code == 400, "revoke with an empty id is refused")
code, v = call("POST", "/api/links/revoke", main, {"id": tok[:6]})
check(code == 200 and v["revoked"] == 1, "owner revokes the link by its id")
check(call("GET", "/api/me", tok)[0] == 401, "the revoked link is refused")
# the log
log = sys.argv[2]
mode = stat.S_IMODE(os.stat(log).st_mode)
check(mode == 0o600, "serve.log is %o" % mode)
txt = open(log).read()
check(main not in txt and st["view_token"] not in txt, "serve.log holds no full token")
# head limits
s = socket.create_connection(("127.0.0.1", port))
try:
    s.sendall(b"GET / HTTP/1.1\r\nX: " + b"a" * 200000)
except OSError:
    pass
s.settimeout(10)
try:
    s.recv(100); closed = True
except (ConnectionResetError, BrokenPipeError):
    closed = True
except socket.timeout:
    closed = False
check(closed, "an endless header line is cut off")
s = socket.create_connection(("127.0.0.1", port)); s.sendall(b"GET / HT"); t = time.time(); s.settimeout(40)
try:
    s.recv(100)
except OSError:
    pass
check(time.time() - t < 30, "a slow head is dropped after %.0f s" % (time.time() - t))
sys.exit(1 if fail else 0)
PY
exit $?
