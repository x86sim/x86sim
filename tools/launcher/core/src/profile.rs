//! x86sim profile: a sampling CPU profiler for apps in a phone (Instruments' Time Profiler).
//!
//! The sampler lives in libarm64jit (linux/arm64jit/prof.cpp). It is started and stopped at run time per process
//! through the debugger's request channel: a request file /run/user/<uid>/x86sim-prof-req-<UDID>-<pid> and SIGUSR1.
//! It writes a raw text file (samples with the host instruction pointer and the guest frame-pointer chain, then the
//! guest blocks of the host addresses, thunk and thread names and the image list). This module symbolicates it with
//! the Mach-O / dyld cache readers (macho.rs, dyldcache.rs: the simulator cache keeps every image's local symbols) and
//! writes speedscope JSON, Firefox Profiler (Gecko) JSON, or a text summary (self / total per function and thread).
//!
//! Limits (README): times are those of the translated x86-64 code, so relative costs differ from a device;
//! frame-pointer stacks only (a frameless leaf shows its caller through LR); a native function the translator runs
//! for the guest (libsystem, objc_msgSend's slow path, ...) is one "[native]" frame; time in the translator itself
//! (compiling, the dispatcher) is its own "[translator]" frame.
use crate::aot::{repo_root, scratch};
use crate::dyldcache::Cache;
use crate::instance::Instance;
use crate::macho::{function_entries, u32le, u64le, AddrSpace, FileImage, Header};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Speedscope,
    Gecko,
    Text,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "speedscope" => Some(Format::Speedscope),
            "gecko" | "firefox" => Some(Format::Gecko),
            "text" | "txt" => Some(Format::Text),
            _ => None,
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Format::Speedscope => "speedscope.json",
            Format::Gecko => "gecko.json",
            Format::Text => "txt",
        }
    }
}

// ---- the process -----------------------------------------------------------------------------------------------

/// A guest process of a phone: its Darwin pid (Darling's pid namespace, as `x86sim pidof`) and host pid.
#[derive(Clone, Debug)]
pub struct Target {
    pub udid: String,
    pub gpid: i32,
    pub hpid: i32,
    pub label: String,
}

pub fn udid_of(dir: &Path) -> Option<String> {
    let env = std::fs::read_to_string(dir.join("sim.env")).ok()?;
    env.lines().find_map(|l| l.strip_prefix("SIMULATOR_UDID=").map(|s| s.trim().to_string()))
}

/// The host pid of the process whose Darwin pid is `gpid` and whose environment has SIMULATOR_UDID=`udid`
/// ("none": a process without one, tests/arm64/run.sh).
pub fn linux_pid(gpid: i32, udid: &str) -> Option<i32> {
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let Ok(st) = std::fs::read_to_string(format!("/proc/{pid}/status")) else { continue };
        let ns: Vec<i32> = st.lines().find_map(|l| l.strip_prefix("NSpid:")).map(|v| v.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
        if ns.len() < 2 || *ns.last().unwrap() != gpid {
            continue;
        }
        let Ok(env) = std::fs::read(format!("/proc/{pid}/environ")) else { continue };
        let want = format!("SIMULATOR_UDID={udid}");
        let has = env.split(|&c| c == 0).any(|v| v == want.as_bytes());
        let any = env.split(|&c| c == 0).any(|v| v.starts_with(b"SIMULATOR_UDID="));
        if has || (udid == "none" && !any) {
            return Some(pid);
        }
    }
    None
}

/// APP (bundle id) or PID of a phone's process.
pub fn resolve(inst: &Instance, app: &str) -> Result<Target> {
    let udid = udid_of(&inst.dir).ok_or_else(|| anyhow!("{} has no sim.env (never booted?)", inst.name))?;
    let gpid: i32 = if let Ok(p) = app.parse() {
        p
    } else {
        let out = Command::new(repo_root().join("cli/x86sim")).arg("--phone").arg(&inst.dir).args(["pidof", app])
            .current_dir(repo_root()).stdin(Stdio::null()).output().context("running x86sim pidof")?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        s.parse().map_err(|_| anyhow!("{app} is not running on {} (x86sim launch {app})", inst.name))?
    };
    let hpid = linux_pid(gpid, &udid).ok_or_else(|| anyhow!("no process {gpid} on {}", inst.name))?;
    Ok(Target { udid, gpid, hpid, label: app.to_string() })
}

fn run_dir() -> PathBuf {
    // SAFETY: getuid has no preconditions
    PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
}

fn request(t: &Target, body: &str) -> Result<()> {
    let p = run_dir().join(format!("x86sim-prof-req-{}-{}", t.udid, t.gpid));
    std::fs::write(&p, body).with_context(|| format!("writing {}", p.display()))?;
    // SAFETY: plain kill(2) of a pid we just looked up
    if unsafe { libc::kill(t.hpid, libc::SIGUSR1) } != 0 {
        let _ = std::fs::remove_file(&p);
        bail!("process {} is gone", t.gpid);
    }
    Ok(())
}

