//! Develop: build apps on Linux and run them on the phones, with omarchy-apple-dev (our fork,
//! external/omarchy-apple-dev, a submodule that follows joshuaswarren/omarchy-apple-dev).
//!
//! The fork brings the toolchain installer (install-toolchain.sh: Swift, xtool built from source, rcodesign, ipsw,
//! pymobiledevice3, the Darwin SDK built from Xcode.xip or cut from an iPhone), the simulator / device / App Store
//! build (ship.sh), device runs (device-run.sh) and the Linux actool / ibtool / momc / xcstringstool. This module
//! wraps it for the `x86sim dev` command (bin/dev.rs), the MCP server's dev_* tools and the launcher's Develop page:
//!   - status(): every piece of the toolchain (Swift, xtool, SDK, device tools), from the same files the installer
//!     checks (tools/sdk-state.sh: the SDK's darwin-sdk-version.txt against the pinned xtool's stamp);
//!   - setup jobs: one background installer run at a time (log, step, exit code in $X86SIM_SCRATCH/dev), so the GUI and
//!     MCP see the same progress; x86Sim never runs sudo for it (--user-only; system packages are shown as a command);
//!   - build(): an xtool project for the simulator (ship.sh --simulator: Xcode's app layout, icon catalog), the
//!     triple from the phone's platform; run(): build, start the phone if asked, install and launch (the key loop);
//!   - pass-throughs for device runs, App Store builds and Xcode project conversion.
use crate::aot::{repo_root, scratch};
use crate::instance::Instance;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The Swift x86Sim installs when there is none: 6.4 matches Xcode 27's SDK (the installer's matching_xcode)
pub const SWIFT_VERSION: &str = "6.4.2";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, dflt: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(dflt))
}

/// The omarchy-apple-dev tree: X86SIM_OAD, else external/omarchy-apple-dev (the AppImage carries its runtime files at
/// the same place in the payload)
pub fn fork_dir() -> PathBuf {
    std::env::var_os("X86SIM_OAD").map(PathBuf::from).unwrap_or_else(|| repo_root().join("external/omarchy-apple-dev"))
}

/// Where x86Sim keeps what it installs for development itself (the Swift toolchain when the system has none)
pub fn dev_home() -> PathBuf {
    std::env::var_os("X86SIM_DEV_HOME").map(PathBuf::from).unwrap_or_else(|| xdg("XDG_DATA_HOME", ".local/share").join("x86sim-dev"))
}

/// The setup job's files and build logs
pub fn work_dir() -> PathBuf {
    scratch().join("dev")
}

fn settings_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("x86sim/develop.json")
}

/// SwiftPM's SDK dir, as SwiftPM (and the fork's scripts) pick it: $XDG_CONFIG_HOME/swiftpm when that is set, else
/// ~/.swiftpm. A launcher started from the desktop often has no XDG_CONFIG_HOME while the installer ran in a terminal
/// that had it: env() then sets it for the children, so both see the same SDK.
fn swiftpm_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x).join("swiftpm"),
        None if !home().join(".swiftpm/swift-sdks").exists() && home().join(".config/swiftpm/swift-sdks").exists() => home().join(".config/swiftpm"),
        None => home().join(".swiftpm"),
    }
}

pub fn sdk_bundle() -> PathBuf {
    swiftpm_dir().join("swift-sdks/darwin.artifactbundle")
}

/// The environment the fork's scripts run with: x86Sim's own Swift (if installed) and ~/.local/bin (xtool, rcodesign,
/// ipsw) first on PATH, XDG_CONFIG_HOME matching sdk_bundle()
pub fn env() -> Vec<(String, String)> {
    let mut path: Vec<PathBuf> = vec![];
    let managed = dev_home().join("swift/usr/bin");
    if managed.join("swift").exists() {
        path.push(managed);
    }
    path.push(home().join(".local/bin"));
    if let Some(p) = std::env::var_os("PATH") {
        path.extend(std::env::split_paths(&p).filter(|d| !d.as_os_str().is_empty()));
    }
    for d in ["/usr/local/bin", "/usr/bin", "/bin"] {
        if !path.iter().any(|p| p == Path::new(d)) {
            path.push(d.into());
        }
    }
    let mut v = vec![("PATH".to_string(), std::env::join_paths(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())];
    if std::env::var_os("XDG_CONFIG_HOME").is_none() && swiftpm_dir() == home().join(".config/swiftpm") {
        v.push(("XDG_CONFIG_HOME".into(), home().join(".config").to_string_lossy().into_owned()));
    }
    // the curses aliases install-toolchain.sh --curses-compat makes for a swift.org toolchain (Arch has no narrow ncurses)
    let compat = home().join(".local/lib/curses-narrow-compat");
    if dev_home().join("swift/usr/bin/swift").exists() && compat.is_dir() {
        let old = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        v.push(("LD_LIBRARY_PATH".into(), if old.is_empty() { compat.to_string_lossy().into_owned() } else { format!("{}:{old}", compat.display()) }));
    }
    v
}

/// A Command with env()
pub fn command(prog: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(prog);
    c.envs(env());
    c
}

/// A program on env()'s PATH
pub fn which(cmd: &str) -> Option<PathBuf> {
    let path = env().into_iter().find(|(k, _)| k == "PATH").map(|(_, v)| v).unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join(cmd)).find(|p| p.is_file())
}

