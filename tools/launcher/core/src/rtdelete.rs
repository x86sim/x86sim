//! Deleting an installed OS version (runtimes/<platform>-<version>-<build>-arm64), as Xcode's Platforms settings do,
//! with a choice of how much goes:
//!   - the version only: runtimes/<dir> (RuntimeRoot, the extracted image, the bundle / download / decrypted staging).
//!     The pre-translated code stays, so a download of the same build later starts fast again.
//!   - everything: also what x86Sim made for that version alone: its JIT/AOT cache dir ($SCRATCH/jc-<build>: the AOT
//!     image and chunks, the placement table, the per-process training / .live files, app archives), its files in
//!     caches other versions share (named `<dyld cache uuid>-…`: the default `jitcache` dir, the training dirs an AOT
//!     config lists, other jc-* dirs), and its download log.
//! Shared data is kept: a cache dir another installed version maps to (jc_for) is never removed whole, and files keyed
//! by a dyld cache uuid another installed version has are kept. A running phone's version is never deleted; phones
//! that are off but use the version are listed (they show "Files missing" and offer the download again).
//! Used by `x86sim-aot runtime delete` (cli/x86sim runtime delete), the MCP tool runtime_delete and the launcher's
//! Versions page.
use crate::aot::{repo_root, scratch, Paths};
use crate::host::{dir_size, Runtime};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub path: PathBuf,
    pub bytes: u64,
    /// what it is, for the confirmation ("runtime files", "pre-translated code", ...)
    pub what: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// "off" | "starting" | "running" | "stopping"
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub dir: String,
    pub label: String,
    pub build: String,
    /// "Delete the version only"
    pub runtime: Vec<Item>,
    pub runtime_bytes: u64,
    /// added by "Delete everything"
    pub caches: Vec<Item>,
    pub caches_bytes: u64,
    /// phones on this version
    pub devices: Vec<Device>,
    /// a download of this build is running
    pub downloading: bool,
    /// an AOT run on its cache dir is live (pid)
    pub aot_running: Option<u32>,
    /// what is kept although it looks related, and why
    pub kept: Vec<String>,
}

impl Plan {
    pub fn running_devices(&self) -> Vec<&Device> {
        self.devices.iter().filter(|d| d.state != "off").collect()
    }
    /// Why it can't be deleted now (None: it can)
    pub fn blocker(&self) -> Option<String> {
        let run = self.running_devices();
        if !run.is_empty() {
            return Some(format!("{} running on it: stop {} first", names(&run), if run.len() == 1 { "it" } else { "them" }));
        }
        if self.downloading {
            return Some("it is downloading: stop the download first".into());
        }
        if let Some(p) = self.aot_running {
            return Some(format!("its code is being pre-translated (x86sim-aot pid {p}): stop that first"));
        }
        None
    }
}

fn names(d: &[&Device]) -> String {
    d.iter().map(|d| d.name.clone()).collect::<Vec<_>>().join(", ")
}

/// The installed version a CLI / MCP argument names: a build ("24K150"), a runtimes/ dir name, or "<platform>
/// <version>" ("tvOS 27.2"); a version shared by two builds (iOS 27.2 24B8117 / 24B91016s) must be named by build.
pub fn find(key: &str) -> Result<Runtime> {
    let k = key.trim();
    let all = crate::host::runtimes();
    if let Some(r) = all.iter().find(|r| r.build.eq_ignore_ascii_case(k) || r.dir == k) {
        return Ok(r.clone());
    }
    let hits: Vec<_> = all.iter().filter(|r| r.label().eq_ignore_ascii_case(k) || r.version == k).collect();
    // a version deleted earlier with "the version only": its pre-translated code is left ($SCRATCH/jc-<build>)
    if hits.is_empty() && !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric()) && scratch().join(format!("jc-{k}")).is_dir() {
        let dir = format!("ios-0-{k}-arm64");
        let root = repo_root().join("runtimes").join(&dir).join("RuntimeRoot");
        return Ok(Runtime { jc: crate::sim::jc_for(&root), platform: "ios".into(), version: String::new(), build: k.into(), root, present: false, partial: false, restricted_to: vec![], dir });
    }
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => bail!("no installed version {k:?} (installed: {})", all.iter().map(|r| format!("{} ({})", r.label(), r.build)).collect::<Vec<_>>().join(", ")),
        _ => bail!("{k:?} is ambiguous: {}; name the build", hits.iter().map(|r| format!("{} ({})", r.label(), r.build)).collect::<Vec<_>>().join(", ")),
    }
}

