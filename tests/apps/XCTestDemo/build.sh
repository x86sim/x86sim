#!/bin/bash
# x86Sim: build XCTestDemo (app + hosted unit tests + logic tests + UI tests) for testing, as `xcodebuild
# build-for-testing` does, on the Mac ($X86SIM_MAC: Xcode's XCTest can't be linked into an Xcode project's targets on
# Linux), and copy the products (.app, .xctest, the UITests-Runner.app and the .xctestrun) back:
#   tests/apps/XCTestDemo/build.sh [OUT]     (default $X86SIM_SCRATCH/apps/XCTestDemo-products)
# X86SIM_PLATFORM=tvos|watchos|xros builds it for that simulator instead of iOS (default OUT: ...-products-<platform>)
# Then: x86sim test <phone> --xctestrun OUT/XCTestDemo_iphonesimulator*.xctestrun  (or run.sh <phone>)
set -euo pipefail
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../../.." && pwd)}
. "$X86SIM_ROOT/cli/lib/config.sh"
MAC=${X86SIM_MAC:?set X86SIM_MAC (ssh host of a Mac with Xcode) in x86sim.env}
PLAT=${X86SIM_PLATFORM:-ios}
case $PLAT in
ios) SDK=iphonesimulator DEST="iOS Simulator" SUF= ;;
tvos) SDK=appletvsimulator DEST="tvOS Simulator" SUF=-tvos ;;
watchos) SDK=watchsimulator DEST="watchOS Simulator" SUF=-watchos ;;
xros) SDK=xrsimulator DEST="visionOS Simulator" SUF=-xros ;;
*) echo "X86SIM_PLATFORM: ios, tvos, watchos or xros" >&2; exit 1 ;;
esac
OUT=${1:-$X86SIM_SCRATCH/apps/XCTestDemo-products$SUF}
rm -rf "$OUT"; mkdir -p "$OUT"
tar cf - -C "$X86SIM_ROOT/tests/apps" --exclude spm XCTestDemo | ssh -o BatchMode=yes "$MAC" "export SDK=$SDK DEST='$DEST';" 'set -e
	W=$(mktemp -d /tmp/xctestdemo.XXXXXX); trap "rm -rf $W" EXIT; cd $W; tar xf -; cd XCTestDemo
	nice xcodebuild -project XCTestDemo.xcodeproj -scheme XCTestDemo -sdk $SDK \
		-destination "generic/platform=$DEST" -derivedDataPath $W/dd ARCHS=arm64 build-for-testing > $W/build.log 2>&1 ||
		{ tail -40 $W/build.log >&2; exit 1; }
	cd $W/dd/Build/Products && COPYFILE_DISABLE=1 tar cf - .' | tar xf - -C "$OUT" 2>/dev/null
ls "$OUT"/*.xctestrun