fn output(prog: &Path, args: &[&str]) -> Option<String> {
    let o = command(prog).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

/// A shell-style assignment in the installer (XTOOL_SHA=..., XTOOL_SDK_VERSION="...")
fn installer_value(key: &str) -> Option<String> {
    let s = std::fs::read_to_string(fork_dir().join("install-toolchain.sh")).ok()?;
    s.lines().find_map(|l| l.strip_prefix(&format!("{key}=")).map(|v| v.trim().trim_matches('"').to_string()))
}

// ---------------------------------------------------------------------------------------------------------------
// status

#[derive(Debug, Clone, Serialize)]
pub struct Piece {
    /// fork, packages, swift, xtool, sdk, device
    pub id: &'static str,
    pub title: &'static str,
    /// ok, missing, stale (installed, but not what the fork pins), warn (optional / partly there)
    pub state: &'static str,
    pub detail: String,
    /// what to do (empty when nothing); `x86sim dev setup <piece>` when x86Sim can do it
    pub fix: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub fork: PathBuf,
    /// the fork's version (CHANGELOG's newest entry) and commit
    pub fork_version: String,
    /// install-toolchain.sh's mode (~/.config/omarchy-apple-dev/mode): full, no-xcode, or "" before the first run
    pub mode: String,
    pub pieces: Vec<Piece>,
    /// SDK platforms (iPhoneSimulator, AppleTVSimulator, ...)
    pub platforms: Vec<String>,
    /// simulator builds work (Swift, current xtool, a current SDK with the simulator platforms)
    pub ready_sim: bool,
    /// device installs work (ready_sim's pieces with iPhoneOS, plus pymobiledevice3 and an xtool sign-in)
    pub ready_device: bool,
    pub job: Option<Job>,
}

fn piece(id: &'static str, title: &'static str, state: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Piece {
    Piece { id, title, state, detail: detail.into(), fix: fix.into() }
}

fn fork_version(dir: &Path) -> String {
    let ver = std::fs::read_to_string(dir.join("CHANGELOG.md"))
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("## ")).map(|l| l.trim_start_matches('#').trim().to_string()))
        .unwrap_or_default();
    let commit = Command::new("git").arg("-C").arg(dir).args(["rev-parse", "--short", "HEAD"]).stderr(Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    match commit {
        Some(c) if !ver.is_empty() => format!("{ver} ({c})"),
        Some(c) => c,
        None => ver,
    }
}

/// The installed Swift: (path, version) of the swift on env()'s PATH
pub fn swift() -> Option<(PathBuf, String)> {
    let p = which("swift")?;
    let v = output(&p, &["--version"]).unwrap_or_default();
    let ver = v.split_whitespace().skip_while(|w| *w != "version").nth(1).unwrap_or("?").to_string();
    Some((p, ver))
}

fn linux_id() -> (String, String, String) {
    let s = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let get = |k: &str| s.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").trim_matches('"').to_string();
    (get("ID"), get("ID_LIKE"), get("VERSION_ID"))
}

/// The system packages xtool's build and the fork's tools need, as one install command for this distribution
pub fn packages_command() -> String {
    let (id, like, _) = linux_id();
    let fam = format!("{id} {like}");
    if fam.contains("arch") {
        "sudo pacman -S --needed usbmuxd zip base-devel git libimobiledevice openssl poppler libheif".into()
    } else if fam.contains("debian") || fam.contains("ubuntu") {
        "sudo apt install build-essential git zip unzip curl pkg-config libimobiledevice-dev libssl-dev libcurl4-openssl-dev libxml2-dev libncurses6 python3 python3-venv usbmuxd poppler-utils libheif-examples (Ubuntu 24.04's libimobiledevice is too old for xtool: see external/omarchy-apple-dev/docs/DISTROS.md)".into()
    } else if fam.contains("fedora") || fam.contains("rhel") {
        "sudo dnf install gcc gcc-c++ make git zip unzip curl pkgconf libimobiledevice-devel openssl-devel libcurl-devel libxml2-devel ncurses-libs python3 usbmuxd poppler-utils libheif-tools".into()
    } else {
        "install: a C compiler, git, zip, curl, pkg-config, libimobiledevice (development files), openssl (development files), python3 with venv, usbmuxd (external/omarchy-apple-dev/docs/DISTROS.md)".into()
    }
}

fn pkg_config_ok() -> bool {
    Command::new("pkg-config").args(["--exists", "libimobiledevice-1.0", "openssl"]).stdin(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn mode() -> String {
    std::fs::read_to_string(xdg("XDG_CONFIG_HOME", ".config").join("omarchy-apple-dev/mode")).or_else(|_| std::fs::read_to_string(home().join(".config/omarchy-apple-dev/mode"))).map(|s| s.trim().to_string()).unwrap_or_default()
}

pub fn status() -> Status {
    let dir = fork_dir();
    let mut v = vec![];
    let fork_ok = dir.join("install-toolchain.sh").is_file();
    v.push(if fork_ok {
        piece("fork", "omarchy-apple-dev", "ok", format!("{} in {}", fork_version(&dir), dir.display()), "")
    } else {
        piece("fork", "omarchy-apple-dev", "missing", format!("not in {}", dir.display()), "git submodule update --init external/omarchy-apple-dev")
    });

    // Swift
    let sw = swift();
    let managed = dev_home().join("swift/usr/bin/swift").exists();
    v.push(match &sw {
        Some((p, ver)) if ver.starts_with("6.4") || ver.starts_with("6.3") => {
            piece("swift", "Swift", "ok", format!("Swift {ver} ({}{})", p.display(), if managed { ", installed by x86Sim" } else { "" }), "")
        }
        Some((p, ver)) => piece("swift", "Swift", "stale", format!("Swift {ver} at {}: the SDK needs 6.4 (Xcode 27) or 6.3 (Xcode 26)", p.display()), "x86sim dev setup swift"),
        None => piece("swift", "Swift", "missing", "no swift on PATH", format!("x86sim dev setup swift (downloads Swift {SWIFT_VERSION} from swift.org into {}, no root)", dev_home().display())),
    });

    // xtool (+ rcodesign, ipsw: the installer's step 3)
    let xt = home().join(".local/bin/xtool");
    let pinned = installer_value("XTOOL_SHA").unwrap_or_default();
    let have = if xt.exists() { output(&xt, &["--version"]).unwrap_or_default() } else { String::new() };
    let commit = have.split_whitespace().last().unwrap_or("").to_string();
    let extras: Vec<&str> = ["rcodesign", "ipsw"].into_iter().filter(|t| !home().join(".local/bin").join(t).exists()).collect();
    let xtool_state = if !xt.exists() {
        "missing"
    } else if !pinned.is_empty() && commit != pinned {
        "stale"
    } else if !extras.is_empty() {
        "warn"
    } else {
        "ok"
    };
    let short = |s: &str| s.chars().take(9).collect::<String>();
    v.push(piece(
        "xtool",
        "xtool",
        xtool_state,
        match xtool_state {
            "missing" => "not installed (built from source, about 7 minutes)".to_string(),
            "stale" => format!("commit {} installed, the fork pins {}", short(&commit), short(&pinned)),
            "warn" => format!("commit {}; missing {}", short(&commit), extras.join(", ")),
            _ => format!("commit {}, rcodesign, ipsw", short(&commit)),
        },
        if xtool_state == "ok" { "" } else { "x86sim dev setup toolchain" },
    ));
    // system packages: only what building xtool needs, and only while xtool isn't built
    if xtool_state == "missing" || xtool_state == "stale" {
        let miss: Vec<&str> = ["git", "zip", "cc", "python3", "pkg-config"].into_iter().filter(|t| which(t).is_none()).collect();
        let ok = miss.is_empty() && pkg_config_ok();
        v.push(piece(
            "packages",
            "System packages",
            if ok { "ok" } else { "missing" },
            if ok { "compiler, git, libimobiledevice, openssl".to_string() } else if miss.is_empty() { "libimobiledevice or openssl development files missing".to_string() } else { format!("missing: {}", miss.join(", ")) },
            if ok { String::new() } else { packages_command() },
        ));
    }

    // the Darwin SDK
    let md = mode();
    let bundle = sdk_bundle();
    let expected = installer_value("XTOOL_SDK_VERSION").unwrap_or_default();
    let stamp = std::fs::read_to_string(bundle.join("darwin-sdk-version.txt")).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut platforms: Vec<String> = std::fs::read_dir(bundle.join("Developer/Platforms")).into_iter().flatten().flatten().filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".platform").map(String::from)).collect();
    platforms.sort();
    let sims: Vec<&str> = platforms.iter().filter(|p| p.ends_with("Simulator")).map(|s| s.as_str()).collect();
    let sdk_state = if !bundle.exists() {
        "missing"
    } else if stamp != expected {
        "stale"
    } else if sims.is_empty() {
        "warn"
    } else {
        "ok"
    };
    let xip_fix = "x86sim dev setup sdk --xip /path/to/Xcode_27.xip (Xcode 27 from developer.apple.com/download/all, free Apple ID)";
    v.push(piece(
        "sdk",
        "iOS SDK",
        sdk_state,
        match sdk_state {
            "missing" if md == "no-xcode" => "no-Xcode mode: device builds use the stubs from sdk-free; simulator builds need the full SDK".to_string(),
            "missing" => "not built yet: it comes from Xcode.xip (one download from Apple)".to_string(),
            "stale" => format!("built by an older xtool (stamp '{}', the pinned xtool needs '{expected}'): rebuilt from Xcode.xip", if stamp.is_empty() { "none" } else { &stamp }),
            "warn" => format!("{} without simulator platforms: an Xcode with the simulator SDKs is needed for x86Sim builds", platforms.join(", ")),
            _ => format!("{} ({})", platforms.join(", "), bundle.display()),
        },
        match sdk_state {
            "ok" => "",
            "stale" => "x86sim dev setup repair (or setup sdk --xip PATH)",
            _ => xip_fix,
        },
    ));

    // device installs and publishing (optional for x86Sim)
    let venv = home().join("pymobile3-venv/bin/python").exists();
    let auth = std::fs::read_dir(home().join(".local/share/xtool")).is_ok_and(|mut d| d.next().is_some());
    v.push(piece(
        "device",
        "Real devices",
        if venv && auth { "ok" } else { "warn" },
        match (venv, auth) {
            (true, true) => "pymobiledevice3 and an Apple ID sign-in: device installs work".to_string(),
            (true, false) => "pymobiledevice3 ready; not signed in to an Apple ID (only for installs on a real iPhone)".to_string(),
            (false, _) => "pymobiledevice3 not installed (only for real iPhones; the toolchain step installs it)".to_string(),
        },
        if auth { "" } else { "x86sim dev auth (interactive Apple ID sign-in, in a terminal)" },
    ));

    // the Swift toolchain's lldb for `x86sim debug` (optional: /usr/bin/lldb is the fallback)
    if let Some(p) = swift_lldb_status() {
        v.push(p);
    }

    let ok = |id: &str| v.iter().any(|p| p.id == id && (p.state == "ok" || (id == "xtool" && p.state == "warn")));
    let ready_sim = fork_ok && ok("swift") && ok("xtool") && ok("sdk");
    let ready_device = ready_sim && platforms.iter().any(|p| p == "iPhoneOS") && venv && auth;
    Status { fork: dir.clone(), fork_version: if fork_ok { fork_version(&dir) } else { String::new() }, mode: md, pieces: v, platforms, ready_sim, ready_device, job: job() }
}

// ---------------------------------------------------------------------------------------------------------------
// the setup job: one at a time, in the background, shared by the GUI, MCP and the CLI's --bg

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    /// what runs: swift, toolchain, sdk, repair, verify, all
    pub kind: String,
    pub pid: u32,
    pub running: bool,
    /// the exit code once finished
    pub rc: Option<i32>,
    pub log: PathBuf,
    /// the installer's last "== N. step ==" heading
    pub step: String,
    /// the last line (a download's progress included)
    pub last: String,
    /// 0..1 from the steps seen so far
    pub progress: f32,
    pub started: u64,
}

