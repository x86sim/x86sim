//! Web Inspector: Safari's and apps' web pages on a phone, over the Remote Web Inspector protocol (RWI) that Safari's
//! Develop menu speaks to a simulator on a Mac.
//!
//! The phone side is the runtime's own `webinspectord` (job com.apple.webinspectord, the `webinspector` feature): on a
//! Mac launchd_sim binds its debugger socket in a temporary directory, here libsimshim binds it at a short host path,
//! `/run/user/<uid>/x86sim-rwi-<UDID>.sock` (mksimdevice.py, host/simshim). The job is on demand: when nobody listens
//! there, an XPC message to com.apple.webinspector through host/openurl (`xpcping`) makes launchd_sim start it, and the
//! web views of running apps connect to it once it posts its availability.
//!
//! Wire format: a 4-byte big-endian length, then a binary plist `{__selector, __argument}`. We send
//! `_rpc_reportIdentifier:`, `_rpc_getConnectedApplications:`, `_rpc_forwardGetListing:` (pages of an app),
//! `_rpc_forwardSocketSetup:` (open an inspector session on a page), `_rpc_forwardSocketData:` (inspector protocol JSON)
//! and `_rpc_forwardDidClose:`; webinspectord answers `_rpc_reportConnectedApplicationList:`,
//! `_rpc_applicationConnected:` / `Disconnected:` / `Updated:`, `_rpc_applicationSentListing:` and
//! `_rpc_applicationSentData:` (inspector protocol JSON for one session, WIRDestinationKey = our sender key).
//!
//! The inspector protocol is WebKit's (not Chrome's): a page session is multiplexed through the `Target` domain
//! (`Target.targetCreated`, `Target.sendMessageToTarget`, `Target.dispatchMessageFromTarget`,
//! `Target.didCommitProvisionalTarget` when a navigation swaps processes). WebKit's own Web Inspector (the frontend the
//! share server serves, share/inspect.rs) handles that itself: a [`Session`] is a raw pipe. [`Page`] unwraps it for the
//! agents' tools (MCP web_eval, web_console, web_dom, web_network).
use crate::instance::Instance;
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------------------------------
// Binary property lists (bplist00): the subset RWI uses

#[derive(Debug, Clone, PartialEq)]
pub enum Pl {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(String),
    Data(Vec<u8>),
    Array(Vec<Pl>),
    Dict(Vec<(String, Pl)>),
}

