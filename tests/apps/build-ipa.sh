#!/bin/bash
# x86Sim: build tests/apps/DeviceHello as a DEVICE app (iphoneos SDK, arm64-apple-ios27.0) on the Mac (X86SIM_MAC) and
# package it as an .ipa (Payload/DeviceHello.app) in $X86SIM_SCRATCH/apps/DeviceHello.ipa: the fixture for
# `x86sim install X.ipa` (tools/launcher core/src/devapp.rs). It has what a real export has: an embedded framework (fat:
# arm64 + arm64e), entitlements in the code signature (application-identifier, keychain-access-groups, get-task-allow),
# CFBundleSupportedPlatforms iPhoneOS / DTPlatformName iphoneos. Signed ad-hoc (no certificate needed): the conversion
# re-signs anyway. Usage: tests/apps/build-ipa.sh
set -e
X86SIM_ROOT=${X86SIM_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}
. "$X86SIM_ROOT/cli/lib/config.sh"
MAC=${X86SIM_MAC:?set X86SIM_MAC (a Mac with Xcode) in x86sim.env}
A=$X86SIM_ROOT/tests/apps; OUT=$X86SIM_SCRATCH/apps; mkdir -p "$OUT"
tar cf - -C "$A" DeviceHello | ssh -o BatchMode=yes "$MAC" 'set -e; W=$(mktemp -d /tmp/x86ipa.XXXXXX); cd $W; tar xf -
	APP=Payload/DeviceHello.app; F=$APP/Frameworks/Greeter.framework; mkdir -p $F
	for a in arm64 arm64e; do xcrun -sdk iphoneos clang -target $a-apple-ios27.0 -dynamiclib -install_name @rpath/Greeter.framework/Greeter \
		-o g-$a DeviceHello/Greeter.c >&2; done
	xcrun lipo -create g-arm64 g-arm64e -output $F/Greeter
	/usr/libexec/PlistBuddy -c "Add CFBundleIdentifier string com.x86sim.test.Greeter" -c "Add CFBundleExecutable string Greeter" \
		-c "Add CFBundlePackageType string FMWK" -c "Add CFBundleSupportedPlatforms array" -c "Add CFBundleSupportedPlatforms:0 string iPhoneOS" \
		-c "Add DTPlatformName string iphoneos" -c "Add MinimumOSVersion string 27.0" -c "Add CFBundleVersion string 1" $F/Info.plist >/dev/null
	xcrun -sdk iphoneos swiftc -target arm64-apple-ios27.0 -Onone -parse-as-library -F $APP/Frameworks -framework Greeter \
		-Xlinker -rpath -Xlinker @executable_path/Frameworks -o $APP/DeviceHello DeviceHello/App.swift >&2
	cat > $APP/Info.plist <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.x86sim.test.DeviceHello</string>
<key>CFBundleName</key><string>DeviceHello</string><key>CFBundleDisplayName</key><string>DeviceHello</string>
<key>CFBundleExecutable</key><string>DeviceHello</string><key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>1.0</string><key>CFBundleVersion</key><string>1</string>
<key>CFBundleSupportedPlatforms</key><array><string>iPhoneOS</string></array>
<key>DTPlatformName</key><string>iphoneos</string><key>DTSDKName</key><string>iphoneos27.2</string>
<key>MinimumOSVersion</key><string>27.0</string><key>UIDeviceFamily</key><array><integer>1</integer><integer>2</integer></array>
<key>UIRequiredDeviceCapabilities</key><array><string>arm64</string></array><key>UILaunchScreen</key><dict/>
<key>UIApplicationSceneManifest</key><dict><key>UIApplicationSupportsMultipleScenes</key><false/></dict>
</dict></plist>
PL
	plutil -convert binary1 $APP/Info.plist
	cat > ents.plist <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>application-identifier</key><string>ABCDE12345.com.x86sim.test.DeviceHello</string>
<key>com.apple.developer.team-identifier</key><string>ABCDE12345</string>
<key>keychain-access-groups</key><array><string>ABCDE12345.com.x86sim.test.DeviceHello</string></array>
<key>get-task-allow</key><true/>
</dict></plist>
PL
	codesign -f -s - $F >&2
	codesign -f -s - --entitlements ents.plist --generate-entitlement-der $APP >&2
	rm -f app.ipa; zip -qry app.ipa Payload; cat app.ipa; cd /; rm -rf $W' > "$OUT/DeviceHello.ipa"
ls -l "$OUT/DeviceHello.ipa"
