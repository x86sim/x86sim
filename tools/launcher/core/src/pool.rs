//! Phone pool: phones kept booted at their home screen for agents and CI to take and give back
//! (`x86sim pool up|take|release|renew|status|down|ksm`, MCP pool_status / phone_take / phone_release, the hub's
//! Pool section).
//!
//! - Members of pool P are the phones "<P>-1", "<P>-2", ... (var/device-a64-<P>-<n>), cloned from "<P>-template", a
//!   phone of the pool's model booted once to its home screen and stopped (so a clone never pays the first boot's
//!   LaunchServices / data migration work). A clone is `cp -a --reflink=auto` of the template (on btrfs / XFS a
//!   reflink: a 3.6 GB phone in well under a second and no extra disk until it diverges), then device-repath.py (the
//!   data's absolute paths) and mksimdevice.py (its own UDID, jobs and sim.env), as device-a64.py create does.
//! - State: var/pool/<P>.json, read and written under flock(var/pool/<P>.lock): a take is atomic across processes
//!   (two agents never get one phone), and the lock is held only for the read-modify-write, never across a boot.
//! - Leases: holder (text), holder pid + its start time (a reused pid is not the holder), expiry. Any pool command
//!   first reaps leases whose holder process is gone or whose time is up: the phone is reset with the lease's mode and
//!   then free again. The MCP server takes with its own pid: an agent session that ends frees its phones.
//! - Reset on release (in a background worker, `x86sim-pool _reset`, so release returns at once; the phone is
//!   "resetting" until the worker is done, and a take never gets it before): none | apps (uninstall the apps installed
//!   during the lease: their data goes with them; then Home) | restart (a fresh boot) | erase (stop, re-clone from the
//!   template, boot).
//! - Memory: `up N` boots phones one at a time and each only while MemAvailable stays above a reserve after the
//!   measured cost of one phone (its processes' PSS plus its share of the GPU helper, re-measured from the pool's
//!   running phones; 1500 MB until measured). systemd-oomd ends the whole user session when memory runs out, so a
//!   larger N is cut down rather than risked (--force overrides).
//! - KSM: pool phones are started from a process with PR_SET_MEMORY_MERGE (Linux 6.4+, no root), inherited by the
//!   whole phone (Darling, launchd_sim, every app): when /sys/kernel/mm/ksm/run is 1 their identical pages (the same
//!   runtime, the same translated code) are merged. Turning KSM on needs root: `x86sim pool ksm on` runs the two sudo
//!   commands in the open (on a terminal only); nothing is ever run with sudo behind the user's back.
use crate::aot::repo_root;
use crate::instance::{self, Instance};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const RESETS: [&str; 4] = ["none", "apps", "restart", "erase"];
pub const DEFAULT_TTL_S: u64 = 1800;
const DEFAULT_COST_MB: f64 = 1500.0;
pub const DEFAULT_RESERVE_MB: f64 = 3072.0;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Lease {
    pub holder: String,
    pub holder_pid: Option<u32>,
    /// the holder's start time (/proc/<pid>/stat field 22): a pid reused by another process is not the holder
    pub holder_start: Option<u64>,
    pub taken: u64,
    pub expires: u64,
    /// reset mode when it is released (or reaped)
    pub reset: String,
    /// bundle ids installed when it was taken (reset "apps" removes the others)
    pub apps_at_take: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Member {
    /// device folder (device-a64-pool-1)
    pub id: String,
    /// "free" | "leased" | "resetting"
    pub state: String,
    pub lease: Option<Lease>,
    /// the reset worker's pid while resetting
    pub worker: Option<u32>,
    /// its start time (proc_start): a pid reused by another process (another x86sim-pool even) is not our worker
    #[serde(default)]
    pub worker_start: Option<u64>,
    /// last event (reaped: holder gone, reset failed: ...)
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    pub pool: String,
    pub model: String,
    /// measured memory of one booted phone (MB), 0 = not yet
    pub cost_mb: f64,
    pub members: BTreeMap<String, Member>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn dir() -> PathBuf {
    repo_root().join("var/pool")
}

fn check_name(pool: &str) -> Result<()> {
    anyhow::ensure!(!pool.is_empty() && pool.len() <= 16 && pool.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "pool name: 1-16 of a-z 0-9 -");
    Ok(())
}

/// Start time of a process (clock ticks since boot), None if it is gone
pub fn proc_start(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let f: Vec<&str> = s.rsplit(')').next()?.split_whitespace().collect();
    // a zombie (exited, not yet waited for) is gone
    if f.first() == Some(&"Z") || f.first() == Some(&"X") {
        return None;
    }
    f.get(19)?.parse().ok()
}

/// The member's reset worker still runs (its pid with the start time recorded; a state file from before start times
/// were recorded: an x86sim-pool at that pid)
fn worker_running(m: &Member) -> bool {
    let Some(pid) = m.worker else { return false };
    match (proc_start(pid), m.worker_start) {
        (None, _) => false,
        (Some(s), Some(w)) => s == w,
        (Some(_), None) => std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "x86sim-pool"),
    }
}

fn holder_alive(l: &Lease) -> bool {
    match l.holder_pid {
        None => true,
        Some(p) => proc_start(p).is_some_and(|s| l.holder_start.is_none_or(|h| h == s)),
    }
}

/// The pool's state under its lock: `f` reads and changes it; it is saved when `f` returns Ok.
pub fn with_state<T>(pool: &str, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
    let lock = lock_pool(pool)?;
    let path = dir().join(format!("{pool}.json"));
    let mut st: State = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| State { pool: pool.into(), ..Default::default() });
    let r = f(&mut st)?;
    crate::aot::write_json(&path, &st)?;
    drop(lock);
    Ok(r)
}

