# Test apps (regression suite for third-party apps)

Small purpose-built iOS apps, built for the arm64 iOS Simulator on the Mac with `swiftc` (no Xcode project), installed
with `x86sim install` and checked logs-first. Every check prints `X86TEST <app> <check> PASS|FAIL <detail>` to stderr
and appends it to `Documents/x86test-results.txt` in the app's data container.

| app | exercises |
|---|---|
| x86ui | SwiftUI List (200 rows), NavigationStack push/pop, repeating animation, TextField + keyboard, SF Symbols, UIGraphicsImageRenderer → PNG |
| x86net | URLSession HTTPS GET + JSON, download task, WKWebView page load (+ JS `document.title`) |
| x86data | UIKit scene lifecycle, Core Data (SQLite store, model built in code), files (write/rename/attributes), UserDefaults, Core Image blur → PNG, local notification (authorization prompt, schedule, delivery) |
| x86webinspect | two WKWebViews for Web Inspector (`x86sim inspect`, MCP `web_*`): `isInspectable = true` (listed: console logs, a fetch, `window.appValue`) and one without (not listed) |

    tests/apps/build.sh [app...]                 build on the Mac (X86SIM_MAC) into $X86SIM_SCRATCH/apps/
    tests/apps/run.sh <phone> [--no-build] [app...]
                                                 build, install, launch, wait for "done", answer permission prompts
                                                 (simtouch dismiss), check results, launchd lines, crash reports;
                                                 screenshot per app in $X86SIM_SCRATCH/apps/run-<time>/

`<phone>` is a phone's name, folder or dir (as `x86sim install`). The phone must be running.
Apps without the UIScene lifecycle trap in iOS 27's UIKit (`_UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption`),
so every app here uses scenes.

## XCTestDemo: XCTest and XCUITest (`x86sim test`)

`XCTestDemo/` is an Xcode project (a SwiftUI app with an app-hosted unit test target, a logic test target and a UI
test target; each has a deliberate failure, the unit tests a skip) and `XCTestDemo/spm/` a Swift package with XCTest
and Swift Testing tests:

    tests/apps/XCTestDemo/build.sh [OUT]          xcodebuild build-for-testing on the Mac (X86SIM_MAC); products and
                                                 the .xctestrun into $X86SIM_SCRATCH/apps/XCTestDemo-products
    tests/apps/XCTestDemo/run.sh <phone> [--no-build] [--no-ui]
                                                 x86sim test --xctestrun, then every result checked against Apple's
                                                 simulator (Xcode 27.2, iOS 27.2: 14 tests, 9 passed, 4 failed with
                                                 their file:line, 1 skipped)
    x86sim test <phone> --spm tests/apps/XCTestDemo/spm
                                                 the package's tests built on Linux (x86sim dev toolchain) and run

## sdkcov: the whole SDK, on iOS, tvOS, watchOS and visionOS

`sdkcov/` is one SwiftUI app, built from the same sources for the iOS, tvOS, watchOS and visionOS simulators, that walks through
everything the latest SDK (27.2) offers an app:

1. **Every framework and library** in Xcode's simulator SDK (`System/Library/Frameworks/*`, including Swift cross-import
   overlays, and `usr/lib/*`): `dlopen` by install name, then its representative exports resolve (Objective-C classes,
   C / C++ symbols, Swift type metadata), taken from the SDK's `.tbd` so only public API is used. The list is
   `sdkcov/manifest-<platform>.json` (`gen-manifest.py`, committed; regenerate after an SDK or runtime update).
2. **Functional checks** per framework (`sdkcov/checks/*.swift`): real work with results checked, all offline and
   offscreen: Metal shaders compiled at run time and their pixels read back, SceneKit / OpenGL ES / Core Image /
   SwiftUI / Charts rendering, AVAssetWriter + VideoToolbox encode / decode, offline AVAudioEngine rendering, AAC,
   Core ML compiling and running a model built in the app, Vision OCR, keychain, Core Data / SwiftData, TCP / UDP /
   HTTP on loopback, WKWebView running JavaScript, and so on.
3. **Coverage**: `Documents/sdkcov-coverage.json` per framework (load result, functional checks), one summary line.

SKIP, never FAIL, for what Apple's own simulator does not support either: frameworks the runtime does not ship, Xcode's
injected diagnostics libraries, and hardware the simulator lacks, reported as the framework itself reports it
(cameras, NFC, UWB, Bluetooth, motion, haptics, ARKit tracking, Apple Intelligence). The app carries the simulated
entitlements Xcode embeds for its capabilities (`sdkcov.entitlements`: keychain, app groups, HealthKit, HomeKit,
Siri, iCloud, push) in `__TEXT,__entitlements`.

    tests/apps/sdkcov/build.sh [ios] [tvos] [watchos] [xros]
    tests/apps/sdkcov/run.sh <device> [--no-build] [--fresh] [--only ID[,ID...]]

`run.sh` picks the platform from the device's runtime, installs, launches, and relaunches until the app reports
`done`: a crash or a hang (the app's watchdog) is reported as FAIL for that check and the next launch resumes after
it. Results, coverage JSON, `report.md` (`report.py`), the app's stdout / stderr per launch and a screenshot go to
`$X86SIM_SCRATCH/apps/sdkcov-<platform>-<time>/`. `--only func:Metal,load:UIKit` runs just those checks once and
prints their results (stderr only; the results file is left alone). One `run.sh` / `inputs.sh` per device at a time
(each launch terminates the app): a second one on a busy device stops with "is busy" (a flock on `sdkcov.lock` in the
device's state dir, `devlock.sh`).

The host-driven input checks (`checks/Inputs.swift`: Shake, Rotate, Face ID / Optic ID enrollment and a match, the
simulated game controller, Simulate Memory Warning) only report state in a normal run; with
`SDKCOV_INPUT_WAIT=<s>` in the app's environment each waits for its input. `tests/apps/sdkcov/inputs.sh <device>
[--no-build] [shake|orientation|biometric|gamepad|memwarn ...]` runs them one at a time and sends the input with the
x86sim CLI meanwhile (Apple's equivalents are in its header).

watchOS and visionOS build against Xcode's own simulator SDKs, copied from the Mac to `sdks/WatchSimulator-xcode.sdk` and
`sdks/XRSimulator-xcode.sdk` (`sdks/XRSimulator.sdk` is sim-sdk.py's substitute, which has iOS's framework list).
`ref-mac.sh xros` needs a visionOS simulator runtime installed on the Mac. `ref-mac.sh <platform> --only ID[,ID...]` runs
just those checks once on Apple's simulator and prints their results, as `run.sh --only` does on x86Sim.
The sdkcov devices run with the "widgets" feature on (`features.json`), as WidgetKit needs chronod; run.sh says so
when a device has it off. Boot them with `X86SIM_AUDIO=1` for the audio checks (simaudio: real-time output, PHASE,
OpenAL, speech). The first speech synthesis after a boot can time out while its extension is translated.

Building on Linux: the toolchain has no SwiftDataMacros plugin and its Foundation macros expand to the open-source
module names, so `checks/Data.swift` writes out what `@Model` and `#Predicate` expand to.
