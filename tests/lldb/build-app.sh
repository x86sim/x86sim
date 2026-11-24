#!/bin/bash
# x86Sim: builds tests/lldb/dbgapp.m into $X86SIM_SCRATCH/apps/dbgapp.app (+ dbgapp.app.dSYM) for the arm64 iOS
# Simulator, with debug info, on Linux (build/linux-env.sh: clang + ld64.lld + dsymutil) or the Mac.
# X86SIM_PLATFORM=tvos|watchos|xros: for that simulator, into dbgapp-<platform>.app (tests/apps/platform.sh).
set -eo pipefail
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
. "$X86SIM_ROOT/build/mac-env.sh"
. "$X86SIM_ROOT/tests/apps/platform.sh"
OUT=$X86SIM_SCRATCH/apps; mkdir -p "$OUT"
S=$X86SIM_ROOT/tests/lldb/dbgapp.m
# watchOS has no UIApplicationMain: dbgapp.m is a WatchKit app there
FW="-framework UIKit -framework Foundation"; [ "${X86SIM_PLATFORM:-ios}" = watchos ] && FW="-framework WatchKit -framework Foundation"
CC=$SIMCC; [ "${X86SIM_PLATFORM:-ios}" = ios ] || CC="xcrun -sdk $PSDK clang -target $PTRIPLE$PMIN-simulator"
N=dbgapp$PSUF
tar cf - -C "$(dirname "$S")" dbgapp.m | mac "set -e; W=\$(mktemp -d /tmp/x86app.XXXXXX); cd \$W; tar xf -
	APP=$N.app; mkdir -p \$APP
	$CC -g -O0 -fobjc-arc $FW -c -o dbgapp.o dbgapp.m
	$CC -g -fobjc-arc $FW -o \$APP/dbgapp dbgapp.o
	dsymutil -o $N.app.dSYM \$APP/dbgapp
	cat > \$APP/Info.plist <<PL
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict>
<key>CFBundleIdentifier</key><string>com.x86sim.test.dbgapp</string>
<key>CFBundleName</key><string>dbgapp</string><key>CFBundleExecutable</key><string>dbgapp</string>
<key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>1.0</string>
<key>CFBundleVersion</key><string>1</string>
$PKEYS
<key>MinimumOSVersion</key><string>$PMIN</string><key>UILaunchScreen</key><dict/>
<key>UIApplicationSceneManifest</key><dict><key>UIApplicationSupportsMultipleScenes</key><false/></dict>
</dict></plist>
PL
	codesign -f -s - \$APP >&2
	tar cf - \$APP $N.app.dSYM; cd /; rm -rf \$W" | (cd "$OUT" && rm -rf $N.app $N.app.dSYM && tar xf -)
ls -d "$OUT/$N.app" "$OUT/$N.app.dSYM"