/// The dyld cache uuid of a runtime (its files' key in shared caches): from the cache itself, else (files already
/// gone) the most common `<uuid>-` prefix in its own cache dir.
fn cache_uuid(r: &Runtime, jc: &Path) -> Option<String> {
    if let Ok(c) = crate::dyldcache::Cache::open(&r.root) {
        return Some(c.uuid_hex());
    }
    let mut count: std::collections::HashMap<String, usize> = Default::default();
    for e in std::fs::read_dir(jc).into_iter().flatten().flatten() {
        if let Some(u) = uuid_prefix(&e.file_name().to_string_lossy()) {
            *count.entry(u.to_string()).or_default() += 1;
        }
    }
    count.into_iter().max_by_key(|c| c.1).map(|c| c.0)
}

/// "c54ad6fa9ff23edb9c8a53a82682d342-…" -> the uuid
fn uuid_prefix(name: &str) -> Option<&str> {
    let u = name.get(..32)?;
    (name.as_bytes().get(32) == Some(&b'-') && u.bytes().all(|b| b.is_ascii_hexdigit())).then_some(u)
}

/// Cache dirs under scratch: jitcache, jc-* (runtimes' and experiments'), and the training dirs AOT configs list
fn shared_dirs(own_jc: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![];
    for e in std::fs::read_dir(scratch()).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if (n == "jitcache" || n.starts_with("jc-")) && e.path().is_dir() {
            v.push(e.path());
        }
    }
    v.push(Paths::default_jc());
    let jcs = v.clone();
    for jc in &jcs {
        if let Ok(b) = std::fs::read(Paths::new(jc).config()) {
            if let Ok(c) = serde_json::from_slice::<serde_json::Value>(&b) {
                for t in c["training"].as_array().into_iter().flatten().filter_map(|t| t.as_str()) {
                    v.push(PathBuf::from(t));
                }
            }
        }
    }
    v.push(PathBuf::from("/tmp/x86sim-a64-img"));
    v.sort();
    v.dedup();
    v.retain(|d| d != own_jc && d.is_dir());
    v
}