fn alive(pid: u32) -> bool {
    pid > 0 && unsafe { libc::kill(pid as i32, 0) == 0 } && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.rsplit(')').next().is_some_and(|r| r.trim_start().starts_with('Z')))
}

/// The steps a job goes through (its "== " headings), for its progress
fn job_steps(kind: &str) -> &'static [&'static str] {
    match kind {
        "swift" => &["== swift: download", "== swift: unpack", "== swift: check"],
        "verify" => &["== verify: new project", "== verify: build"],
        "lldb" => &["== swift lldb: /", "== swift lldb: download", "== swift lldb: unpack", "== swift lldb: toolchain view"],
        "repair" => &["== repair", "Installed actool", "Building OpenAppleMacrosServer"],
        _ => &["== swift", "== 1", "== 3.", "== 4.", "== 5.", "== 6.", "Installed actool", "Building OpenAppleMacrosServer", "== 7."],
    }
}

/// The last job (running or finished), if any
pub fn job() -> Option<Job> {
    let d = work_dir();
    let pid: u32 = std::fs::read_to_string(d.join("job.pid")).ok()?.trim().parse().ok()?;
    let kind = std::fs::read_to_string(d.join("job.kind")).unwrap_or_default().trim().to_string();
    let rc = std::fs::read_to_string(d.join("job.rc")).ok().and_then(|s| s.trim().parse().ok());
    let log = d.join("job.log");
    let started = std::fs::metadata(d.join("job.pid")).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let text = tail_bytes(&log, 256 * 1024);
    let steps = job_steps(&kind);
    let mut seen = 0usize;
    let mut step = String::new();
    for l in text.lines() {
        if let Some(i) = steps.iter().position(|s| l.starts_with(s)) {
            seen = seen.max(i + 1);
        }
        if l.starts_with("== ") {
            step = l.trim_matches(|c| c == '=' || c == ' ').to_string();
        }
    }
    let last = text.lines().rev().map(|l| l.rsplit('\r').find(|s| !s.trim().is_empty()).unwrap_or("").trim().to_string()).find(|l| !l.is_empty()).unwrap_or_default();
    let running = rc.is_none() && alive(pid);
    let progress = if rc == Some(0) { 1.0 } else { seen as f32 / (steps.len() as f32 + 1.0) };
    Some(Job { kind, pid, running, rc: rc.or(if running { None } else { Some(-1) }), log, step, last, progress, started })
}

pub fn tail_bytes(p: &Path, max: u64) -> String {
    let Ok(mut f) = std::fs::File::open(p) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max {
        use std::io::Seek;
        let _ = f.seek(std::io::SeekFrom::Start(len - max));
    }
    let mut b = vec![];
    let _ = f.read_to_end(&mut b);
    String::from_utf8_lossy(&b).into_owned()
}

/// The x86sim-dev binary next to this one (the GUI, MCP server and CLI are built together)
pub fn dev_exe() -> PathBuf {
    let side = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("x86sim-dev")));
    side.filter(|p| p.exists()).unwrap_or_else(|| repo_root().join("tools/launcher/target/release/x86sim-dev"))
}