/// A running profile: the raw file the process writes.
#[derive(Clone, Debug)]
pub struct Session {
    pub target: Target,
    pub raw: PathBuf,
    pub started: Instant,
}

pub fn profiles_dir() -> PathBuf {
    scratch().join("profiles")
}

/// Starts the sampler in the process (period_us of CPU time per sample, at most max_s seconds).
pub fn start(t: &Target, period_us: u32, max_s: u32) -> Result<Session> {
    let dir = profiles_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let raw = dir.join(format!("raw-{}-{}-{stamp}.txt", &t.udid[..t.udid.len().min(8)], t.gpid));
    let part = raw.with_extension("txt.part");
    request(t, &format!("start {period_us} {max_s} {}\n", raw.display()))?;
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if part.exists() || raw.exists() {
            return Ok(Session { target: t.clone(), raw, started: Instant::now() });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(run_dir().join(format!("x86sim-prof-req-{}-{}", t.udid, t.gpid)));
    bail!("process {} did not start the profiler (a translator without x86sim profile, or an app that handles SIGUSR1 itself)", t.gpid)
}

/// Stops it and waits for the complete raw file (a process that exited meanwhile leaves "<raw>.part": used as is).
pub fn stop(s: &Session) -> Result<PathBuf> {
    let part = s.raw.with_extension("txt.part");
    let alive = request(&s.target, "stop\n").is_ok();
    let end = Instant::now() + Duration::from_secs(if alive { 60 } else { 1 });
    while Instant::now() < end {
        if s.raw.exists() {
            return Ok(s.raw.clone());
        }
        if !alive || linux_pid(s.target.gpid, &s.target.udid) != Some(s.target.hpid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if s.raw.exists() {
        return Ok(s.raw.clone());
    }
    if part.exists() {
        return Ok(part);
    }
    bail!("no profile from process {} ({})", s.target.gpid, s.raw.display())
}

// ---- the raw file ----------------------------------------------------------------------------------------------

#[derive(Default, Debug)]
pub struct RawSample {
    pub t: u64,
    pub tid: i32,
    pub ip: u64,
    /// thunk index the thread was in (native code), -1 guest code, -2 no guest call on the thread
    pub thunk: i32,
    pub pc: u64,
    pub lr: u64,
    pub frames: Vec<u64>,
}

#[derive(Default, Debug)]
pub struct Raw {
    pub pid: i32,
    pub exe: String,
    pub mode: String,
    pub period_us: u32,
    pub samples: Vec<RawSample>,
    pub blocks: HashMap<u64, u64>,
    pub hosts: HashMap<u64, String>,
    pub thunks: HashMap<i32, String>,
    pub threads: HashMap<i32, String>,
    pub images: Vec<(u64, String)>,
    pub lost: u64,
    pub complete: bool,
    pub warning: Option<String>,
}

fn hx(s: &str) -> u64 {
    u64::from_str_radix(s, 16).unwrap_or(0)
}

pub fn parse_raw(text: &str) -> Raw {
    let mut r = Raw { period_us: 1000, ..Default::default() };
    for l in text.lines() {
        let (tag, rest) = l.split_once(' ').unwrap_or((l, ""));
        match tag {
            "S" => {
                let v: Vec<&str> = rest.split(' ').collect();
                if v.len() < 4 {
                    continue;
                }
                let mut s = RawSample { t: v[0].parse().unwrap_or(0), tid: v[1].parse().unwrap_or(0), ip: hx(v[2]), thunk: v[3].parse().unwrap_or(-2), ..Default::default() };
                if s.thunk != -2 && v.len() >= 6 {
                    s.pc = hx(v[4]);
                    s.lr = hx(v[5]);
                    s.frames = v[6..].iter().map(|x| hx(x)).filter(|&a| a != 0).collect();
                }
                r.samples.push(s);
            }
            "P" => {
                let (p, e) = rest.split_once(' ').unwrap_or((rest, ""));
                r.pid = p.parse().unwrap_or(0);
                r.exe = e.to_string();
            }
            "M" => {
                let (m, p) = rest.split_once(' ').unwrap_or((rest, "1000"));
                r.mode = m.to_string();
                r.period_us = p.trim().parse().unwrap_or(1000);
            }
            "W" => r.warning = Some(rest.to_string()),
            "B" => {
                if let Some((a, b)) = rest.split_once(' ') {
                    r.blocks.insert(hx(a), hx(b));
                }
            }
            "H" => {
                let (a, o) = rest.split_once(' ').unwrap_or((rest, "-"));
                r.hosts.insert(hx(a), o.to_string());
            }
            "N" => {
                if let Some((i, n)) = rest.split_once(' ') {
                    r.thunks.insert(i.parse().unwrap_or(-1), n.to_string());
                }
            }
            "T" => {
                if let Some((i, n)) = rest.split_once(' ') {
                    r.threads.insert(i.parse().unwrap_or(0), n.to_string());
                }
            }
            "I" => {
                if let Some((a, p)) = rest.split_once(' ') {
                    r.images.push((hx(a), p.to_string()));
                }
            }
            "E" => {
                r.complete = true;
                r.lost = rest.split(' ').nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
            }
            _ => {}
        }
    }
    r
}

// ---- symbols ---------------------------------------------------------------------------------------------------

/// One loaded image: its __TEXT range at run time and its function symbols (run-time addresses, sorted).
struct Img {
    lo: u64,
    hi: u64,
    name: String,
    /// (address, name; None: a function start without a symbol)
    syms: Vec<(u64, Option<String>)>,
}

fn darwin_to_linux(p: &str) -> String {
    for pre in ["/private/Volumes/SystemRoot", "/Volumes/SystemRoot"] {
        if let Some(r) = p.strip_prefix(pre) {
            if r.starts_with('/') {
                return r.to_string();
            }
        }
    }
    p.to_string()
}

fn symbols(h: &Header, mem: &dyn AddrSpace, slide: u64) -> Vec<(u64, Option<String>)> {
    let mut v: BTreeMap<u64, Option<String>> = BTreeMap::new();
    for a in function_entries(h, mem) {
        v.insert(a.wrapping_add(slide), None);
    }
    if let Some((symoff, nsyms, stroff, strsize)) = h.symtab {
        let nl = h.linkedit_addr(symoff as u64).and_then(|a| mem.read(a, nsyms as usize * 16));
        let st = h.linkedit_addr(stroff as u64).and_then(|a| mem.read(a, strsize as usize));
        if let (Some(nl), Some(st)) = (nl, st) {
            let code = h.code_ranges();
            for i in 0..nsyms as usize {
                let e = i * 16;
                let ty = nl[e + 4];
                if ty & 0xe0 != 0 || ty & 0x0e != 0x0e {
                    continue;
                }
                let a = u64le(nl, e + 8);
                if !code.iter().any(|&(lo, hi)| a >= lo && a < hi) {
                    continue;
                }
                let sx = u32le(nl, e) as usize;
                if sx >= st.len() {
                    continue;
                }
                let end = st[sx..].iter().position(|&c| c == 0).map(|n| sx + n).unwrap_or(st.len());
                let name = String::from_utf8_lossy(&st[sx..end]).into_owned();
                if name.is_empty() || name.starts_with("ltmp") || name.starts_with("l_") {
                    continue;
                }
                let slot = v.entry(a.wrapping_add(slide)).or_insert(None);
                // a global name over a local alias at the same address; first one wins otherwise
                if slot.is_none() || ty & 1 != 0 {
                    *slot = Some(name);
                }
            }
        }
    }
    v.into_iter().collect()
}

pub struct Symbolizer {
    imgs: Vec<Img>,
}

impl Symbolizer {
    /// The images of a raw profile, read from the phone's runtime (dyld cache) and the files of the others.
    pub fn new(raw: &Raw, runtime_root: Option<&Path>, hit: &dyn Fn(u64, u64) -> bool) -> Symbolizer {
        let cache = runtime_root.and_then(|r| Cache::open(r).ok());
        let by_path: HashMap<&str, usize> = cache.as_ref().map(|c| c.images.iter().enumerate().map(|(i, im)| (im.path.as_str(), i)).collect()).unwrap_or_default();
        let mut imgs = Vec::new();
        let mut files: Vec<(u64, String, String)> = Vec::new();
        for (mh, path) in &raw.images {
            let lin = darwin_to_linux(path);
            let name = lin.rsplit('/').next().unwrap_or(&lin).to_string();
            // a cache image: its install name is the path below the runtime root
            let install = lin.find("/RuntimeRoot/").map(|i| &lin[i + "/RuntimeRoot".len()..]).unwrap_or(&lin);
            if let (Some(c), Some(&i)) = (&cache, by_path.get(install)) {
                let im = &c.images[i];
                let Some(h) = c.header(im) else { continue };
                let slide = mh.wrapping_sub(im.addr);
                let text = h.segments.iter().find(|s| s.name == "__TEXT").map(|s| s.vmsize).unwrap_or(0);
                let (lo, hi) = (*mh, mh + text);
                let syms = if hit(lo, hi) { symbols(&h, c, slide) } else { Vec::new() };
                imgs.push(Img { lo, hi, name, syms });
            } else {
                files.push((*mh, lin, name));
            }
        }
        for (mh, lin, name) in files {
            let Ok(bytes) = std::fs::read(&lin) else { continue };
            let Some(fi) = FileImage::parse(&bytes) else { continue };
            let h = &fi.header;
            let slide = mh.wrapping_sub(h.text_vmaddr());
            let text = h.segments.iter().find(|s| s.name == "__TEXT").map(|s| s.vmsize).unwrap_or(0);
            let (lo, hi) = (mh, mh + text);
            let syms = if hit(lo, hi) { symbols(h, &fi, slide) } else { Vec::new() };
            imgs.push(Img { lo, hi, name, syms });
        }
        imgs.sort_by_key(|i| i.lo);
        Symbolizer { imgs }
    }

    /// (function name, image) of a guest code address; unnamed functions as "image+0xoff" of their start.
    pub fn lookup(&self, a: u64) -> (String, String) {
        let i = self.imgs.partition_point(|im| im.lo <= a);
        if i > 0 {
            let im = &self.imgs[i - 1];
            if a < im.hi {
                let k = im.syms.partition_point(|s| s.0 <= a);
                if k > 0 {
                    let (start, n) = &im.syms[k - 1];
                    return (n.clone().unwrap_or_else(|| format!("{}+{:#x}", im.name, start - im.lo)), im.name.clone());
                }
                return (format!("{}+{:#x}", im.name, a - im.lo), im.name.clone());
            }
        }
        // the translator's stub dylibs (libsystem's guest side, thread start) sit high, outside the image list
        if a >= 0x7f00_0000_0000 {
            return ("[x86sim stub code]".into(), "x86sim".into());
        }
        (format!("{a:#x}"), "?".to_string())
    }
}

/// Darwin symbol names as people read them: no leading underscore, C++ and Swift demangled (c++filt and the Swift
/// toolchain's swift-demangle when they are there).
fn demangle_all(names: &mut [String]) {
    let filt = |prog: &Path, args: &[&str], idx: &[usize], names: &mut [String]| {
        if idx.is_empty() {
            return;
        }
        let Ok(mut ch) = Command::new(prog).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return };
        let input: String = idx.iter().map(|&i| format!("{}\n", names[i])).collect();
        if let Some(mut si) = ch.stdin.take() {
            let _ = std::thread::spawn(move || si.write_all(input.as_bytes()));
        }
        let Ok(out) = ch.wait_with_output() else { return };
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() == idx.len() {
            for (k, &i) in idx.iter().enumerate() {
                if !lines[k].trim().is_empty() {
                    names[i] = lines[k].trim().to_string();
                }
            }
        }
    };
    let cxx: Vec<usize> = (0..names.len()).filter(|&i| names[i].starts_with("__Z")).collect();
    let swift: Vec<usize> = (0..names.len()).filter(|&i| names[i].starts_with("_$s") || names[i].starts_with("$s") || names[i].starts_with("_$S")).collect();
    for n in names.iter_mut() {
        if n.starts_with('_') && !n.starts_with("__Z") {
            n.remove(0);
        }
    }
    for i in &cxx {
        names[*i].remove(0);
    }
    if let Some(p) = which(&["c++filt", "llvm-cxxfilt"]) {
        filt(&p, &[], &cxx, names);
    }
    let sd = std::fs::read_dir(repo_root().join("sdks")).ok().and_then(|d| {
        d.flatten().map(|e| e.path().join("usr/bin/swift-demangle")).find(|p| p.exists())
    }).or_else(|| which(&["swift-demangle"]));
    if let Some(p) = sd {
        filt(&p, &["--simplified"], &swift, names);
    }
}

fn which(progs: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for p in progs {
        for d in std::env::split_paths(&path) {
            let f = d.join(p);
            if f.is_file() {
                return Some(f);
            }
        }
    }
    None
}

// ---- the symbolicated profile ----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Guest,
    Native,
    Translator,
}

#[derive(Debug)]
pub struct Frame {
    pub name: String,
    pub image: String,
    pub kind: Kind,
}

#[derive(Debug, Default)]
pub struct Thread {
    pub tid: i32,
    pub name: String,
    /// (time ns since the first sample, stack index)
    pub samples: Vec<(u64, usize)>,
}

#[derive(Debug, Default)]
pub struct Profile {
    pub process: String,
    pub pid: i32,
    pub mode: String,
    pub period_ns: u64,
    pub frames: Vec<Frame>,
    /// root -> leaf frame indexes
    pub stacks: Vec<Vec<usize>>,
    pub threads: Vec<Thread>,
    pub duration_ns: u64,
    pub lost: u64,
    pub complete: bool,
    pub warning: Option<String>,
}

pub fn symbolicate(raw: &Raw, runtime_root: Option<&Path>) -> Profile {
    // the guest addresses of every sample (images whose symbols are needed)
    let mut addrs: Vec<u64> = Vec::new();
    for s in &raw.samples {
        if let Some(b) = raw.blocks.get(&s.ip) {
            addrs.push(*b);
        } else if s.thunk == -1 && s.ip == 0 {
            addrs.push(s.pc);
        }
        addrs.push(s.lr);
        addrs.extend(&s.frames);
    }
    addrs.sort_unstable();
    addrs.dedup();
    let hit = |lo: u64, hi: u64| {
        let i = addrs.partition_point(|&a| a < lo);
        i < addrs.len() && addrs[i] < hi
    };
    // the runtime of the images' paths (".../RuntimeRoot/usr/lib/..."), when not given
    let inferred: Option<PathBuf> = raw.images.iter().find_map(|(_, p)| {
        let l = darwin_to_linux(p);
        l.find("/RuntimeRoot/").map(|i| PathBuf::from(&l[..i + "/RuntimeRoot".len()]))
    });
    let sym = Symbolizer::new(raw, runtime_root.or(inferred.as_deref()), &hit);
    // a return address is the instruction after the call: look up a-4 (a call at a function's end)
    let mut cache: HashMap<u64, (String, String)> = HashMap::new();
    let mut look = |a: u64, ret: bool| -> (String, String) {
        let q = if ret { a.saturating_sub(4) } else { a };
        cache.entry(q).or_insert_with(|| sym.lookup(q)).clone()
    };
    let mut frame_ix: HashMap<(String, String, Kind), usize> = HashMap::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut intern = |name: String, image: String, kind: Kind, frames: &mut Vec<Frame>| -> usize {
        *frame_ix.entry((name.clone(), image.clone(), kind)).or_insert_with(|| {
            frames.push(Frame { name, image, kind });
            frames.len() - 1
        })
    };
    let mut stack_ix: HashMap<Vec<usize>, usize> = HashMap::new();
    let mut stacks: Vec<Vec<usize>> = Vec::new();
    let mut threads: BTreeMap<i32, Thread> = BTreeMap::new();
    let t0 = raw.samples.iter().map(|s| s.t).min().unwrap_or(0);
    let t1 = raw.samples.iter().map(|s| s.t).max().unwrap_or(0);
    for s in &raw.samples {
        // leaf -> root
        let mut st: Vec<(String, String, Kind)> = Vec::new();
        let leaf_guest: Option<u64> = raw.blocks.get(&s.ip).copied().or(if s.ip == 0 && s.thunk == -1 { Some(s.pc) } else { None });
        if let Some(pc) = leaf_guest {
            let (n, i) = look(pc, false);
            st.push((n, i, Kind::Guest));
        } else if s.thunk >= 0 {
            let n = raw.thunks.get(&s.thunk).cloned().unwrap_or_else(|| format!("thunk #{}", s.thunk));
            st.push((format!("[native] {}", n.trim_start_matches('_')), "native".into(), Kind::Native));
        } else {
            // not translated code and no native call: the translator (libarm64jit, the code cache's dispatcher and
            // helpers: "anon"), or host code it called (glibc)
            let host = raw.hosts.get(&s.ip).map(String::as_str).unwrap_or("-");
            let obj = host.split(' ').next().unwrap_or("-");
            let obj = obj.rsplit('/').next().unwrap_or(obj);
            let what = if obj.starts_with("libarm64jit.so") {
                "[translator]"
            } else if obj == "-" || obj == "anon" || obj.starts_with('[') {
                "[translator: code cache runtime]"
            } else {
                "[translator: host library]"
            };
            st.push((what.into(), obj.to_string(), Kind::Translator));
        }
        if s.thunk != -2 {
            let chain: Vec<(String, String)> = s.frames.iter().map(|&a| look(a, true)).collect();
            if s.lr != 0 {
                let lr = look(s.lr, true);
                let leaf_fn = &st[0].0;
                let first = chain.first().map(|c| &c.0);
                // LR is the caller of a frameless leaf; a leaf with its own frame has it again in the chain (or a
                // stale LR inside itself)
                if &lr.0 != leaf_fn && Some(&lr.0) != first {
                    st.push((lr.0, lr.1, Kind::Guest));
                }
            }
            for (n, i) in chain {
                st.push((n, i, Kind::Guest));
            }
        } else {
            st.push(("[thread without guest code]".into(), "-".into(), Kind::Translator));
        }
        let ids: Vec<usize> = st.into_iter().rev().map(|(n, i, k)| intern(n, i, k, &mut frames)).collect();
        let si = *stack_ix.entry(ids.clone()).or_insert_with(|| {
            stacks.push(ids);
            stacks.len() - 1
        });
        let th = threads.entry(s.tid).or_insert_with(|| Thread {
            tid: s.tid,
            name: raw.threads.get(&s.tid).cloned().filter(|n| n != "-").unwrap_or_else(|| format!("thread {}", s.tid)),
            samples: Vec::new(),
        });
        th.samples.push((s.t - t0, si));
    }
    // demangle the names once
    let mut names: Vec<String> = frames.iter().map(|f| f.name.clone()).collect();
    demangle_all(&mut names);
    for (f, n) in frames.iter_mut().zip(names) {
        f.name = n;
    }
    let mut threads: Vec<Thread> = threads.into_values().collect();
    threads.sort_by(|a, b| b.samples.len().cmp(&a.samples.len()));
    let exe = darwin_to_linux(&raw.exe);
    Profile {
        process: exe.rsplit('/').next().unwrap_or(&exe).to_string(),
        pid: raw.pid,
        mode: raw.mode.clone(),
        period_ns: raw.period_us as u64 * 1000,
        frames,
        stacks,
        threads,
        duration_ns: t1.saturating_sub(t0),
        lost: raw.lost,
        complete: raw.complete,
        warning: raw.warning.clone(),
    }
}

// ---- outputs ---------------------------------------------------------------------------------------------------

pub fn speedscope(p: &Profile) -> Value {
    let frames: Vec<Value> = p.frames.iter().map(|f| json!({"name": f.name, "file": f.image})).collect();
    let profiles: Vec<Value> = p.threads.iter().map(|t| {
        let samples: Vec<&Vec<usize>> = t.samples.iter().map(|&(_, s)| &p.stacks[s]).collect();
        let weights: Vec<u64> = t.samples.iter().map(|_| p.period_ns).collect();
        json!({"type": "sampled", "name": format!("{} ({})", t.name, t.tid), "unit": "nanoseconds", "startValue": 0,
               "endValue": p.period_ns * t.samples.len() as u64, "samples": samples, "weights": weights})
    }).collect();
    json!({"$schema": "https://www.speedscope.app/file-format-schema.json", "exporter": "x86sim profile",
           "name": format!("{} (pid {})", p.process, p.pid), "activeProfileIndex": 0,
           "shared": {"frames": frames}, "profiles": profiles})
}

/// Firefox Profiler: the Gecko profile format (version 24, upgraded on import by profiler.firefox.com).
pub fn gecko(p: &Profile) -> Value {
    let cats = json!([
        {"name": "Other", "color": "grey", "subcategories": ["Other"]},
        {"name": "Guest code", "color": "blue", "subcategories": ["Other"]},
        {"name": "Native (host library)", "color": "green", "subcategories": ["Other"]},
        {"name": "Translator", "color": "orange", "subcategories": ["Other"]},
    ]);
    let cat = |k: Kind| match k {
        Kind::Guest => 1,
        Kind::Native => 2,
        Kind::Translator => 3,
    };
    let threads: Vec<Value> = p.threads.iter().map(|t| {
        let mut strings: Vec<String> = Vec::new();
        let mut str_ix: HashMap<String, usize> = HashMap::new();
        let mut frame_rows: Vec<Value> = Vec::new();
        let mut frame_ix: HashMap<usize, usize> = HashMap::new();
        let mut stack_rows: Vec<Value> = Vec::new();
        let mut stack_ix: HashMap<(Option<usize>, usize), usize> = HashMap::new();
        let mut samples: Vec<Value> = Vec::new();
        for &(time, s) in &t.samples {
            let mut prefix: Option<usize> = None;
            for &f in &p.stacks[s] {
                let fi = *frame_ix.entry(f).or_insert_with(|| {
                    let fr = &p.frames[f];
                    let label = if fr.image.is_empty() || fr.image == "-" { fr.name.clone() } else { format!("{} ({})", fr.name, fr.image) };
                    let si = *str_ix.entry(label.clone()).or_insert_with(|| {
                        strings.push(label);
                        strings.len() - 1
                    });
                    frame_rows.push(json!([si, false, 0, null, null, null, cat(fr.kind), 0]));
                    frame_rows.len() - 1
                });
                let key = (prefix, fi);
                let k = *stack_ix.entry(key).or_insert_with(|| {
                    stack_rows.push(json!([prefix, fi]));
                    stack_rows.len() - 1
                });
                prefix = Some(k);
            }
            samples.push(json!([prefix, time as f64 / 1e6, 0]));
        }
        json!({
            "name": t.name, "processType": "default", "processName": p.process, "pid": p.pid, "tid": t.tid,
            "registerTime": 0, "unregisterTime": null,
            "samples": {"schema": {"stack": 0, "time": 1, "eventDelay": 2}, "data": samples},
            "markers": {"schema": {"name": 0, "startTime": 1, "endTime": 2, "phase": 3, "category": 4, "data": 5}, "data": []},
            "stackTable": {"schema": {"prefix": 0, "frame": 1}, "data": stack_rows},
            "frameTable": {"schema": {"location": 0, "relevantForJS": 1, "innerWindowID": 2, "implementation": 3, "line": 4, "column": 5, "category": 6, "subcategory": 7}, "data": frame_rows},
            "stringTable": strings,
        })
    }).collect();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0);
    json!({
        "meta": {"version": 24, "startTime": now - p.duration_ns as f64 / 1e6, "shutdownTime": null, "interval": p.period_ns as f64 / 1e6,
                 "stackwalk": 1, "processType": 0, "product": format!("x86Sim {} (pid {})", p.process, p.pid), "debug": false,
                 "gcpoison": false, "asyncstack": false, "presymbolicated": true, "categories": cats, "markerSchema": [],
                 "platform": "x86Sim (arm64 translated to x86-64)", "oscpu": "", "abi": "", "misc": "", "toolkit": "", "appBuildID": "", "sourceURL": "",
                 "physicalCPUs": 0, "logicalCPUs": 0, "pausedRanges": []},
        "libs": [], "threads": threads, "processes": [], "pausedRanges": [],
    })
}

