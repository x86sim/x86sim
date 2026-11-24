#!/bin/bash
# x86Sim: builds tests/lldb/swiftdbg.swift into $X86SIM_SCRATCH/apps/swiftdbg.app (+ swiftdbg.app.dSYM) for the arm64
# iOS Simulator, -g -Onone, on Linux (build/linux-swiftc.sh via linux-bin's xcrun) or the Mac.
set -e
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
. "$X86SIM_ROOT/build/mac-env.sh"
OUT=$X86SIM_SCRATCH/apps; mkdir -p "$OUT"
S=$X86SIM_ROOT/tests/lldb/swiftdbg.swift
tar cf - -C "$(dirname "$S")" swiftdbg.swift | mac "set -e; W=\$(mktemp -d /tmp/x86app.XXXXXX); cd \$W; tar xf -
	APP=swiftdbg.app; mkdir -p \$APP
	nice xcrun -sdk iphonesimulator swiftc -target arm64-apple-ios27.0-simulator -Onone -g -parse-as-library \
		-module-name swiftdbg -o \$APP/swiftdbg swiftdbg.swift >&2
	# swiftc -g runs dsymutil itself (\$APP/swiftdbg.dSYM, with the swiftmodule lldb needs): out of the bundle
	mv \$APP/swiftdbg.dSYM swiftdbg.app.dSYM
	cat > \$APP/Info.plist <<PL
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict>
<key>CFBundleIdentifier</key><string>com.x86sim.test.swiftdbg</string>
<key>CFBundleName</key><string>swiftdbg</string><key>CFBundleExecutable</key><string>swiftdbg</string>
<key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleSupportedPlatforms</key><array><string>iPhoneSimulator</string></array>
<key>DTPlatformName</key><string>iphonesimulator</string><key>MinimumOSVersion</key><string>27.0</string>
<key>UIDeviceFamily</key><array><integer>1</integer></array><key>UILaunchScreen</key><dict/>
<key>UIApplicationSceneManifest</key><dict><key>UIApplicationSupportsMultipleScenes</key><false/></dict>
</dict></plist>
PL
	codesign -f -s - \$APP >&2
	tar cf - \$APP swiftdbg.app.dSYM; cd /; rm -rf \$W" | (cd "$OUT" && rm -rf swiftdbg.app swiftdbg.app.dSYM && tar xf -)
ls -d "$OUT/swiftdbg.app" "$OUT/swiftdbg.app.dSYM"