impl Pl {
    pub fn get(&self, k: &str) -> Option<&Pl> {
        match self {
            Pl::Dict(d) => d.iter().find(|(n, _)| n == k).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Pl::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn dict(&self) -> &[(String, Pl)] {
        match self {
            Pl::Dict(d) => d,
            _ => &[],
        }
    }
    /// A string, or data as UTF-8 (inspector messages come as data)
    pub fn text(&self) -> Option<String> {
        match self {
            Pl::Str(s) => Some(s.clone()),
            Pl::Data(d) => Some(String::from_utf8_lossy(d).into_owned()),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Pl::Int(i) => Some(*i),
            Pl::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn truthy(&self) -> bool {
        matches!(self, Pl::Bool(true)) || matches!(self, Pl::Int(i) if *i != 0)
    }
}

fn dict(kv: Vec<(&str, Pl)>) -> Pl {
    Pl::Dict(kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub fn bplist_encode(top: &Pl) -> Vec<u8> {
    // objects in order, each referring to later ones by index (no sharing: messages are small)
    fn count(p: &Pl) -> usize {
        1 + match p {
            Pl::Array(a) => a.iter().map(count).sum(),
            Pl::Dict(d) => d.iter().map(|(_, v)| 1 + count(v)).sum(),
            _ => 0,
        }
    }
    let n = count(top);
    let rs: usize = if n < 256 { 1 } else if n < 65536 { 2 } else { 4 };
    let mut out = b"bplist00".to_vec();
    let mut offsets: Vec<usize> = Vec::with_capacity(n);
    fn len_marker(out: &mut Vec<u8>, kind: u8, len: usize) {
        if len < 15 {
            out.push(kind << 4 | len as u8);
        } else {
            out.push(kind << 4 | 0xf);
            int_obj(out, len as i64);
        }
    }
    fn int_obj(out: &mut Vec<u8>, i: i64) {
        if (0..=0xff).contains(&i) {
            out.extend([0x10, i as u8]);
        } else if (0..=0xffff).contains(&i) {
            out.push(0x11);
            out.extend((i as u16).to_be_bytes());
        } else if (0..=0xffff_ffff).contains(&i) {
            out.push(0x12);
            out.extend((i as u32).to_be_bytes());
        } else {
            out.push(0x13);
            out.extend(i.to_be_bytes());
        }
    }
    fn put_ref(out: &mut Vec<u8>, r: usize, rs: usize) {
        out.extend(&(r as u32).to_be_bytes()[4 - rs..]);
    }
    // writes p as object `idx` and its children after it; returns the next free index
    fn write(p: &Pl, idx: usize, out: &mut Vec<u8>, offsets: &mut Vec<usize>, rs: usize) -> usize {
        offsets.push(out.len());
        debug_assert_eq!(offsets.len(), idx + 1);
        match p {
            Pl::Null => {
                out.push(0);
                idx + 1
            }
            Pl::Bool(b) => {
                out.push(if *b { 9 } else { 8 });
                idx + 1
            }
            Pl::Int(i) => {
                int_obj(out, *i);
                idx + 1
            }
            Pl::Real(f) => {
                out.push(0x23);
                out.extend(f.to_be_bytes());
                idx + 1
            }
            Pl::Str(s) => {
                if s.is_ascii() {
                    len_marker(out, 5, s.len());
                    out.extend(s.as_bytes());
                } else {
                    let u: Vec<u16> = s.encode_utf16().collect();
                    len_marker(out, 6, u.len());
                    for c in u {
                        out.extend(c.to_be_bytes());
                    }
                }
                idx + 1
            }
            Pl::Data(d) => {
                len_marker(out, 4, d.len());
                out.extend(d);
                idx + 1
            }
            Pl::Array(a) => {
                len_marker(out, 0xa, a.len());
                // child indices: laid out right after this object, each subtree in turn
                let refs_at = out.len();
                out.resize(out.len() + a.len() * rs, 0);
                let mut next = idx + 1;
                let mut refs = Vec::with_capacity(a.len());
                for v in a {
                    refs.push(next);
                    next = write(v, next, out, offsets, rs);
                }
                let mut r = Vec::new();
                for x in refs {
                    put_ref(&mut r, x, rs);
                }
                out[refs_at..refs_at + r.len()].copy_from_slice(&r);
                next
            }
            Pl::Dict(d) => {
                len_marker(out, 0xd, d.len());
                let refs_at = out.len();
                out.resize(out.len() + d.len() * 2 * rs, 0);
                let mut next = idx + 1;
                let (mut kr, mut vr) = (Vec::new(), Vec::new());
                for (k, v) in d {
                    kr.push(next);
                    next = write(&Pl::Str(k.clone()), next, out, offsets, rs);
                    vr.push(next);
                    next = write(v, next, out, offsets, rs);
                }
                let mut r = Vec::new();
                for x in kr.into_iter().chain(vr) {
                    put_ref(&mut r, x, rs);
                }
                out[refs_at..refs_at + r.len()].copy_from_slice(&r);
                next
            }
        }
    }
    write(top, 0, &mut out, &mut offsets, rs);
    let table = out.len();
    for o in &offsets {
        out.extend((*o as u32).to_be_bytes());
    }
    out.extend([0u8; 6]);
    out.push(4);
    out.push(rs as u8);
    out.extend((offsets.len() as u64).to_be_bytes());
    out.extend(0u64.to_be_bytes());
    out.extend((table as u64).to_be_bytes());
    out
}

pub fn bplist_decode(b: &[u8]) -> Result<Pl> {
    if b.len() < 40 || &b[..8] != b"bplist00" {
        bail!("not a binary plist");
    }
    let t = &b[b.len() - 32..];
    let os = t[6] as usize;
    let rs = t[7] as usize;
    let n = u64::from_be_bytes(t[8..16].try_into()?) as usize;
    let top = u64::from_be_bytes(t[16..24].try_into()?) as usize;
    let table = u64::from_be_bytes(t[24..32].try_into()?) as usize;
    if !(1..=8).contains(&os) || !(1..=8).contains(&rs) || n > b.len() || table.checked_add(n * os).is_none_or(|e| e > b.len()) {
        bail!("bad plist trailer");
    }
    let be = |s: &[u8]| s.iter().fold(0u64, |a, x| a << 8 | *x as u64);
    let off = |i: usize| -> Result<usize> {
        if i >= n {
            bail!("plist object {i} out of range");
        }
        Ok(be(&b[table + i * os..table + (i + 1) * os]) as usize)
    };
    fn obj(b: &[u8], i: usize, depth: usize, off: &dyn Fn(usize) -> Result<usize>, rs: usize, be: &dyn Fn(&[u8]) -> u64) -> Result<Pl> {
        if depth > 64 {
            bail!("plist too deep");
        }
        let o = off(i)?;
        let get = |a: usize, l: usize| b.get(a..a + l).ok_or_else(|| anyhow!("plist object past the end"));
        let m = *b.get(o).ok_or_else(|| anyhow!("plist offset past the end"))?;
        let (kind, low) = (m >> 4, (m & 0xf) as usize);
        // a length: in the marker, or an int object after it
        let len = |o: usize| -> Result<(usize, usize)> {
            if low != 0xf {
                return Ok((low, o + 1));
            }
            let im = *b.get(o + 1).ok_or_else(|| anyhow!("plist length past the end"))?;
            let w = 1usize << (im & 0xf);
            Ok((be(get(o + 2, w)?) as usize, o + 2 + w))
        };
        Ok(match kind {
            0 => match m {
                8 => Pl::Bool(false),
                9 => Pl::Bool(true),
                _ => Pl::Null,
            },
            1 => {
                let w = 1usize << low;
                let v = get(o + 1, w)?;
                Pl::Int(if w == 8 { i64::from_be_bytes(v.try_into()?) } else { be(v) as i64 })
            }
            2 => match low {
                2 => Pl::Real(f32::from_be_bytes(get(o + 1, 4)?.try_into()?) as f64),
                _ => Pl::Real(f64::from_be_bytes(get(o + 1, 8)?.try_into()?)),
            },
            3 => Pl::Real(f64::from_be_bytes(get(o + 1, 8)?.try_into()?)),
            4 => {
                let (l, s) = len(o)?;
                Pl::Data(get(s, l)?.to_vec())
            }
            5 => {
                let (l, s) = len(o)?;
                Pl::Str(String::from_utf8_lossy(get(s, l)?).into_owned())
            }
            6 => {
                let (l, s) = len(o)?;
                let u: Vec<u16> = get(s, l * 2)?.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                Pl::Str(String::from_utf16_lossy(&u))
            }
            8 => Pl::Int(be(get(o + 1, low + 1)?) as i64),
            0xa | 0xc => {
                let (l, s) = len(o)?;
                let mut v = Vec::with_capacity(l.min(4096));
                for k in 0..l {
                    v.push(obj(b, be(get(s + k * rs, rs)?) as usize, depth + 1, off, rs, be)?);
                }
                Pl::Array(v)
            }
            0xd => {
                let (l, s) = len(o)?;
                let mut v = Vec::with_capacity(l.min(4096));
                for k in 0..l {
                    let key = obj(b, be(get(s + k * rs, rs)?) as usize, depth + 1, off, rs, be)?;
                    let val = obj(b, be(get(s + (l + k) * rs, rs)?) as usize, depth + 1, off, rs, be)?;
                    v.push((key.text().unwrap_or_default(), val));
                }
                Pl::Dict(v)
            }
            _ => Pl::Null,
        })
    }
    obj(b, top, 0, &off, rs, &be)
}

// ---------------------------------------------------------------------------------------------------------------------
// The connection to webinspectord

/// webinspectord's debugger socket for a phone (mksimdevice.py's rwi_socket)
pub fn socket_path(inst: &Instance) -> PathBuf {
    PathBuf::from(format!("/run/user/{}/x86sim-rwi-{}.sock", unsafe { libc::getuid() }, inst.udid))
}

/// One request to host/openurl (the com.x86sim.openurl job), as cli/libexec/simctl.py sends it: a line
/// "req <id> <json>" in <data>/tmp/x86sim-openurl, the answer a line "result <id> <json>" in its log
pub fn helper_request(inst: &Instance, req: &Value, timeout: Duration) -> Result<Value> {
    let data = inst.dir.join("data");
    let q = data.join("tmp/x86sim-openurl");
    let log = data.join("var/log/x86sim-openurl.log");
    let rid = format!("{:012x}", rand_u64() & 0xffff_ffff_ffff);
    let start = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let staging = q.with_extension(format!("new.{}.{rid}", std::process::id()));
    std::fs::write(&staging, format!("req {rid} {req}\n"))?;
    let lock = std::fs::OpenOptions::new().create(true).append(true).open(q.with_extension("lock"))?;
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    for _ in 0..100 {
        if !q.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let r = std::fs::rename(&staging, &q);
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    r?;
    let key = format!("result {rid} ");
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
        let Ok(mut f) = std::fs::File::open(&log) else { continue };
        use std::io::{Seek, SeekFrom};
        let _ = f.seek(SeekFrom::Start(start));
        let mut s = String::new();
        let _ = f.read_to_string(&mut s);
        if let Some(l) = s.lines().find(|l| l.starts_with(&key)) {
            if let Ok(v) = serde_json::from_str::<Value>(&l[key.len()..]) {
                return Ok(v);
            }
        }
    }
    bail!("{}: no answer from the device's helper in {} s (is it running?)", inst.name, timeout.as_secs())
}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    u64::from_le_bytes(b)
}

fn uuid() -> String {
    let (a, b) = (rand_u64(), rand_u64());
    format!("{:08X}-{:04X}-4{:03X}-{:04X}-{:012X}", a >> 32, (a >> 16) & 0xffff, a & 0xfff, (b >> 48) & 0x3fff | 0x8000, b & 0xffff_ffff_ffff)
}

pub struct Conn {
    s: UnixStream,
    id: String,
    buf: Vec<u8>,
}

impl Conn {
    /// Connects to the phone's webinspectord, starting it first if nobody listens. webinspectord exits once its last
    /// debugger goes (EnableTransactions), and a connection made while it was exiting is reset: so a connection counts
    /// once webinspectord has answered (_rpc_reportCurrentState comes at once), else it is made again.
    pub fn open(inst: &Instance) -> Result<Conn> {
        let mut last = None;
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(400));
            }
            let s = Self::connect(inst)?;
            let mut c = Conn { s, id: uuid(), buf: vec![] };
            let ok = c.send("_rpc_reportIdentifier:", vec![]).and_then(|_| c.recv(Duration::from_secs(5)));
            match ok {
                Ok(_) => return Ok(c),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("webinspectord did not answer")))
    }

    fn connect(inst: &Instance) -> Result<UnixStream> {
        let path = socket_path(inst);
        if let Ok(s) = UnixStream::connect(&path) {
            return Ok(s);
        }
        if crate::ring::Ring::new(&inst.ring).seq() == 0 {
            bail!("{} is not running", inst.name);
        }
        if !inst.dir.join("LaunchDaemons/com.apple.webinspectord.plist").exists() {
            bail!("{}: no Web Inspector on this device (the webinspector feature is off, or the device was booted before it existed): turn it on and restart the device", inst.name);
        }
        // an XPC message to its Mach service: launchd_sim starts the job
        let r = helper_request(inst, &json!({"cmd": "xpcping", "service": "com.apple.webinspector"}), Duration::from_secs(20))?;
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match UnixStream::connect(&path) {
                Ok(s) => return Ok(s),
                Err(e) if Instant::now() > end => bail!("{}: webinspectord did not come up ({e}; helper: {})", inst.name, r),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }

    pub fn send(&mut self, sel: &str, mut arg: Vec<(&str, Pl)>) -> Result<()> {
        arg.push(("WIRConnectionIdentifierKey", Pl::Str(self.id.clone())));
        let p = bplist_encode(&dict(vec![("__selector", Pl::Str(sel.into())), ("__argument", dict(arg))]));
        let mut m = (p.len() as u32).to_be_bytes().to_vec();
        m.extend(p);
        self.s.write_all(&m).context("writing to webinspectord")
    }

    /// The next message (selector, argument), or None after `timeout`
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<(String, Pl)>> {
        let end = Instant::now() + timeout;
        loop {
            if self.buf.len() >= 4 {
                let n = u32::from_be_bytes(self.buf[..4].try_into()?) as usize;
                if n > 256 << 20 {
                    bail!("webinspectord sent a {n}-byte message");
                }
                if self.buf.len() >= 4 + n {
                    let m = bplist_decode(&self.buf[4..4 + n]);
                    self.buf.drain(..4 + n);
                    let m = m?;
                    let sel = m.get("__selector").and_then(Pl::str).unwrap_or("").to_string();
                    return Ok(Some((sel, m.get("__argument").cloned().unwrap_or(Pl::Null))));
                }
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.s.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
            let mut b = [0u8; 65536];
            match self.s.read(&mut b) {
                Ok(0) => bail!("webinspectord closed the connection"),
                Ok(k) => self.buf.extend(&b[..k]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(None),
                Err(e) => return Err(e).context("reading from webinspectord"),
            }
        }
    }

    pub fn try_clone(&self) -> Result<UnixStream> {
        Ok(self.s.try_clone()?)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Targets: applications and their pages

#[derive(Debug, Clone, Serialize)]
pub struct WebPage {
    /// "<app id>/<page id>", what the other calls take
    pub id: String,
    pub app_id: String,
    pub page: i64,
    pub title: String,
    pub url: String,
    /// web-page | javascript | service-worker | itml | automation
    pub kind: String,
    pub bundle: String,
    pub app_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebApp {
    pub id: String,
    pub bundle: String,
    pub name: String,
    pub pid: Option<i64>,
    pub active: bool,
    pub proxy: bool,
    pub host: Option<String>,
    pub pages: Vec<WebPage>,
}

fn kind_of(t: &str) -> &'static str {
    match t {
        "WIRTypeWebPage" | "WIRTypeWeb" => "web-page",
        "WIRTypeJavaScript" => "javascript",
        "WIRTypeServiceWorker" => "service-worker",
        "WIRTypeITML" => "itml",
        "WIRTypeAutomation" => "automation",
        _ => "other",
    }
}

fn app_of(id: &str, a: &Pl) -> WebApp {
    let s = |k: &str| a.get(k).and_then(Pl::text).unwrap_or_default();
    WebApp {
        id: id.to_string(),
        bundle: s("WIRApplicationBundleIdentifierKey"),
        name: s("WIRApplicationNameKey"),
        pid: id.strip_prefix("PID:").and_then(|p| p.parse().ok()),
        active: a.get("WIRIsApplicationActiveKey").is_some_and(Pl::truthy),
        proxy: a.get("WIRIsApplicationProxyKey").is_some_and(Pl::truthy),
        host: a.get("WIRHostApplicationIdentifierKey").and_then(Pl::text),
        pages: vec![],
    }
}

/// Every application with inspectable content and its pages (Safari's tabs, WKWebViews with isInspectable, JSContexts).
/// `wait`: how long to collect listings (they come one message per application).
pub fn targets(inst: &Instance, wait: Duration) -> Result<Vec<WebApp>> {
    retry(|| targets_on(&mut Conn::open(inst)?, wait))
}

/// `targets` on a connection made already (a tool then opens its session on the same one)
pub fn targets_on(c: &mut Conn, wait: Duration) -> Result<Vec<WebApp>> {
    c.send("_rpc_getConnectedApplications:", vec![])?;
    let mut apps: Vec<WebApp> = vec![];
    let mut asked: Vec<String> = vec![];
    let mut listed: Vec<String> = vec![];
    let end = Instant::now() + wait;
    let mut got_list = false;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        // done when the list came and every application asked for has answered
        if left.is_zero() || (got_list && asked.iter().all(|a| listed.contains(a)) && !asked.is_empty()) {
            break;
        }
        let Some((sel, a)) = c.recv(left.min(Duration::from_millis(300)))? else {
            if got_list && asked.is_empty() {
                break;
            }
            continue;
        };
        match sel.as_str() {
            "_rpc_reportConnectedApplicationList:" => {
                got_list = true;
                for (id, v) in a.get("WIRApplicationDictionaryKey").map(Pl::dict).unwrap_or(&[]) {
                    if !apps.iter().any(|x| &x.id == id) {
                        apps.push(app_of(id, v));
                    }
                    if !asked.contains(id) {
                        asked.push(id.clone());
                        c.send("_rpc_forwardGetListing:", vec![("WIRApplicationIdentifierKey", Pl::Str(id.clone()))])?;
                    }
                }
            }
            "_rpc_applicationConnected:" | "_rpc_applicationUpdated:" => {
                let id = a.get("WIRApplicationIdentifierKey").and_then(Pl::text).unwrap_or_default();
                if !apps.iter().any(|x| x.id == id) {
                    apps.push(app_of(&id, &a));
                }
                if !asked.contains(&id) {
                    asked.push(id.clone());
                    c.send("_rpc_forwardGetListing:", vec![("WIRApplicationIdentifierKey", Pl::Str(id))])?;
                }
            }
            "_rpc_applicationSentListing:" => {
                let id = a.get("WIRApplicationIdentifierKey").and_then(Pl::text).unwrap_or_default();
                listed.push(id.clone());
                let pages: Vec<(String, Pl)> = a.get("WIRListingKey").map(|l| l.dict().to_vec()).unwrap_or_default();
                if let Some(app) = apps.iter_mut().find(|x| x.id == id) {
                    app.pages.clear();
                    for (_, p) in pages {
                        let s = |k: &str| p.get(k).and_then(Pl::text).unwrap_or_default();
                        let page = p.get("WIRPageIdentifierKey").and_then(Pl::int).unwrap_or(0);
                        app.pages.push(WebPage {
                            id: format!("{id}/{page}"),
                            app_id: id.clone(),
                            page,
                            title: s("WIRTitleKey"),
                            url: s("WIRURLKey"),
                            kind: kind_of(&s("WIRTypeKey")).into(),
                            bundle: app.bundle.clone(),
                            app_name: app.name.clone(),
                        });
                    }
                    app.pages.sort_by_key(|p| p.page);
                }
            }
            _ => {}
        }
    }
    // proxies (a WebContent process standing for its app) carry the app's bundle in their host's entry
    let hosts: Vec<(String, String, String)> = apps.iter().map(|a| (a.id.clone(), a.bundle.clone(), a.name.clone())).collect();
    for a in apps.iter_mut() {
        if let Some((_, b, n)) = a.host.as_ref().and_then(|h| hosts.iter().find(|x| &x.0 == h)) {
            for p in a.pages.iter_mut() {
                if p.bundle.is_empty() {
                    p.bundle = b.clone();
                }
                if p.app_name.is_empty() {
                    p.app_name = n.clone();
                }
            }
        }
    }
    Ok(apps)
}

/// The page a tool means: "<app id>/<page id>" exactly, else the first web page whose URL, title or app (bundle id or
/// name) contains `sel` (any case); no `sel`: the active application's first web page, else any web page
pub fn pick(apps: &[WebApp], sel: Option<&str>) -> Result<WebPage> {
    let all: Vec<&WebPage> = apps.iter().flat_map(|a| a.pages.iter()).collect();
    if all.is_empty() {
        bail!("no inspectable web content: open a page in Safari, or an app whose WKWebView has isInspectable = true");
    }
    let sel = sel.map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = sel {
        if let Some(p) = all.iter().find(|p| p.id == s) {
            return Ok((*p).clone());
        }
        let l = s.to_lowercase();
        let m = |p: &&&WebPage| {
            let name = p.app_name.to_lowercase();
            // an app without a registered bundle id reports "process-<executable>": its bundle id's last part matches
            p.url.to_lowercase().contains(&l)
                || p.title.to_lowercase().contains(&l)
                || p.bundle.to_lowercase() == l
                || name == l
                || (!name.is_empty() && l.rsplit('.').next() == Some(name.as_str()))
        };
        return all
            .iter()
            .filter(|p| p.kind == "web-page")
            .find(m)
            .or_else(|| all.iter().find(m))
            .map(|p| (*p).clone())
            .ok_or_else(|| anyhow!("no web page matches {s:?}; pages: {}", all.iter().map(|p| format!("{} {}", p.id, p.url)).collect::<Vec<_>>().join(", ")));
    }
    let active: Vec<&str> = apps.iter().filter(|a| a.active).map(|a| a.id.as_str()).collect();
    all.iter()
        .filter(|p| p.kind == "web-page")
        .find(|p| active.contains(&p.app_id.as_str()))
        .or_else(|| all.iter().find(|p| p.kind == "web-page"))
        .or_else(|| all.first())
        .map(|p| (*p).clone())
        .ok_or_else(|| anyhow!("no web page"))
}

// ---------------------------------------------------------------------------------------------------------------------
// Sessions

/// X86SIM_RWI_TRACE=<file>: every inspector message of every session appended there (debugging)
fn trace(dir: &str, m: &str) {
    if let Some(p) = std::env::var_os("X86SIM_RWI_TRACE") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let cut: String = m.chars().take(4000).collect();
            let _ = writeln!(f, "{dir} {cut}");
        }
    }
}

/// An inspector session on one page: inspector protocol messages (JSON text) both ways, as the page's backend sends them
pub struct Session {
    pub conn: Conn,
    pub page: WebPage,
    sender: String,
    chunks: Vec<u8>,
    pub closed: bool,
}

impl Session {
    pub fn open(inst: &Instance, page: &WebPage) -> Result<Session> {
        let mut conn = Conn::open(inst)?;
        // webinspectord forwards to an application it has told this connection about
        conn.send("_rpc_getConnectedApplications:", vec![])?;
        Self::on(conn, page)
    }

    pub fn on(mut conn: Conn, page: &WebPage) -> Result<Session> {
        let sender = uuid();
        conn.send(
            "_rpc_forwardSocketSetup:",
            vec![
                ("WIRApplicationIdentifierKey", Pl::Str(page.app_id.clone())),
                ("WIRPageIdentifierKey", Pl::Int(page.page)),
                ("WIRSenderKey", Pl::Str(sender.clone())),
                ("WIRAutomaticallyPause", Pl::Bool(false)),
                // a key the relay copies to the app with dictionaryWithValuesForKeys: (a missing one arrives as NSNull);
                // 1: the app may split a big message (WIRMessageDataTypeChunk ... FinalChunk), joined in recv
                ("WIRMessageDataTypeChunkSupportedKey", Pl::Int(1)),
            ],
        )?;
        Ok(Session { conn, page: page.clone(), sender, chunks: vec![], closed: false })
    }

    pub fn send(&mut self, json: &str) -> Result<()> {
        trace(">", json);
        self.conn.send(
            "_rpc_forwardSocketData:",
            vec![
                ("WIRApplicationIdentifierKey", Pl::Str(self.page.app_id.clone())),
                ("WIRPageIdentifierKey", Pl::Int(self.page.page)),
                ("WIRSenderKey", Pl::Str(self.sender.clone())),
                ("WIRSocketDataKey", Pl::Data(json.as_bytes().to_vec())),
            ],
        )
    }

    /// The next inspector message for this session, or None after `timeout` (Err once the page or app went away)
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<String>> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let Some((sel, a)) = self.conn.recv(left)? else { return Ok(None) };
            match sel.as_str() {
                "_rpc_applicationSentData:" => {
                    if a.get("WIRDestinationKey").and_then(Pl::text).is_some_and(|d| d != self.sender) {
                        continue;
                    }
                    let Some(Pl::Data(d)) = a.get("WIRMessageDataKey").cloned().or_else(|| a.get("WIRMessageDataKey").and_then(Pl::text).map(|s| Pl::Data(s.into_bytes()))) else { continue };
                    match a.get("WIRMessageDataTypeKey").and_then(Pl::str) {
                        Some("WIRMessageDataTypeChunk") => {
                            self.chunks.extend(d);
                            continue;
                        }
                        Some("WIRMessageDataTypeFinalChunk") => {
                            self.chunks.extend(d);
                            let all = std::mem::take(&mut self.chunks);
                            let m = String::from_utf8_lossy(&all).into_owned();
                            trace("<", &m);
                            return Ok(Some(m));
                        }
                        _ => {
                            let m = String::from_utf8_lossy(&d).into_owned();
                            trace("<", &m);
                            return Ok(Some(m));
                        }
                    }
                }
                "_rpc_applicationDisconnected:" if a.get("WIRApplicationIdentifierKey").and_then(Pl::text).as_deref() == Some(&self.page.app_id) => {
                    self.closed = true;
                    bail!("{} went away", if self.page.app_name.is_empty() { &self.page.app_id } else { &self.page.app_name });
                }
                "_rpc_applicationSentListing:" if a.get("WIRApplicationIdentifierKey").and_then(Pl::text).as_deref() == Some(&self.page.app_id) => {
                    let gone = !a.get("WIRListingKey").map(Pl::dict).unwrap_or(&[]).iter().any(|(_, p)| p.get("WIRPageIdentifierKey").and_then(Pl::int) == Some(self.page.page));
                    if gone {
                        self.closed = true;
                        bail!("the page was closed");
                    }
                }
                _ => {}
            }
        }
    }

    pub fn close(&mut self) {
        if !self.closed {
            let _ = self.conn.send(
                "_rpc_forwardDidClose:",
                vec![
                    ("WIRApplicationIdentifierKey", Pl::Str(self.page.app_id.clone())),
                    ("WIRPageIdentifierKey", Pl::Int(self.page.page)),
                    ("WIRSenderKey", Pl::Str(self.sender.clone())),
                ],
            );
            self.closed = true;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// A page for the tools: commands to the page's target (unwrapped from the Target domain), its events collected
pub struct Page {
    pub s: Session,
    target: Option<String>,
    /// every target the page announced (id, type: page, frame, worker, ...); WebKit 27 gives frames their own
    pub targets: Vec<(String, String)>,
    /// no Target domain (a JSContext, an older backend): commands go straight to the session
    direct: bool,
    next_id: i64,
    pub events: Vec<Value>,
}

impl Page {
    pub fn on(conn: Conn, page: &WebPage) -> Result<Page> {
        let s = Session::on(conn, page)?;
        let mut p = Page { s, target: None, targets: vec![], direct: false, next_id: 1, events: vec![] };
        // a web page announces its target at once
        let end = Instant::now() + Duration::from_secs(if page.kind == "web-page" { 8 } else { 2 });
        while p.target.is_none() && Instant::now() < end {
            let left = end.saturating_duration_since(Instant::now());
            match p.s.recv(left.min(Duration::from_millis(500)))? {
                Some(m) => {
                    p.take(&m);
                }
                None if page.kind != "web-page" => break,
                None => {}
            }
        }
        if p.target.is_none() {
            p.direct = true;
        } else {
            // the page's other targets (frames, workers) follow at once
            p.pump(Duration::from_millis(300))?;
        }
        Ok(p)
    }

    fn take(&mut self, m: &str) -> Option<Value> {
        let v: Value = serde_json::from_str(m).ok()?;
        match v["method"].as_str() {
            Some("Target.targetCreated") => {
                let t = &v["params"]["targetInfo"];
                if let Some(id) = t["targetId"].as_str() {
                    self.targets.push((id.to_string(), t["type"].as_str().unwrap_or("").to_string()));
                }
                // the page's own target (not a worker), and the provisional one only once it is committed
                if t["type"].as_str().is_none_or(|k| k == "page") && t["isProvisional"].as_bool() != Some(true) {
                    self.target = t["targetId"].as_str().map(String::from);
                }
                None
            }
            Some("Target.targetDestroyed") => {
                let id = v["params"]["targetId"].as_str().unwrap_or("");
                self.targets.retain(|(t, _)| t != id);
                None
            }
            Some("Target.didCommitProvisionalTarget") => {
                self.target = v["params"]["newTargetId"].as_str().map(String::from);
                None
            }
            Some("Target.dispatchMessageFromTarget") => {
                let inner: Value = serde_json::from_str(v["params"]["message"].as_str().unwrap_or("")).ok()?;
                if inner.get("id").is_some() {
                    return Some(inner);
                }
                self.events.push(inner);
                None
            }
            Some(_) => {
                if self.direct {
                    self.events.push(v);
                }
                None
            }
            None if self.direct => Some(v),
            None => None,
        }
    }

    /// A protocol command; its result (Err with the backend's error message)
    pub fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.call_to(None, method, params, timeout)
    }

    /// A command to one of the page's targets (None: the page's own)
    pub fn call_to(&mut self, target: Option<&str>, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let inner = json!({"id": id, "method": method, "params": params});
        if self.direct {
            self.s.send(&inner.to_string())?;
        } else {
            let t = target.map(String::from).or_else(|| self.target.clone()).ok_or_else(|| anyhow!("the page has no target"))?;
            let outer = json!({"id": id + 1_000_000, "method": "Target.sendMessageToTarget", "params": {"targetId": t, "message": inner.to_string()}});
            self.s.send(&outer.to_string())?;
        }
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("{method}: no answer in {} s", timeout.as_secs());
            }
            let Some(m) = self.s.recv(left)? else { continue };
            if let Some(r) = self.take(&m) {
                if r["id"].as_i64() == Some(id) {
                    if let Some(e) = r.get("error") {
                        bail!("{method}: {}", e["message"].as_str().unwrap_or(&e.to_string()));
                    }
                    return Ok(r["result"].clone());
                }
            }
        }
    }

    /// Collects events for `d`
    pub fn pump(&mut self, d: Duration) -> Result<()> {
        let end = Instant::now() + d;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            if let Some(m) = self.s.recv(left)? {
                self.take(&m);
            }
        }
    }

    /// Runtime.evaluate: the value (by value; a promise is awaited), or Err with the exception's text
    pub fn eval(&mut self, expr: &str, timeout: Duration) -> Result<Value> {
        let mut r = self.call(
            "Runtime.evaluate",
            json!({"expression": expr, "objectGroup": "x86sim", "returnByValue": false, "generatePreview": false, "includeCommandLineAPI": true}),
            timeout,
        )?;
        if r["wasThrown"].as_bool() != Some(true) {
            if let Some(id) = r["result"]["objectId"].as_str().map(String::from) {
                // WebKit's Runtime.evaluate has no awaitPromise: Runtime.awaitPromise; other objects by value
                // (WebKit's RemoteObject has no "promise" subtype: a promise is className Promise)
                r = if r["result"]["subtype"].as_str() == Some("promise") || r["result"]["className"].as_str() == Some("Promise") {
                    self.call("Runtime.awaitPromise", json!({"promiseObjectId": id, "returnByValue": true}), timeout)?
                } else {
                    self.call("Runtime.callFunctionOn", json!({"objectId": id, "functionDeclaration": "function() { return this; }", "returnByValue": true}), timeout)?
                };
                let _ = self.call("Runtime.releaseObjectGroup", json!({"objectGroup": "x86sim"}), Duration::from_secs(3));
            }
        }
        let res = &r["result"];
        if r["wasThrown"].as_bool() == Some(true) {
            bail!("{}", res["description"].as_str().map(String::from).unwrap_or_else(|| res.to_string()));
        }
        Ok(match res["type"].as_str() {
            Some("undefined") => Value::Null,
            _ if res.get("value").is_some() => res["value"].clone(),
            _ => res["description"].clone(),
        })
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// What the tools answer

/// The page a tool means, opened for commands on one connection
fn page_of(inst: &Instance, target: Option<&str>) -> Result<(WebPage, Page)> {
    let mut c = Conn::open(inst)?;
    let apps = targets_on(&mut c, Duration::from_secs(3))?;
    let p = pick(&apps, target)?;
    let page = Page::on(c, &p)?;
    Ok((p, page))
}

/// A tool's work, again once if webinspectord went away under it (it exits when its last debugger leaves)
fn retry<T>(f: impl Fn() -> Result<T>) -> Result<T> {
    match f() {
        Err(e) if format!("{e:#}").contains("webinspectord") => {
            std::thread::sleep(Duration::from_millis(500));
            f()
        }
        r => r,
    }
}

/// web_eval
fn eval_once(inst: &Instance, target: Option<&str>, expr: &str, timeout: Duration) -> Result<Value> {
    let (p, mut page) = page_of(inst, target)?;
    let v = page.eval(expr, timeout)?;
    Ok(json!({"page": p.id, "url": p.url, "title": p.title, "value": v}))
}

/// web_dom: outerHTML of the first element matching `selector` (no selector: the document), cut at `max` characters
fn dom_once(inst: &Instance, target: Option<&str>, selector: Option<&str>, max: usize) -> Result<Value> {
    let (p, mut page) = page_of(inst, target)?;
    let sel = selector.map(str::trim).filter(|s| !s.is_empty());
    let expr = match sel {
        Some(s) => format!(
            "(() => {{ const all = document.querySelectorAll({0}); return {{count: all.length, html: all.length ? all[0].outerHTML : null}}; }})()",
            serde_json::to_string(s)?
        ),
        None => "({count: 1, html: document.documentElement.outerHTML})".to_string(),
    };
    let v = page.eval(&expr, Duration::from_secs(15))?;
    let html = v["html"].as_str().map(String::from);
    let (html, cut) = match html {
        Some(h) if h.chars().count() > max => (Some(h.chars().take(max).collect::<String>()), true),
        h => (h, false),
    };
    Ok(json!({"page": p.id, "url": p.url, "selector": sel, "matches": v["count"], "html": html, "truncated": cut}))
}

fn console_line(m: &Value) -> Value {
    json!({
        "level": m["level"], "source": m["source"], "type": m["type"], "text": m["text"],
        "url": m["url"], "line": m["line"], "column": m["column"], "repeat": m["repeatCount"],
        "timestamp": m["timestamp"],
    })
}

/// web_console: the page's console messages so far, then new ones for `follow`; at most `limit`, the newest. WebKit
/// keeps a page's messages only once an inspector has connected to it (developer extras): what it logged before the
/// first inspection is gone, so `reload` reloads the page with the console on and collects from its load.
fn console_once(inst: &Instance, target: Option<&str>, follow: Duration, limit: usize, reload: bool) -> Result<Value> {
    let (p, mut page) = page_of(inst, target)?;
    // as Web Inspector starts a target: the console replays what the page logged before once the inspector is
    // enabled and initialized (Console.enable alone answered only new messages)
    let _ = page.call("Inspector.enable", json!({}), Duration::from_secs(10));
    let _ = page.call("Runtime.enable", json!({}), Duration::from_secs(10));
    page.call("Console.enable", json!({}), Duration::from_secs(10))?;
    // and on the page's frame targets (WebKit 27: a frame's console messages live in its own target)
    let others: Vec<String> = page.targets.iter().filter(|(t, k)| Some(t) != page.target.as_ref() && k != "worker").map(|(t, _)| t.clone()).collect();
    for t in &others {
        let _ = page.call_to(Some(t), "Runtime.enable", json!({}), Duration::from_secs(5));
        let _ = page.call_to(Some(t), "Console.enable", json!({}), Duration::from_secs(5));
    }
    let _ = page.call("Inspector.initialized", json!({}), Duration::from_secs(10));
    let follow = if reload {
        page.events.clear();
        page.call("Page.reload", json!({}), Duration::from_secs(10))?;
        follow.max(Duration::from_secs(3))
    } else {
        follow
    };
    page.pump(Duration::from_millis(400) + follow)?;
    let mut msgs: Vec<Value> = vec![];
    for e in page.events.drain(..) {
        match e["method"].as_str() {
            Some("Console.messageAdded") => msgs.push(console_line(&e["params"]["message"])),
            Some("Console.messageRepeatCountUpdated") => {
                if let Some(last) = msgs.last_mut() {
                    last["repeat"] = e["params"]["count"].clone();
                }
            }
            Some("Console.messagesCleared") => msgs.clear(),
            _ => {}
        }
    }
    let n = msgs.len();
    let msgs: Vec<Value> = msgs.into_iter().skip(n.saturating_sub(limit)).collect();
    let _ = page.call("Console.disable", json!({}), Duration::from_secs(3));
    Ok(json!({"page": p.id, "url": p.url, "count": n, "messages": msgs}))
}

/// web_network: the page's requests so far (the Resource Timing entries: every request since the page loaded; no
/// status codes there), then live ones (Network domain events) for `follow`; `reload`: the page reloaded with the
/// network domain on, every request of its load live (with status and MIME type)
fn network_once(inst: &Instance, target: Option<&str>, follow: Duration, limit: usize, reload: bool) -> Result<Value> {
    let (p, mut page) = page_of(inst, target)?;
    let follow = if reload { follow.max(Duration::from_secs(3)) } else { follow };
    let past = if reload { json!([]) } else { page.eval(
        "(() => { const nav = performance.getEntriesByType('navigation').concat(performance.getEntriesByType('resource')); \
         return nav.map(e => ({url: e.name, type: e.initiatorType || e.entryType, status: e.responseStatus || null, \
         start_ms: Math.round(e.startTime), duration_ms: Math.round(e.duration), size: e.transferSize || e.encodedBodySize || null})); })()",
        Duration::from_secs(10),
    )? };
    let mut live: Vec<Value> = vec![];
    if !follow.is_zero() {
        page.call("Network.enable", json!({}), Duration::from_secs(10))?;
        if reload {
            page.events.clear();
            page.call("Page.reload", json!({}), Duration::from_secs(10))?;
        }
        page.pump(follow)?;
        let mut by_id: Vec<(String, Value)> = vec![];
        for e in page.events.drain(..) {
            let ps = &e["params"];
            let rid = ps["requestId"].as_str().unwrap_or("").to_string();
            let slot = |by_id: &mut Vec<(String, Value)>| -> usize {
                if let Some(i) = by_id.iter().position(|(r, _)| *r == rid) {
                    return i;
                }
                by_id.push((rid.clone(), json!({})));
                by_id.len() - 1
            };
            match e["method"].as_str() {
                Some("Network.requestWillBeSent") => {
                    let i = slot(&mut by_id);
                    by_id[i].1["url"] = ps["request"]["url"].clone();
                    by_id[i].1["method"] = ps["request"]["method"].clone();
                    by_id[i].1["type"] = ps["type"].clone();
                }
                Some("Network.responseReceived") => {
                    let i = slot(&mut by_id);
                    by_id[i].1["status"] = ps["response"]["status"].clone();
                    by_id[i].1["mime"] = ps["response"]["mimeType"].clone();
                    if by_id[i].1["url"].is_null() {
                        by_id[i].1["url"] = ps["response"]["url"].clone();
                    }
                }
                Some("Network.loadingFinished") => {
                    let i = slot(&mut by_id);
                    by_id[i].1["finished"] = json!(true);
                }
                Some("Network.loadingFailed") => {
                    let i = slot(&mut by_id);
                    by_id[i].1["error"] = ps["errorText"].clone();
                }
                _ => {}
            }
        }
        let _ = page.call("Network.disable", json!({}), Duration::from_secs(3));
        live = by_id.into_iter().map(|(_, v)| v).collect();
    }
    let past: Vec<Value> = past.as_array().cloned().unwrap_or_default();
    let n = past.len();
    Ok(json!({"page": p.id, "url": p.url, "requests": past.into_iter().skip(n.saturating_sub(limit)).collect::<Vec<_>>(), "live": live}))
}

/// web_eval: JavaScript in a page, its value
pub fn eval(inst: &Instance, target: Option<&str>, expr: &str, timeout: Duration) -> Result<Value> {
    retry(|| eval_once(inst, target, expr, timeout))
}

/// web_dom
pub fn dom(inst: &Instance, target: Option<&str>, selector: Option<&str>, max: usize) -> Result<Value> {
    retry(|| dom_once(inst, target, selector, max))
}

/// web_console
pub fn console(inst: &Instance, target: Option<&str>, follow: Duration, limit: usize, reload: bool) -> Result<Value> {
    retry(|| console_once(inst, target, follow, limit, reload))
}

/// web_network
pub fn network(inst: &Instance, target: Option<&str>, follow: Duration, limit: usize, reload: bool) -> Result<Value> {
    retry(|| network_once(inst, target, follow, limit, reload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bplist_round_trip() {
        let v = dict(vec![
            ("__selector", Pl::Str("_rpc_forwardSocketData:".into())),
            (
                "__argument",
                dict(vec![
                    ("WIRSocketDataKey", Pl::Data(br#"{"id":1}"#.to_vec())),
                    ("WIRPageIdentifierKey", Pl::Int(7)),
                    ("big", Pl::Int(1 << 40)),
                    ("neg", Pl::Int(-3)),
                    ("flag", Pl::Bool(true)),
                    ("utf", Pl::Str("été ✓".into())),
                    ("arr", Pl::Array((0..300).map(Pl::Int).collect())),
                    ("long", Pl::Str("x".repeat(40))),
                ]),
            ),
        ]);
        let b = bplist_encode(&v);
        assert_eq!(bplist_decode(&b).unwrap(), v);
    }

    #[test]
    fn bplist_python_made() {
        // plistlib.dumps({"a": [1, "b", b"\x00"], "c": True}, fmt=FMT_BINARY)
        let b: &[u8] = &[0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xd2, 0x01, 0x02, 0x03, 0x07, 0x51, 0x61, 0x51, 0x63, 0xa3, 0x04, 0x05, 0x06, 0x10, 0x01, 0x51, 0x62, 0x41, 0x00, 0x09, 0x08, 0x0d, 0x0f, 0x11, 0x15, 0x17, 0x19, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1c];
        let v = bplist_decode(b).unwrap();
        assert_eq!(v.get("c"), Some(&Pl::Bool(true)));
        assert_eq!(v.get("a"), Some(&Pl::Array(vec![Pl::Int(1), Pl::Str("b".into()), Pl::Data(vec![0])])));
    }

    #[test]
    fn picks_pages() {
        let page = |app: &str, n: i64, url: &str| WebPage {
            id: format!("{app}/{n}"),
            app_id: app.into(),
            page: n,
            title: "T".into(),
            url: url.into(),
            kind: "web-page".into(),
            bundle: "com.apple.mobilesafari".into(),
            app_name: "Safari".into(),
        };
        let apps = vec![WebApp {
            id: "PID:9".into(),
            bundle: "com.apple.mobilesafari".into(),
            name: "Safari".into(),
            pid: Some(9),
            active: true,
            proxy: false,
            host: None,
            pages: vec![page("PID:9", 1, "https://a.example/"), page("PID:9", 2, "https://b.example/x")],
        }];
        assert_eq!(pick(&apps, None).unwrap().page, 1);
        assert_eq!(pick(&apps, Some("b.example")).unwrap().page, 2);
        assert_eq!(pick(&apps, Some("PID:9/2")).unwrap().page, 2);
        assert!(pick(&apps, Some("nothing")).is_err());
        assert!(pick(&[], None).is_err());
    }
}