/// The pool's lock, held (exclusive) until the file is dropped. The lock file can be removed (down --delete, holding
/// it): a waiter that then gets the lock on the removed file sees it is no longer the one at the path and opens again,
/// so two processes never both "hold" it.
fn lock_pool(pool: &str) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    check_name(pool)?;
    std::fs::create_dir_all(dir())?;
    let path = dir().join(format!("{pool}.lock"));
    for _ in 0..100 {
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
        // SAFETY: flock on an fd we own; released when `lock` is dropped
        anyhow::ensure!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0, "locking the pool");
        let (held, now) = (lock.metadata()?, std::fs::metadata(&path));
        if now.is_ok_and(|n| n.ino() == held.ino() && n.dev() == held.dev()) {
            return Ok(lock);
        }
    }
    anyhow::bail!("locking the pool: its lock file keeps changing")
}

pub fn template_name(pool: &str) -> String {
    format!("{pool}-template")
}

fn member_dir(name: &str) -> PathBuf {
    repo_root().join("var").join(format!("device-a64-{name}"))
}

/// Live state of the pool's phones (state, home) by folder
fn live() -> BTreeMap<String, instance::Summary> {
    instance::summaries(0).into_iter().map(|s| (s.id.clone(), s)).collect()
}

fn ready(s: Option<&instance::Summary>) -> bool {
    s.is_some_and(|s| s.state == "running" && s.home)
}

/// Bundle ids of the apps installed on a phone (its Containers/Bundle/Application/*/*.app), read from the files
pub fn installed_apps(dev: &Path) -> Vec<String> {
    let root = dev.join("data/Containers/Bundle/Application");
    let mut plists = vec![];
    for c in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for a in std::fs::read_dir(c.path()).into_iter().flatten().flatten() {
            if a.path().extension().is_some_and(|e| e == "app") {
                plists.push(a.path().join("Info.plist"));
            }
        }
    }
    if plists.is_empty() {
        return vec![];
    }
    let out = Command::new("python3")
        .args(["-I", "-c", "import plistlib,sys\nfor p in sys.argv[1:]:\n  try: print(plistlib.load(open(p,'rb'))['CFBundleIdentifier'])\n  except Exception: pass"])
        .args(&plists)
        .output();
    let mut v: Vec<String> = out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect()).unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

/// The sibling x86sim-pool binary (the reset worker), next to whichever launcher binary runs
fn pool_bin() -> PathBuf {
    let here = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
    let p = here.join("x86sim-pool");
    if p.exists() { p } else { repo_root().join("tools/launcher/target/release/x86sim-pool") }
}

/// Starts the background reset of a member (marks it resetting). Called under the lock.
fn spawn_reset(st: &mut State, id: &str, mode: &str, why: &str) -> Result<()> {
    let m = st.members.get_mut(id).context("not a member")?;
    let apps = m.lease.as_ref().map(|l| l.apps_at_take.join(",")).unwrap_or_default();
    let log = std::fs::File::create(dir().join(format!("{}.{id}.reset.log", st.pool)))?;
    let mut cmd = Command::new(pool_bin());
    cmd.args(["_reset", &st.pool, id, mode, &apps]).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid in the child before exec: the worker outlives the caller (an MCP server, a CLI)
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("starting the reset worker")?;
    m.state = "resetting".into();
    m.worker = Some(child.id());
    m.worker_start = proc_start(child.id());
    // a long-lived caller (the MCP server, the hub) waits for it, so it leaves no zombie
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    m.note = format!("{why}; reset {mode}");
    m.lease = None;
    Ok(())
}