/// What deleting `r` would remove, and what stands in the way.
pub fn plan(r: &Runtime) -> Plan {
    let rtdir = repo_root().join("runtimes").join(&r.dir);
    let mut runtime = vec![];
    if rtdir.symlink_metadata().is_ok() {
        runtime.push(Item { bytes: dir_size(&rtdir), path: rtdir.clone(), what: "runtime files (RuntimeRoot, extracted image, staged download)".into() });
    }
    let others: Vec<Runtime> = crate::host::runtimes().into_iter().filter(|o| o.dir != r.dir).collect();
    let jc = crate::sim::jc_for(&r.root);
    let mut kept = vec![];
    let mut caches = vec![];
    let uuid = cache_uuid(r, &jc);
    // files keyed by a uuid that another installed version also has stay (two builds of one cache)
    let uuid_shared = uuid.as_ref().is_some_and(|u| others.iter().any(|o| crate::dyldcache::Cache::open(&o.root).map(|c| c.uuid_hex() == *u).unwrap_or(false)));
    let jc_private = jc.file_name().is_some_and(|n| n.to_string_lossy() == format!("jc-{}", r.build)) && !others.iter().any(|o| crate::sim::jc_for(&o.root) == jc);
    if jc.is_dir() {
        if jc_private {
            caches.push(Item { bytes: dir_size(&jc), path: jc.clone(), what: "pre-translated code (AOT image, JIT cache, placement, training)".into() });
        } else {
            kept.push(format!("{} is shared with other versions: only this version's files in it go", jc.display()));
        }
    }
    let mut shared_files = |dir: &Path, what: &str| {
        let Some(u) = uuid.as_deref() else { return };
        if uuid_shared {
            return;
        }
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if uuid_prefix(&n) == Some(u) {
                let p = e.path();
                let bytes = if p.is_dir() { dir_size(&p) } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
                caches.push(Item { path: p, bytes, what: what.into() });
            }
        }
    };
    if jc.is_dir() && !jc_private {
        shared_files(&jc, "this version's files in a shared cache");
    }
    for d in shared_dirs(&jc) {
        shared_files(&d, "this version's files in a shared cache");
    }
    if uuid_shared {
        kept.push(format!("files keyed by dyld cache {} stay: another installed version has the same cache", uuid.as_deref().unwrap_or("?")));
    }
    // the shared jitcache's own AOT image when it was built for this runtime
    if !jc_private && jc.is_dir() {
        let cfg = Paths::new(&jc).load_config();
        if cfg.runtime_root == r.root && Paths::new(&jc).aot.is_dir() {
            let a = Paths::new(&jc).aot;
            caches.push(Item { bytes: dir_size(&a), path: a, what: "its AOT image in the shared cache dir".into() });
        }
    }
    let log = crate::host::fetch_log(&r.build);
    if log.exists() {
        caches.push(Item { bytes: std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0), path: log, what: "download log".into() });
    }
    kept.push(format!("cli/libexec/arm64/placement-{0}.txt / training-{0}.x86t ship with x86Sim (part of the repo)", r.build));
    // a cache configured outside the scratch dir (an AOT config's training dir, say) is never removed: listed as kept
    // here rather than failing delete() halfway through
    let (ok, outside): (Vec<Item>, Vec<Item>) = caches.into_iter().partition(|i| removable(&r.dir, &i.path));
    caches = ok;
    for i in outside {
        kept.push(format!("{} stays: outside runtimes/ and the scratch dir", i.path.display()));
    }
    let gone = r.version.is_empty();
    let sums = crate::instance::summaries(0);
    let devices = crate::instance::list()
        .into_iter()
        .filter(|i| i.runtime_dir == r.dir || gone && i.build == r.build)
        .map(|i| Device { state: sums.iter().find(|s| s.id == i.id).map(|s| s.state.clone()).unwrap_or_else(|| "off".into()), id: i.id, name: i.name })
        .collect();
    let aot_running = std::fs::read(Paths::new(&jc).progress())
        .ok()
        .and_then(|b| serde_json::from_slice::<crate::aot::Progress>(&b).ok())
        .filter(|p| matches!(p.state.as_str(), "planning" | "building" | "assembling") && p.pid != 0 && Path::new(&format!("/proc/{}", p.pid)).exists())
        .map(|p| p.pid);
    Plan {
        dir: r.dir.clone(),
        label: if gone { format!("build {} (already deleted; its leftover data)", r.build) } else { r.label() },
        build: r.build.clone(),
        runtime_bytes: runtime.iter().map(|i| i.bytes).sum(),
        runtime,
        caches_bytes: caches.iter().map(|i| i.bytes).sum(),
        caches,
        devices,
        downloading: crate::host::fetches().iter().any(|f| f.build == r.build),
        aot_running,
        kept,
    }
}

/// Only runtimes/<dir> and paths under the scratch dir (or /tmp/x86sim-a64-img) are ever removed
fn removable(dir: &str, path: &Path) -> bool {
    let rtroot = repo_root().join("runtimes");
    path.parent() == Some(rtroot.as_path()) && path.file_name().is_some_and(|n| n.to_string_lossy() == dir) || path.starts_with(scratch()) && path != scratch() || path.starts_with("/tmp/x86sim-a64-img/")
}