/// Self and total samples per function (a function counted once per sample for total).
fn tally<'a>(p: &Profile, samples: impl Iterator<Item = &'a (u64, usize)>) -> (u64, Vec<(usize, u64, u64)>) {
    let mut selfc: HashMap<usize, u64> = HashMap::new();
    let mut total: HashMap<usize, u64> = HashMap::new();
    let mut n = 0u64;
    for &(_, s) in samples {
        n += 1;
        let st = &p.stacks[s];
        if let Some(&leaf) = st.last() {
            *selfc.entry(leaf).or_default() += 1;
        }
        let mut seen: Vec<usize> = Vec::with_capacity(st.len());
        for &f in st {
            if !seen.contains(&f) {
                seen.push(f);
                *total.entry(f).or_default() += 1;
            }
        }
    }
    let mut v: Vec<(usize, u64, u64)> = total.iter().map(|(&f, &t)| (f, selfc.get(&f).copied().unwrap_or(0), t)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
    (n, v)
}

pub fn text(p: &Profile, top: usize) -> String {
    let mut o = String::new();
    let all: Vec<&(u64, usize)> = p.threads.iter().flat_map(|t| t.samples.iter()).collect();
    let ms = |n: u64| n as f64 * p.period_ns as f64 / 1e6;
    let pct = |a: u64, n: u64| if n == 0 { 0.0 } else { 100.0 * a as f64 / n as f64 };
    let (n, v) = tally(p, all.iter().copied());
    o += &format!("x86sim profile: {} (pid {}), {:.1} s, {} samples of {:.1} ms CPU each ({:.0} ms CPU){}{}\n",
        p.process, p.pid, p.duration_ns as f64 / 1e9, n, p.period_ns as f64 / 1e6, ms(n),
        if p.mode == "perf" { "" } else { ", polled (no CPU-time sampling: perf_event_open refused)" },
        if p.complete { "" } else { ", INCOMPLETE (the process exited)" });
    if p.lost > 0 {
        o += &format!("  ({} samples lost)\n", p.lost);
    }
    o += "  times are of the translated x86-64 code (relative costs differ from a device)\n";
    let mut by_kind: HashMap<Kind, u64> = HashMap::new();
    for &&(_, s) in &all {
        if let Some(&leaf) = p.stacks[s].last() {
            *by_kind.entry(p.frames[leaf].kind).or_default() += 1;
        }
    }
    o += &format!("  guest code {:.1}%, native library functions {:.1}%, translator {:.1}%\n",
        pct(by_kind.get(&Kind::Guest).copied().unwrap_or(0), n), pct(by_kind.get(&Kind::Native).copied().unwrap_or(0), n),
        pct(by_kind.get(&Kind::Translator).copied().unwrap_or(0), n));
    let row = |o: &mut String, f: usize, s: u64, t: u64, n: u64| {
        let fr = &p.frames[f];
        let mut name = fr.name.clone();
        if name.chars().count() > 90 {
            name = name.chars().take(87).collect::<String>() + "...";
        }
        *o += &format!("  {:6.1}% {:8.1} ms  {:6.1}% total  {}  [{}]\n", pct(s, n), ms(s), pct(t, n), name, fr.image);
    };
    o += &format!("\nTop {top} by self time (all threads):\n");
    for &(f, s, t) in v.iter().take(top) {
        row(&mut o, f, s, t, n);
    }
    let mut vt = v.clone();
    vt.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
    o += &format!("\nTop {top} by total time (all threads):\n");
    for &(f, s, t) in vt.iter().take(top) {
        row(&mut o, f, s, t, n);
    }
    o += "\nThreads:\n";
    for th in &p.threads {
        let (tn, tv) = tally(p, th.samples.iter());
        o += &format!("\n  {} (tid {}): {} samples, {:.1}% of the profile\n", th.name, th.tid, tn, pct(tn, n));
        for &(f, s, t) in tv.iter().take(top.min(8)) {
            row(&mut o, f, s, t, tn);
        }
    }
    o
}

/// Symbolicates a raw file and writes `format` to `out` (None: next to the raw file). Answers (output path, text summary).
pub fn report(raw_path: &Path, runtime_root: Option<&Path>, format: Format, out: Option<&Path>, top: usize) -> Result<(PathBuf, String, Profile)> {
    let text_raw = std::fs::read_to_string(raw_path).with_context(|| format!("reading {}", raw_path.display()))?;
    let raw = parse_raw(&text_raw);
    if raw.samples.is_empty() {
        bail!("no samples (the process used no CPU while it was profiled, or it exited at once)");
    }
    let prof = symbolicate(&raw, runtime_root);
    let summary = text(&prof, top);
    let base = raw_path.to_string_lossy().trim_end_matches(".part").trim_end_matches(".txt").replace("/raw-", &format!("/{}-", prof.process));
    let out = out.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(format!("{base}.{}", format.ext())));
    let body = match format {
        Format::Speedscope => serde_json::to_string(&speedscope(&prof))?,
        Format::Gecko => serde_json::to_string(&gecko(&prof))?,
        Format::Text => summary.clone(),
    };
    std::fs::write(&out, body).with_context(|| format!("writing {}", out.display()))?;
    Ok((out, summary, prof))
}