/// Frees leases whose holder is gone or whose time is up, and settles members whose reset worker ended. Under the lock.
fn reap(st: &mut State) {
    let t = now();
    let ids: Vec<String> = st.members.keys().cloned().collect();
    for id in ids {
        let m = st.members[&id].clone();
        match m.state.as_str() {
            "leased" => {
                let Some(l) = &m.lease else {
                    st.members.get_mut(&id).unwrap().state = "free".into();
                    continue;
                };
                let why = if !holder_alive(l) {
                    Some(format!("holder {} (pid {}) is gone", l.holder, l.holder_pid.unwrap_or(0)))
                } else if t >= l.expires {
                    Some(format!("lease of {} expired", l.holder))
                } else {
                    None
                };
                if let Some(why) = why {
                    let mode = if l.reset.is_empty() { "apps".to_string() } else { l.reset.clone() };
                    if mode == "none" {
                        let mm = st.members.get_mut(&id).unwrap();
                        mm.state = "free".into();
                        mm.lease = None;
                        mm.note = format!("{why}; no reset");
                    } else if let Err(e) = spawn_reset(st, &id, &mode, &why) {
                        st.members.get_mut(&id).unwrap().note = format!("{why}; reset failed to start: {e:#}");
                    }
                }
            }
            "resetting" if !worker_running(&m) => {
                let mm = st.members.get_mut(&id).unwrap();
                mm.state = "free".into();
                mm.worker = None;
                mm.worker_start = None;
            }
            _ => {}
        }
    }
}

/// Measured memory of one booted pool phone: its PSS plus its share of the GPU helper (0 if none is up)
fn measure(st: &State, lv: &BTreeMap<String, instance::Summary>) -> f64 {
    let v: Vec<f64> = st
        .members
        .keys()
        .filter_map(|id| lv.get(id))
        .filter(|s| s.state == "running" && s.home)
        .map(|s| s.memory_mb + s.gpu_memory_mb.unwrap_or(0.0) / s.gpu_shared_by.unwrap_or(1).max(1) as f64)
        .collect();
    if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
}

pub fn mem_available_mb() -> f64 {
    std::fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<f64>().ok())
        .map(|k| k / 1024.0)
        .unwrap_or(0.0)
}

// ---- KSM ----

#[derive(Debug, Clone, Serialize)]
pub struct Ksm {
    /// /sys/kernel/mm/ksm exists
    pub available: bool,
    /// /sys/kernel/mm/ksm/run == 1
    pub running: bool,
    pub pages_to_scan: u64,
    /// MB saved now (pages_sharing * page size)
    pub saved_mb: f64,
    /// the commands that turn it on (root)
    pub enable: Vec<String>,
}

pub fn ksm() -> Ksm {
    let r = |f: &str| std::fs::read_to_string(format!("/sys/kernel/mm/ksm/{f}")).ok().and_then(|s| s.trim().parse::<u64>().ok());
    // SAFETY: plain libc call
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as f64;
    Ksm {
        available: Path::new("/sys/kernel/mm/ksm/run").exists(),
        running: r("run") == Some(1),
        pages_to_scan: r("pages_to_scan").unwrap_or(0),
        saved_mb: r("pages_sharing").unwrap_or(0) as f64 * page / 1048576.0,
        enable: vec!["echo 1000 | sudo tee /sys/kernel/mm/ksm/pages_to_scan".into(), "echo 1 | sudo tee /sys/kernel/mm/ksm/run".into()],
    }
}

/// Marks this process (and so every phone it starts: the flag is inherited across fork and exec) for KSM merging.
/// Unprivileged; does nothing until KSM runs.
pub fn opt_in_ksm() -> bool {
    // SAFETY: prctl(PR_SET_MEMORY_MERGE = 67, 1); fails harmlessly on kernels before 6.4
    unsafe { libc::prctl(67, 1, 0, 0, 0) == 0 }
}

// ---- clone ----

/// Whether `cp --reflink` really reflinks where phones live (btrfs, XFS): tried on a small file in var/pool
pub fn reflink_supported() -> bool {
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let (a, b) = (d.join(".reflink-probe"), d.join(".reflink-probe2"));
    let _ = std::fs::write(&a, b"x");
    let ok = Command::new("cp").args(["--reflink=always"]).arg(&a).arg(&b).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    ok
}

