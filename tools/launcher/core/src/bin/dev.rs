//! x86sim-dev (`x86sim dev ...`): build apps on Linux and run them on x86Sim's phones, with omarchy-apple-dev
//! (external/omarchy-apple-dev). The logic is x86sim_core::dev, shared with the MCP server and the launcher.
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use x86sim_core::dev;

const USAGE: &str = "usage: x86sim dev <command> [args]

Toolchain (omarchy-apple-dev: Swift, xtool, the iOS SDK; nothing needs root)
  status [--json]      every piece: Swift, xtool, SDK, device tools, and a running setup job
  setup [STEP] [--mode full|no-xcode] [--xip Xcode.xip] [--bg]
                       STEP: all (default: Swift if missing, then the installer), swift (swift.org's toolchain into
                       ~/.local/share/x86sim-dev), toolchain (xtool, rcodesign, ipsw, pymobiledevice3; the SDK too with
                       --xip), sdk --xip PATH, repair (after a Swift swap), verify (build a new app end to end),
                       lldb (the Swift toolchain's lldb for x86sim debug: a private libpython, about 75 MB; also
                       done on the first x86sim debug).
                       --bg runs it in the background (the launcher's Develop page and MCP dev_status show it)
  job [--follow|--cancel]
                       the background setup job: its step and log
  auth                 xtool auth: Apple ID sign-in (interactive), for installs on a real iPhone
  env                  shell exports for using xtool / swift directly (eval \"$(x86sim dev env)\")

Projects (DIR defaults to the current directory)
  new NAME [PARENT]    a new SwiftUI app (xtool new), for every platform x86Sim runs
  projects [--json]    recent projects
  build [DIR] [--phone P | --platform ios|tvos|watchos|xros] [--release]
                       simulator build (ship.sh --simulator); the .app's path is the last line
  run [DIR] [--phone P] [--start] [--release] [--no-build] [-- APP ARGS]
                       build for the phone's platform, install and launch it (--start: boot the phone if off)
  test [DIR] [--phone P] [--start] [--secs N] [--shot OUT.png]
                       run, wait N s (default 10), check it still runs, take a screenshot; exit 1 if it died
  test --xctest [DIR] [--phone P] [-- x86sim test OPTIONS]
                       the package's XCTest / Swift Testing tests: swift build --build-tests for the phone's
                       simulator, run in the phone by `x86sim test --spm` (per-test results, junit.xml; exit 1 if one
                       failed). App-hosted and UI test bundles come from Xcode: see README (Tests)

Real devices and publishing (pass-throughs to omarchy-apple-dev, run in DIR)
  device [DIR] [ARGS]  device-run.sh: build, install and launch on a USB / tailnet iPhone (--lldb, --console, ...)
  ship [DIR] [ARGS]    ship.sh: App Store .ipa, validate, --upload to TestFlight (flutter/ship.sh for Flutter)
  ship-mac [DIR] [ARGS] ship-mac.sh: macOS app, Developer ID / App Store
  convert APP.xcodeproj [ARGS]
                       tools/xcodeproj2xtool.py: an Xcode project as an xtool project
  xtool ARGS           xtool itself, with the toolchain's PATH
  path                 the omarchy-apple-dev directory";

fn take_opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 >= args.len() {
        return None;
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

/// The first argument when it is a directory (the project), else the current directory
fn take_dir(args: &mut Vec<String>) -> PathBuf {
    if args.first().is_some_and(|a| !a.starts_with('-') && Path::new(a).is_dir()) {
        PathBuf::from(args.remove(0))
    } else {
        PathBuf::from(".")
    }
}

fn print(l: &str) {
    println!("{l}");
}

fn state_mark(s: &str) -> &'static str {
    match s {
        "ok" => "ok     ",
        "warn" => "check  ",
        "stale" => "update ",
        _ => "missing",
    }
}

fn exec(mut c: std::process::Command) -> ExitCode {
    match c.status() {
        Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("x86sim dev: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "help" | "-h" | "--help") {
        println!("{USAGE}");
        return if args.is_empty() { ExitCode::from(2) } else { ExitCode::SUCCESS };
    }
    let cmd = args.remove(0);
    match run(&cmd, args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("x86sim dev {cmd}: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cmd: &str, mut args: Vec<String>) -> anyhow::Result<ExitCode> {
    // app arguments after `--`
    let app_args: Vec<String> = match args.iter().position(|a| a == "--") {
        Some(i) => {
            let rest = args.split_off(i);
            rest[1..].to_vec()
        }
        None => vec![],
    };
    Ok(match cmd {
        "status" => {
            let st = dev::status();
            if take_flag(&mut args, "--json") {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                for p in &st.pieces {
                    println!("{} {:<18} {}", state_mark(p.state), p.title, p.detail);
                    if !p.fix.is_empty() {
                        println!("        -> {}", p.fix);
                    }
                }
                println!("mode: {}", if st.mode.is_empty() { "(not chosen yet: full)" } else { &st.mode });
                println!("simulator builds: {}   real devices: {}", if st.ready_sim { "ready" } else { "not ready" }, if st.ready_device { "ready" } else { "not ready" });
                if let Some(j) = &st.job {
                    println!("setup job {}: {} {}", j.kind, if j.running { "running".to_string() } else { format!("finished, exit {}", j.rc.unwrap_or(-1)) }, j.step);
                }
            }
            ExitCode::SUCCESS
        }
        "setup" => {
            let bg = take_flag(&mut args, "--bg");
            let kind = if args.first().is_some_and(|a| !a.starts_with('-')) { args.remove(0) } else { "all".into() };
            if bg {
                let j = dev::start_job(&kind, &args)?;
                println!("setup {} started (pid {}); follow it: x86sim dev job --follow   log: {}", j.kind, j.pid, j.log.display());
                return Ok(ExitCode::SUCCESS);
            }
            let mode = take_opt(&mut args, "--mode");
            let xip = take_opt(&mut args, "--xip").map(PathBuf::from).or_else(|| std::env::var_os("XCODE_XIP").map(PathBuf::from));
            if let Some(x) = &xip {
                let mut s = dev::load_settings();
                s.xip = x.to_string_lossy().into_owned();
                let _ = dev::save_settings(&s);
            }
            let mut say = print;
            let rc = match kind.as_str() {
                "swift" => {
                    dev::install_swift(&mut say)?;
                    0
                }
                "toolchain" | "sdk" | "all" => {
                    if kind == "sdk" {
                        anyhow::ensure!(xip.is_some() || mode.as_deref() == Some("no-xcode"), "setup sdk needs --xip /path/to/Xcode.xip (or --mode no-xcode)");
                    }
                    if dev::swift().is_none() {
                        println!("== swift (none on PATH: swift.org's {} into {})", dev::SWIFT_VERSION, dev::dev_home().display());
                        dev::install_swift(&mut say)?;
                    }
                    let rc = dev::run_installer(mode.as_deref(), xip.as_deref(), false, &mut say)?;
                    let st = dev::status();
                    if kind == "all" && st.ready_sim {
                        dev::verify(&mut say)?;
                        println!("== verified: a new app builds for the simulator");
                        // the Swift lldb for x86sim debug: optional (x86sim debug falls back to /usr/bin/lldb)
                        if dev::setup_lldb(&mut say).unwrap_or(1) != 0 {
                            println!("the Swift lldb is not set up (x86sim dev setup lldb shows why): x86sim debug uses /usr/bin/lldb");
                        }
                    }
                    if rc != 0 && st.pieces.iter().any(|p| p.id == "sdk" && p.state != "ok") && st.pieces.iter().any(|p| p.id == "xtool" && p.state != "missing") {
                        println!("toolchain ready except the iOS SDK: x86sim dev setup sdk --xip /path/to/Xcode.xip");
                    }
                    rc
                }
                "repair" => dev::run_installer(mode.as_deref(), xip.as_deref(), true, &mut say)?,
                "lldb" => dev::setup_lldb(&mut say)?,
                "verify" => {
                    let app = dev::verify(&mut say)?;
                    println!("== verified: {}", app.display());
                    0
                }
                k => anyhow::bail!("unknown step {k}: swift, toolchain, sdk, repair, verify, lldb or all"),
            };
            ExitCode::from(rc.clamp(0, 255) as u8)
        }
        "job" => {
            if take_flag(&mut args, "--cancel") {
                dev::cancel_job()?;
                println!("cancelled");
                return Ok(ExitCode::SUCCESS);
            }
            let follow = take_flag(&mut args, "--follow") || take_flag(&mut args, "-f");
            let Some(j) = dev::job() else {
                println!("no setup job yet");
                return Ok(ExitCode::SUCCESS);
            };
            if follow {
                let mut c = std::process::Command::new("tail");
                c.args(["-n", "+1", "-F", "--pid"]).arg(j.pid.to_string()).arg(&j.log);
                exec(c);
            } else {
                print!("{}", dev::tail_bytes(&j.log, 4000));
            }
            let j = dev::job().unwrap_or(j);
            println!("-- setup {}: {} ({})", j.kind, if j.running { "running".to_string() } else { format!("finished, exit {}", j.rc.unwrap_or(-1)) }, j.step);
            ExitCode::from(if j.running { 0 } else { j.rc.unwrap_or(1).clamp(0, 255) as u8 })
        }
        "new" => {
            anyhow::ensure!(!args.is_empty(), "new NAME [PARENT]");
            let name = args.remove(0);
            let parent = args.first().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            let d = dev::new_project(&name, &parent.canonicalize().unwrap_or(parent.clone()), &mut |l| println!("{l}"))?;
            dev::remember(&d, None);
            println!("{}", d.display());
            ExitCode::SUCCESS
        }
        "projects" => {
            let r = dev::recent();
            if take_flag(&mut args, "--json") {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for p in r {
                    println!("{:<20} {:<8} {:<32} {}", p.name, p.kind, p.bundle_id, p.dir.display());
                }
            }
            ExitCode::SUCCESS
        }
        "build" => {
            let phone = take_opt(&mut args, "--phone");
            let platform = take_opt(&mut args, "--platform");
            let release = take_flag(&mut args, "--release");
            let dir = take_dir(&mut args);
            let platform = match (platform, phone) {
                (Some(p), _) => p,
                (None, Some(ph)) => dev::pick_phone(Some(&ph))?.platform,
                (None, None) => "ios".into(),
            };
            let app = dev::build(&dir, &dev::BuildOpts { platform, release }, &mut |l| eprintln!("{l}"))?;
            println!("{}", app.display());
            ExitCode::SUCCESS
        }
        "test" if take_flag(&mut args, "--xctest") => {
            // XCTest bundles of a Swift package, run by cli/libexec/xctest.py (x86sim test --spm)
            let phone = take_opt(&mut args, "--phone");
            let dir = take_dir(&mut args);
            anyhow::ensure!(args.is_empty(), "unexpected arguments: {} (x86sim test options go after --)", args.join(" "));
            let inst = dev::pick_phone(phone.as_deref())?;
            let dir = dir.canonicalize().unwrap_or(dir);
            let mut c = dev::command(x86sim_core::aot::repo_root().join("cli/x86sim"));
            c.arg("--phone").arg(&inst.dir).arg("test").arg("--spm").arg(&dir).args(&app_args);
            exec(c)
        }
        "run" | "test" => {
            let phone = take_opt(&mut args, "--phone");
            let secs: u64 = take_opt(&mut args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(10);
            let shot = take_opt(&mut args, "--shot").map(PathBuf::from);
            let o = dev::RunOpts { release: take_flag(&mut args, "--release"), start: take_flag(&mut args, "--start"), no_build: take_flag(&mut args, "--no-build"), args: app_args };
            let dir = take_dir(&mut args);
            anyhow::ensure!(args.is_empty(), "unexpected arguments: {} (app arguments go after --)", args.join(" "));
            let inst = dev::pick_phone(phone.as_deref())?;
            if cmd == "run" {
                let r = dev::run(&dir, &inst, &o, &mut |l| println!("{l}"))?;
                println!("== {} running on {} ({:.0} s)", r.bundle_id, r.phone, r.secs);
                ExitCode::SUCCESS
            } else {
                let t = dev::smoke_test(&dir, &inst, &o, secs, shot.as_deref(), &mut |l| println!("{l}"))?;
                println!("== {}: {}", if t.passed { "PASS" } else { "FAIL" }, if t.passed { format!("{} still running after {secs} s", t.run.bundle_id) } else { format!("{} not running after {secs} s", t.run.bundle_id) });
                ExitCode::from(if t.passed { 0 } else { 1 })
            }
        }
        "device" | "ship" | "ship-mac" => {
            let dir = take_dir(&mut args);
            args.extend(app_args);
            exec(dev::passthrough(cmd, &dir, &args)?)
        }
        "convert" => {
            args.extend(app_args);
            exec(dev::passthrough("convert", Path::new("."), &args)?)
        }
        "auth" | "xtool" => {
            let mut c = dev::command(dev::which("xtool").unwrap_or_else(|| PathBuf::from("xtool")));
            if cmd == "auth" {
                c.arg("auth");
            }
            c.args(&args).args(&app_args);
            exec(c)
        }
        "path" => {
            println!("{}", dev::fork_dir().display());
            ExitCode::SUCCESS
        }
        "env" => {
            for (k, v) in dev::env() {
                println!("export {k}='{}'", v.replace('\'', "'\\''"));
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    })
}
