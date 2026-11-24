#!/bin/bash
# x86Sim: builds the x86sim profile test targets for the arm64 iOS Simulator (Linux clang + ld64.lld, or the Mac):
# $X86SIM_SCRATCH/apps/profapp.app (tests/profile/profapp.m) and $X86SIM_SCRATCH/apps/profhot.sim (profhot.c,
# a command-line binary for tests/arm64/run.sh). Frame pointers as Apple's toolchain always keeps them on arm64.
# X86SIM_PLATFORM=tvos|watchos|xros: profapp-<platform>.app for that simulator (tests/apps/platform.sh), no profhot.
set -eo pipefail
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
. "$X86SIM_ROOT/build/mac-env.sh"
. "$X86SIM_ROOT/tests/apps/platform.sh"
OUT=$X86SIM_SCRATCH/apps; mkdir -p "$OUT"
FW="-framework UIKit -framework QuartzCore -framework CoreGraphics -framework Foundation"
[ "${X86SIM_PLATFORM:-ios}" = watchos ] && FW="-framework WatchKit -framework Foundation"
CC=$SIMCC HOT=1; [ "${X86SIM_PLATFORM:-ios}" = ios ] || { CC="xcrun -sdk $PSDK clang -target $PTRIPLE$PMIN-simulator"; HOT=; }
N=profapp$PSUF
tar cf - -C "$X86SIM_ROOT/tests/profile" profwork.h profhot.c profapp.m | mac "set -e; W=\$(mktemp -d \${TMPDIR:-/tmp}/x86prof.XXXXXX); cd \$W; tar xf -
	APP=$N.app; mkdir -p \$APP
	$CC -O1 -fobjc-arc $FW -o \$APP/profapp profapp.m
	${HOT:+$SIMCC -O1 -o profhot.sim profhot.c}
	cat > \$APP/Info.plist <<PL
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict>
<key>CFBundleIdentifier</key><string>com.x86sim.test.profapp</string>
<key>CFBundleName</key><string>profapp</string><key>CFBundleExecutable</key><string>profapp</string>
<key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>1.0</string>
<key>CFBundleVersion</key><string>1</string>
$PKEYS
<key>MinimumOSVersion</key><string>$PMIN</string><key>UILaunchScreen</key><dict/>
<key>UIApplicationSceneManifest</key><dict><key>UIApplicationSupportsMultipleScenes</key><false/></dict>
</dict></plist>
PL
	codesign -f -s - \$APP >&2
	${HOT:+codesign -f -s - profhot.sim >&2}
	tar cf - \$APP ${HOT:+profhot.sim}; cd /; rm -rf \$W" | (cd "$OUT" && rm -rf $N.app ${HOT:+profhot.sim} && tar xf -)
ls -d "$OUT/$N.app" ${HOT:+"$OUT/profhot.sim"}