/// Starts `x86sim-dev setup <kind> <args>` in the background (its own session: it outlives the GUI). One job at a time.
pub fn start_job(kind: &str, args: &[String]) -> Result<Job> {
    if let Some(j) = job().filter(|j| j.running) {
        bail!("a setup job is already running ({}, pid {}): wait for it or cancel it", j.kind, j.pid);
    }
    anyhow::ensure!(matches!(kind, "swift" | "toolchain" | "sdk" | "repair" | "verify" | "lldb" | "all"), "unknown setup step {kind:?}: swift, toolchain, sdk, repair, verify, lldb or all");
    let d = work_dir();
    std::fs::create_dir_all(&d)?;
    for f in ["job.rc", "job.log"] {
        let _ = std::fs::remove_file(d.join(f));
    }
    std::fs::write(d.join("job.kind"), kind)?;
    let child = Command::new("setsid")
        .arg("sh")
        .arg("-c")
        .arg(r#"log=$1; rc=$2; shift 2; "$@" > "$log" 2>&1 < /dev/null; echo $? > "$rc""#)
        .arg("sh")
        .arg(d.join("job.log"))
        .arg(d.join("job.rc"))
        .arg(dev_exe())
        .arg("setup")
        .arg(kind)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting the setup job (setsid)")?;
    std::fs::write(d.join("job.pid"), child.id().to_string())?;
    // reap it in the background so it never lingers as a zombie of a long-lived GUI / MCP server
    std::thread::spawn(move || {
        let mut c = child;
        let _ = c.wait();
    });
    std::thread::sleep(Duration::from_millis(200));
    job().context("job vanished")
}

/// Stops the running setup job (its whole session: the installer and what it started)
pub fn cancel_job() -> Result<()> {
    let j = job().filter(|j| j.running).context("no setup job is running")?;
    unsafe {
        libc::kill(-(j.pid as i32), libc::SIGTERM);
    }
    std::fs::write(work_dir().join("job.rc"), "143")?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// setup steps (run by `x86sim dev setup`, in the foreground: the job above runs them through x86sim-dev)

/// Runs a command with its output passed to `say` line by line (stdout and stderr together). Returns the exit code.
pub fn stream(mut cmd: Command, say: &mut dyn FnMut(&str)) -> Result<i32> {
    let (mut r, w) = std::io::pipe()?;
    let w2 = w.try_clone()?;
    let mut child = cmd.stdout(w).stderr(w2).stdin(Stdio::null()).spawn().with_context(|| format!("running {:?}", cmd.get_program()))?;
    drop(cmd);
    let mut buf = vec![0u8; 8192];
    let mut line = Vec::new();
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' {
                say(&String::from_utf8_lossy(&line));
                line.clear();
            } else {
                line.push(b);
            }
        }
    }
    if !line.is_empty() {
        say(&String::from_utf8_lossy(&line));
    }
    Ok(child.wait()?.code().unwrap_or(-1))
}

/// swift.org's build for this distribution (directory, file suffix)
fn swift_flavor() -> (&'static str, &'static str) {
    let (id, like, ver) = linux_id();
    let fam = format!("{id} {like}");
    if fam.contains("ubuntu") || id == "debian" || fam.contains("debian") {
        if id == "debian" {
            return ("debian12", "debian12");
        }
        if ver.starts_with("22.") {
            return ("ubuntu2204", "ubuntu22.04");
        }
        return ("ubuntu2404", "ubuntu24.04");
    }
    // Fedora, RHEL and everything else (Arch: with the narrow-curses aliases the fork's --curses-compat makes)
    ("ubi9", "ubi9")
}

pub fn swift_url() -> String {
    let (d, f) = swift_flavor();
    format!("https://download.swift.org/swift-{SWIFT_VERSION}-release/{d}/swift-{SWIFT_VERSION}-RELEASE/swift-{SWIFT_VERSION}-RELEASE-{f}.tar.gz")
}

/// Installs swift.org's toolchain into dev_home()/toolchains and links dev_home()/swift to it (no root). env() puts it
/// first on PATH for everything the fork runs.
pub fn install_swift(say: &mut dyn FnMut(&str)) -> Result<()> {
    let base = dev_home();
    let (_, f) = swift_flavor();
    let name = format!("swift-{SWIFT_VERSION}-RELEASE-{f}");
    let dest = base.join("toolchains").join(&name);
    std::fs::create_dir_all(base.join("toolchains"))?;
    if !dest.join("usr/bin/swift").exists() {
        let url = std::env::var("X86SIM_DEV_SWIFT_URL").unwrap_or_else(|_| swift_url());
        let tgz = base.join(format!("{name}.tar.gz"));
        say(&format!("== swift: download {url}"));
        let mut c = Command::new("curl");
        c.args(["-fL", "--retry", "3", "-C", "-", "--progress-bar", "-o"]).arg(&tgz).arg(&url);
        anyhow::ensure!(stream(c, say)? == 0, "download failed");
        say(&format!("== swift: unpack into {}", dest.display()));
        let tmp = base.join("toolchains").join(format!(".{name}.tmp"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        // keep the usr/ level: SwiftPM finds the toolchain by walking up from the binary to usr/lib/swift
        let mut t = Command::new("tar");
        t.arg("-xzf").arg(&tgz).arg("--strip-components=1").arg("-C").arg(&tmp);
        anyhow::ensure!(stream(t, say)? == 0, "unpacking failed");
        anyhow::ensure!(tmp.join("usr/bin/swift").exists(), "no usr/bin/swift in the archive");
        std::fs::rename(&tmp, &dest)?;
        let _ = std::fs::remove_file(&tgz);
    }
    let link = base.join("swift");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(dest.strip_prefix(&base).unwrap_or(&dest), &link)?;
    say("== swift: check");
    // a toolchain built for RHEL / Ubuntu needs the narrow curses sonames (Arch has only the wide ones): the fork's
    // --curses-compat aliases them in a private dir and in the toolchain's own lib dirs
    let runs = |bin: &str| Command::new(dest.join("usr/bin").join(bin)).arg("--version").envs(env()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !runs("swift-frontend") || !runs("swift") {
        say("the toolchain misses libraries this system names differently: adding aliases (install-toolchain.sh --curses-compat)");
        anyhow::ensure!(fork_dir().join("install-toolchain.sh").exists(), "{} missing", fork_dir().display());
        let mut c = command("bash");
        c.arg(fork_dir().join("install-toolchain.sh")).arg("--curses-compat").arg(&dest);
        stream(c, say)?;
        anyhow::ensure!(runs("swift"), "swift still does not start (see the unresolved libraries above)");
    }
    let (p, ver) = swift().context("swift still not found")?;
    say(&format!("Swift {ver} at {}", p.display()));
    Ok(())
}

/// cli/libexec/swiftlldb.py: the Swift toolchain's lldb made to run for `x86sim debug` (a private libpython, the Darwin
/// Swift resource dir). The script holds the logic; `x86sim debug` runs the same setup on first use.
fn swift_lldb_script() -> PathBuf {
    repo_root().join("cli/libexec/swiftlldb.py")
}

/// The "lldb" status piece: None when the script is not there (an old payload)
fn swift_lldb_status() -> Option<Piece> {
    let script = swift_lldb_script();
    if !script.exists() {
        return None;
    }
    let out = command("python3").arg(&script).args(["status", "--json"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let state = match v["state"].as_str() {
        Some("ok") => "ok",
        Some("missing") => "missing",
        _ => "warn",
    };
    let detail = v["detail"].as_str().unwrap_or("").to_string();
    let detail = if state == "ok" { format!("{detail} (x86sim debug: Swift values, po, expression)") } else { format!("{detail}; x86sim debug uses /usr/bin/lldb (no Swift values)") };
    Some(piece("lldb", "Swift lldb", state, detail, v["fix"].as_str().unwrap_or("")))
}

/// Sets up the Swift toolchain's lldb (cli/libexec/swiftlldb.py setup). Returns its exit code.
pub fn setup_lldb(say: &mut dyn FnMut(&str)) -> Result<i32> {
    let script = swift_lldb_script();
    anyhow::ensure!(script.exists(), "{} not found", script.display());
    let mut c = command("python3");
    c.arg(&script).arg("setup");
    stream(c, say)
}

/// install-toolchain.sh in user-only mode (no sudo, no system packages): xtool, rcodesign, ipsw, pymobiledevice3, and
/// the SDK when an Xcode.xip is given (or mode no-xcode). Returns its exit code.
pub fn run_installer(mode: Option<&str>, xip: Option<&Path>, repair: bool, say: &mut dyn FnMut(&str)) -> Result<i32> {
    let script = fork_dir().join("install-toolchain.sh");
    anyhow::ensure!(script.exists(), "{} not found (git submodule update --init external/omarchy-apple-dev)", script.display());
    let mut c = command("bash");
    c.arg(&script).current_dir(fork_dir());
    if let Some(m) = mode.filter(|m| !m.is_empty()) {
        c.args(["--mode", m]);
    } else if self::mode().is_empty() {
        c.args(["--mode", "full"]);
    }
    c.arg(if repair { "--repair" } else { "--user-only" });
    if let Some(x) = xip {
        anyhow::ensure!(x.is_file(), "{} is not a file", x.display());
        c.env("XCODE_XIP", x);
    }
    stream(c, say)
}

/// Builds a fresh `xtool new` project for the simulator in the work dir: the whole toolchain end to end
pub fn verify(say: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    let d = work_dir().join("verify");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d)?;
    say("== verify: new project");
    let p = new_project("Verify", &d, say)?;
    say("== verify: build");
    let app = build(&p, &BuildOpts { platform: "ios".into(), release: false }, say);
    // a check, not a project of the user's: out of the recent list again
    let mut s = load_settings();
    let root = d_canon(&d_root());
    s.recent.retain(|d| !d.starts_with(&root));
    let _ = save_settings(&s);
    app
}

fn d_root() -> PathBuf {
    work_dir().join("verify")
}

fn d_canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or(p.to_path_buf())
}

// ---------------------------------------------------------------------------------------------------------------
// projects

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// recently built projects, newest first
    pub recent: Vec<PathBuf>,
    /// the phone the last run used (its folder)
    pub phone: String,
    /// the Xcode.xip path last typed in
    pub xip: String,
}

pub fn load_settings() -> Settings {
    std::fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_settings(s: &Settings) -> Result<()> {
    let p = settings_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    crate::aot::write_json(&p, s)
}

pub fn remember(dir: &Path, phone: Option<&str>) {
    let mut s = load_settings();
    let dir = dir.canonicalize().unwrap_or(dir.to_path_buf());
    s.recent.retain(|d| d != &dir);
    s.recent.insert(0, dir);
    s.recent.truncate(12);
    if let Some(p) = phone {
        s.phone = p.to_string();
    }
    let _ = save_settings(&s);
}

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub dir: PathBuf,
    pub name: String,
    /// xtool (Package.swift + xtool.yml: builds here), flutter, swiftpm (no xtool.yml), xcodeproj (convert it first)
    pub kind: String,
    pub bundle_id: String,
    /// the last simulator build's .app, if any
    pub app: Option<PathBuf>,
}

fn yml_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix(&format!("{key}:"))).map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|v| !v.is_empty())
}