/// One profile of `app` (None: the app in front) for `secs` seconds, for the browser viewer's Profile button:
/// (file name, the file's bytes).
pub fn take(inst: &Instance, app: Option<&str>, secs: f64, format: Format) -> Result<(String, Vec<u8>)> {
    let app = match app.filter(|a| !a.is_empty()) {
        Some(a) => a.to_string(),
        None => crate::ax::snapshot(inst, crate::ax::SnapOpts::default())?.front.first().cloned().filter(|f| !f.is_empty())
            .ok_or_else(|| anyhow!("no app in front"))?,
    };
    let t = resolve(inst, &app)?;
    let s = start(&t, 1000, secs.ceil() as u32 + 60)?;
    std::thread::sleep(Duration::from_secs_f64(secs.clamp(0.5, 120.0)));
    let raw = stop(&s)?;
    let (path, _, _) = report(&raw, Some(&inst.runtime_root), format, None, 25)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "profile.json".into());
    Ok((name, std::fs::read(&path)?))
}

// ---- x86sim profile --------------------------------------------------------------------------------------------

pub const USAGE: &str = "usage: x86sim profile [PHONE] APP|PID [--secs N] [--out FILE] [--format speedscope|gecko|text] [--period-us N] [--top N]
       x86sim profile --raw RAWFILE [--runtime DIR] [--out FILE] [--format ...]   (symbolicate a raw sample file again)
  Samples the app's threads while they run on a CPU (1 sample per --period-us of CPU time, default 1000) for --secs
  seconds (default 10), prints the top functions by self and total time and writes the profile (default speedscope
  JSON under $X86SIM_SCRATCH/profiles; gecko: Firefox Profiler). Open speedscope files at https://www.speedscope.app,
  gecko files at https://profiler.firefox.com.";