/// Removes what the plan lists (`all`: the caches too). Returns the bytes freed. Paths are checked again, all of them
/// before anything is removed (removable()).
pub fn delete(p: &Plan, all: bool) -> Result<u64> {
    if let Some(b) = p.blocker() {
        bail!("can't delete {}: {b}", p.label);
    }
    let mut freed = 0;
    let mut items: Vec<&Item> = p.runtime.iter().collect();
    if all {
        items.extend(p.caches.iter());
    }
    if let Some(it) = items.iter().find(|it| !removable(&p.dir, &it.path)) {
        bail!("refusing to remove {} (outside runtimes/ and the scratch dir): nothing deleted", it.path.display());
    }
    for it in items {
        let path = &it.path;
        let md = match path.symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let r = if md.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        r.with_context(|| format!("removing {}", path.display()))?;
        freed += it.bytes;
    }
    Ok(freed)
}

/// Stops the phones running on the plan's version (each by its own stop), then waits up to `secs` for them to be off.
pub fn stop_devices(p: &Plan, secs: u64) -> Result<()> {
    for d in p.running_devices() {
        let Some(i) = crate::instance::find(&d.id) else { continue };
        crate::sim::stop(&i.dir).with_context(|| format!("stopping {}", d.name))?;
    }
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs() < secs {
        let sums = crate::instance::summaries(0);
        if p.devices.iter().all(|d| sums.iter().find(|s| s.id == d.id).is_none_or(|s| s.state == "off")) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    bail!("the devices did not stop within {secs} s")
}

pub fn gb(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1} GB", b as f64 / 1e9)
    } else {
        format!("{:.0} MB", b as f64 / 1e6)
    }
}

/// The plan as text (CLI)
pub fn describe(p: &Plan) -> String {
    let mut s = if p.runtime.is_empty() { format!("{}\n", p.label) } else { format!("{} (build {}, runtimes/{})\n", p.label, p.build, p.dir) };
    s += &format!("  delete the version only: {}\n", gb(p.runtime_bytes));
    for i in &p.runtime {
        s += &format!("    {:>9}  {}  ({})\n", gb(i.bytes), i.path.display(), i.what);
    }
    s += &format!("  delete everything (--all): {} more\n", gb(p.caches_bytes));
    for i in &p.caches {
        s += &format!("    {:>9}  {}  ({})\n", gb(i.bytes), i.path.display(), i.what);
    }
    for k in &p.kept {
        s += &format!("  kept: {k}\n");
    }
    if !p.devices.is_empty() {
        s += &format!(
            "  devices on it: {} (they can't start until it is downloaded again)\n",
            p.devices.iter().map(|d| format!("{} [{}]", d.name, d.state)).collect::<Vec<_>>().join(", ")
        );
    }
    if let Some(b) = p.blocker() {
        s += &format!("  blocked: {b}\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_goes_when_one_path_is_outside() {
        let d = scratch().join(format!("rtdelete-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let inside = d.join("a");
        std::fs::write(&inside, b"x").unwrap();
        let item = |p: &Path| Item { path: p.to_path_buf(), bytes: 1, what: String::new() };
        let mut p = Plan { dir: "x".into(), label: "x".into(), build: "x".into(), runtime: vec![], runtime_bytes: 0, caches: vec![item(&inside), item(Path::new("/etc/hostname"))], caches_bytes: 2, devices: vec![], downloading: false, aot_running: None, kept: vec![] };
        assert!(delete(&p, true).is_err());
        assert!(inside.exists(), "the item inside stayed: the check came before any removal");
        // a running phone blocks it (aot.rs re-plans after its y/N prompt so a phone started meanwhile is seen)
        p.caches.pop();
        p.devices.push(Device { id: "device-a64-x".into(), name: "x".into(), state: "starting".into() });
        assert!(delete(&p, true).is_err());
        assert!(inside.exists());
        p.devices.clear();
        assert_eq!(delete(&p, true).unwrap(), 1);
        assert!(!inside.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn uuid_prefixes() {
        assert_eq!(uuid_prefix("c54ad6fa9ff23edb9c8a53a82682d342-0000000000060040.stubs"), Some("c54ad6fa9ff23edb9c8a53a82682d342"));
        assert_eq!(uuid_prefix("placement.txt"), None);
        assert_eq!(uuid_prefix("c54ad6fa9ff23edb9c8a53a82682d34z-x"), None);
    }
}
