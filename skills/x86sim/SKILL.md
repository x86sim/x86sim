---
name: x86sim
description: Drive x86Sim's simulated iPhones, iPads, Apple TVs, Apple Watches and Vision Pros on Linux from a shell - start or take a phone, install a .app / .ipa, launch it, read the screen as text, tap and type by element, wait for results, take screenshots, read logs and crash reports, run the build-install-launch loop, share a phone with a human in a browser. Use when testing, automating or debugging an iOS / iPadOS / tvOS / watchOS / visionOS app on x86Sim, or when asked to look at or operate a simulated Apple device on this machine.
---

# Driving x86Sim phones

x86Sim runs Apple's simulator runtimes on x86-64 Linux. Every device is called a *phone* (also iPads, TVs, watches,
Vision Pros) and is named by its name (`ram2`), folder (`device-a64-ram2`) or path.

**Commands.** In a checkout: `cli/x86sim` and `tools/launcher/target/release/x86sim-aot`. From the AppImage:
`./x86sim-*.AppImage x86sim ...` and `./x86sim-*.AppImage aot ...`. Below, `x86sim` and `x86sim-aot` stand for those.
`x86sim help`, `x86sim pool help`, `x86sim dev help`, `x86sim serve --help` print the exact flags.

**Always name the phone** (`x86sim --phone NAME <command>`). Without it the CLI uses `X86SIM_DEVICE` from
`x86sim.env`, and the MCP tools use the only running phone or the configured one: not necessarily yours, and other
agents or the user may be using other phones.

## 1. Get a phone

```sh
x86sim-aot sim status                         # every phone: model, OS, off/starting/running, MB, CPU, "home"
x86sim device models                          # models; then: x86sim device create --model "iPhone 17 Pro" qa
x86sim-aot --device qa sim start              # boots in the background (stop / restart likewise)
x86sim --phone qa wait home 300               # returns once the home screen is drawn
```

Or take a booted one from a pool (someone ran `x86sim pool up N`):

```sh
phone=$(x86sim pool take --ttl 1800)          # atomic; prints the name; reset + freed at release or TTL
...                                           # x86sim pool renew "$phone" --ttl 1800 for long jobs
x86sim pool release "$phone"                  # uninstalls what you installed (default --reset apps)
```

`--hold-parent` ties the lease to the calling shell. If each of your commands runs in a fresh shell, that shell exits
at once and the lease ends with it: use `--ttl` (or `--pid` of a process that lives as long as your job).

## 2. Install and launch

```sh
x86sim install qa build/MyApp.app             # simulator build; .ipa / .xcarchive / device .app are converted
x86sim launch qa com.example.myapp -flag value   # --console: stream stdout/stderr; --env K=V; -w waits for a debugger
x86sim terminate qa com.example.myapp
x86sim listapps qa --json
x86sim get_app_container qa com.example.myapp data   # a Linux path: read the app's files directly
x86sim privacy qa grant photos com.example.myapp     # skip the permission prompt (as simctl privacy)
```

FairPlay-encrypted App Store downloads and binaries without arm64 are refused; `x86sim-devapp inspect X.ipa` says why.

## 3. The loop: snapshot, act, read the answer

```sh
x86sim --phone qa snapshot
```
```
phone qa · 402x874 pt · front com.apple.Preferences
app com.apple.Preferences:
@e17 button "General" id=com.apple.settings.general (201,319)
@e28 searchfield "Search" (201,822)
status bar: 2:23 AM | ...
```

One line per accessibility element: ref, role, label, `value=`, `id=`, states, centre point in points. It covers the
app in front (UIKit, SwiftUI), web content (DOM), SpringBoard alerts, the home screen and the status bar.

```sh
x86sim --phone qa tap @e17                    # by ref
x86sim --phone qa tap "General"               # by label: exact, then prefix, then substring; alerts win
x86sim --phone qa tap id:com.apple.settings.general
x86sim --phone qa find "Wi"                   # what a target would match, best first (no action)
x86sim --phone qa type "me@example.com" --submit      # into the focused field; --clear empties it first
x86sim --phone qa key cmd+a                   # return tab escape delete up down ... or combos
x86sim --phone qa scroll down 1 --at @e5      # precise, in screens; scroll-to TARGET scrolls until it shows
x86sim --phone qa swipe back                  # up/down/left/right, home, notifications, controlcenter, back
x86sim --phone qa long-press @e9 --ms 800
x86sim --phone qa press home                  # hardware button; open URL opens a URL / deep link
x86sim --phone qa wait-for "Done" --timeout 20        # --gone: until it disappears; wakes on new frames
```

