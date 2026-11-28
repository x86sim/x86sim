# x86Sim and Apple's Simulator

How x86Sim compares with what Apple ships for its simulators on a Mac: Xcode, `xcrun simctl` and Simulator.app.
Both run the same simulator runtimes (iOS, iPadOS, tvOS, watchOS and visionOS 27.x, downloaded from Apple's
catalogue), so the question is what surrounds the runtime: the host, the tools and the hardware stand-ins.

Ratings: **same** (works as on a Mac), **close** (works, with a difference named), **gap** (missing or not working),
**better** (x86Sim does something Apple's tools don't). "Verified" names how it was checked; **unverified** means
nobody has compared it yet, on one side or both.

The bar is Apple's *simulator*, not hardware: what Apple's simulator cannot do (cameras, most sensors,
entitlement-gated services) is not counted as an x86Sim gap.

## How it was verified

- **sdkcov** (`tests/apps/sdkcov`): one app built for all four simulator platforms that exercises every public
  framework of the 27.2 SDK with real assertions, run on x86Sim and on Apple's simulator on a Mac mini
  (`ref-mac.sh`). In the last full regression pass (2026-11-15) PASS / SKIP / FAIL were identical to Apple's on all
  four: iOS 248/18/0, tvOS 203/7/0, watchOS 143/5/0, visionOS 208/11/0. Later rounds added checks; their state per
  framework, and every remaining difference, is in
  [COVERAGE-GAPS.md](../tests/apps/sdkcov/COVERAGE-GAPS.md).
- **webcov** (`tests/web/webcov`): 261 web-platform checks in Safari with expected values, run on x86Sim and on
  Apple's simulator on the Mac mini (`run.sh --ref-mac`); see [Web platform](#web-platform).
- **Input checks** (`tests/apps/sdkcov/inputs.sh`): shake, orientation, biometrics, game controller, memory warning,
  push, context menus, pickers, widgets, Control Center and others driven on both sides.
- **The regression apps** in `tests/apps` (UIKit, SwiftUI, networking, Core Data, Metal, SpriteKit, audio).
- **End-to-end scripts** that drive `x86sim-mcp` on a phone as an agent does: `tests/ax` (snapshots, actions, OCR and
  marks, permission prompts, games, network capture and mocks; `tools_e2e.py`: what every tool shares — schemas,
  unknown and missing arguments, devices that are off, a sweep of every tool), `tests/dev` (every app toolchain of
  [BUILDING-APPS.md](BUILDING-APPS.md), built on Linux and run), `tests/share` (the browser view), `tests/account`,
  `tests/devapp`, `tests/mcp`, `tests/lldb`, `tests/profile`. They need a booted phone and are run by hand, not by CI
  (CI builds the AppImage).

## Running apps

