# x86Sim: sourced by the test app builds (tests/apps/build.sh, tests/lldb/build-app.sh, tests/profile/build.sh) after
# build/mac-env.sh. X86SIM_PLATFORM=ios (default) | tvos | watchos | xros picks the simulator SDK; sets
#   PSDK  xcrun -sdk name        PTRIPLE  the target without its minimum version: $PTRIPLE$PMIN-simulator
#   PMIN  the minimum OS version (as the guest helpers: 17.0 / 10.0 / 1.0) for -target / MinimumOSVersion
#   PSUF  "" or -tvos/-watchos/-xros (output names)       PFAM  UIDeviceFamily integers
#   PKEYS the Info.plist keys that name the platform (CFBundleSupportedPlatforms, DTPlatformName, UIDeviceFamily, and
#         WKApplication / WKWatchOnly on watchOS: a standalone watch app)
# On Linux the watchOS and visionOS builds use Xcode's SDKs (sdks/*-xcode.sdk): the plain ones are substitutes.
case ${X86SIM_PLATFORM:-ios} in
ios) PSDK=iphonesimulator PTRIPLE=arm64-apple-ios PMIN=17.0 PSUF= PPLAT=iPhoneSimulator PFAM='<integer>1</integer><integer>2</integer>' ;;
tvos) PSDK=appletvsimulator PTRIPLE=arm64-apple-tvos PMIN=17.0 PSUF=-tvos PPLAT=AppleTVSimulator PFAM='<integer>3</integer>' ;;
watchos) PSDK=watchsimulator PTRIPLE=arm64-apple-watchos PMIN=10.0 PSUF=-watchos PPLAT=WatchSimulator PFAM='<integer>4</integer>'
	[ "$X86SIM_BUILD" = linux ] && export SDK_WATCHOS=$X86SIM_ROOT/sdks/WatchSimulator-xcode.sdk ;;
xros) PSDK=xrsimulator PTRIPLE=arm64-apple-xros PMIN=1.0 PSUF=-xros PPLAT=XRSimulator PFAM='<integer>7</integer>'
	[ "$X86SIM_BUILD" = linux ] && export SDK_XROS=$X86SIM_ROOT/sdks/XRSimulator-xcode.sdk ;;
*) echo "X86SIM_PLATFORM: ios, tvos, watchos or xros" >&2; exit 1 ;;
esac
PKEYS="<key>CFBundleSupportedPlatforms</key><array><string>$PPLAT</string></array>
<key>DTPlatformName</key><string>$PSDK</string><key>UIDeviceFamily</key><array>$PFAM</array>"
[ "${X86SIM_PLATFORM:-ios}" = watchos ] && PKEYS="$PKEYS
<key>WKApplication</key><true/><key>WKWatchOnly</key><true/>"
# the platform of a phone (var/device-a64-NAME/runtime: ios-27.2-..., tvos-..., watchos-..., xros-...)
phone_platform() { local p; p=$(cut -d- -f1 "$X86SIM_ROOT/var/device-a64-$1/runtime" 2>/dev/null); echo "${p:-ios}"; }
