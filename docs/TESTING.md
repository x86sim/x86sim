# Testing apps on x86Sim

XCTest and XCUITest runs, and the Accessibility Inspector (inspection, audit, actions). For driving an app as an
agent does (snapshots, `expect`, `batch`), see [AGENT-TOOLS.md](AGENT-TOOLS.md).

## Tests (XCTest, XCUITest)

`x86sim test` runs test bundles on a phone as `xcodebuild test-without-building` does in Apple's simulator, and the
MCP tool `xctest_run` does the same for agents (with `result_bundle`, `test_iterations`, `retry_tests_on_failure`,
`run_tests_until_failure` and `only_test_configuration`):

```sh
x86sim test qa --xctestrun Products/MyApp_iphonesimulator27.2-arm64.xctestrun   # what build-for-testing wrote
x86sim test qa --xctestrun F.xctestrun --only-testing MyAppTests/LoginTests/testBadPassword --junit junit.xml
x86sim test qa --app MyApp.app                          # unit tests hosted in the app (its PlugIns/*.xctest)
x86sim test qa --test-bundle MyLogicTests.xctest        # logic tests (no host app)
x86sim test qa --ui-test-runner MyAppUITests-Runner.app --app MyApp.app        # XCUITest
x86sim test qa --spm MyPackage                          # a Swift package's tests, built here (x86sim dev test --xctest)
x86sim test qa --xctestrun F.xctestrun --coverage       # code coverage (products built with CLANG_COVERAGE_MAPPING=YES)
x86sim test qa --xctestrun F.xctestrun --result-bundle R.xcresult   # also an Xcode result bundle
```

- **Unit tests hosted in an app**: the app is installed and launched with `libXCTestBundleInject.dylib`, which loads
  the bundle into it, as Xcode does. **Logic tests** run in the `xctest` tool. **UI tests**: the runner and the app
  under test are installed and the runner's `XCUIApplication` launches, queries and taps the app through the
  runtime's `testmanagerd` (the phone's `xctest` feature, on by default, started only when a UI test runs; ~440 MB
  while it runs).