pub fn project(dir: &Path) -> Result<Project> {
    let dir = dir.canonicalize().with_context(|| format!("{} not found", dir.display()))?;
    anyhow::ensure!(dir.is_dir(), "{} is not a directory", dir.display());
    let base = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let yml = std::fs::read_to_string(dir.join("xtool.yml")).ok();
    let kind = if yml.is_some() && dir.join("Package.swift").exists() {
        "xtool"
    } else if dir.join("pubspec.yaml").exists() {
        "flutter"
    } else if dir.join("Package.swift").exists() {
        "swiftpm"
    } else if std::fs::read_dir(&dir)?.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".xcodeproj")) {
        "xcodeproj"
    } else {
        bail!("{}: not an app project (no xtool.yml + Package.swift, pubspec.yaml or .xcodeproj)", dir.display());
    };
    let y = yml.unwrap_or_default();
    let name = yml_value(&y, "product").unwrap_or(base);
    let app = std::fs::read_dir(dir.join("xtool")).into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "app"));
    Ok(Project { name, kind: kind.into(), bundle_id: yml_value(&y, "bundleID").unwrap_or_default(), app, dir })
}

/// The recent projects that still exist
pub fn recent() -> Vec<Project> {
    load_settings().recent.iter().filter_map(|d| project(d).ok()).collect()
}

/// `xtool new NAME` in PARENT (a SwiftUI app); its platforms are widened to every simulator x86Sim runs (the template
/// has iOS and macOS only). Returns the project dir.
pub fn new_project(name: &str, parent: &Path, say: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    anyhow::ensure!(!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()), "the name must be letters, digits and _ (a Swift identifier)");
    std::fs::create_dir_all(parent)?;
    let dir = parent.join(name);
    anyhow::ensure!(!dir.exists(), "{} already exists", dir.display());
    let xt = home().join(".local/bin/xtool");
    anyhow::ensure!(xt.exists(), "xtool is not installed: x86sim dev setup toolchain");
    let mut c = command(&xt);
    c.args(["new", name, "--skip-setup"]).current_dir(parent);
    anyhow::ensure!(stream(c, say)? == 0, "xtool new failed");
    let pkg = dir.join("Package.swift");
    if let Ok(s) = std::fs::read_to_string(&pkg) {
        if !s.contains(".tvOS(") {
            let s = s.replacen(".macOS(.v14),", ".macOS(.v14),\n        .tvOS(.v17),\n        .watchOS(.v10),\n        .visionOS(.v1),", 1);
            std::fs::write(&pkg, s)?;
        }
    }
    Ok(dir)
}

// ---------------------------------------------------------------------------------------------------------------
// build and run

#[derive(Debug, Clone)]
pub struct BuildOpts {
    /// ios, tvos, watchos, xros (a phone's Instance::platform)
    pub platform: String,
    pub release: bool,
}

pub fn triple(platform: &str) -> String {
    match platform {
        "tvos" | "watchos" | "xros" => format!("arm64-apple-{platform}-simulator"),
        _ => "arm64-apple-ios-simulator".into(),
    }
}