- **Every action answers with the next screen**, after it has settled (no new frames, no transition running). Read
  that answer instead of taking a new snapshot. `--after diff` gives only the changes (`+` new, `-` gone, `~`
  changed), `--after none` nothing. `snapshot --diff` diffs against the last snapshot.
- **Refs are stable**: an element keeps its ref while it stays on screen, across snapshots and separate commands, so
  a ref seen a few steps back still works. They follow the row in a scrolled table or list (cells are recycled) and
  the DOM node on a web page (repeated text such as "by" or "|" on every row keeps its own ref after a scroll). They
  reset when the phone reboots.
- Web pages are read in every frame, iframes of other sites too (`frame_url` in `--json`).
- Points follow the interface: a portrait-only app on a phone held sideways keeps portrait coordinates, landscape
  apps get landscape ones (the first line says `· landscape`). An iPhone Duo's snapshot names the screen in use
  (`· inner screen` unfolded, `· outer screen`); taps go to that screen.
- A process that does not answer in time is named in a `busy (no answer): <bundle> (pid N: ...)` line.
- An element the app has loaded but scrolled out of view is scrolled in before a tap. `snapshot --all` lists those.
- Prefer `id:` targets for your own app (set `accessibilityIdentifier`), labels for system UI.
- `type` inserts text as the software keyboard does (any characters, no layout issues). `--keys` sends hardware key
  presses instead (US layout), for games and key handlers. Tap the field first (or it types into the focused one).

### Accessibility checks

```sh
x86sim --phone qa ax-audit                 # the app in front: findings with ref, frame, severity, fix
x86sim --phone qa ax-audit --dynamic-type  # also at the largest text size (set for the check, then put back)
x86sim --phone qa ax-inspect @e7           # every accessibility property of one element
x86sim --phone qa ax-action increment @e7  # activate | increment | decrement | scroll --dir up | escape | magic_tap
x86sim --phone qa ax-action custom @e9 --name Archive
```

Rules: `missing-label`, `label-not-readable` (file / SF Symbol / identifier names), `hit-region` (< 44 pt),
`contrast` (WCAG 4.5:1, 3:1 large text), `text-clipped`, `dynamic-type`, `duplicate-label`, `hidden-interactive`
(VoiceOver cannot reach it: no ref, use its frame), `trait-mismatch`. MCP: `ax_audit` (JSON in structuredContent),
`ax_inspect`, `ax_action`. Fix, rebuild, re-run the audit until it is clean.

## 4. Screenshots: only when you need to see pixels

A snapshot is a few hundred bytes and 10-35 ms; an image costs far more context. Use screenshots to check layout,
colours, images, rendering, or when the snapshot shows too little (games, Metal views, canvas content).

```sh
x86sim --phone qa screenshot out.png 3        # newest frame; the number divides the size (3 on a 3x phone = points)
x86sim --phone qa io recordVideo --secs 10 out.mp4
```

Without a divisor the PNG is full device pixels, while snapshot coordinates are points. The MCP `screenshot` tool
returns points by default.

## 5. Clean, repeatable screenshots

```sh
x86sim --phone qa ui appearance dark          # content_size large|extra-large|accessibility-medium...,
                                              # increase_contrast / reduce_transparency / reduce_motion enabled|disabled
x86sim --phone qa status_bar override --time 9:41 --batteryLevel 100 --batteryState charged --cellularBars 4 --wifiBars 3
x86sim --phone qa locale --language fr-CA --locale fr_CA   # apps pick it up at their next start
x86sim --phone qa timezone Europe/Paris       # next boot
x86sim --phone qa pbcopy "text"               # pbpaste; --host uses the Linux clipboard
x86sim --phone qa location set 48.8584,2.2945
x86sim --phone qa push com.example.myapp payload.json
```

`x86sim fidelity fast|high` trades looks for speed (fast: a 3x iPhone drawn at 2x plus reduced effects; next boot).

## 6. Logs and crashes

```sh
x86sim --phone qa logs com.example.myapp      # its os_log + the last crash report, symbolicated
x86sim --phone qa logs com.example.myapp --relaunch   # restart it with os_log recorded and follow
x86sim --phone qa log stream --app com.example.myapp --match ERROR
x86sim --phone qa launch --console com.example.myapp  # stdout / stderr here
x86sim --phone qa logs                        # the phone's service logs; logs LABEL tails one
```