- Each test process gets the `.xctestconfiguration` Xcode would write (selection, target app, environment from the
  `.xctestrun`), without an IDE session. `--only-testing` / `--skip-testing` take Xcode's forms (`Target`,
  `Target/Class`, `Target/Class/test`, Swift Testing's `Target/Suite/test()` and `Target/function()`) and become the
  identifiers Xcode 27.2 sends for them, each test running once. A Swift package's tests are named by test target as
  in Xcode (`CalcKitTests/meanOfNothing()`): SwiftPM builds them into one `<Package>PackageTests` bundle, which
  `--spm` links again once per test target from SwiftPM's objects (and with XCTest, as Xcode links every test
  bundle), so each target runs alone as in Xcode, its selections and skips matched in its own module even when two
  targets have a test of the same name (`tests/apps/XCTestDemo/spm-multi`).
  Results: XCTest's log as it runs (Swift Testing's too), a summary with each failure's `file:line` and message,
  `junit.xml` and `results.json` (per test: passed / failed / skipped / expected_failure, duration, failures) in
  `$X86SIM_SCRATCH/xctest/<phone>-<time>/`; exit status 0 when nothing failed, 1 otherwise. Known issues are
  reported as Xcode 27.2 does: a test whose only issues were expected (`XCTExpectFailure`, Swift Testing's
  `withKnownIssue`) is `expected_failure` ("Expected Failure" in an `.xcresult`, not a failure; junit.xml says so in
  its output), its issues in `expected_failures` (file, line, message, `reason`: the failure reason or the known
  issue's comment, from `libsimswt`), counted in `counts.expected_failures`; a known issue that did not occur
  fails (`Known issue was not recorded`, `Expected failure '...' but none recorded`). Swift Testing's tests
  are named as Xcode names them, by suite path (`Second/Inner/same()`; the same function in two suites is two
  tests), from Swift Testing's own events: `libsimswt` (`host/simswt`, built here on first use against the
  platform's Testing.framework) is inserted into the test process and writes them; without it they come from the
  console lines, which name no suite. Each issue keeps its call stack (`stack` in `results.json`: the frames in the
  test bundle with function and `file:line`, as Xcode shows them), symbolicated here (llvm-symbolizer, a dSYM made
  by dsymutil from a Debug build's objects); a failed `#expect` / `#require` keeps its expression with each
  subexpression's value (`expression`). A test that fails by throwing, or an `XCTExpectFailure` a thrown error
  matched, gets the line it threw from, as Xcode reports it: the stack XCTest captured at the throw.
- **Attachments and screen recordings**: the tests' XCTest attachments (screenshots, UI hierarchies, their own) go to
  `attachments/<Class>.<test>/` and are listed per test in `results.json`, kept as the `.xctestrun`'s
  `UserAttachmentLifetime` / `SystemAttachmentLifetime` say (Xcode's default, deleteOnSuccess: only a failed test's
  are kept, unless an attachment sets its own `lifetime`). UI tests are screen-recorded test by test,
  as Xcode does (`PreferredScreenCaptureFormat` screenRecording, its default): a failed test keeps its
  `Screen_Recording.mp4` (`SystemAttachmentLifetime` deleteOnSuccess; keepAlways keeps all), `--no-screen-recording`
  turns it off.
- **Parallel testing** (`--parallel-testing`, `--parallel-workers N`; the stand-in's `-parallel-testing-enabled YES`
  and `-parallel-testing-worker-count N`; an `.xctestrun` target's `ParallelizationEnabled`): a target's XCTest
  classes are split over workers (the biggest first, each to the least loaded), each a test process of its own, as
  Xcode's workers; the classes come from the bundle itself. Xcode clones the simulator for its workers; here they
  share the device: logic tests run at the same time, tests hosted in an app (one copy per device) one worker after
  another. The results are the target's, as one run.
- **Several devices** (`--also-on PHONE`, repeatable; the stand-in's several `-destination`s): the devices test at
  the same time, as Xcode's destinations (at most 4, `--max-concurrent-destinations` /
  `-maximum-concurrent-test-simulator-destinations`; `--serial-destinations` / `-disable-concurrent-destination-testing`
  one after another), and one result bundle holds an action per device, as Xcode's (`xcresulttool` shows each test
  per device; XCTestDemo's on an iPhone and an iPad: Apple's counts). The stand-in prints what `xcodebuild` prints
  then ("Testing on multiple destinations concurrently. ...", "Test suite 'X' started on 'phone - xctest (pid)'",
  "Test case 'C.t()' passed on '...'", "Testing passed / failed on 'phone'").
- **Performance tests** (`measure {}`, `measure(metrics:)`): each metric XCTest measured (`libsimshim` takes them
  where Xcode's IDE session would) is kept with the test (`metrics` in `results.json`: identifier, name, units,
  values, baseline, limits, polarity) and in a result bundle as Xcode keeps it (`xcresulttool get test-results
  metrics`, `hasPerformanceMetrics`; equal to Xcode 27.2's but for the measured values). Baselines work as Xcode's:
  Swift Build copies the project's `xcshareddata/xcbaselines` into the test bundle, and `x86sim test` hands XCTest
  the one for this machine and device (`baselineFileRelativePath` / `baselineFileURL`, as Xcode's harness), which
  then compares and fails a regression. `--record-baselines` is Xcode's "Set Baseline": each metric's average
  into the project's `xcbaselines`, for this machine (its CPU and board, as Xcode's `localComputer`) and the
  device; a baseline recorded on a Mac names that Mac, so it is not this machine's, as in Xcode.
- **Test plans**: a scheme's test plan (`.xctestplan`) gives an `.xctestrun` with one set of targets per
  configuration (Swift Build writes it in `build-for-testing`: each configuration's environment, arguments,
  language and region, skipped and selected tests, time limits); `x86sim test` runs every configuration in turn
  (`--only-test-configuration` / `--skip-test-configuration`, the stand-in's `-only-test-configuration` /
  `-skip-test-configuration`) and the result bundle has a run per configuration, its tests' URLs with Xcode's
  `?config=<name>` (equal to Xcode's for a two-configuration plan of XCTestDemo). `-testPlan NAME` builds that plan
  instead of the scheme's default.
- **Repetitions and time limits** (`--test-iterations N`, `--retry-tests-on-failure`, `--run-tests-until-failure`,
  `--test-timeouts-enabled YES`, `--default-test-execution-time-allowance S`, `--maximum-test-execution-time-allowance
  S`; the `xcodebuild` stand-in passes its options of those names): XCTest repeats the tests in the test process as
  Xcode 27.2 asks it to (the same repetition policy and limits in its configuration). Each run is kept
  (`iterations` in `results.json`, "Repetition N" / "First Run", "Retry N" in a result bundle), and the test's result
  is Xcode's: with retries its last run's, otherwise failed when any run failed. With
  `--test-repetition-relaunch` (`-test-repetition-relaunch-enabled YES`) each repetition is a new test process, as
  Xcode relaunches it: fixed iterations N processes, until-failure until one fails, retries every test again while
  one fails (Xcode's bundles of the three: equal).
- **Code coverage** (`--coverage`, MCP `xctest_run coverage: true`, `xcodebuild -enableCodeCoverage YES` or a scheme
  with "Gather coverage"): each test process and the app a UI test drives write LLVM profiles (`LLVM_PROFILE_FILE`, as
  Xcode sets it), merged into `coverage/Coverage.profdata` and reported per product and file as `xccov view --report`
  does (`coverage/report.txt`, `coverage.json`, `lcov.info`, "coverage" in `results.json`). The products need coverage
  instrumentation (`CLANG_COVERAGE_MAPPING=YES`, what the `xcodebuild` stand-in passes; `--spm` builds them so).
  XCTestDemo's line counts equal Xcode 27.2's, whole run and a two-test selection.
- **Result bundles** (`--result-bundle R.xcresult`, `xcodebuild -resultBundlePath`): an `.xcresult` in Xcode 27.2's
  format (`cli/libexec/xcresult.py`, written on Linux), so `xcresulttool get test-results summary / tests /
  test-details`, `export attachments`, `xccov view --report` and the CI tools built on them read it as one Xcode made:
  results, failures with `file:line`, their call stacks and `#expect` values, skips, expected failures, attachments
  and screen recordings, code coverage down to functions and line ranges, Swift Testing's suites and each argument of
  a parameterized test, UI tests' activity trees (UI snapshots, waits, taps) with their attachments, the frames
  Xcode names in the UI test runner, and the action log Xcode writes (`get log --type action`: each test process,
  its suites and test cases, XCTest's and Swift Testing's, with their own output, summary, failures, skips, known
  issues and Xcode's "N tests total, P passed, ..." lines; each install and launch with its environment; the
  coverage report's steps), and the diagnostics in Xcode's layout (`export diagnostics`: per test target its
  output, the UI tests' app's output, `StandardOutputAndStandardError-<bundle id>.txt`, and XCTest's debug log as
  the session log, which XCTest would otherwise print among the test output). `tests/xcresult/check.py` compares
  bundles with Apple's on the comparison Mac: test-details, the legacy issue summaries, frames, activities, the
  action log and the diagnostics' files.
  The `xcodebuild` stand-in writes the bundle at `-resultBundlePath` (without it, where Xcode does:
  `<derived data>/Logs/Test/Test-<Scheme>-<time>.xcresult`), prints what `xcodebuild` prints (its "Command line
  invocation:", the test processes' output, "Test session results, code coverage, and logs:", "Failing tests:" as
  Xcode names them, `** TEST [EXECUTE ]SUCCEEDED / FAILED **`, which CI log parsers read; `x86sim test
  --xcodebuild ACTION` does the same; a build, in `build`, `build-for-testing` and `test`, prints Xcode's log of it,
  task by task, with the compiler's diagnostics as Swift prints them, and a failed test build ends with Xcode's
  "Testing failed:" and "The following build commands failed:") and puts `x86sim test`'s own output next to the bundle
  (`<bundle>.x86sim/`). Its `test` also puts the build in it, as Xcode does: the build log (`xcresulttool get log
  --type build`: one section per Swift Build task that ran, with Xcode's titles, commands and times, read from the
  build's `XCBuildData`), its errors and warnings (`get build-results`, with the issue types of the compilers'
  serialized diagnostics: Swift's diagnostic groups, clang's categories), and when the build fails, a bundle with the
  errors and "Testing cancelled because the build failed." (`x86sim test --build-log`).
- **Where the bundles come from**, built on Linux:
  - a Swift package: `x86sim dev test --xctest` / `x86sim test --spm`, which build its tests (`swift build
    --build-tests` for the device's simulator) with the `x86sim dev` toolchain and run them;
  - an Xcode project or workspace: the dev toolchain's `xcodebuild` stand-in on Apple's Swift Build
    (`external/omarchy-apple-dev/linux-bin/xcodebuild`, after `x86sim dev setup swiftbuild`). `xcodebuild
    build-for-testing` writes the products and the `.xctestrun`, and `xcodebuild test` goes on to run them on an
    x86Sim device through `x86sim test` (the destination's `name=`, or `$XCODEBUILD_PHONE`, names the device). Unit
    test targets (XCTest and Swift Testing) and UI-test targets build, for iOS, tvOS, watchOS and visionOS: Swift
    Build makes each `<Target>-Runner.app` from Xcode's XCTRunner.app, which the SDK keeps from Xcode.xip (an SDK built
    before that has none: rebuild it with `x86sim dev setup sdk --xip`). A project without shared schemes gets
    generated schemes that leave its test target out: share the scheme. A fastlane lane's `scan` runs the same way
    (`tests/dev/xcode_e2e.py`). `tests/apps/XCTestDemo/run.sh <phone> --linux` builds the fixture that way.
  - Bundles Xcode itself built also run: copy `xcodebuild build-for-testing`'s `Build/Products` directory with the
    `.xctestrun`. That is what the XCTestDemo fixture checks on purpose
    (`tests/apps/XCTestDemo/build.sh` runs `xcodebuild` on the comparison Mac, so that the test is of bundles made by
    Xcode, not of our build).
- **XCTest's frameworks** (`XCTest.framework`, `XCTestCore`, `XCUIAutomation`, `libXCTestBundleInject.dylib`) are
  Xcode's, not the runtime's: `x86sim test` takes them from the Darwin SDK `x86sim dev setup sdk --xip Xcode.xip` builds
  on Linux (Xcode.xip from developer.apple.com, a free Apple ID; Apple publishes no XCTest download without one),
  `sdks/<Platform>.platform` (`x86sim test fetch-platform` copies them there) or `--platform DIR`.
- **Every platform**: `tests/apps/XCTestDemo` builds for iOS, tvOS, watchOS and visionOS (`X86SIM_PLATFORM=tvos
  tests/apps/XCTestDemo/build.sh`; `run.sh <phone>` picks the phone's). Its unit, logic and UI tests give Apple's
  results on iPhone, iPad, Apple TV, Apple Watch and Vision Pro.

`tests/apps/XCTestDemo/run.sh <phone>` runs the fixture (hosted unit tests, logic tests, UI tests, each with a
deliberate failure) and checks every result against Apple's simulator; `tests/apps/XCTestDemo/spm-multi/check.sh
<phone>` does the same for a Swift package with four test targets (selections, skips, known issues, values).

### Differences from Xcode

What still differs from Xcode 27.2 on a Mac, measured with the fixtures above (none of it changes a test's result):

- **No IDE session.** Xcode hands the test process its configuration over its IDE session; `x86sim test` writes the
  `.xctestconfiguration` file `xcodebuild test-without-building` would read, with the fields Xcode 27.2 sets there
  (UI tests on the main thread, the `.xctestrun`'s attachment lifetimes, the scheme's products for symbolication and
  crash reports, the apps' bundle infos, the test time allowance). Only what needs the session is left out: its
  identifier, `reportResultsToIDE`, the IDE's capabilities and `testApplicationDependencies` (with it,
  `XCUIApplication` asks Xcode to launch the app).
- **Screen recordings** are made by x86Sim from the device's frame ring (`x86sim io recordVideo`, 30 fps), not by
  Xcode; the runner is told so (`-DisableDiagnosticScreenshots YES`), and XCTest then records the UI snapshot Xcode's
  run has instead of screenshots.
- **Result bundles**: tests, attachments and targets carry the times they really ran (`started` in `results.json`,
  from `libsimshim` and `libsimswt`). The build action is "not requested", as in `test-without-building`. After the
  `xcodebuild` stand-in's `test`, the build log has Xcode's sections for the same build (XCTestDemo: 256 against
  Xcode's 251; Swift Build on Linux has no "Build stat cache" step and runs two more each of "Generate DER
  entitlements", "Process product packaging" and "Write Entitlements-Simulated.plist"); the Swift driver's per-file
  jobs and explicit modules are timed from the files they wrote, and their compiler output is only the diagnostics
  (Xcode keeps the whole output, the failed frontend command with it). The run destination's host record describes
  the Linux machine (the environment reads `XCTestDemo`, not `XCTestDemo · Built with macOS …`). In the action log,
  "Launch actions" and "Generating coverage report" have Xcode's steps but not all of its notes (Xcode's console
  policy, resume and per-app profile download lines), and a Swift Testing test's summary is its own last line
  (Xcode's, as Swift Testing runs tests in parallel, is often another test's). How often a UI test's "Waiting …" activity re-checks before it times out
  depends on timing, so a slow host can record a different count than Apple's.
- **Selection**: a bare `-only-testing Class` (no target) selects that class in every target; Xcode refuses it.
- **Swift packages**: SwiftPM builds one `<Package>PackageTests` bundle; `x86sim test --spm` relinks it once per test
  target (as Xcode's bundles are). If that relink fails it runs the one bundle and says so; Swift Testing selections
  and skips then match no test target (XCTest ones still work).
- **Code coverage** comes from LLVM's tools (`llvm-profdata`, `llvm-cov`), not `xccov`'s own; the counts are the same.
- **Darwin SDKs built before XCTRunner.app was kept** (and before the `-rpath=$ORIGIN` fix) can't build UI-test targets
  or visionOS package tests: rebuild with `x86sim dev setup sdk --xip Xcode.xip`.

## Accessibility Inspector

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
([docs/protocols/SIMAX.md](protocols/SIMAX.md)). Font, color and truncation are read from UIKit labels; for
SwiftUI text the contrast comes from the pixels alone, and clipping is not seen.

Per platform: Apple TV and Apple Watch get no touch-target check (the focus engine, the watch's own sizes), Vision
Pro checks 60 pt and no contrast (a window's place in the room is not known), and its frames are each window's own
points, so the viewer's inspector lists the elements instead of outlining them on the screen.