/// One Swift build at a time on this computer (they are heavy): an flock on the work dir's build.lock
struct BuildLock(std::fs::File);
impl BuildLock {
    fn take(say: &mut dyn FnMut(&str)) -> Result<BuildLock> {
        std::fs::create_dir_all(work_dir())?;
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(work_dir().join("build.lock"))?;
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            say("waiting for another build to finish…");
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        }
        Ok(BuildLock(f))
    }
}
impl Drop for BuildLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Builds an xtool project for the simulator with ship.sh --simulator. Output goes to `say` and to
/// work_dir()/build-<name>.log. Returns the .app.
pub fn build(dir: &Path, o: &BuildOpts, say: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    let p = project(dir)?;
    match p.kind.as_str() {
        "xtool" => {}
        "flutter" => bail!("{}: Flutter apps build for real iPhones only (x86sim dev device / ship); simulator builds need an xtool project", p.dir.display()),
        "xcodeproj" => bail!("{}: an Xcode project: convert it first (x86sim dev convert {}/<App>.xcodeproj)", p.dir.display(), p.dir.display()),
        _ => bail!("{}: a SwiftPM package without xtool.yml (add one: `version: 1` and `bundleID: com.you.App`)", p.dir.display()),
    }
    let st = status();
    if !st.ready_sim {
        let why: Vec<String> = st.pieces.iter().filter(|x| matches!(x.id, "swift" | "xtool" | "sdk" | "fork") && x.state != "ok" && x.state != "warn").map(|x| format!("{}: {}", x.title, x.detail)).collect();
        if !why.is_empty() {
            bail!("the toolchain is not ready ({}); see x86sim dev status", why.join("; "));
        }
    }
    let _lock = BuildLock::take(say)?;
    let log_path = work_dir().join(format!("build-{}.log", p.name));
    let mut log = std::fs::File::create(&log_path).ok();
    let t = triple(&o.platform);
    say(&format!("== build {} for {t} ({})", p.name, if o.release { "release" } else { "debug" }));
    let mut c = command("bash");
    c.arg(fork_dir().join("ship.sh")).arg("--simulator").current_dir(&p.dir).env("TRIPLE", &t).env("CONFIGURATION", if o.release { "release" } else { "debug" });
    let mut app_line = None;
    let rc = stream(c, &mut |l: &str| {
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "{l}");
        }
        if let Some(a) = l.strip_prefix("simulator app ready: ") {
            app_line = Some(a.trim().to_string());
        }
        say(l);
    })?;
    anyhow::ensure!(rc == 0, "build failed (exit {rc}); log: {}", log_path.display());
    let app = app_line.map(|a| p.dir.join(a)).or(project(&p.dir)?.app).context("the build wrote no .app")?;
    remember(&p.dir, None);
    say(&format!("app: {}", app.display()));
    Ok(app)
}

#[derive(Debug, Clone)]
pub struct RunOpts {
    pub release: bool,
    /// start the phone when it's off (else an error)
    pub start: bool,
    /// skip the build: install the last one
    pub no_build: bool,
    /// arguments for the app
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub phone: String,
    pub app: PathBuf,
    pub bundle_id: String,
    pub installed: String,
    pub launched: String,
    pub secs: f64,
}

fn x86sim_cli(args: &[&std::ffi::OsStr]) -> Result<String> {
    let out = Command::new(repo_root().join("cli/x86sim")).args(args).current_dir(repo_root()).stdin(Stdio::null()).output().context("running cli/x86sim")?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("{}", if err.is_empty() { text } else { err });
    }
    Ok(text)
}

/// The phone's state from the monitor: (state, home)
fn phone_state(inst: &Instance) -> (String, bool) {
    crate::instance::summaries(0).into_iter().find(|s| s.id == inst.id).map(|s| (s.state, s.home)).unwrap_or(("off".into(), false))
}

/// Waits until the phone runs (its home screen, or for runtimes the detector doesn't know, just running)
pub fn wait_running(inst: &Instance, secs: u64, say: &mut dyn FnMut(&str)) -> Result<()> {
    let t0 = Instant::now();
    let mut said = String::new();
    loop {
        let (state, home) = phone_state(inst);
        if state != said {
            say(&format!("{}: {state}", inst.name));
            said = state.clone();
        }
        if state == "running" && (home || t0.elapsed() > Duration::from_secs(120)) {
            return Ok(());
        }
        if state == "off" && t0.elapsed() > Duration::from_secs(30) {
            bail!("{} did not start (see its boot log in the launcher)", inst.name);
        }
        anyhow::ensure!(t0.elapsed() < Duration::from_secs(secs), "{} not running after {secs} s", inst.name);
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The key loop: build for the phone's platform, start the phone if asked, install and launch
pub fn run(dir: &Path, inst: &Instance, o: &RunOpts, say: &mut dyn FnMut(&str)) -> Result<RunResult> {
    let t0 = Instant::now();
    let p = project(dir)?;
    let app = if o.no_build {
        p.app.clone().context("no earlier build (xtool/<App>.app): build first")?
    } else {
        build(&p.dir, &BuildOpts { platform: inst.platform.clone(), release: o.release }, say)?
    };
    let bundle_id = crate::apps::bundle_id(&app).unwrap_or(p.bundle_id.clone());
    let (state, _) = phone_state(inst);
    if state == "off" {
        anyhow::ensure!(o.start, "{} is off: start it (launcher, MCP sim_start) or pass --start", inst.name);
        say(&format!("== start {}", inst.name));
        crate::sim::start(&inst.dir)?;
        wait_running(inst, 1200, say)?;
    } else if state != "running" {
        wait_running(inst, 1200, say)?;
    }
    say(&format!("== install {} on {}", bundle_id, inst.name));
    // a phone that just started may not answer installs yet: retry for a few minutes (as the fork's sim-run.sh)
    let mut installed = String::new();
    for i in 0..8 {
        match x86sim_cli(&["install".as_ref(), inst.dir.as_os_str(), app.as_os_str()]) {
            Ok(a) => {
                installed = a;
                break;
            }
            Err(e) if i < 7 && format!("{e:#}").contains("no answer") => {
                say("the phone does not answer yet; trying again in 20 s");
                std::thread::sleep(Duration::from_secs(20));
            }
            Err(e) => return Err(e.context("install")),
        }
    }
    say(&installed);
    say(&format!("== launch {bundle_id}"));
    let mut a: Vec<&std::ffi::OsStr> = vec!["launch".as_ref(), inst.dir.as_os_str(), "--terminate-running-process".as_ref(), bundle_id.as_ref()];
    a.extend(o.args.iter().map(|s| std::ffi::OsStr::new(s.as_str())));
    let mut launched = String::new();
    for i in 0..6 {
        match x86sim_cli(&a) {
            Ok(l) => {
                launched = l;
                break;
            }
            Err(e) if i < 5 && !format!("{e:#}").contains("unknown to FrontBoard") => {
                say(&format!("launch: {e:#}; trying again in 15 s"));
                std::thread::sleep(Duration::from_secs(15));
            }
            Err(e) => return Err(e.context("launch")),
        }
    }
    say(&launched);
    remember(&p.dir, Some(&inst.id));
    Ok(RunResult { phone: inst.name.clone(), app, bundle_id, installed, launched, secs: t0.elapsed().as_secs_f64() })
}

#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub run: RunResult,
    /// the app's pid after the wait (None: it exited or crashed)
    pub pid: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub passed: bool,
}

/// A smoke test: run, wait `secs`, check the app is still running, take a screenshot
pub fn smoke_test(dir: &Path, inst: &Instance, o: &RunOpts, secs: u64, shot: Option<&Path>, say: &mut dyn FnMut(&str)) -> Result<TestResult> {
    let r = run(dir, inst, o, say)?;
    say(&format!("== wait {secs} s"));
    std::thread::sleep(Duration::from_secs(secs));
    let pid = x86sim_cli(&["pidof".as_ref(), inst.dir.as_os_str(), r.bundle_id.as_ref()]).ok().filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()));
    let out = shot.map(Path::to_path_buf).unwrap_or_else(|| work_dir().join(format!("test-{}-{}.png", r.bundle_id, inst.id)));
    let screenshot = x86sim_cli(&["io".as_ref(), inst.dir.as_os_str(), "screenshot".as_ref(), out.as_os_str()]).ok().map(|_| out).filter(|p| p.exists());
    say(&format!("pid: {}", pid.as_deref().unwrap_or("not running")));
    if let Some(s) = &screenshot {
        say(&format!("screenshot: {}", s.display()));
    }
    Ok(TestResult { passed: pid.is_some(), run: r, pid, screenshot })
}

