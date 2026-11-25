#!/bin/bash
# x86Sim: hostile device builds for the .ipa conversion (tools/launcher core/src/devapp.rs). Each crafted archive must be
# refused with an error (never a panic, never a write outside the work dir):
#   symlink-app   Payload/Evil.app is a symlink to an app elsewhere on disk (the conversion would rewrite and re-sign it)
#   symlink-plist Payload/A.app/Info.plist is a symlink to a plist outside
#   dotdot        an entry named Payload/../../escape.txt
#   bomb          4 MB of zeros (a cap of 1 MB set through X86SIM_DEVAPP_MAX_MB)
#   macho         a thin arm64 Mach-O with ncmds 2 but one 24-byte load command (the review's panic)
# Usage: tests/devapp/hostile.sh   (needs tools/launcher built; works in $X86SIM_SCRATCH/devapp-hostile)
set -u
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
. "$X86SIM_ROOT/cli/lib/config.sh"
BIN=$X86SIM_ROOT/tools/launcher/target/release/x86sim-devapp
[ -x "$BIN" ] || { echo "build tools/launcher first" >&2; exit 2; }
W=$X86SIM_SCRATCH/devapp-hostile; rm -rf "$W"; mkdir -p "$W"
python3 -I - "$W" <<'PY'
import os, stat, struct, sys, zipfile
w = sys.argv[1]
# the "victim": a device app somewhere else on disk
v = os.path.join(w, "victim/Real.app"); os.makedirs(v)
plist = (b'<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>Real</string>'
         b'<key>CFBundleIdentifier</key><string>x.real</string><key>CFBundleSupportedPlatforms</key><array><string>iPhoneOS</string></array>'
         b'<key>DTPlatformName</key><string>iphoneos</string></dict></plist>')
open(os.path.join(v, "Info.plist"), "wb").write(plist)
def macho(cmds, ncmds=None):
    body = b"".join(cmds)
    return struct.pack("<8I", 0xfeedfacf, 0x0100000c, 0, 2, len(cmds) if ncmds is None else ncmds, len(body), 0, 0) + body
good = macho([struct.pack("<6I", 0x32, 24, 2, 0x1b0000, 0x1b0000, 0)]) + b"\0" * 64
open(os.path.join(v, "Real"), "wb").write(good)
def link(z, name, target):
    i = zipfile.ZipInfo(name); i.create_system = 3; i.external_attr = (stat.S_IFLNK | 0o777) << 16
    z.writestr(i, target)
with zipfile.ZipFile(os.path.join(w, "symlink-app.ipa"), "w") as z:
    link(z, "Payload/Evil.app", v)
with zipfile.ZipFile(os.path.join(w, "symlink-plist.ipa"), "w") as z:
    z.writestr("Payload/A.app/A", good)
    link(z, "Payload/A.app/Info.plist", os.path.join(v, "Info.plist"))
with zipfile.ZipFile(os.path.join(w, "dotdot.ipa"), "w") as z:
    z.writestr("Payload/A.app/Info.plist", plist.replace(b"Real", b"A"))
    z.writestr("Payload/A.app/A", good)
    z.writestr("Payload/../../escape.txt", b"x")
with zipfile.ZipFile(os.path.join(w, "bomb.ipa"), "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("Payload/A.app/Info.plist", plist.replace(b"Real", b"A"))
    z.writestr("Payload/A.app/A", good)
    z.writestr("Payload/A.app/zeros", b"\0" * (4 << 20))
with zipfile.ZipFile(os.path.join(w, "macho.ipa"), "w") as z:
    z.writestr("Payload/A.app/Info.plist", plist.replace(b"Real", b"A"))
    z.writestr("Payload/A.app/A", macho([struct.pack("<6I", 0x32, 24, 2, 0, 0, 0)], ncmds=2))
PY
sum0=$(cd "$W/victim" && find . -type f -exec sha256sum {} + | sort)
fail=0
for t in symlink-app symlink-plist dotdot bomb macho; do
	out=$(X86SIM_DEVAPP_MAX_MB=1 "$BIN" convert "$W/$t.ipa" --work "$W/work-$t" 2>&1); rc=$?
	if [ $rc = 1 ] && ! grep -q panicked <<<"$out"; then echo "ok   $t: refused: ${out##*: }"
	else echo "FAIL $t: rc=$rc $out"; fail=1; fi
done
[ "$sum0" = "$(cd "$W/victim" && find . -type f -exec sha256sum {} + | sort)" ] && echo "ok   the victim app is unchanged" || { echo "FAIL the victim app changed"; fail=1; }
[ ! -e "$X86SIM_SCRATCH/escape.txt" ] && [ ! -e "$W/escape.txt" ] || { echo "FAIL escape.txt written"; fail=1; }
rm -rf "$W"
exit $fail
