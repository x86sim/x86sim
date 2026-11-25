<div align="center">

# x86Sim

**Apple's iOS, iPadOS, tvOS, watchOS and visionOS simulators on x86-64 Linux.**

[![Latest release](https://img.shields.io/github/v/release/x86sim/x86sim?label=release)](https://github.com/x86sim/x86sim/releases/latest)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)](LICENSE)

[Download](https://github.com/x86sim/x86sim/releases/latest) · [Quick start](#quick-start) · [Usage](#usage) · [For AI agents](#for-ai-agents) · [Building apps](#building-and-shipping-apps) · [vs Apple's Simulator](docs/PARITY.md) · [Architecture](docs/ARCHITECTURE.md) · [Developing](docs/DEVELOPMENT.md)

<img src="docs/screens/launcher-devices.webp" alt="The x86Sim launcher with several simulated devices running side by side" width="100%">

</div>

## About

Apple ships its device simulators only for Macs. x86Sim runs the same arm64 simulator runtimes, unmodified, on an
ordinary x86-64 Linux PC. It downloads a runtime from Apple onto your machine, runs it on a macOS-compatible system
layer, translates its arm64 code to x86-64 and draws its Metal graphics with Vulkan.

It comes with a desktop launcher, a `simctl`-style command line, a browser view and an MCP server, so devices can be
used by hand, from scripts or by an AI agent. No Mac and no Apple ID are needed. [PARITY.md](docs/PARITY.md)
compares it with Apple's Simulator feature by feature, gaps included.

## For AI agents

A coding agent can run an app on a device and check it the way a tester would, without screenshots for every step:

- **The screen as text.** `x86sim snapshot` (MCP `snapshot`) lists the accessibility elements on screen, each with a
  stable ref; `tap`, `type`, `scroll-to`, `wait-for` and the other actions take a ref, an identifier or label text
  and answer with the screen after it settled. See [Agents](#agents-reading-the-screen-and-acting-on-it).
- **Accessibility audits.** `x86sim ax-audit` (MCP `ax_audit`) checks the screen as Xcode's Accessibility Inspector
  does and answers with findings and fixes; `ax_inspect` and `ax_action` inspect an element and perform its
  accessibility actions ([Accessibility Inspector](#accessibility-inspector)).
- **Devices on demand.** A pool keeps devices booted; an agent takes one with a lease and gives it back
  ([Device pool](#device-pool)). Simulator and device builds (`.ipa`) install the same way.
- **The whole loop.** `x86sim dev run` builds an app, installs and launches it; `x86sim logs BUNDLEID` returns its
  `os_log` and its last crash, symbolicated ([Building and shipping apps](#building-and-shipping-apps)).
- **A human in the loop.** `x86sim serve` puts a device in a browser, shareable over Tailscale or cloudflared
  ([In a browser](#in-a-browser-local-or-remote)).

Agents that work from a shell: [skills/x86sim/SKILL.md](skills/x86sim/SKILL.md) is a skill file that teaches the
loop (copy it to `.claude/skills/x86sim/` or your agent's equivalent). Agents that speak MCP:
`claude mcp add x86sim -- /path/to/x86sim-*.AppImage mcp` ([MCP server](#mcp-server)).

## Features

**Devices**

- iPhone, iPad, Apple TV, Apple Watch and Apple Vision Pro: 82 device models, including the dual-screen iPhone
  Duo, whose hinge moves from closed to flat on a slider, with the poses of Xcode's DeviceHub (closed, half open in
  portrait or landscape, open).
- Several devices at once. Each runs in its own container with its own apps, data and settings; starting or stopping
  one leaves the others alone.
- The stock system apps of each runtime, such as Settings, Safari, Photos, Files, Maps and Health.

**Apps**

- Install, launch, terminate and uninstall your own apps: arm64 iOS-simulator builds, the kind Xcode produces for
  the simulator, and device builds: an `.ipa`, an `.xcarchive` or a device `.app` is converted on install (see
  [Device builds](#device-builds)). In the launcher, drop one on a device's window.
- Run XCTest unit tests and XCUITest UI tests from an `.xctestrun` or test bundles, with per-test results, JUnit XML
  and an exit status for CI (see [Tests](#tests-xctest-xcuitest)).
- A pool of devices kept booted for agents and CI, taken and given back with leases (see [Device pool](#device-pool)).
- Launch with arguments and environment variables, stream an app's stdout, stderr or `os_log`, or have it wait for a
  debugger.
- Debug an app with LLDB: attach to it running or launch it stopped, then breakpoints, stepping, backtraces of every
  thread, variables, registers and memory (see [Debugging (LLDB)](#debugging-lldb)).
- Profile an app's CPU use (as Instruments' Time Profiler): top functions by self and total time per thread, and a
  speedscope or Firefox Profiler file (see [Profiling (CPU)](#profiling-cpu)).
- Find an app's bundle, data and app-group containers as ordinary Linux paths.

**Graphics and input**

- GPU rendering: Metal is translated to Vulkan for the system's render server and for apps. Software rendering is
  available as a fallback.
- Touch with the mouse: tap, swipe, edge gestures, pinch and two-finger scroll. Your keyboard types into the device,
  and Ctrl+V pastes from the Linux clipboard.
- The keyboard menu of Apple's Simulator: Toggle Software Keyboard (Ctrl+K in the viewer) brings up the on-screen
  keyboard while a hardware keyboard is connected, and Connect Hardware Keyboard (Ctrl+Shift+K) can disconnect it so the
  software keyboard comes up on every text field, as on a device. Both are in the launcher's Simulate section, the
  browser viewer, `x86sim keyboard` and the MCP tool `sim_keyboard`.
- Home, lock and volume buttons; an on-screen Siri Remote for Apple TV; a Digital Crown and side button for Apple
  Watch; look-and-pinch for Apple Vision Pro, driven by the mouse.

**Performance and memory**

- arm64 code is translated just in time and kept in a persistent cache.
- Pre-translation builds an ahead-of-time image for a runtime, once, so boots and app launches are fast. It updates
  incrementally when something changes.
- Feature sets: turn off the parts of the system a device doesn't need (Siri, Maps, Wallet, widgets, the live
  wallpaper and others), individually or with the Full, Lean and Minimal presets. Each one lists its memory cost.
- An optional memory cap per device, and a clean-up service that reclaims idle background processes.
- Live memory, CPU and per-process figures for every device.

**Developer tools**

- Simulated push notifications, simulated location (a point, a route between waypoints or one of Apple's scenarios),
  opening URLs, and adding photos and videos to the Photos library.
- Screenshots and H.264 / HEVC video recording.
- Device settings as `simctl` has them: light / dark appearance, Dynamic Type text size, Increase Contrast, Reduce
  Transparency and Reduce Motion; status bar overrides (time, battery, Wi-Fi and cellular bars, carrier, data network)
  for clean screenshots; the pasteboard (copy, paste, or kept in sync with the Linux clipboard); keychain reset;
  language, region and time zone.
- Erase a device back to new, or clone it.
- An app's logs in one command: its `os_log` and the crash report of its last run, symbolicated.
- A fidelity setting per device: Fast draws a 3x iPhone at 2x with Reduce Transparency and Reduce Motion on, High
  keeps the native scale and every effect.
- Scripted input: tap, swipe, pinch, type, hardware keys, Digital Crown; shake, rotation, Face ID / Touch ID / Optic
  ID, a game controller, an iPad's pointer, memory warnings.
- The screen as text: accessibility elements with stable refs, and actions on them, for agents and tests.
- Every device in a browser, locally or shared over Tailscale or cloudflared, with touch, keyboard and buttons.
- Build apps on Linux and run them on a device in one step (`x86sim dev`, from omarchy-apple-dev).
- Per-service logs for every device, with filtering. Installed apps' `os_log` / `Logger` output is stored and can be
  read back with `OSLogStore`.

**Setup and updates**

- One AppImage. A setup page checks the computer, downloads a runtime from Apple, pre-translates it and creates the
  first device.
- No root where unprivileged user namespaces are available.
- Runtimes are managed on a Versions page: download, resume, remove and delete iOS, tvOS, watchOS and visionOS
  versions. Deleting asks whether to keep the version's pre-translated code (a later download of it starts fast) or
  remove everything that belongs to it, with the size of each.
- The AppImage updates itself, on a stable or a pre-release channel.
- The launcher follows the Omarchy theme live (or the desktop's light / dark setting), and every page can be driven
  from the keyboard: `?` lists the shortcuts.

<table>
<tr>
<td width="50%"><img src="docs/screens/launcher-iphone.webp" alt="An iPhone in the launcher, next to its memory, CPU and per-process figures"></td>
<td width="50%"><img src="docs/screens/launcher-versions.webp" alt="The Versions page, listing installed runtimes and those available from Apple"></td>
</tr>
<tr>
<td align="center"><sub>A device with its memory, CPU and processes</sub></td>
<td align="center"><sub>Runtimes, downloaded from Apple</sub></td>
</tr>
<tr>
<td width="50%"><img src="docs/screens/launcher-apple-tv.webp" alt="An Apple TV in the launcher, with its remote beside the screen"></td>
<td width="50%"><img src="docs/screens/launcher-apple-watch.webp" alt="An Apple Watch in the launcher, showing its app grid"></td>
</tr>
<tr>
<td align="center"><sub>Apple TV</sub></td>
<td align="center"><sub>Apple Watch</sub></td>
</tr>
<tr>
<td width="50%"><img src="docs/screens/launcher-vision-pro.webp" alt="An Apple Vision Pro in the launcher, showing its Home View"></td>
<td width="50%"><img src="docs/screens/launcher-keys.webp" alt="The launcher's keyboard shortcuts"></td>
</tr>
<tr>
<td align="center"><sub>Apple Vision Pro</sub></td>
<td align="center"><sub>Driven from the keyboard</sub></td>
</tr>
</table>

## Quick start

1. Download `x86sim-*-x86_64.AppImage` from the [latest release](https://github.com/x86sim/x86sim/releases/latest).
2. Make it executable and run it:
   ```sh
   chmod +x x86sim-*-x86_64.AppImage
   ./x86sim-*-x86_64.AppImage
   ```
3. Follow the **Setup** page. It checks your computer, prepares the simulator host, downloads iOS 27.2 from Apple
   (about 8 GB), pre-translates it and creates your first device.

Where unprivileged user namespaces are available, nothing is installed outside your home directory and no root is
needed. Where they are blocked (Ubuntu's default, for example), Setup asks for your password once to install the
simulator host under `/opt/x86sim`.

For an Apple TV, an Apple Watch or an Apple Vision Pro, download tvOS, watchOS or visionOS on the **Versions** page,
then choose **Add device**.

## Requirements

| | |
|---|---|
| **OS** | x86-64 Linux with glibc 2.39 or newer: Ubuntu 24.04+, Debian 13, Fedora 39+, Arch and derivatives |
| **CPU** | x86-64 with SSE4.1, POPCNT and CX16 |
| **GPU** | A Vulkan GPU and driver: NVIDIA (proprietary driver) or Mesa (RADV, ANV). Without one, rendering falls back to software |
| **Memory** | 8 GB minimum, 16 GB recommended |
| **Disk** | 45 GB free while a runtime downloads and unpacks |
| **Kernel** | overlayfs; Linux 6.11 or newer is a little faster |

Setup also looks for a few common programs (`python3`, `curl`, `openssl`, `setsid`) and tells you if one is
missing. Everything else is inside the AppImage.

FoundationModels (Apple's on-device language model) needs a Mac with Apple Intelligence, as Apple's own simulators do:
set `X86SIM_FM_RELAY` to it (see `x86sim.env.example`); without one the model reports that it isn't ready.

systemd is optional. With a systemd user manager, each phone runs in a scope with a disk IO cap and low CPU priority
(and the optional memory cap `X86SIM_MEM_MAX`), so a cold boot doesn't stall the desktop. Without one (Devuan, Void,
Gentoo/OpenRC, ...) phones run without those limits and everything else works the same.

## Supported devices and versions

| Device | Runtime | Models | Input |
|---|---|---|---|
| **iPhone** | iOS 27 | 34, from iPhone 11 to iPhone 18 Pro Max, and the iPhone Duo | Touch and gestures, keyboard, Home, lock and volume |
| **iPad** | iOS 27 (iPadOS) | 29: iPad, iPad mini, iPad Air, iPad Pro | Touch and gestures, keyboard, Home, lock and volume |
| **Apple TV** | tvOS 27 | 4: Apple TV 4K, 2nd and 3rd generation | Siri Remote on screen; arrow keys, Return and Escape |
| **Apple Watch** | watchOS 27 | 13: Series 9 to 12, SE 3, Ultra 2 to 4 | Touch, Digital Crown, side button |
| **Apple Vision Pro** | visionOS 27 | 2: at 4K, or at 1080p (the same layout, a quarter of the pixels) | Look and pinch with the mouse, Digital Crown |

Runtimes come straight from Apple's own catalogue, the one Xcode reads. Each copy is downloaded on your machine and
is never redistributed. iOS 27.2 is the default; the Versions page lists the other 27.x releases Apple offers,
with betas if you ask for them.

## Usage

### Launcher

The launcher is what the AppImage opens. Its pages:

- **Devices**: every device as a card with a live screen, running devices first. Click a card to open it; start,
  stop, restart, add, rename and remove devices, or open one in its own window. A device's page has its screen and buttons, its feature toggles, memory, CPU and
  processes, and its logs. Its Simulate section sets appearance, text size, Increase Contrast and fidelity, and
  erases or clones the device.
- **Versions**: the runtimes on this computer and those available from Apple. Delete removes an installed version:
  only its files, or also its pre-translated code and caches. Devices that use it are listed, and a running one has
  to be stopped first.
- **Pre-translate**: build or update the ahead-of-time image for a runtime, and see what it takes on disk.
- **Settings**: appearance (System, Dark or Light), graphics (GPU or software), screen scaling, memory cap and
  clean-up, disk use, and updates.
- **Setup**: the first-run steps, which stay available afterwards as a check of the computer.
- **Develop**: the app toolchain (see [Building and shipping apps](#building-and-shipping-apps)): what is installed,
  install / repair, recent projects with Build and Run.

The Devices page also shows device pools (see [Device pool](#device-pool)).

Arrow keys or `hjkl` move between devices and rows, Enter opens, the number keys switch pages, `?` lists every
shortcut and `q` quits. Closing the launcher leaves running devices running.

### Command line

`x86sim` follows `xcrun simctl` where there is an equivalent. From the AppImage it is
`./x86sim-*.AppImage x86sim <command>`; in a checkout it is `cli/x86sim`.

```sh
x86sim device models                          # the models a device can be
x86sim device create --model "iPhone 17 Pro" work
x86sim runtime list all                       # runtimes in Apple's catalogue
x86sim runtime delete 24K150 --dry-run        # what deleting a version frees (--all: its caches too)

x86sim install MyApp.app                      # an arm64 iOS-simulator build
x86sim install MyApp.ipa                      # or a device build: .ipa, .xcarchive, device .app
x86sim launch --console com.example.myapp -arg value
x86sim terminate com.example.myapp
x86sim listapps --json
x86sim get_app_container com.example.myapp data

x86sim openurl https://example.com
x86sim push com.example.myapp payload.json    # simulated remote notification
x86sim location set 48.8584,2.2945            # also: routes and scenarios
x86sim addmedia photo.jpg clip.mov
x86sim log stream --app com.example.myapp
x86sim logs com.example.myapp                 # its os_log and last crash report, symbolicated

x86sim ui appearance dark                     # also: content_size, increase_contrast, reduce_motion, ...
x86sim status_bar override --time 9:41 --batteryLevel 100 --batteryState charged --cellularBars 4 --wifiBars 3
x86sim pbcopy "text"                          # pbpaste; --host: the Linux clipboard; pbsync keeps both the same
x86sim locale --language fr-CA --locale fr_CA # also: timezone Europe/Paris, keychain reset
x86sim fidelity fast                          # 3x iPhone at 2x + reduced effects (high: native)
x86sim erase work                             # back to new; clone work work2 copies it (device off)

x86sim tap 200 400                            # also: swipe, pinch, type, key, home, lock, crown
x86sim --phone work snapshot                  # the screen as text (see "Agents" below); --phone on any command
x86sim screenshot shot.png
x86sim io recordVideo --secs 10 out.mp4
```

`x86sim help` lists every command; `--phone NAME` (a name, folder or path) before a command picks the device, and
most simctl-style commands also take it as their first argument. A second tool, `x86sim-aot`
(`./x86sim-*.AppImage aot`), starts and stops devices (`x86sim-aot --device work sim start|stop|restart`, `sim status`
lists them), edits feature sets and builds the pre-translated image from a terminal.

### Device builds

`x86sim install` (and the MCP tool `app_install`, and a drop on the launcher) also takes what you would install on an
iPhone: an `.ipa`, an `.xcarchive` or a device `.app`. The arm64 code is the same either way, so the conversion is
metadata, on a copy (the input is never changed):

- every Mach-O in the bundle (app, frameworks, app extensions, a watch app) is retagged to the simulator platform
  (`LC_BUILD_VERSION`, or an old `LC_VERSION_MIN_*` rewritten as one); a fat file keeps its arm64 slice;
- each bundle's `Info.plist` names the simulator (`CFBundleSupportedPlatforms`, `DTPlatformName`, `DTSDKName`);
- the app is re-signed ad hoc, each binary keeping the entitlements its signature had (rcodesign).

App Store downloads are encrypted with FairPlay (`LC_ENCRYPTION_INFO` cryptid 1) for the device they were bought
on and are refused with a message saying so; export a development, ad-hoc or enterprise build instead. Binaries
without an arm64 slice (armv7, arm64e only, x86_64) are refused too. `x86sim-devapp inspect X.ipa` shows what a
bundle holds; `tests/apps/build-ipa.sh` builds a small device `.ipa` (an embedded fat framework, entitlements) on a
Mac as a fixture.

### Tests (XCTest, XCUITest)

`x86sim test` runs test bundles on a phone as `xcodebuild test-without-building` does in Apple's simulator, and the
MCP tool `xctest_run` does the same for agents:

```sh
x86sim test qa --xctestrun Products/MyApp_iphonesimulator27.2-arm64.xctestrun   # what build-for-testing wrote
x86sim test qa --xctestrun F.xctestrun --only-testing MyAppTests/LoginTests/testBadPassword --junit junit.xml
x86sim test qa --app MyApp.app                          # unit tests hosted in the app (its PlugIns/*.xctest)
x86sim test qa --test-bundle MyLogicTests.xctest        # logic tests (no host app)
x86sim test qa --ui-test-runner MyAppUITests-Runner.app --app MyApp.app        # XCUITest
x86sim test qa --spm MyPackage                          # a Swift package's tests, built here (x86sim dev test --xctest)
```

- **Unit tests hosted in an app**: the app is installed and launched with `libXCTestBundleInject.dylib`, which loads
  the bundle into it, as Xcode does. **Logic tests** run in the `xctest` tool. **UI tests**: the runner and the app
  under test are installed and the runner's `XCUIApplication` launches, queries and taps the app through the
  runtime's `testmanagerd` (the phone's `xctest` feature, on by default, started only when a UI test runs; ~440 MB
  while it runs).
- Each test process gets the `.xctestconfiguration` Xcode would write (selection, target app, environment from the
  `.xctestrun`), without an IDE session. Results: XCTest's log as it runs (Swift Testing's too), a summary with each
  failure's `file:line` and message, `junit.xml` and `results.json` (per test: passed / failed / skipped, duration,
  failures) in `$X86SIM_SCRATCH/xctest/<phone>-<time>/`; exit status 0 when nothing failed, 1 otherwise.
- **Where the bundles come from**: `xcodebuild build-for-testing` on a Mac (copy its `Build/Products` dir, with the
  `.xctestrun`; `tests/apps/XCTestDemo/build.sh` does this over ssh), or for a Swift package `x86sim dev test --xctest`
  / `x86sim test --spm`, which builds its tests on Linux with the `x86sim dev` toolchain. Test bundles inside an Xcode
  project (an app's unit tests, UI test runners) need Xcode's build system.
- **XCTest's frameworks** (`XCTest.framework`, `XCTestCore`, `XCUIAutomation`, `libXCTestBundleInject.dylib`) are
  Xcode's, not the runtime's: `x86sim test` takes them from `sdks/<Platform>.platform` (`x86sim test fetch-platform`
  copies them from `$X86SIM_MAC`), `--platform DIR`, or the SDK `x86sim dev setup sdk --xip` installed.
- **Every platform**: `tests/apps/XCTestDemo` builds for iOS, tvOS, watchOS and visionOS (`X86SIM_PLATFORM=tvos
  tests/apps/XCTestDemo/build.sh`; `run.sh <phone>` picks the phone's). Its unit, logic and UI tests give Apple's
  results on iPhone, iPad, Apple TV, Apple Watch and Vision Pro.

`tests/apps/XCTestDemo/run.sh <phone>` runs the fixture (hosted unit tests, logic tests, UI tests, each with a
deliberate failure) and checks every result against Apple's simulator.

### Debugging (LLDB)

`x86sim debug` attaches LLDB to an app (or any process) in a running phone, as Xcode's Debug > Attach to Process:

```sh
x86sim debug qa com.example.myapp                       # attach to the running app: an interactive lldb
x86sim debug qa com.example.myapp --launch              # relaunch it stopped before dyld runs: break in main, +load
x86sim debug qa com.example.myapp --wait                # attach as soon as you start it
x86sim debug qa com.example.myapp --symbols build/MyApp.app.dSYM --batch -o 'breakpoint set -n viewDidLoad' -o continue -o bt
x86sim debug qa 1234                                    # a process by pid (x86sim pidof)
```

Breakpoints (by name, file and line, address), `step` / `next` / `finish`, `bt all`, `frame variable`, `register
read`, `memory read`, `image list` / `image lookup` (every image of the dyld cache, read from the process), and stops at
crashes (`EXC_BAD_ACCESS`, Swift traps as `EXC_BREAKPOINT`) work. Detaching removes the breakpoints and the app runs on. The MCP tools `debug_start`,
`debug_command` and `debug_stop` keep an lldb session per phone. `tests/lldb/run.sh <phone>` checks it end to end.

Which lldb: the Swift toolchain's (swift.org's, the one `x86sim dev setup` installs, or a distribution's `swift`
package) when there is one, so Swift is debugged as in Xcode: `frame variable` shows Swift structs, classes, arrays,
strings, dictionaries, optionals and enums, and `po` / `expression` evaluate Swift (`po self`, `expression point.x + 1`).
As installed it does not start on most desktops (it links the `libpython3.X` it was built with, 3.12 for Swift 6.4), so
the first `x86sim debug` sets it up, without root and without touching the system's Python (`x86sim dev setup lldb` does
the same beforehand, `x86sim dev status` shows it):

- a private CPython of that version (python-build-standalone's shared build, pinned by sha256, trimmed: about 75 MB) in
  `~/.local/share/x86sim-dev/swift-lldb/python-3.X`, used through `LD_LIBRARY_PATH` / `PYTHONHOME` for lldb only;
- `~/.local/share/x86sim-dev/swift-lldb/toolchain`, a view of the toolchain whose Swift resource dir is the Darwin one
  of the SDK `x86sim dev setup` built (the Linux one's module maps collide with the iOS SDK's, so `po` failed). It holds
  a copy of liblldb (lldb finds its resource dir from liblldb's real path): a reflink on btrfs / XFS (no space), else
  about 230 MB.

`x86sim debug` then points lldb at the phone's simulator SDK (`target.sdk-path`) and loads the app's dSYM (next to the
`.app`, or `--symbols`): Swift's type info comes from the dSYM's `__swift_ast` and the binary's reflection metadata
(`swiftc -g` makes both). Without the Swift toolchain it falls back to the host's `/usr/bin/lldb` (LLVM 17 or later) and
says so: C and Objective-C as above, Swift frames symbolized but Swift values only as raw memory.
`X86SIM_LLDB=/path/to/lldb` runs that lldb as is; `X86SIM_SWIFT_LLDB=0` skips the Swift one.

How: the app's arm64 code runs translated by libarm64jit, so no debugserver can see it. libarm64jit has a GDB remote
stub (`linux/arm64jit/gdbstub.cpp`) that speaks debugserver's dialect on `/run/user/<uid>/x86sim-dbg-<UDID>-<pid>.sock`.
It is off until asked for: `X86SIM_A64_DEBUG=1` (or `wait`) in the launch environment, or at run time a request file
and `SIGUSR1`. Breakpoints are substituted in the translator's code fetch, never written to the app's memory.

Limits:

- **Swift**: needs the Swift toolchain (above) and a dSYM or the build's `.swiftmodule` for `po` / `expression`;
  without them `frame variable` still works from the reflection metadata. The first Swift expression in a session
  imports the app's modules from the SDK (some seconds). See the Swift row of `docs/PARITY.md` for what is checked.
- **Functions run natively** never hit a breakpoint: `objc_msgSend` (inlined by the translator), everything in
  libsystem_kernel, libsystem_platform and libsystem_pthread (system calls, `pthread_*`, `_platform_memmove`: Darling's
  native code behind stubs), `malloc` and friends, and a few accelerated routines (LZFSE, ASTC, Swift conformance
  scans). Stepping into one steps over it; a thread blocked in one shows the stub's `ret` as its pc.
- **No watchpoints**, and no hardware breakpoints (they are software ones).
- **A stopped app is a stopped app**: SpringBoard's watchdog may kill an app stopped at launch for long (Apple's
  simulator does the same); system daemons stopped for long can stall the phone.
- **Attach at run time** uses `SIGUSR1`: an app that handles `SIGUSR1` itself can only be debugged with `--launch`.
- With `--launch` the app's `+load` methods and initializers run after the first `continue`, so breakpoints there hit;
  images loaded later (`dlopen`) are reported to lldb as they load.

### Profiling (CPU)

`x86sim profile` samples an app's threads while they run on a CPU and shows where the time goes, as Instruments' Time
Profiler does:

```sh
x86sim profile qa com.example.myapp                          # 10 s: top functions + a speedscope file
x86sim profile qa com.example.myapp --secs 5 --format gecko  # Firefox Profiler (https://profiler.firefox.com)
x86sim profile qa 1234 --format text --out top.txt           # a process by pid; the text summary as the file
x86sim profile --raw ~/scratch/x86sim/profiles/raw-....txt --format speedscope   # symbolicate raw samples again
```

It prints the busiest functions by self and total time for the process and per thread, and how the time splits
between the app's (guest) code, native library functions and the translator, then writes the profile (default:
speedscope JSON under `$X86SIM_SCRATCH/profiles`, open it at https://www.speedscope.app). The MCP tools `profile_start`
/ `profile_stop` do the same around anything you do meanwhile (`profile_stop` answers the summary and the file), and
the browser viewer's **Profile 5 s** button (control links) downloads a profile of the app in front.

How: the sampler is in the translator (`linux/arm64jit/prof.cpp`), off until asked for and started at run time through
the debugger's request channel (a request file and `SIGUSR1`); nothing runs while no profile is taken. It counts each
thread's CPU time with `perf_event_open` (user mode, allowed at the default `perf_event_paranoid` 2) and takes one
sample per millisecond of CPU time: the exact host instruction, mapped back to the guest code block it belongs to, and
the guest frame-pointer chain. Symbols come from the simulator's dyld cache (which keeps every library's local symbols)
and the app's binary; C++ and Swift names are demangled. Overhead: none while off; about 1% with one busy thread
sampled at 1 kHz (`tests/profile/run.sh <phone> [--settings]`, or `--standalone` without a phone, checks it).

Limits:

- **Timings are of the translated x86-64 code**: relative costs differ from a device (floating point, atomics and
  `objc_msgSend` cost differently than on arm64). Time the translator spends on itself shows up as `[translator]`
  (compiling, libarm64jit) and `[translator: code cache runtime]` (the dispatcher between blocks).
- **Frame-pointer stacks only**: a leaf function without a frame shows its caller through LR; code built without frame
  pointers loses callers (Apple's arm64 code keeps them).
- A library function the translator runs natively (libsystem, `pthread_*`, `memmove`, `malloc`) is one `[native] name`
  frame; time blocked in a system call is not CPU time and is not sampled (nor is kernel time).
- Threads are named by their Linux name (`pthread_setname_np`); dispatch queue labels are not shown.
- Like `x86sim debug`, an app that handles `SIGUSR1` itself cannot be profiled. No allocations / Leaks instrument.

### Device pool

For agents and CI, a pool keeps devices booted at their home screen; a caller takes one, uses it and gives it back.

```sh
x86sim pool up 3 --model "iPhone 17 Pro"      # pool-1..pool-3, booted one at a time while memory allows
phone=$(x86sim pool take --ttl 900 --hold-parent)   # a free booted device, held by this shell, at most 15 min
x86sim install "$phone" MyApp.ipa && x86sim launch "$phone" com.example.myapp
x86sim pool release "$phone"                  # reset in the background, then free again
x86sim pool status                            # who holds what, memory per device, room for more
x86sim pool down --delete
```

- **Leases** end at their TTL (`--ttl`, default 30 min; `pool renew`) or when the holder process exits (`--pid`,
  `--hold-parent`; the MCP server holds with its own process, so a session that ends frees its devices). Any pool
  command reaps them. Taking is atomic across processes (a lock around the pool's state in `var/pool/`).
- **Reset on release** (`--reset` at take or release): `apps` (default: uninstall the apps installed during the
  lease, with their data, then Home), `restart` (a fresh boot), `erase` (re-cloned from the template), `none`.
- **Fast creation**: members are clones of `<pool>-template`, a device of the pool's model booted once and stopped,
  copied with `cp --reflink=auto` (instant and sharing disk on btrfs / XFS) and given their own paths and UDID.
- **Memory**: `pool up` boots a device only while available memory stays above a reserve (3 GB, `--reserve-mb`)
  after the measured cost of one device; it stops short rather than risk the out-of-memory killer (`--force`).
- **KSM**: pool devices opt in to kernel same-page merging (`PR_SET_MEMORY_MERGE`, no root). KSM itself is off
  until root turns it on: `x86sim pool ksm` shows the two commands, `x86sim pool ksm on` runs them with sudo on a
  terminal, in the open.
- `--pool NAME` keeps several pools (one model each). The launcher's Devices page shows each pool and can release a
  device; the MCP tools are `pool_status`, `phone_take` and `phone_release`.

### Agents: reading the screen and acting on it

An agent drives a running phone through the screen as text. `snapshot` lists the accessibility elements on screen:
the app in front (UIKit and SwiftUI), web pages it shows (links, buttons, fields, headings and text from the DOM),
and SpringBoard (alerts and other system UI on top, the home screen, the status bar). Each element gets a ref, its
role, label, value, identifier, states, and the point to touch:

```
phone ram2 · 402x874 pt · front com.apple.Preferences
app com.apple.Preferences:
@e17 button "General" id=com.apple.settings.general (201,319)
@e18 button "Accessibility" id=com.apple.settings.accessibility (201,371)
@e28 searchfield "Search" (201,822)
status bar: 2:23 AM | Cellular: No signal | 100% battery power: Not charging
```

- **Refs are stable.** An element keeps its ref for as long as it is on screen, across snapshots and CLI calls,
  so an agent can act on a ref it saw a few steps back. `snapshot diff=true` shows only what changed (`+` new,
  `-` gone, `~` a label, value or state changed).
- **Actions answer with the result.** `tap`, `long_press`, `type`, `key`, `scroll`, `scroll_to`, `swipe`,
  `press`, `open_url`, `app_launch` and `app_terminate` wait until the screen has settled (no new frames and no
  transition still running) and answer with the next snapshot, or only the changes (`after: "diff"`), or nothing
  (`after: "none"`).
- **Targets**: a ref (`@e3`), an accessibility identifier (`id:com.apple.settings.general`), label text (exact,
  then prefix, then substring, with an alert over the app before the app beneath it), or `x`/`y` in points. An
  element the app has loaded but scrolled out of view is scrolled into view before it is tapped.
- **Typing** inserts text into the focused field as the software keyboard does, so any characters work (`clear`
  empties the field first, `submit` presses return). `keys=true` presses hardware keys instead.
- **`wait_for`** returns when a target appears (or, with `gone`, disappears). It checks again on every new
  frame rather than polling on a timer.
- **`screenshot`** returns the screen as an image, one pixel per point by default, so its coordinates match the
  snapshot's.

Each of these MCP tools takes `phone` (a name, a folder or a path). The same commands are on the command line:

```sh
x86sim --phone ram2 snapshot [--diff] [--all] [--json]
x86sim --phone ram2 tap @e17                 # or: tap "General", tap id:com.apple.settings.general
x86sim --phone ram2 type "hello@example.com" --submit
x86sim --phone ram2 scroll-to "Privacy & Security"
x86sim --phone ram2 wait-for "Wi-Fi" --timeout 10
x86sim --phone ram2 screenshot shot.png 3    # device pixels divided by 3: points on a 3x phone
```

On the command line, `tap X Y` and `swipe X1 Y1 X2 Y2` with plain numbers are device pixels, as before; targets
(refs, `id:`, text) are the way to use a snapshot's points. `find TARGET` shows what a target would match without
acting. [skills/x86sim/SKILL.md](skills/x86sim/SKILL.md) puts it together for coding agents.

Snapshots come from host/simax, a reader loaded into every process that has UIKit
([docs/protocols/SIMAX.md](docs/protocols/SIMAX.md)). On an iPhone 17, a snapshot that has settled takes about 10 ms in Settings, about 15 ms on the
home screen, and 25-35 ms for a long web page (about 130 elements).

Per platform:

- **iPad**: windowed apps (Windowed Apps, Stage Manager) are reported where they are on the display; `front` lists
  the top window's app first, SpringBoard's window controls and the windows behind it are an "app windows" group,
  and an element under another app's window is marked `[covered]`. Landscape, the software keyboard's keys,
  `key cmd+a`-style shortcuts and `pointer` (the iPad's mouse) work as on a device.
- **Apple TV** has no touchscreen: the snapshot names what has the focus (`focus: @e13 button "General"`), and `tap`
  moves the focus onto its target with the remote's arrows, checking after each press, then presses select, as
  XCUIRemote does; `scroll` and `swipe` move the focus, `press menu` goes back, `press tv` home. `type` writes into
  tvOS's full-screen keyboard.
- **Apple Watch**: the watch face and the app grid are the home screen; a text field's input sheet (keyboard,
  Scribble, dictation) is listed and `type` writes into it; `crown` scrolls.
- **Apple Vision Pro**: windows float in a rendered room, so a snapshot's points are each app's own window points,
  not display positions. `tap` acts through accessibility (the element's activate, else a tap made inside the app at
  its activation point, as VoiceOver does); `scroll` / `swipe` move the front app's scroll view; `press home` (the
  Digital Crown) opens the Home View. `pointer` drives the system pointer by display position.

### Accessibility Inspector

What Xcode's Accessibility Inspector does, for any app on a phone:

- **Inspection.** In the browser viewer (`x86sim serve`) and the launcher's phone page, *Inspect* outlines the element
  under the mouse; a click shows its properties: label, value, hint, traits, identifier, frame, activation point,
  actions and custom actions, custom rotors, input labels, language, whether it responds to user interaction, its
  class, view and view controller, the accessibility hierarchy above it, and its text's font, color and whether it is
  cut off. `x86sim ax-inspect [TARGET | X Y]` (MCP `ax_inspect`) prints the same.
- **Audit.** `x86sim ax-audit [PHONE]` (MCP `ax_audit`, the viewer's and the launcher's *Run audit*) checks the app in
  front: controls and images without a label, labels that are file names, SF Symbol names or identifiers, touch
  targets under 44 x 44 pt (60 on Vision Pro), text contrast under WCAG 4.5:1 (3:1 for large text: the app's text
  color against the background sampled from the screen), text cut off, Dynamic Type (`--dynamic-type`: the screen
  again at the largest accessibility text size, which is set for the check and put back), duplicate labels, controls
  and tap targets VoiceOver cannot reach, and views that take taps without the button trait. Each finding has a
  severity, the element's ref and frame and a fix; `--json` for tools.
- **Actions.** `x86sim ax-action NAME [TARGET]` (MCP `ax_action`, the inspector's buttons) does what VoiceOver does:
  activate, increment, decrement, scroll (`--dir`), escape, magic tap, and custom actions by name. They need no
  point on the screen, which helps on Apple TV and Vision Pro.

```sh
x86sim ax-audit qa                       # the app in front on phone qa
x86sim --phone qa ax-audit --dynamic-type --json
x86sim --phone qa ax-inspect "Message from Anna"
x86sim --phone qa ax-action custom "Message from Anna" --name Reply
```

`tests/apps/x86a11ybad` is a screen that breaks each rule on purpose. The data comes from simax's full tree
([docs/protocols/SIMAX.md](docs/protocols/SIMAX.md)). Font, color and truncation are read from UIKit labels; for
SwiftUI text the contrast comes from the pixels alone, and clipping is not seen.

### MCP server

`x86sim-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server over stdio for AI agents and
other MCP clients.

```sh
claude mcp add x86sim -- /path/to/x86sim-*.AppImage mcp    # AppImage
claude mcp add x86sim -- /path/to/x86sim-mcp               # built from source (tools/launcher/target/release)
```

Tools that act on a device take `phone` (a name, a folder or a path); without it they use the only running device,
else the configured one. Erase, clone and keychain reset need it.

| Group | Tools |
|---|---|
| Devices | `phones_list` (`sim_status` is the same), `sim_devices`, `sim_add_device`, `sim_start`, `sim_stop`, `sim_restart`, `runtime_delete`, `aot_status` |
| Feature sets | `features_list`, `feature_get`, `feature_set`, `features_preset` |
| Apps | `app_install` (simulator or device builds), `app_uninstall`, `app_launch`, `app_terminate`, `app_list`, `app_logs` |
| Tests | `xctest_run` (XCTest / XCUITest: an `.xctestrun` or bundles; per-test results, see [Tests](#tests-xctest-xcuitest)) |
| Debugging | `debug_start`, `debug_command`, `debug_stop` (an lldb session on an app, see [Debugging (LLDB)](#debugging-lldb)) |
| Profiling | `profile_start`, `profile_stop` (CPU profile of an app, see [Profiling (CPU)](#profiling-cpu)) |
| Screen as text and actions | `snapshot`, `tap`, `long_press`, `type`, `key`, `scroll`, `scroll_to`, `swipe`, `wait_for`, `press`, `open_url`, `screenshot` |
| Accessibility Inspector | `ax_inspect`, `ax_audit`, `ax_action` |
| Hardware | `sim_button`, `sim_crown`, `sim_fold`, `sim_shake`, `sim_orientation`, `sim_biometric`, `sim_gamepad`, `sim_pointer`, `sim_system`, `sim_keyboard`, `sim_memory_warning` |
| Settings (as simctl) | `sim_ui`, `sim_status_bar`, `sim_pasteboard`, `sim_keychain_reset`, `sim_locale`, `sim_timezone`, `sim_fidelity`, `sim_erase`, `sim_clone` |
| Capture | `sim_screenshot` (a PNG file), `sim_record_video` |
| Pool | `pool_status`, `phone_take`, `phone_release` |
| Sharing | `share_start`, `share_stop`, `share_status`, `share_link` (they return the URLs) |
| Web content | `web_targets`, `web_eval`, `web_console`, `web_dom`, `web_network` (Safari's pages and inspectable WKWebViews, see [Web Inspector](#web-inspector)) |
| Develop | `dev_status`, `dev_setup`, `dev_setup_cancel`, `dev_new`, `dev_projects`, `dev_build`, `dev_run`, `dev_test`, `dev_device`, `dev_ship`, `dev_convert` |

### In a browser (local or remote)

`x86sim serve` puts every device in a browser tab: a dashboard with live thumbnails (start, stop, share), and a
device's live screen with its controls. The screen streams as H.264 decoded by WebCodecs (JPEG where the browser
lacks it, e.g. plain HTTP on the LAN); quality, size and frame rate are picked on the page.

- **Input**: click or drag to touch (edge swipes become Home, Control Center and Notification Center as on a
  device), real multi-touch from a touchscreen, Alt + drag to pinch or rotate, Shift + drag for a two-finger pan, the
  wheel scrolls (the Digital Crown on a watch, the remote's swipes on a TV), the hardware keyboard (Ctrl / Cmd is
  Command), Ctrl+V types the clipboard into the device, F1 to F4 Home, Lock and volume, Keyboard > Software / Hardware
  (Toggle Software Keyboard, Connect Hardware Keyboard).
- **Controls per platform**: iPhone / iPad buttons (Home, Lock, volume, Siri, Action, App Switcher), rotate, shake,
  the iPhone Duo's fold poses; an Apple TV's Siri Remote; an Apple Watch's crown, side button and hand gestures; a
  Vision Pro's crown, and the mouse as its system pointer (hover, click, drag; the wheel scrolls where it points).
- **Sound**: Sound on plays the device's audio in the page (Opus decoded by WebCodecs, 16-bit PCM where the
  browser lacks it), ~70 ms from the server to the speaker (a tap on a button that beeps is heard ~115 ms later),
  with a volume per viewer; browsers start audio only from a click, so it is off until then. Mic on (control links, HTTPS or this machine) sends the browser's microphone to the
  device's input instead of the host's.
- **On a phone's browser**: the screen fills the window, with Home, App Switcher, Type… and Sound under it and the
  other controls below; fingers touch, two fingers pinch, rotate and pan.
- **Capture**: a full-size PNG screenshot, and recording the stream to a video file.
- **Links**: a control link (watch and drive) and a view-only link; more links with their own expiry, limited to
  one device or not, from the Share dialog (with a QR code) or `x86sim serve link`; revoke them there.

```sh
x86sim serve --open                 # http://127.0.0.1:7380, opened in the browser
x86sim serve --bg --tailscale       # also https://<machine>.<tailnet>.ts.net, tailnet only (tailscale serve)
x86sim serve --bg --funnel          # the same address, public (tailscale funnel)
x86sim serve --bg --cloudflared     # a public https://*.trycloudflare.com quick tunnel
x86sim serve --cloudflared --cf-tunnel NAME --cf-host phones.example.com   # a named tunnel you set up
x86sim serve --listen 0.0.0.0:7380  # the LAN, plain HTTP
x86sim serve status | stop | stop cloudflared
x86sim serve link --phone work --ttl 2h          # a view-only link; --control to drive
```

It listens on loopback unless told otherwise and has no TLS of its own: remote access goes through Tailscale or
cloudflared, which bring HTTPS. Only a browser on this machine gets in without a token; everything else, tunnels
included, needs the token in the link. `--ttl` makes the printed links expire. In the launcher, the globe button
beside a device's screen opens it in the browser, and its Share section turns tunnels on and off, copies links and
stops sharing. Tailscale must be logged in, with Serve (and Funnel) enabled for the tailnet; `x86sim serve` shows
the admin link when they are not.

### Web Inspector

Safari's pages and apps' web views can be inspected as from Safari's Develop menu on a Mac: WebKit's own Web
Inspector (Elements, Console, Network, Sources, Timelines, Storage, ...) opens in your browser.

```sh
x86sim inspect                      # the running phone's pages, and the inspector page in the browser
x86sim inspect qa --url example.com # straight to the inspector on the first page whose URL or title has it
x86sim inspect qa --list            # only list them (id, app, kind, title, URL)
```

The viewer of `x86sim serve` has the same under **Web pages…** (control links only: an inspector runs code in the
page). Agents get MCP tools that need no browser: `web_targets` (pages per app), `web_eval` (JavaScript, its value;
promises are awaited), `web_console` (the page's messages, then new ones for `follow_s`), `web_dom` (outerHTML of a
CSS selector or the document) and `web_network` (every request since the page loaded, then live ones). WebKit keeps
a page's console messages only from its first inspection on; `reload: true` reloads the page and collects its load's
console messages or requests (with status codes).

- Safari's pages are always inspectable. An app's `WKWebView` (or `JSContext`) is listed only when the app sets
  `isInspectable = true`, as in Apple's simulator.
- The phone side is the runtime's `webinspectord`, started on demand at the first inspect (the `webinspector`
  feature, on by default; nothing runs until then). Its socket is `/run/user/<uid>/x86sim-rwi-<UDID>.sock` and speaks
  Apple's Remote Web Inspector protocol (`tools/launcher/core/src/rwi.rs`).
- The frontend is WebKit's own (WebInspectorUI, as built in WebKitGTK 2.54.3). The AppImage carries it, so nothing
  needs installing. Lookup order: `X86SIM_INSPECTOR_UI` (a WebInspectorUI directory, e.g. a WebKit checkout's),
  the AppImage's copy, a checkout's `tools/launcher/target/webinspector` (made by `build/webinspector.sh`: a pinned
  WebKitGTK package, sha256 checked), then the host's WebKitGTK (webkit2gtk-4.1 or webkitgtk-6.0, extracted once into
  `~/.cache/x86sim/webinspector`). `x86sim serve`'s log names the copy in use. It is a newer or older WebKit than the
  phone's, so a panel may miss a command now and then. The MCP tools work without it.

## Building and shipping apps

x86Sim runs and tests apps. The other half of iOS development without a Mac, building, signing, installing on a
real iPhone and publishing, is [omarchy-apple-dev](https://github.com/joshuaswarren/omarchy-apple-dev). x86Sim
carries our fork of it ([x86sim/omarchy-apple-dev](https://github.com/x86sim/omarchy-apple-dev)) in
`external/omarchy-apple-dev` and in the AppImage, and wraps it as `x86sim dev`, the launcher's **Develop** page and
the MCP server's `dev_*` tools:

```sh
x86sim dev status                     # Swift, xtool, the iOS SDK, device tools: what is ready, what to do next
x86sim dev setup                      # install what is missing (no root): Swift from swift.org if there is none,
                                      # xtool, rcodesign, ipsw, pymobiledevice3, then a test build
x86sim dev setup sdk --xip ~/Downloads/Xcode_27.xip   # the iOS SDK, from Apple's Xcode download (free Apple ID)
x86sim dev new HelloApp               # a SwiftUI app (xtool new), set up for iOS, tvOS, watchOS and visionOS
x86sim dev run HelloApp --phone QA    # build for that device's platform, install and launch: the edit-run loop
x86sim dev test HelloApp              # the same, then check it still runs and take a screenshot
x86sim dev device | ship | ship-mac | convert | auth   # real iPhones, App Store / TestFlight, Xcode projects
```

- **Build** Swift, SwiftUI, Objective-C, C and C++ apps with Swift and xtool, along with Flutter and Kotlin
  Multiplatform, as simulator builds for x86Sim (`ship.sh --simulator`) or device builds for an iPhone. Linux versions
  of `actool`, `ibtool`, `momc` and `xcstringstool` are included, and Xcode projects can be converted.
- **Run on a device**: `device-run.sh` (`x86sim dev device`) pairs an iPhone, installs and launches the app, attaches
  LLDB, streams its console and symbolicates crash reports.
- **Publish**: `ship.sh` (`x86sim dev ship`) builds a release `.ipa`, creates certificates and profiles, signs it and
  uploads it to App Store Connect and TestFlight. `ship-mac.sh` does the same for macOS apps, with Developer ID signing
  and optional notarization.

A free Apple ID is enough to download Xcode for the SDK and to install on your own device; TestFlight and the App Store
need a paid developer account. The toolchain installs into your home directory (`x86sim dev setup` passes the
installer `--user-only` and never runs sudo; when xtool's build needs system packages, `dev status` prints the
command for your distribution). `x86sim repos upstream-sync external/omarchy-apple-dev` merges upstream into the fork.

## How it works

- **[Darling](https://github.com/darlinghq/darling)** provides the macOS-compatible system layer: the Mach-O loader, XNU system
  call and Mach IPC emulation, and one container per device. x86Sim's fork extends it for what the simulator
  runtimes need.
- **[dynarmic](https://github.com/azahar-emu/dynarmic)** translates the runtime's arm64 code to x86-64, just in time
  or ahead of time into a pre-translated image. The lowest system libraries stay native x86-64 code, so system calls
  are not translated.
- **[metal2vulkan](https://github.com/steelbrain/metal2vulkan)** runs Metal on Vulkan. The runtime's own Metal
  implementation talks to a native Linux process, `mtlvk`, with buffers and textures shared rather than copied.
- **Stand-ins for the Mac host.** Everything CoreSimulator and macOS would provide around a runtime is replaced by
  small programs: `simfb` for the display, `simhid` for input, and others for cross-process surfaces, DNS, network
  state and host file services.

[ARCHITECTURE.md](docs/ARCHITECTURE.md) describes each part, [METAL.md](docs/METAL.md) the graphics path, and
[docs/protocols](docs/protocols) the wire formats.

## Building from source

```sh
git clone --recursive https://github.com/x86sim/x86sim.git
cd x86sim
cp x86sim.env.example x86sim.env

make darling-install                             # Darling; asks for sudo (bin/darling is setuid root)
make mtlvk                                       # Metal on Vulkan
linux/arm64jit/install.sh                        # the arm64 translator, installed into Darling
(cd tools/launcher && cargo build --release)     # launcher, x86sim-aot, x86sim-mcp
```

[DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the build dependencies, fetching runtimes, the repository layout and
day-to-day use. The simulator-side binaries are committed, so no Mac is needed to build or run. Release AppImages
are built by `build/release/` and GitHub Actions; see [RELEASE.md](docs/RELEASE.md).

## Status and limitations

x86Sim is young software. The runtimes boot to their home screens, stock and third-party apps run, and the tools
above work. These are the known gaps:

- **Apps** need arm64 code: simulator builds, or device builds that are converted on install. FairPlay-encrypted App
  Store downloads do not run.
- **Debugging**: LLDB attaches (`x86sim debug`) but has no Swift expression support and no watchpoints (see
  [Debugging (LLDB)](#debugging-lldb)); of Instruments only a CPU profiler (`x86sim profile`, no allocations or
  Leaks; see [Profiling (CPU)](#profiling-cpu)). XCTest and
  XCUITest bundles run (`x86sim test`), but an app's own test targets are built on a Mac (`xcodebuild
  build-for-testing`); only Swift package tests build on Linux.
- **Maps** shows no map tiles: the map data comes from a Mac and is not part of a release.
- **Audio** is not played on the host unless `X86SIM_AUDIO=1` sends it to PipeWire (which also records the host's
  microphone); the browser view plays it either way. Apps always see audio devices.
- **Sensors and accessories**: no accelerometer, Apple Pencil, external displays or CarPlay.
- **Pairing** an Apple Watch with an iPhone is not supported.
- **Networking**: ordinary DNS and sockets work; Bonjour works between apps on one device but not with the network
  (no multicast DNS on the host).
- **Web media**: in Safari and web views only the first `<video>` / `<audio>` load of a session plays (missing POSIX
  AIO in Darling); see the web platform report, [tests/web/webcov/COVERAGE.md](tests/web/webcov/COVERAGE.md).
- **Sandboxing**: simulator processes are not sandboxed.
- **System logging**: `logd` runs for installed apps only; the runtime's own processes do not log. `x86sim log stream`
  covers one app or process at a time.

[PARITY.md](docs/PARITY.md) compares every area with Apple's Simulator and says what was verified.
[WORKAROUNDS.md](docs/WORKAROUNDS.md) lists everything that is approximated and what a faithful version would need.

## License and credits

x86Sim is free software, licensed under the [GNU General Public License v3.0 or later](LICENSE). Bundled components
keep their own licenses; see [NOTICE](NOTICE).

x86Sim builds on:

- [Darling](https://github.com/darlinghq/darling), used through the [x86sim fork](https://github.com/x86sim/darling)
- [dynarmic](https://github.com/azahar-emu/dynarmic), used through the [x86sim fork](https://github.com/x86sim/dynarmic)
- [metal2vulkan](https://github.com/steelbrain/metal2vulkan), used through the [x86sim fork](https://github.com/x86sim/metal2vulkan)
- [ipsw](https://github.com/blacktop/ipsw), [7-Zip](https://www.7-zip.org),
  [dnspython](https://github.com/rthalley/dnspython), astcenc and lzfse

Each fork carries its changes on a branch named `x86sim`.

Thanks to [agent-sim](https://github.com/Developing-Gamer/agent-sim) for ideas on driving phones from AI agents.

Apple's simulator runtimes, SDKs and Xcode are not included and are never redistributed. Apple, iPhone, iPad, iOS,
Xcode and Metal are trademarks of Apple Inc. x86Sim is not affiliated with or endorsed by Apple.