// ---------------------------------------------------------------------------------------------------------------
// pass-throughs (the CLI runs them on the terminal; MCP captures them)

/// The fork's script for a verb: device -> device-run.sh, ship -> ship.sh (flutter/ship.sh for a Flutter app),
/// ship-mac -> ship-mac.sh, convert -> tools/xcodeproj2xtool.py
pub fn passthrough(verb: &str, dir: &Path, args: &[String]) -> Result<Command> {
    let f = fork_dir();
    let kind = project(dir).map(|p| p.kind).unwrap_or_default();
    let mut c = match verb {
        "device" => {
            let mut c = command("bash");
            c.arg(f.join("device-run.sh"));
            c
        }
        "ship" if kind == "flutter" => {
            let mut c = command("bash");
            c.arg(f.join("flutter/ship.sh"));
            c
        }
        "ship" => {
            let mut c = command("bash");
            c.arg(f.join("ship.sh"));
            c
        }
        "ship-mac" => {
            let mut c = command("bash");
            c.arg(f.join("ship-mac.sh"));
            c
        }
        "convert" => {
            let mut c = command("python3");
            c.arg("-I").arg(f.join("tools/xcodeproj2xtool.py"));
            c
        }
        _ => bail!("unknown verb {verb}"),
    };
    c.args(args).current_dir(dir);
    // flutter/ship.sh [--upload] <app dir>
    if verb == "ship" && kind == "flutter" {
        c.arg(dir);
    }
    Ok(c)
}

/// The phone a dev command is about: a name / folder / path, else the only running phone, else the one the last run
/// used, else the only phone
pub fn pick_phone(key: Option<&str>) -> Result<Instance> {
    let names = || crate::instance::list().iter().map(|i| i.name.clone()).collect::<Vec<_>>().join(", ");
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        return crate::instance::find(k).ok_or_else(|| anyhow::anyhow!("no phone {k:?} (phones: {})", names()));
    }
    let running: Vec<_> = crate::instance::summaries(0).into_iter().filter(|s| s.state != "off").collect();
    if running.len() == 1 {
        return crate::instance::find(&running[0].id).context("phone vanished");
    }
    if running.is_empty() {
        let last = load_settings().phone;
        if let Some(i) = crate::instance::find(&last).filter(|_| !last.is_empty()) {
            return Ok(i);
        }
        let all = crate::instance::list();
        if all.len() == 1 {
            return Ok(all[0].clone());
        }
    }
    bail!("say which phone (--phone NAME; phones: {})", names())
}

// ---------------------------------------------------------------------------------------------------------------
// MCP tools (x86sim-mcp merges these into its list and sends dev_* calls here)