| Feature | x86Sim | Rating | Verified |
|---|---|---|---|
| Install a simulator `.app` | `x86sim install`, MCP `app_install`, drop on the launcher | same | regression apps, sdkcov |
| Install a device build (`.ipa`, `.xcarchive`, device `.app`) | converted on install: Mach-O platform retagged, `Info.plist` platform keys, ad-hoc re-sign keeping entitlements | better (simctl installs simulator builds only) | `tests/apps/build-ipa.sh` fixture (embedded fat framework, entitlements) |
| App Store downloads | refused: FairPlay-encrypted | same (neither runs them) | refused in code (`LC_ENCRYPTION_INFO` cryptid 1) |
| Launch with arguments, environment, `--console`, `--terminate-running-process` | `x86sim launch` (`SIMCTL_CHILD_*` too) | same | regression apps |
| Wait for a debugger (`-w`) | the app waits; `x86sim debug --launch` launches it stopped before dyld and attaches | same | `tests/lldb/run.sh` |
| Terminate, uninstall, `listapps`, `get_app_container` | yes | same | regression apps |
| Build apps | `x86sim dev` (omarchy-apple-dev: Swift, xtool, the iOS SDK from Xcode's `.xip`): Swift packages, Xcode projects and workspaces (Apple's Swift Build, with an `xcodebuild` stand-in), CocoaPods, Carthage, XcodeGen, Tuist, React Native / Expo, Flutter, Kotlin Multiplatform, .NET for iOS, Rust (Tauri, Dioxus, Bevy), Capacitor / Ionic, Cordova, NativeScript, Godot, Unity ([BUILDING-APPS.md](BUILDING-APPS.md)) | close: no Xcode IDE, no Interface Builder canvas, no SwiftUI previews; Bazel `rules_apple` and Unreal Engine do not build on Linux; `actool`, `ibtool`, `momc`, `xcstringstool` have Linux versions | `tests/dev/*_e2e.py`: each toolchain's sample built on Linux, installed, read and tapped on a phone |
| Debugger (LLDB attach to a simulator app) | `x86sim debug`, MCP `debug_start` / `debug_command` / `debug_stop`: the host's lldb over a GDB remote stub in the translator (`gdbstub.cpp`); attach to a running app or launch it stopped; breakpoints, step, finish, `bt all`, variables, registers, memory, image list, crash stops | close: Swift values (`frame variable`, `po`, `expression`) with the Swift toolchain's lldb, set up on first use (private libpython, Darwin resource dir; `/usr/bin/lldb` fallback without Swift values); no watchpoints, natively run functions (`objc_msgSend`, libsystem thunks) never hit breakpoints | `tests/lldb/run.sh` (C / Objective-C app: launch stopped, break, step, variables, finish, attach, worker thread; Settings `bt all`); `tests/lldb/run-swift.sh` (Swift app: line breakpoint, demangled `bt`, `frame variable` of struct / array / String / Dictionary / Optional / enum, `po` struct and object, `expression`, MCP session); the C / Objective-C checks and the MCP session also on iPad, Apple TV, Apple Watch and Vision Pro (2026-11-24: 13 / 13 each; the stub reports an `os_version` per platform so lldb keeps the loader it serves) |
| XCTest / XCUITest runs (`xcodebuild test-without-building`) | `x86sim test`, MCP `xctest_run`: an `.xctestrun` or bundles; app-hosted unit tests (`libXCTestBundleInject`), logic tests (`xctest`), UI tests (runner + `testmanagerd`, the `xctest` feature); per-test results with `file:line`, JUnit XML, JSON, exit 0 / 1 | close: same results, attachments, UI tests' screen recordings (kept for failures), code coverage (`--coverage`: Xcode's line counts) and `.xcresult` bundles (`--result-bundle`, read by `xcresulttool` / `xccov` as Xcode's: call stacks, `#expect` values, activities, the action and build logs, diagnostics,
performance metrics); repetitions, retries, time limits, test plan configurations, several devices; no IDE session; the rest is listed in [TESTING.md](TESTING.md#differences-from-xcode); XCTest's frameworks come from Xcode (`sdks/` or the `x86sim dev` SDK) | `tests/apps/XCTestDemo/run.sh`: 14 tests (hosted, logic, UI; passes, deliberate failures, a skip) = Apple's iOS 27.2 simulator; the same on iPad, tvOS, watchOS and visionOS (all 14; watchOS's and visionOS's `testTypeName` fail as on Apple's); Swift package tests with Swift Testing |
| Build test bundles (`xcodebuild build-for-testing`) | Swift packages: `x86sim dev test --xctest` (`swift build --build-tests` for the simulator, then run). Xcode projects: the dev toolchain's `xcodebuild` stand-in (`build-for-testing`, `test`: Swift Build writes the `.xctestrun`, `x86sim test` runs it) for unit test targets (XCTest and Swift Testing) and UI-test targets (the `<Target>-Runner.app` from the SDK's XCTRunner.app, taken from Xcode.xip), on every simulator platform | close: a project without shared schemes gets generated schemes without its test target | XCTestDemo built here (`run.sh --linux`): all 14 = Apple on iPhone, Apple TV, Apple Watch and Vision Pro; `tests/apps/XCTestDemo/spm`; `tests/dev/xcode_e2e.py --fastlane` (a lane's `scan` runs the tests on the phone) |
| Accessibility Inspector: inspection | browser viewer and launcher: *Inspect* outlines the element under the mouse and shows its properties (label, value, hint, traits, identifier, frame, activation point, actions, custom actions, rotors, input labels, language, respondsToUserInteraction, class, view, view controller, hierarchy, font, text color, truncation); `x86sim ax-inspect`, MCP `ax_inspect` | close: no live notifications log, no "speak" preview, no settings pane; Vision Pro has no outline on the display (a list instead) | x86a11ybad + Settings, iPhone; x86a11ybad on iPad, Apple TV and Vision Pro (Vision Pro: per-window frames, the viewer lists the elements), Settings on Apple Watch (2026-11-24) |
| Accessibility Inspector: audit | `x86sim ax-audit`, MCP `ax_audit`, *Run audit* in the viewer / launcher: missing labels, unreadable labels, hit regions (44 pt), contrast (WCAG), text clipped, Dynamic Type (at the largest accessibility size), duplicate labels, controls VoiceOver cannot reach, missing button trait; JSON with ref, frame, severity, fix | close: Xcode's rule set and wording differ; SwiftUI text has no clipping check; contrast is sampled, not read from the render tree | x86a11ybad (every rule fires), Settings; iPad, Apple TV, Vision Pro (x86a11ybad), Apple Watch (Settings): no hit-region check on TV and Watch, 60 pt on Vision Pro, no contrast check on Vision Pro; the viewer's Inspect routes on iPad, TV and Vision Pro; the label with a tap gesture has the button trait from the runtime on iPad, TV and Vision Pro, so no trait-mismatch there (2026-11-24) |
| Accessibility Inspector: actions | activate, increment, decrement, scroll, escape, magic tap, custom actions by name: `x86sim ax-action`, MCP `ax_action`, the inspector's buttons | same | x86a11ybad on iPhone, iPad, Apple TV (no slider there), Vision Pro; escape and scroll on Apple Watch (2026-11-24) |
| Safari / WKWebView Web Inspector (Safari's Develop menu) | the runtime's `webinspectord` on demand (`webinspector` feature) with its debugger socket at `/run/user/<uid>/x86sim-rwi-<UDID>.sock`; WebKit's own Web Inspector in the browser (`x86sim inspect`, the viewer's Web pages… button: Elements, Console, Network, Sources, …); MCP `web_targets`, `web_eval`, `web_console`, `web_dom`, `web_network` | close: the frontend is WebKitGTK 2.54.3's build of WebInspectorUI (in the AppImage; a checkout: build/webinspector.sh or the host's WebKitGTK), not Safari 27's, so a panel that needs a newer protocol command may miss it; app WKWebViews only with `isInspectable = true`, as on Apple's simulator; console messages from before a page's first inspection are not kept (WebKit; `reload` in the tools) | `r2wi` phone: Safari page with console logs and a fetch (Elements, Console, Network shown; MCP tools), `tests/apps/x86webinspect` (inspectable web view listed, the other not); iPad and Vision Pro: Safari and x86webinspect, every `web_*` tool; Apple TV (no WebKit, no Safari): an app's inspectable JSContext is listed, `web_eval` / `web_console` work; Apple Watch: no web content to inspect, as on Apple's (2026-11-24) |
| Instruments: Time Profiler (CPU) | `x86sim profile`, MCP `profile_start` / `profile_stop`, the viewer's Profile button: the translator samples the app's threads by CPU time (`perf_event_open`, `prof.cpp`), stacks from frame pointers, symbolicated against the dyld cache and the app; speedscope, Firefox Profiler or a text top-N per thread | close: times are of the translated x86-64 code (relative costs differ from a device), frame-pointer stacks only, natively run library functions are one frame, no kernel time, no dispatch queue names | `tests/profile/run.sh` (busy test app: its hot function on top; Settings scrolling), also on iPad, Apple TV, Apple Watch and Vision Pro (2026-11-24) |
| Instruments (other instruments: Allocations, Leaks, ...), Memory Graph, view debugger | none | gap | |
| Crash reports | `x86sim logs BUNDLEID`: the last crash, symbolicated against the dyld cache and the app; why an app aborted (`abort_with_reason` / `abort_with_payload` text, dyld's halts) in `app_launch`'s error, `app-state` and `logs` | close: the translator's guest backtrace, not Apple's `.ips` report | on phones; `tests/apps/x86dyldfail` |
| App `os_log` / `Logger` | stored by `logd`, read back with `OSLogStore`; `x86sim log stream --app`, `x86sim logs BUNDLEID` | close: the runtime's own processes do not log; one app or process at a time | sdkcov `OSLog` check |
| Sandboxing | simulator processes are not sandboxed | gap (sandbox extensions are dummy tokens; see [WORKAROUNDS.md](WORKAROUNDS.md)) | |

## Simulated hardware and system features

| Feature | x86Sim | Rating | Verified |
|---|---|---|---|
| Touch, multi-touch, edge gestures | mouse in the launcher; real multi-touch in the browser; `tap` / `swipe` / `pinch` / `pan2` from the CLI, MCP `pinch` (also two-finger rotation) | same | inputs.sh |
| Hardware keyboard, paste | yes; Ctrl+V types the Linux clipboard | same | |
| Toggle Software Keyboard, Connect Hardware Keyboard (I/O > Keyboard, Cmd-K / Shift-Cmd-K) | `x86sim keyboard software on\|off\|toggle`, `keyboard hardware connect\|disconnect`, MCP `sim_keyboard`, launcher Simulate section and Ctrl+K / Ctrl+Shift+K, browser viewer; Apple's mechanisms (the on-screen-keyboard key on the keyboard service; GraphicsServices' hardware-keyboard state, as CoreSimulatorBridge sets it); the connection is kept across boots | same | wt-kbd 2026-11-24: shown with the hardware keyboard connected, disconnected = software keyboard on focus and while typing, `type` both ways, snapshot lists the keys; compared with Apple's iOS 27.2 simulator; 2026-11-24: `type` and `sim_keyboard` on iPad, Apple TV (edits in its full-screen keyboard), Apple Watch (its input sheet; `type keys=true` does not reach it, unchecked against Apple's) and Vision Pro |
| Buttons (Home, Lock, side, volume, Siri, Action, Apple Pay) | `x86sim button`, MCP `sim_button` | same | inputs.sh |
| Shake, rotate, orientation, memory warning | `shake`, `rotate`, `orientation`, `memwarn`; MCP `sim_shake`, `sim_orientation`, `sim_memory_warning` | same | inputs.sh |
| Face ID / Touch ID / Optic ID | `biometric enroll\|unenroll\|match\|nomatch`, MCP `sim_biometric` | same | inputs.sh |
| Location (point, route, scenarios) | `x86sim location`, as `simctl location`; MCP `sim_location` | same | sdkcov location checks |
| Push notifications | `x86sim push`, MCP `sim_push` | same | inputs.sh push |
| Privacy grants | `x86sim privacy`, MCP `sim_privacy` | same | |
| Game controller | `x86sim gamepad`, MCP `sim_gamepad` | same | inputs.sh |
| iPad pointer and trackpad | `pointer`, `trackpad on\|off`, MCP `sim_pointer`, `sim_system` | close: trackpad gestures are not simulated (as Apple's "Simulate Trackpad"; pinch / pan2 instead) | GCMouse input unreachable on Apple's side too |
| Digital Crown, watch side button, hand gestures | `crown`, `button side`, `gesture doubletap\|wristflick` | same | |
| Apple TV Siri Remote | on-screen remote; arrows, Return, Escape; `button select\|menu\|playpause\|tv` | close: the keyboard stands in for the remote's services | browser viewer's remote driven from headless Chrome (2026-11-24) |
| Vision Pro look and pinch | the mouse as the system pointer (hover, click, drag, wheel), `pointer`, `crown`; elements through accessibility ([AGENT-TOOLS.md](AGENT-TOOLS.md)) | close: the pointer stands in for eyes and hands | x86Sim's side on a Vision Pro (2026-11-24); not compared with Simulator.app's input (the reference Mac runs Apple's simulator through `simctl`, without Simulator.app) |
| iPhone Duo hinge | slider and DeviceHub poses (closed, half open portrait / landscape, open); MCP `sim_fold`; drawn in 3D from Apple's model when the user's Xcode.xip gave it, flat otherwise | same | on phones |
| Appearance, Dynamic Type, Increase Contrast, Reduce Transparency / Motion | `x86sim ui`, MCP `sim_ui` | same; appearance is "unsupported" on tvOS and watchOS, as Apple's (`simctl ui appearance`: ENOTSUP) | on phones, iPad, Vision Pro; Apple's answers on the Mac (2026-11-24) |
| Status bar overrides | `x86sim status_bar`, MCP `sim_status_bar` | same; not supported on tvOS and watchOS, as Apple's | on phones (dark at 9:41), iPad, Vision Pro |
| Pasteboard | `pbcopy`, `pbpaste`, `pbsync` (Linux clipboard) | same | on phones |
| Language, region, time zone | `locale`, `timezone` | same (time zone applies at the next boot) | on phones |
| Keychain | `keychain reset` | close: no `add-cert` / `add-root-cert` | reset checked with a probe app |
| Audio output | every device has audio devices; heard on Linux (PipeWire) with `X86SIM_AUDIO=1` and in the browser view (`x86sim serve`) | close | sdkcov audio checks with `X86SIM_AUDIO=1`; the browser view's sound (a 440 Hz tone from AudioDemo) on iPad, Apple TV, Apple Watch and Vision Pro (2026-11-24) |
| Microphone | simaudio's microphone | same, or better than a Mac without an input device | sdkcov P12 |
| Cameras | none | same (Apple's simulator has none) | sdkcov |
| Accelerometer, gyroscope, Apple Pencil input | none | gap for Pencil input (unverified on Apple's side); motion sensors: unverified on Apple's side | PencilKit canvas checked with a finger stroke |
| External displays, CarPlay | one built-in display | gap (Simulator.app: I/O > External Displays) | |
| Networking, DNS | Linux's network | same | regression apps, sdkcov |
| Bonjour | on the device itself (apps on one phone find each other) | gap across the network: no multicast DNS on the host's LAN | sdkcov MultipeerConnectivity, on-device Bonjour |
| Network conditioning | none | gap (unverified what Xcode 27 offers here) | |
| Maps | the app runs | close: tiles draw (boot-a64.sh writes geod's seed manifest; geod downloads Apple's resources) | |
| Siri | the Siri button and feature toggle exist | unverified | |
| Apple Intelligence (FoundationModels) | through a Mac with Apple Intelligence (`X86SIM_FM_RELAY`) | same requirement as Apple's simulator; without a Mac: "not ready" | sdkcov with the relay (iPad = Apple) |
| Pairing a watch with an iPhone | not supported | gap | |
| GPU (Metal) | translated to Vulkan (`mtlvk`) on NVIDIA and AMD (RADV); a GPU per device (`x86sim gpu`, MCP `sim_gpu`: auto, one GPU, or `cpu` = lavapipe); software fallback | close: coverage is what metal2vulkan translates ([METAL.md](METAL.md)); some Metal features abort on Apple's simulator too | sdkcov graphics checks |
| iCloud sync | none | gap | |

## simctl commands

| `xcrun simctl` | x86Sim | Rating |
|---|---|---|
| `list` | `x86sim-aot sim status`, `x86sim device list`, MCP `phones_list` / `sim_devices` | same |
| `create` | `x86sim device create --model M [NAME]`, MCP `sim_add_device` | same |
| `boot`, `shutdown` | `x86sim-aot --device NAME sim start\|stop\|restart`, MCP `sim_start` / `sim_stop` / `sim_restart` | same (start returns at once: wait with `x86sim wait home`) |
| `delete` | `x86sim device delete NAME [--stop]`, MCP `sim_delete`, the launcher | same |
| `rename` | the launcher only | close |
| `erase`, `clone` | `x86sim erase NAME`, `x86sim clone NAME NEW`, MCP `sim_erase` / `sim_clone` (phone off, named explicitly) | same |
| `install`, `uninstall`, `launch`, `terminate`, `listapps`, `get_app_container` | same names | same (plus device builds) |
| `openurl`, `addmedia`, `push`, `privacy`, `location`, `spawn` | same names | same |
| `ui`, `status_bar`, `pbcopy`, `pbpaste`, `pbsync` | same names | same |
| `keychain` | `reset` only | close |
| `io screenshot`, `io recordVideo` | same (`--codec h264\|hevc`); also `record start\|stop`, MCP `sim_record_video` | same; no `io enumerate` |
| `spawn ... log stream` | `x86sim log stream --app\|--process` | close (one app or process) |
| `runtime list / add / delete` | `x86sim runtime list\|fetch\|delete` (downloads from Apple's catalogue) | same |
| `appinfo`, `getenv`, `install_app_data`, `icloud_sync`, `diagnose`, `logverbose`, `monitor`, `upgrade`, `darwinup` | none | gap |
| `pair`, `unpair`, `pair_activate` | none | gap |
| (none) | `locale`, `timezone`, `fidelity`, `logs BUNDLEID`, `biometric`, `shake`, `rotate`, `gamepad`, `button`, `crown` | better: Simulator.app menu items as commands |

## Web platform

Safari 27.2 on an iPhone 17 Pro, the same runtime on both sides; 261 checks of the JavaScript engine (ES2025,
WebAssembly incl. SIMD / threads / exceptions / GC, SharedArrayBuffer with COOP / COEP), workers, storage, network,
graphics (Canvas, WebGL with read-back pixels, fonts, SVG, CSS effects), media, device APIs, DOM / CSS and a few
benchmarks. Full table, diagnosis and perf ratios: [tests/web/webcov/COVERAGE.md](../tests/web/webcov/COVERAGE.md).

| Area | x86Sim | Rating | Verified |
|---|---|---|---|
| JavaScript, WebAssembly, workers, storage, network, graphics, DOM / CSS | every check as on Apple's (WebGPU absent on both; SharedWorker passed on x86Sim, timed out in Apple's runs). Open: WebGL found no context on a newly created device in a later run, with or without the AIO change (COVERAGE.md) | same | webcov, 2026-11-24 |
| `<video>` / `<audio>` | every media check as on Apple's, any number per session (H.264 / HEVC / AAC / WAV / Opus-in-MP4, native HLS, full screen, requestVideoFrameCallback, MediaElementSource; VP9 WebM, MSE / ManagedMediaSource, WebCodecs, Web Audio decoding). MediaToolbox's byte cache uses POSIX AIO, which Darling now implements (macOS 27's limits and errors) | same | webcov + `tests/arm64/aioprobe.c` (also on the Mac), 2026-11-24 |
| Permission prompts (microphone, location, motion), geolocation from `x86sim location`, clipboard, wake lock, Web Share, WebAuthn sheet, speech | work on x86Sim; Apple's side not compared (its website prompts were not answered by the Mac runner) | **unverified** | webcov x86Sim side, 2026-11-24 |
| Speed | rendering at par (canvas 2D, requestAnimationFrame, WebGL ≈ 1.0x Apple's on the Mac mini, WebAssembly loop 1.1x); JavaScript and DOM 0.1-0.5x; geometric mean 0.37x | close | webcov perf checks, 2026-11-24 |

## For agents

| Feature | x86Sim | Rating |
|---|---|---|
| The screen as text | `x86sim snapshot`, MCP `snapshot`: the app's accessibility elements (UIKit, SwiftUI), web content, SpringBoard alerts, home screen, status bar, with stable refs and diffs | better: simctl has none; XCUITest reads the same tree only inside a test bundle |
| Act on elements | `tap`, `type`, `key`, `scroll`, `scroll_to`, `swipe`, `long_press`, `press`, `wait_for` by ref, identifier, label or point; each answers with the settled screen | better |
| iPad windows | windowed apps placed where they show, the top window's app first, elements under another window `[covered]` | better |
| Apple TV | the focus in the snapshot; `tap` moves the focus with the remote and selects (as XCUIRemote), `scroll` / `swipe` move it | better |
| Apple Watch | face, app grid, the text input sheet (type into it); crown | better |
| Vision Pro | windows' elements in their own points; `tap` through accessibility (activate, else a tap made in the app), `scroll` the front app's scroll view; no display points | close: no tap at a display point (windows float in 3D); `sim_pointer` for that |
| Screenshot in points | MCP `screenshot` (default) | better: coordinates match the snapshot |
| Text in pixels | OCR of views with no elements (Metal, games, canvas): `[ocr]` text elements with refs, found by `tap` / `wait_for` too; `screenshot --marks` outlines every element with its ref | better |
| Forms and controls | `fill` (a whole form), `set_value` (sliders, switches, steppers, pickers, dates), `drag`, `expect` (checks with conditions), `batch` (several tools in one call) | better |
| Game engines and other UI kits | Flutter's semantics tree, SpriteKit / SceneKit named nodes, Unity objects (IL2CPP) in the snapshot | better |
| An app's network traffic | `x86sim net`, MCP `net_capture` / `net_log` / `net_request` / `net_mock`: URLSession requests with headers, bodies and timings, mock answers and failures, no proxy ([protocols/NET.md](protocols/NET.md)) | better: simctl has none (Instruments' network template not compared) |
| MCP server | `x86sim-mcp`, 103 tools | unverified against what Xcode 27 ships for agents |
| Device pool with leases | `x86sim pool`, MCP `phone_take` / `phone_release` | better |
| Build-install-launch in one call | `x86sim dev run`, MCP `dev_run` | close to `xcodebuild` + `simctl` |
| Watch or drive a device from a browser, share it | `x86sim serve`, MCP `share_*`, Tailscale / cloudflared | better: Apple has no browser streaming |

Element snapshots and actions are verified on iPhone, iPad (windowed apps, landscape, keyboard, pointer), Apple TV,
Apple Watch and Vision Pro (2026-11-24). XCTestDemo (`tests/apps/XCTestDemo/run.sh`) gives Apple's results (unit,
logic and UI tests) on iPhone, iPad, Apple TV, Apple Watch and Vision Pro.

## Multi-device

| Feature | x86Sim | Rating |
|---|---|---|
| Several devices booted at once | yes, each in its own container | same |
| Different OS versions side by side | yes (`device create --ios`) | same |
| Memory per device | measured idle (2026-11-15): iPhone ~0.9 GB, Apple TV ~0.5 GB, Apple Watch ~0.7 GB, Vision Pro ~1.2 GB; with every feature on (2026-11-26): iPhone ~1.0 GB, iPad Pro 13 ~1.2 GB, Vision Pro ~1.9 GB with its GPU helper | unverified against a Mac |
| Trimming a device | feature sets (Full, Lean, Minimal) with each part's memory cost; per-device memory cap; idle clean-up | better |
| Shared memory between devices | KSM opt-in for pool devices (`x86sim pool ksm`) | better (Linux only) |
| Fast copies | reflink clones (`clone`, pool members) | same as APFS clones on a Mac |
| Watch paired to an iPhone | no | gap |

## Speed

x86Sim translates the runtime's arm64 code to x86-64 (ahead of time into a pre-translated image, the rest just in
time) and Metal to Vulkan; a Mac with Apple silicon runs both natively. Measured on the development machine in the
2026-11-15 regression pass, with an up-to-date pre-translated image: boot to the home screen in about 14 s (iPhone),
11 s (Apple TV), 17 s (Apple Watch) and 14-17 s (Vision Pro). Without any translation cache (a new runtime before its
image is built) an iPhone 18 Pro reached the home screen in about 16 s (2026-11-25): cold code starts in the
translator's interpreter tier, so a first boot need not wait for pre-translation, which then runs in the background.
An element snapshot takes 10-35 ms.

Side by side with Apple's simulator on the Mac mini, only web content has been measured (webcov's perf checks in
Safari, 2026-11-24: rendering about as fast, JavaScript and DOM at 0.1-0.5x, see [Web platform](#web-platform)); app
CPU speed, frame rates and launch times relative to a Mac are otherwise **unverified**. Compute-heavy app code runs translated and is expected to be slower than
on Apple silicon. `x86sim fidelity fast` draws a 3x iPhone at 2x with reduced effects for more frame rate on a weak GPU.