pub fn cli(args: &[String]) -> i32 {
    match cli_inner(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("x86sim profile: {e:#}");
            1
        }
    }
}

fn cli_inner(args: &[String]) -> Result<()> {
    let (mut phone, mut secs, mut out, mut format, mut period, mut top, mut raw, mut runtime, mut udid) =
        (None::<String>, 10.0f64, None::<PathBuf>, Format::Speedscope, 1000u32, 25usize, None::<PathBuf>, None::<PathBuf>, None::<String>);
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    let val = |i: &mut usize| -> Result<String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| anyhow!("{} needs a value\n{USAGE}", args[*i - 1]))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--secs" | "-s" => secs = val(&mut i)?.parse().context("--secs")?,
            "--out" | "-o" => out = Some(PathBuf::from(val(&mut i)?)),
            "--format" | "-f" => {
                let f = val(&mut i)?;
                format = Format::parse(&f).ok_or_else(|| anyhow!("unknown format {f} (speedscope, gecko, text)"))?;
            }
            "--period-us" => period = val(&mut i)?.parse().context("--period-us")?,
            "--top" => top = val(&mut i)?.parse().context("--top")?,
            "--phone" => phone = Some(val(&mut i)?),
            "--raw" => raw = Some(PathBuf::from(val(&mut i)?)),
            "--runtime" => runtime = Some(PathBuf::from(val(&mut i)?)),
            "--udid" => udid = Some(val(&mut i)?),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            a if a.starts_with('-') => bail!("unknown argument {a}\n{USAGE}"),
            a => pos.push(a.to_string()),
        }
        i += 1;
    }
    if let Some(r) = raw {
        let (path, summary, _) = report(&r, runtime.as_deref(), format, out.as_deref(), top)?;
        print!("{summary}");
        println!("\nprofile: {}", path.display());
        return Ok(());
    }
    let app = match pos.len() {
        1 => pos[0].clone(),
        2 => {
            phone = Some(pos[0].clone());
            pos[1].clone()
        }
        _ => bail!("{USAGE}"),
    };
    // a process outside any phone (tests/arm64/run.sh): --udid none PID
    let (target, rt) = if let Some(u) = udid {
        let gpid: i32 = app.parse().context("--udid takes a PID")?;
        let hpid = linux_pid(gpid, &u).ok_or_else(|| anyhow!("no process {gpid} with SIMULATOR_UDID {u}"))?;
        (Target { udid: u, gpid, hpid, label: app.clone() }, runtime)
    } else {
        let inst = match phone.as_deref() {
            Some(p) => crate::instance::find(p),
            None => std::env::var("X86SIM_DEVICE").ok().and_then(|d| crate::instance::find(&d)).or_else(|| {
                let running: Vec<_> = crate::instance::summaries(0).into_iter().filter(|s| s.state != "off").collect();
                match running.as_slice() {
                    [one] => crate::instance::find(&one.id),
                    _ => None,
                }
            }),
        }
        .ok_or_else(|| anyhow!("which device? ({})", crate::instance::list().iter().map(|i| i.name.clone()).collect::<Vec<_>>().join(", ")))?;
        (resolve(&inst, &app)?, Some(runtime.unwrap_or(inst.runtime_root.clone())))
    };
    let s = start(&target, period, (secs.ceil() as u32).saturating_add(60))?;
    eprintln!("x86sim profile: sampling {} (pid {}) for {secs} s", target.label, target.gpid);
    std::thread::sleep(Duration::from_secs_f64(secs));
    let raw = stop(&s)?;
    let (path, summary, _) = report(&raw, rt.as_deref(), format, out.as_deref(), top)?;
    print!("{summary}");
    println!("\nprofile: {}", path.display());
    println!("raw samples: {}", raw.display());
    Ok(())
}