pub fn mcp_tools() -> Vec<serde_json::Value> {
    use serde_json::json;
    let phone = json!({"type": "string", "description": "the phone: its name, folder (device-a64-qa) or path. Default: the only running phone, else the one the last run used"});
    let project = json!({"type": "string", "description": "the project directory (an xtool project: Package.swift + xtool.yml)"});
    let args = json!({"type": "array", "items": {"type": "string"}});
    vec![
        json!({"name": "dev_status", "description": "Develop (omarchy-apple-dev): the app toolchain's state: Swift, xtool, the iOS SDK (and its simulator platforms), device tools; ready_sim says whether simulator builds work. Includes the running / last setup job with its step and log tail, and the recent projects.",
               "inputSchema": {"type": "object", "properties": {}}}),
        json!({"name": "dev_setup", "description": "Start installing a piece of the toolchain in the background (no root needed; poll dev_status for its progress). step: all (Swift if missing, then the installer, then a test build), swift (swift.org's toolchain into the user's data dir), toolchain (xtool built from source ~7 min, rcodesign, ipsw, pymobiledevice3), sdk (needs xip: the path of an Xcode 27 .xip the user downloaded from developer.apple.com), repair (re-register the SDK after a Swift swap), verify (build a new app end to end), lldb (the Swift toolchain's lldb for debug_start: a private libpython, ~75 MB; debug_start also does it on first use).",
               "inputSchema": {"type": "object", "properties": {"step": {"type": "string", "enum": ["all", "swift", "toolchain", "sdk", "repair", "verify", "lldb"]}, "mode": {"type": "string", "enum": ["full", "no-xcode"]}, "xip": {"type": "string"}}, "required": ["step"]}}),
        json!({"name": "dev_setup_cancel", "description": "Stop the running setup job.", "inputSchema": {"type": "object", "properties": {}}}),
        json!({"name": "dev_new", "description": "Make a new SwiftUI app project (xtool new) in parent (default: ~/x86sim-apps), set up for every simulator platform. Returns its directory.",
               "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "a Swift identifier, e.g. HelloApp"}, "parent": {"type": "string"}}, "required": ["name"]}}),
        json!({"name": "dev_projects", "description": "Recently built app projects: name, kind, bundle id, directory, last build.", "inputSchema": {"type": "object", "properties": {}}}),
        json!({"name": "dev_build", "description": "Build an xtool project for the simulator (omarchy-apple-dev ship.sh --simulator), for a phone's platform (or platform: ios, tvos, watchos, xros). Returns the .app path and the build log's tail.",
               "inputSchema": {"type": "object", "properties": {"project": project, "phone": phone, "platform": {"type": "string", "enum": ["ios", "tvos", "watchos", "xros"]}, "release": {"type": "boolean"}}, "required": ["project"]}}),
        json!({"name": "dev_run", "description": "Build an app project for a phone, install it and launch it (the edit-build-run loop). start=true boots the phone first when it is off (minutes). args go to the app. Returns the bundle id, the install / launch answers and the log's tail.",
               "inputSchema": {"type": "object", "properties": {"project": project, "phone": phone, "start": {"type": "boolean"}, "release": {"type": "boolean"}, "no_build": {"type": "boolean"}, "args": args}, "required": ["project"]}}),
        json!({"name": "dev_test", "description": "Smoke-test an app project on a phone: build, install, launch, wait secs (default 10), check it is still running, take a screenshot (PNG path in the answer). passed=false when the app died.",
               "inputSchema": {"type": "object", "properties": {"project": project, "phone": phone, "start": {"type": "boolean"}, "secs": {"type": "integer"}, "screenshot": {"type": "string"}}, "required": ["project"]}}),
        json!({"name": "dev_device", "description": "omarchy-apple-dev device-run.sh in the project: build, sign, install and launch on a real iPhone over USB or the tailnet (needs a paired iPhone and xtool auth). args are device-run.sh's (e.g. [\"--console\", \"20\"]); interactive LLDB is not available here. Runs up to 30 minutes; returns the output's tail.",
               "inputSchema": {"type": "object", "properties": {"project": project, "args": args}, "required": ["project"]}}),
        json!({"name": "dev_ship", "description": "omarchy-apple-dev ship.sh in the project (flutter/ship.sh for a Flutter app): an App Store .ipa, signed and validated offline; args [\"--upload\"] uploads to TestFlight (needs ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_PATH in the environment), [\"--device\", TEAMID], [\"--xcarchive\"]. ship_mac=true runs ship-mac.sh. Returns the output's tail.",
               "inputSchema": {"type": "object", "properties": {"project": project, "args": args, "ship_mac": {"type": "boolean"}}, "required": ["project"]}}),
        json!({"name": "dev_convert", "description": "Convert an Xcode project to an xtool project (omarchy-apple-dev tools/xcodeproj2xtool.py). args: its options (--out DIR, --target NAME, ...).",
               "inputSchema": {"type": "object", "properties": {"xcodeproj": {"type": "string"}, "args": args}, "required": ["xcodeproj"]}}),
    ]
}

/// Last `n` lines of collected output
fn last_lines(v: &[String], n: usize) -> String {
    v[v.len().saturating_sub(n)..].join("\n")
}

/// A pass-through run with a time limit, output collected
fn captured(mut c: Command, secs: u64) -> Result<(i32, Vec<String>)> {
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let l2 = lines.clone();
    c.process_group(0);
    let (mut r, w) = std::io::pipe()?;
    let w2 = w.try_clone()?;
    let mut child = c.stdout(w).stderr(w2).stdin(Stdio::null()).spawn()?;
    drop(c);
    let pid = child.id() as i32;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = r.read_to_string(&mut s);
        l2.lock().unwrap().extend(s.lines().map(String::from));
    });
    let t0 = Instant::now();
    let rc = loop {
        if let Some(s) = child.try_wait()? {
            break s.code().unwrap_or(-1);
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            unsafe { libc::kill(-pid, libc::SIGTERM) };
            let _ = child.wait();
            break 124;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = reader.join();
    let v = lines.lock().unwrap().clone();
    Ok((rc, v))
}

use std::os::unix::process::CommandExt;

pub fn mcp_call(name: &str, a: &serde_json::Value) -> Result<serde_json::Value> {
    use serde_json::json;
    let s = |k: &str| -> String { a[k].as_str().unwrap_or("").to_string() };
    let strs = |k: &str| -> Vec<String> { a[k].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default() };
    let mut log: Vec<String> = vec![];
    Ok(match name {
        "dev_status" => {
            let st = status();
            let tail = st.job.as_ref().map(|j| tail_bytes(&j.log, 6000).lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
            json!({"status": st, "job_log_tail": tail, "recent": recent()})
        }
        "dev_setup" => {
            let mut args = vec![];
            if !s("mode").is_empty() {
                args.extend(["--mode".to_string(), s("mode")]);
            }
            if !s("xip").is_empty() {
                args.extend(["--xip".to_string(), s("xip")]);
            }
            let j = start_job(&s("step"), &args)?;
            json!({"job": j, "note": "runs in the background: poll dev_status (job.step, job.progress, job_log_tail)"})
        }
        "dev_setup_cancel" => {
            cancel_job()?;
            json!({"cancelled": true})
        }
        "dev_new" => {
            let parent = if s("parent").is_empty() { home().join("x86sim-apps") } else { PathBuf::from(s("parent")) };
            let d = new_project(&s("name"), &parent, &mut |l| log.push(l.to_string()))?;
            remember(&d, None);
            json!({"project": project(&d)?, "log": last_lines(&log, 20)})
        }
        "dev_projects" => json!({"projects": recent()}),
        "dev_build" => {
            let platform = if !s("platform").is_empty() { s("platform") } else if !s("phone").is_empty() { pick_phone(Some(&s("phone")))?.platform } else { "ios".into() };
            let r = build(Path::new(&s("project")), &BuildOpts { platform, release: a["release"].as_bool().unwrap_or(false) }, &mut |l| log.push(l.to_string()));
            match r {
                Ok(app) => json!({"app": app, "bundle_id": crate::apps::bundle_id(&app), "log_tail": last_lines(&log, 25)}),
                Err(e) => bail!("{e:#}\n{}", last_lines(&log, 40)),
            }
        }
        "dev_run" | "dev_test" => {
            let inst = pick_phone(a["phone"].as_str())?;
            let o = RunOpts { release: a["release"].as_bool().unwrap_or(false), start: a["start"].as_bool().unwrap_or(false), no_build: a["no_build"].as_bool().unwrap_or(false), args: strs("args") };
            let dir = PathBuf::from(s("project"));
            if name == "dev_run" {
                match run(&dir, &inst, &o, &mut |l| log.push(l.to_string())) {
                    Ok(r) => json!({"result": r, "log_tail": last_lines(&log, 25)}),
                    Err(e) => bail!("{e:#}\n{}", last_lines(&log, 40)),
                }
            } else {
                let shot = a["screenshot"].as_str().map(PathBuf::from);
                match smoke_test(&dir, &inst, &o, a["secs"].as_u64().unwrap_or(10), shot.as_deref(), &mut |l| log.push(l.to_string())) {
                    Ok(t) => json!({"result": t, "log_tail": last_lines(&log, 25)}),
                    Err(e) => bail!("{e:#}\n{}", last_lines(&log, 40)),
                }
            }
        }
        "dev_device" | "dev_ship" => {
            let verb = if name == "dev_device" { "device" } else if a["ship_mac"].as_bool().unwrap_or(false) { "ship-mac" } else { "ship" };
            let (rc, out) = captured(passthrough(verb, Path::new(&s("project")), &strs("args"))?, 1800)?;
            json!({"exit": rc, "ok": rc == 0, "output_tail": last_lines(&out, 60)})
        }
        "dev_convert" => {
            let mut args = vec![s("xcodeproj")];
            args.extend(strs("args"));
            let (rc, out) = captured(passthrough("convert", Path::new("."), &args)?, 600)?;
            json!({"exit": rc, "ok": rc == 0, "output_tail": last_lines(&out, 60)})
        }
        _ => bail!("unknown tool {name}"),
    })
}