## 6b. Debug with LLDB

```sh
x86sim debug qa com.example.myapp --batch -o 'bt all' -o detach          # attach, look, let it run on
x86sim debug qa com.example.myapp --launch --symbols MyApp.app.dSYM --batch \
    -o 'breakpoint set -n viewDidLoad' -o continue -o bt -o 'frame variable' -o detach
```

MCP: `debug_start {app, launch?, symbols?}`, then `debug_command {command}` (any lldb command; `continue` / `next` wait
up to `timeout_s` for the next stop), `debug_stop`. C, Objective-C and Swift work: with the Swift toolchain's lldb
(set up on the first debug, or `x86sim dev setup lldb`; `debug_start` answers `"swift": true`) `frame variable`, `po`
and `expression` show Swift values (pass the app's dSYM for `po` / `expression`). Without it, /usr/bin/lldb: Swift
values only as raw memory. `objc_msgSend` and libsystem functions run natively: breakpoints
there never hit. Always `detach` / `debug_stop`: a stopped app stays stopped.

## 6c. Profile CPU use

```sh
x86sim profile qa com.example.myapp --secs 5 --format text      # top functions by self / total time, per thread
```

MCP: `profile_start {app}`, do the slow thing (scroll, tap...), then `profile_stop {format?}`: the summary and a
speedscope / gecko file. Samples count CPU time only (an idle app gives no samples: make it busy while profiling).
Times are of translated x86-64 code: trust the ranking, not absolute numbers. `[translator...]` frames are the
translator's own time, `[native] name` a library function run natively.

## 7. Build, run, repeat (omarchy-apple-dev)

```sh
x86sim dev status                             # Swift, xtool, iOS SDK: what is ready, what to run next
x86sim dev new HelloApp                       # SwiftUI app for iOS / tvOS / watchOS / visionOS
x86sim dev run HelloApp --phone qa --start    # build for the phone's platform, install, launch (--start boots it)
x86sim dev test HelloApp --phone qa --shot out.png    # run, wait, fail if it died, screenshot
```

### Unit and UI tests (XCTest, XCUITest)

```sh
x86sim test qa --xctestrun Products/MyApp_iphonesimulator27.2-arm64.xctestrun   # from xcodebuild build-for-testing
x86sim test qa --xctestrun F.xctestrun --only-testing MyAppTests/LoginTests/testBadPassword   # Target/Class/test
x86sim test qa --app MyApp.app                # unit tests in MyApp.app/PlugIns; --test-bundle B.xctest: logic tests
x86sim test qa --ui-test-runner MyAppUITests-Runner.app --app MyApp.app
x86sim dev test --xctest MyPackage --phone qa # a Swift package's tests, built on Linux, then run
```

Prints XCTest's log, then the failing tests with `file:line` and `** TEST SUCCEEDED / FAILED **`; exit 0 / 1.
`results.json` (per test: result, duration, failures) and `junit.xml` go to the dir it names (`--out`, `--json -`
for JSON only). MCP: `xctest_run`. App test targets and UI test runners come from a Mac's `xcodebuild
build-for-testing` (copy the whole `Build/Products` dir with the `.xctestrun`). XCTest's frameworks come from Xcode:
if it says "no XCTest frameworks", run `x86sim test fetch-platform` (needs `X86SIM_MAC`) or `x86sim dev setup sdk
--xip`. UI tests need the phone's `xctest` feature (on by default; `x86sim-aot --device qa features set xctest on`
and a restart if someone turned it off). A run takes seconds for unit tests, ~5-10 s per UI test.

## 8. Show a phone to a human

```sh
x86sim serve --bg                             # http://127.0.0.1:7380: every phone in a browser
x86sim serve --bg --tailscale                 # HTTPS on the tailnet; --funnel / --cloudflared: public
x86sim serve link --phone qa --control --ttl 1h       # a link to give out (without --control: watch only)
x86sim serve status | x86sim serve stop
```

The page plays the phone's sound (Sound on) and can send the viewer's microphone (Mic on). Links off this machine
carry a token; `--ttl` makes them expire. Public tunnels expose a phone to anyone with the
link: use them only when asked.

## 9. Web pages (Safari, WKWebView)

```sh
x86sim inspect qa --list                      # inspectable pages: id, app, title, URL
x86sim inspect qa --url example.com           # WebKit's Web Inspector on that page, in the browser
```

MCP: `web_targets`, `web_eval` (`expression`; a promise is awaited), `web_console` (`follow_s`), `web_dom`
(`selector`), `web_network` (`follow_s`); `target` is a page id from `web_targets`, text in its URL or title, or the
app. Prefer them to screenshots for web content: the DOM and console say what a page did. Messages a page logged
before it was first inspected are not kept (WebKit): `web_console` / `web_network` with `reload: true` reload it and
catch its load. An app's WKWebView shows
up only if the app sets `isInspectable = true` (as on Apple's simulator); Safari's pages always do.

## Gotchas

- **Destructive commands need an explicit phone, which must be off**: `x86sim erase qa`, `x86sim clone qa qa2`
  (positional name; `--phone` is not enough). `keychain reset` deletes every item. Never erase a phone you did not
  create or take.
- **Coordinates**: `x86sim tap X Y` and `swipe X1 Y1 X2 Y2` with plain numbers are *device pixels* (the old input
  path). Snapshot points go through refs, text or `id:`; the MCP `tap` takes `x`/`y` in points.
- The **first snapshot** of a running process turns on accessibility automation (as XCUITest does) and loads UIKit's
  accessibility bundles: it is 0.1-0.2 s slower, and the setting stays on in that phone.
- An empty or thin snapshot means the screen has no accessibility elements (a game, a Metal view, a custom-drawn
  control): fall back to a screenshot and `tap X Y` in device pixels.
- **Per platform** (snapshot and actions checked on iPhone, iPad, Apple TV, Apple Watch and Vision Pro):
  - *iPad*: windowed apps are placed where they show; `front` lists the top window's app first, SpringBoard's window
    chrome (window controls, resize grabbers, a card per window behind) is the "app windows" group, and an element
    under another app's window is marked `[covered]` (tap refuses it: bring its app forward first). `pointer` /
    MCP `sim_pointer` drive the iPad's mouse; `key cmd+a` etc. are hardware-keyboard shortcuts.
  - *Apple TV*: no touches; the snapshot ends with `focus: @e13 ...`. `tap TARGET` moves the focus there with the
    remote's arrows and presses select (`long-press`: select held); `scroll` / `swipe` move the focus; `press menu`
    is back, `press tv` home. `type` writes into tvOS's full-screen keyboard; leave it with `tap Done`.
  - *Apple Watch*: touches, `crown DELTA` (MCP `sim_crown`, ~120 a row) to scroll; `press home` is the crown press
    (face <-> app grid). A text field opens watchOS's input sheet, listed in the snapshot; `type` then `tap Done`.
  - *Vision Pro*: (x,y) are points in each app's own window, not display positions: `tap TARGET` acts through
    accessibility (activate, else a tap made inside the app), `scroll` moves the front app's scroll view, `press
    home` (Digital Crown) toggles the Home View. A point cannot be tapped; `pointer at NX NY` + `pointer click` drive
    the system pointer by display position.
  - tvOS and watchOS have no `ui appearance` and no `status_bar` overrides (unsupported, as in Apple's simulator),
    and no software keyboard to toggle.
- Settings changes (`ui`, `status_bar`, `locale`) on a phone that is off apply at its next boot.
- Stop a phone with `x86sim-aot --device NAME sim stop` (or MCP `sim_stop`), never by signalling processes; kill only
  PIDs you started, never `pkill -f`.
- A boot reaches the home screen in roughly 10-20 s with an up-to-date pre-translated image, longer on a phone's
  first boot; `sim start` returns at once, so always `wait home` (or poll `sim status` for `home`).

## MCP

The same features are MCP tools (`x86sim-mcp`, stdio): `snapshot`, `tap`, `type`, `wait_for`, `screenshot`,
`app_install`, `app_launch`, `xctest_run`, `web_eval`, `phone_take`, `share_start`, `sim_ui`, `app_logs`, `dev_run`, `ax_audit`, `profile_start` and the rest (85 tools;
README.md lists them by group). Every tool takes `phone`.

```sh
claude mcp add x86sim -- /path/to/x86sim-*.AppImage mcp       # AppImage
claude mcp add x86sim -- /path/to/x86sim/tools/launcher/target/release/x86sim-mcp   # checkout
```

`phone_take` leases are held by the MCP server's process: they end with the session or at `ttl_s`.