/// Makes phone `name` (var/device-a64-<name>) as a copy of the template `tpl` (off): reflink copy, then its own paths
/// (device-repath.py) and identity (mksimdevice.py: UDID, jobs, sim.env), as device-a64.py create does for its base.
pub fn clone_phone(tpl: &Instance, name: &str) -> Result<PathBuf> {
    let dst = member_dir(name);
    anyhow::ensure!(!name.is_empty() && !name.contains('/') && !name.contains(".."), "bad device name {name:?}");
    anyhow::ensure!(!dst.exists(), "{} exists", dst.display());
    let out = Command::new("cp").args(["-a", "--reflink=auto"]).arg(&tpl.dir).arg(&dst).output()?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dst);
        bail!("copying {}: {}", tpl.dir.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    let data = dst.join("data");
    for x in ["var/log", "var/run", "tmp"] {
        let _ = std::fs::remove_dir_all(data.join(x));
        std::fs::create_dir_all(data.join(x))?;
    }
    for x in ["simhid.sock", "framebuffer.bgra"] {
        let _ = std::fs::remove_file(data.join(x));
    }
    std::fs::write(dst.join("name"), format!("{name}\n"))?;
    let root = repo_root();
    // without it the clone's apps would run in the template's containers: a failure is fatal
    // --force: the copy has never booted (a fresh dir, or the erased phone after wait_gone), and the script's own
    // "booted" check also matches any shell whose command line mentions the dir
    let out = Command::new("python3").arg(root.join("cli/libexec/arm64/device-repath.py")).arg(&dst).arg(&tpl.dir).arg("--force").output()?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dst);
        bail!("device-repath.py {name}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let out = Command::new("python3")
        .arg(root.join("cli/libexec/mksimdevice.py"))
        .arg(&dst)
        .env("X86SIM_ARCH", "arm64")
        .env("X86SIM_DEVICE", &dst)
        .env("X86SIM_RUNTIME", &tpl.runtime_root)
        .env("X86SIM_STATE", root.join("var/run-a64"))
        .env("X86SIM_JOBLOG", "1")
        .stdout(Stdio::null())
        .output()?;
    anyhow::ensure!(out.status.success(), "mksimdevice.py: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(dst)
}

// ---- boot helpers ----

/// Waits until the phone is running and at its home screen (restarting a stalled first boot once)
pub fn wait_home(id: &str, secs: u64) -> Result<()> {
    let t0 = std::time::Instant::now();
    let mut restarted = false;
    let mut mon = instance::Monitor::default();
    loop {
        let insts: Vec<Instance> = instance::list().into_iter().filter(|i| i.id == id).collect();
        let inst = insts.first().context("device vanished")?.clone();
        let sts = mon.poll(&insts);
        if let Some(s) = sts.first() {
            if s.phase == instance::Phase::Running && s.home {
                return Ok(());
            }
            if !restarted && instance::first_boot_stalled(s) {
                restarted = true;
                crate::sim::restart(&inst.dir)?;
            }
            if s.phase == instance::Phase::Off && t0.elapsed().as_secs() > 60 {
                bail!("{id} stopped while booting (see {})", instance::boot_log(&inst).display());
            }
        }
        anyhow::ensure!(t0.elapsed().as_secs() < secs, "{id}: no home screen after {secs} s");
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

fn running(id: &str) -> bool {
    live().get(id).is_some_and(|s| s.state != "off")
}

/// Processes whose command line names the phone's data dir (as device-repath.py's and boot-a64.sh's check): after a
/// stop, Darling's processes of the phone take a few seconds more to end than its launchd_sim
fn phone_procs(dev: &Path) -> usize {
    let key = format!("{}/data/", dev.canonicalize().unwrap_or_else(|_| dev.to_path_buf()).display());
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        // not shells, interpreters or tools whose command line merely mentions the path (a script running
        // `grep ... <dev>/data/...` is not the phone: device-repath.py's own check took one for a booted phone)
        .filter(|e| !matches!(std::fs::read_to_string(e.path().join("comm")).unwrap_or_default().trim(), "bash" | "sh" | "dash" | "zsh" | "fish" | "python3" | "python" | "grep" | "find" | "cp" | "rm" | "x86sim-pool" | "x86sim-mcp" | "timeout" | "sleep" | "tail"))
        .filter(|e| std::fs::read(e.path().join("cmdline")).is_ok_and(|c| c.windows(key.len()).any(|w| w == key.as_bytes())))
        .count()
}

/// Waits until a stopped phone has no process left (up to `secs`)
fn wait_gone(dev: &Path, secs: u64) -> Result<()> {
    for _ in 0..secs {
        if phone_procs(dev) == 0 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    bail!("{}: {} process(es) still run after its stop", dev.display(), phone_procs(dev))
}

/// Boots the pool's template once to its home screen and stops it: clones start from a phone that booted
pub fn prepare_template(tpl: &Instance, log: &mut dyn FnMut(&str)) -> Result<()> {
    if instance::home_marker(&tpl.dir).exists() {
        return Ok(());
    }
    log(&format!("preparing {}: first boot (once; its clones skip it)", tpl.name));
    opt_in_ksm();
    crate::sim::start(&tpl.dir)?;
    let r = wait_home(&tpl.id, 1200);
    crate::sim::stop(&tpl.dir)?;
    r?;
    // its stop must be complete before it is copied
    for _ in 0..60 {
        if !running(&tpl.id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    wait_gone(&tpl.dir, 90)
}

#[derive(Debug, Clone, Default)]
pub struct UpOpts {
    pub model: String,
    /// iOS version / build (as sim_add_device), default 27.2
    pub os: String,
    pub preset: Option<String>,
    pub reserve_mb: f64,
    pub force: bool,
}

/// Makes the pool N phones big and boots those that are off, one at a time, while memory allows. Returns the
/// members booted and running.
pub fn up(pool: &str, n: usize, o: &UpOpts, log: &mut dyn FnMut(&str)) -> Result<Vec<String>> {
    check_name(pool)?;
    let model = with_state(pool, |st| {
        if st.model.is_empty() {
            st.model = if o.model.is_empty() { "iPhone 17 Pro".into() } else { o.model.clone() };
        } else if !o.model.is_empty() && o.model != st.model {
            bail!("pool {pool} is of {} devices (down --delete to change the model)", st.model);
        }
        Ok(st.model.clone())
    })?;
    // the template
    let tname = template_name(pool);
    let tpl = match instance::find(&format!("device-a64-{tname}")) {
        Some(t) => t,
        None => {
            log(&format!("creating {tname} ({model})"));
            let rt = pick_runtime(&model, if o.os.is_empty() { "27.2" } else { &o.os })?;
            let d = crate::sim::create_device(&model, &tname, &rt)?;
            // only ever a phone dir of ours under var/ (never a path made of a script's report)
            anyhow::ensure!(d == member_dir(&tname), "device create made {} (expected {})", d.display(), member_dir(&tname).display());
            if let Some(p) = &o.preset {
                crate::features::apply_preset(&d, p)?;
            }
            instance::load(&d)
        }
    };
    anyhow::ensure!(!running(&tpl.id), "{} is running: stop it first (clones are copied from it)", tpl.name);
    prepare_template(&tpl, log)?;
    wait_gone(&tpl.dir, 30)?;
    // members 1..n (clones)
    let reflink = reflink_supported();
    for i in 1..=n {
        let name = format!("{pool}-{i}");
        let id = format!("device-a64-{name}");
        if !member_dir(&name).exists() {
            let t0 = std::time::Instant::now();
            clone_phone(&tpl, &name)?;
            log(&format!("{name}: cloned from {} in {:.1} s ({})", tpl.name, t0.elapsed().as_secs_f64(), if reflink { "reflink" } else { "full copy: this file system has no reflinks" }));
        }
        with_state(pool, |st| {
            st.members.entry(id.clone()).or_insert_with(|| Member { id: id.clone(), state: "free".into(), ..Default::default() });
            Ok(())
        })?;
    }
    // boot those that are off, one at a time, within memory
    opt_in_ksm();
    let mut up = vec![];
    for i in 1..=n {
        let id = format!("device-a64-{pool}-{i}");
        let lv = live();
        if lv.get(&id).is_some_and(|s| s.state != "off") {
            wait_home(&id, 900)?;
            up.push(id);
            continue;
        }
        let cost = with_state(pool, |st| {
            let m = measure(st, &lv);
            if m > 0.0 {
                st.cost_mb = m;
            }
            Ok(if st.cost_mb > 0.0 { st.cost_mb } else { DEFAULT_COST_MB })
        })?;
        let avail = mem_available_mb();
        let reserve = if o.reserve_mb > 0.0 { o.reserve_mb } else { DEFAULT_RESERVE_MB };
        if avail - cost < reserve && !o.force {
            log(&format!(
                "stopping at {} of {n} devices: {avail:.0} MB available, one more needs ~{cost:.0} MB and {reserve:.0} MB stay free (--reserve-mb / --force){}",
                up.len(),
                if ksm().running { "" } else { "; KSM is off: `x86sim pool ksm on` lets devices share identical memory" }
            ));
            break;
        }
        log(&format!("booting {pool}-{i} ({avail:.0} MB available, ~{cost:.0} MB per device)"));
        crate::sim::start(&member_dir(&format!("{pool}-{i}")))?;
        wait_home(&id, 900)?;
        log(&format!("{pool}-{i}: at its home screen"));
        up.push(id);
    }
    // measure now that they are up
    let lv = live();
    with_state(pool, |st| {
        let m = measure(st, &lv);
        if m > 0.0 {
            st.cost_mb = m;
        }
        Ok(())
    })?;
    Ok(up)
}

fn pick_runtime(model: &str, want: &str) -> Result<PathBuf> {
    let model_id = crate::sim::models().into_iter().find(|m| m.0 == model || m.1 == model).map(|m| m.1).with_context(|| format!("no model {model:?} (x86sim device models)"))?;
    let pname = crate::sim::platform_name(crate::sim::model_platform(&model_id));
    let c: Vec<_> = crate::sim::runtimes()
        .into_iter()
        .filter(|r| r.0.starts_with(&format!("{pname} ")) && (r.0.starts_with(&format!("{pname} {want} ")) || r.0.contains(want)))
        .filter(|r| crate::sim::runtime_allows(&r.1, &model_id))
        .collect();
    c.iter().find(|r| !crate::sim::runtime_restricted_to(&r.1).is_empty()).or(c.first()).map(|r| r.1.clone()).with_context(|| format!("no {pname} {want} runtime for {model}"))
}

// ---- take / release ----

#[derive(Debug, Clone, Default)]
pub struct TakeOpts {
    pub holder: String,
    pub holder_pid: Option<u32>,
    pub ttl_s: u64,
    pub reset: String,
    /// wait up to this long for a free phone
    pub wait_s: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Taken {
    /// the phone's name (as x86sim / MCP tools take it)
    pub phone: String,
    pub id: String,
    pub path: PathBuf,
    pub expires: u64,
    pub ttl_s: u64,
    pub reset: String,
}

/// Takes a booted, free phone of the pool (atomic across processes)
pub fn take(pool: &str, o: &TakeOpts) -> Result<Taken> {
    let reset = if o.reset.is_empty() { "apps".to_string() } else { o.reset.clone() };
    anyhow::ensure!(RESETS.contains(&reset.as_str()), "reset: one of {}", RESETS.join(", "));
    let ttl = if o.ttl_s == 0 { DEFAULT_TTL_S } else { o.ttl_s };
    let t0 = std::time::Instant::now();
    loop {
        let lv = live();
        let got = with_state(pool, |st| {
            reap(st);
            let id = st.members.values().find(|m| m.state == "free" && ready(lv.get(&m.id))).map(|m| m.id.clone());
            let Some(id) = id else {
                return Ok(None);
            };
            let dev = lv[&id].path.clone();
            let l = Lease {
                holder: if o.holder.is_empty() { "?".into() } else { o.holder.clone() },
                holder_pid: o.holder_pid,
                holder_start: o.holder_pid.and_then(proc_start),
                taken: now(),
                expires: now() + ttl,
                reset: reset.clone(),
                apps_at_take: installed_apps(&dev),
            };
            anyhow::ensure!(o.holder_pid.is_none() || l.holder_start.is_some(), "holder pid {} is not running", o.holder_pid.unwrap_or(0));
            let m = st.members.get_mut(&id).unwrap();
            m.state = "leased".into();
            m.note.clear();
            let expires = l.expires;
            m.lease = Some(l);
            Ok(Some(Taken { phone: lv[&id].name.clone(), id: id.clone(), path: dev, expires, ttl_s: ttl, reset: reset.clone() }))
        })?;
        if let Some(t) = got {
            return Ok(t);
        }
        if t0.elapsed().as_secs() >= o.wait_s {
            let s = status(pool)?;
            bail!(
                "no free device in pool {pool} ({} members: {} leased, {} resetting, {} not booted){}",
                s.members.len(),
                s.members.iter().filter(|m| m.state == "leased").count(),
                s.members.iter().filter(|m| m.state == "resetting").count(),
                s.members.iter().filter(|m| !m.ready && m.state == "free").count(),
                if s.members.is_empty() { format!(": x86sim pool up N --pool {pool}") } else { String::new() }
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

fn member_of(st: &State, phone: &str) -> Result<String> {
    let inst = instance::find(phone).with_context(|| format!("no device {phone:?}"))?;
    anyhow::ensure!(st.members.contains_key(&inst.id), "{phone} is not in pool {}", st.pool);
    Ok(inst.id)
}

/// Gives a phone back: it is reset in the background (`reset`: none | apps | restart | erase; default the lease's)
pub fn release(pool: &str, phone: &str, reset: Option<&str>) -> Result<String> {
    with_state(pool, |st| {
        let id = member_of(st, phone)?;
        let m = &st.members[&id];
        anyhow::ensure!(m.state == "leased", "{phone} is not taken ({})", m.state);
        let mode = reset.map(String::from).or_else(|| m.lease.as_ref().map(|l| l.reset.clone())).filter(|s| !s.is_empty()).unwrap_or_else(|| "apps".into());
        anyhow::ensure!(RESETS.contains(&mode.as_str()), "reset: one of {}", RESETS.join(", "));
        if mode == "none" {
            let m = st.members.get_mut(&id).unwrap();
            m.state = "free".into();
            m.lease = None;
            m.note = "released (no reset)".into();
        } else {
            spawn_reset(st, &id, &mode, "released")?;
        }
        Ok(mode)
    })
}

/// Extends a lease
pub fn renew(pool: &str, phone: &str, ttl_s: u64) -> Result<u64> {
    with_state(pool, |st| {
        reap(st);
        let id = member_of(st, phone)?;
        let m = st.members.get_mut(&id).unwrap();
        let l = m.lease.as_mut().filter(|_| m.state == "leased").with_context(|| format!("{phone} is not taken"))?;
        l.expires = now() + if ttl_s == 0 { DEFAULT_TTL_S } else { ttl_s };
        Ok(l.expires)
    })
}

/// The reset worker (`x86sim-pool _reset`): runs detached, then the member is free again (reap sees the worker gone)
pub fn reset_worker(pool: &str, id: &str, mode: &str, apps_at_take: &[String]) -> Result<()> {
    opt_in_ksm();
    let inst = instance::find(id).context("device vanished")?;
    let say = |s: &str| println!("[{}] {id}: {s}", now());
    match mode {
        "apps" => {
            let now_apps = installed_apps(&inst.dir);
            let new: Vec<&String> = now_apps.iter().filter(|a| !apps_at_take.contains(a)).collect();
            if running(id) {
                for a in &new {
                    match crate::apps::uninstall(&inst, a) {
                        Ok(_) => say(&format!("uninstalled {a}")),
                        Err(e) => say(&format!("uninstall {a}: {e:#}")),
                    }
                }
                crate::hid::Hid::new(&inst.sock).press_for("home", 100);
                say("home");
            } else if !new.is_empty() {
                say("off: apps left; restarting instead");
                crate::sim::start(&inst.dir)?;
                wait_home(id, 900)?;
                for a in &new {
                    let _ = crate::apps::uninstall(&inst, a);
                }
            }
        }
        "restart" => {
            crate::sim::restart(&inst.dir)?;
            std::thread::sleep(std::time::Duration::from_secs(5));
            wait_home(id, 900)?;
            say("restarted");
        }
        "erase" => {
            let tpl = instance::find(&format!("device-a64-{}", template_name(pool))).context("the pool's template is gone")?;
            if running(id) {
                crate::sim::stop(&inst.dir)?;
            }
            for _ in 0..60 {
                if !running(id) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            anyhow::ensure!(!running(id), "{id} did not stop");
            wait_gone(&inst.dir, 90)?;
            let name = inst.id.strip_prefix("device-a64-").unwrap_or(&inst.id).to_string();
            std::fs::remove_dir_all(&inst.dir)?;
            clone_phone(&tpl, &name)?;
            crate::sim::start(&member_dir(&name))?;
            wait_home(id, 900)?;
            say("erased (fresh clone of the template) and booted");
        }
        _ => {}
    }
    let _ = pool;
    Ok(())
}

// ---- status / down ----

#[derive(Debug, Clone, Serialize)]
pub struct MemberStatus {
    pub phone: String,
    pub id: String,
    /// free | leased | resetting
    pub state: String,
    /// booted and at its home screen
    pub ready: bool,
    /// off | starting | running | stopping
    pub phase: String,
    pub memory_mb: f64,
    pub holder: Option<String>,
    pub holder_pid: Option<u32>,
    pub expires_in_s: Option<i64>,
    pub reset: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolStatus {
    pub pool: String,
    pub model: String,
    pub template: Option<String>,
    pub members: Vec<MemberStatus>,
    pub free: usize,
    pub cost_mb: f64,
    pub mem_available_mb: f64,
    /// phones that could still boot now (by available memory, the cost and the default reserve)
    pub room_for: usize,
    pub ksm: Ksm,
    pub reflink: bool,
}

pub fn status(pool: &str) -> Result<PoolStatus> {
    let lv = live();
    let st = with_state(pool, |st| {
        reap(st);
        let m = measure(st, &lv);
        if m > 0.0 {
            st.cost_mb = m;
        }
        Ok(st.clone())
    })?;
    let t = now() as i64;
    let members: Vec<MemberStatus> = st
        .members
        .values()
        .map(|m| {
            let s = lv.get(&m.id);
            MemberStatus {
                phone: s.map(|s| s.name.clone()).unwrap_or_else(|| m.id.clone()),
                id: m.id.clone(),
                state: m.state.clone(),
                ready: ready(s),
                phase: s.map(|s| s.state.clone()).unwrap_or_else(|| "missing".into()),
                memory_mb: s.map(|s| s.memory_mb).unwrap_or(0.0),
                holder: m.lease.as_ref().map(|l| l.holder.clone()),
                holder_pid: m.lease.as_ref().and_then(|l| l.holder_pid),
                expires_in_s: m.lease.as_ref().map(|l| l.expires as i64 - t),
                reset: m.lease.as_ref().map(|l| l.reset.clone()),
                note: m.note.clone(),
            }
        })
        .collect();
    let cost = if st.cost_mb > 0.0 { st.cost_mb } else { DEFAULT_COST_MB };
    let avail = mem_available_mb();
    Ok(PoolStatus {
        pool: pool.into(),
        model: st.model.clone(),
        template: instance::find(&format!("device-a64-{}", template_name(pool))).map(|t| t.name),
        free: members.iter().filter(|m| m.state == "free" && m.ready).count(),
        members,
        cost_mb: (cost * 10.0).round() / 10.0,
        mem_available_mb: avail.round(),
        room_for: ((avail - DEFAULT_RESERVE_MB) / cost).floor().max(0.0) as usize,
        ksm: ksm(),
        reflink: reflink_supported(),
    })
}

/// Pools with a state file (var/pool/*.json)
pub fn pools() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").map(String::from))
        .collect();
    v.sort();
    v
}

/// Stops the pool's phones (leased ones only with `force`); `delete`: removes them, the template and the state too
pub fn down(pool: &str, delete: bool, force: bool, log: &mut dyn FnMut(&str)) -> Result<()> {
    let st = with_state(pool, |st| {
        reap(st);
        Ok(st.clone())
    })?;
    let busy: Vec<&String> = st.members.values().filter(|m| m.state != "free").map(|m| &m.id).collect();
    anyhow::ensure!(force || busy.is_empty(), "taken or resetting: {} (--force to stop them anyway)", busy.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    for id in st.members.keys() {
        // a reset worker still running would boot it again
        if let Some(p) = st.members[id].worker.filter(|_| worker_running(&st.members[id])) {
            // SAFETY: the worker pid recorded for this member, checked to be our worker (pid and start time)
            unsafe {
                libc::kill(p as i32, libc::SIGTERM);
            }
        }
        if running(id) {
            log(&format!("stopping {id}"));
            crate::sim::stop(&repo_root().join("var").join(id))?;
        }
    }
    if delete {
        let mut ids: Vec<String> = st.members.keys().cloned().collect();
        ids.push(format!("device-a64-{}", template_name(pool)));
        let mut mon = instance::Monitor::default();
        for id in ids {
            let Some(inst) = instance::find(&id) else { continue };
            let mut ok = false;
            for _ in 0..60 {
                let s = mon.poll(std::slice::from_ref(&inst));
                if s.first().is_none_or(|s| s.phase == instance::Phase::Off && s.launchd_pid.is_none()) {
                    ok = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            anyhow::ensure!(ok, "{id} did not stop");
            let s = mon.poll(std::slice::from_ref(&inst));
            instance::delete(&inst, s.first())?;
            log(&format!("deleted {id}"));
        }
        // removed while holding the lock (lock_pool: a waiter on the old file opens the path again)
        let lock = lock_pool(pool)?;
        let _ = std::fs::remove_file(dir().join(format!("{pool}.json")));
        let _ = std::fs::remove_file(dir().join(format!("{pool}.lock")));
        drop(lock);
        for e in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(&format!("{pool}.device-a64-")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    } else {
        with_state(pool, |st| {
            for m in st.members.values_mut() {
                m.state = "free".into();
                m.lease = None;
                m.worker = None;
                m.worker_start = None;
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_and_reused_holders() {
        let me = std::process::id();
        let l = Lease { holder_pid: Some(me), holder_start: proc_start(me), ..Default::default() };
        assert!(holder_alive(&l));
        let reused = Lease { holder_pid: Some(me), holder_start: proc_start(me).map(|s| s + 1), ..Default::default() };
        assert!(!holder_alive(&reused));
        let gone = Lease { holder_pid: Some(0x3fff_fff0), holder_start: Some(1), ..Default::default() };
        assert!(!holder_alive(&gone));
        assert!(holder_alive(&Lease::default()));
    }

    #[test]
    fn workers_by_pid_and_start_time() {
        let me = std::process::id();
        let m = |pid: Option<u32>, start: Option<u64>| Member { worker: pid, worker_start: start, ..Default::default() };
        assert!(worker_running(&m(Some(me), proc_start(me))));
        // the pid reused by another process (here: this test, under another start time) is not our worker
        assert!(!worker_running(&m(Some(me), proc_start(me).map(|s| s + 1))));
        assert!(!worker_running(&m(Some(0x3fff_fff0), Some(1))));
        assert!(!worker_running(&m(None, None)));
    }

    #[test]
    fn a_removed_lock_file_is_not_held_twice() {
        // a pool name of this test's own; its lock and state live in var/pool
        let pool = format!("t{}", std::process::id() % 100000);
        let a = lock_pool(&pool).unwrap();
        let p = dir().join(format!("{pool}.lock"));
        let (tx, rx) = std::sync::mpsc::channel();
        let pool2 = pool.clone();
        let waiter = std::thread::spawn(move || {
            let l = lock_pool(&pool2).unwrap();
            use std::os::unix::fs::MetadataExt;
            tx.send(l.metadata().unwrap().ino()).unwrap();
            l
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        // removed while held (down --delete); a third process opens the new file
        std::fs::remove_file(&p).unwrap();
        drop(a);
        let ino = rx.recv().unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(ino, std::fs::metadata(&p).unwrap().ino(), "the waiter holds the file at the path, not the removed one");
        drop(waiter.join().unwrap());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn names() {
        assert!(check_name("pool").is_ok());
        assert!(check_name("wt-pool").is_ok());
        assert!(check_name("../x").is_err());
        assert!(check_name("").is_err());
    }
}
