//! Agents' view of a phone: the screen as text (accessibility elements with stable refs) and actions on elements.
//!
//! The guest side is host/simax (libsimax.dylib, dlopened by libsimshim in every process with UIKit): each application
//! listens on `<ring dir>/x86sim-ax-<udid>-<pid>.sock` and keeps `<...>.fg` while it is on screen, so a snapshot asks
//! only the processes on screen (front app(s), SpringBoard, view services), in parallel, and never waits on a
//! suspended app. Protocol: docs/protocols/SIMAX.md.
//!
//! Refs (`@e12`) are kept per phone in `<state dir>/ax-refs.json`: an element keeps its ref across snapshots (and
//! across CLI invocations) while it is the same element (same object, else same app + role + id / label), so an agent
//! can act on a ref it saw several steps ago. Actions (tap, type, scroll, ...) settle on the frame ring (no new frame
//! for a moment) and answer with the screen that follows.
use crate::hid::{self, Hid};
use crate::instance::Instance;
use crate::ring::Ring;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SPRINGBOARD: &str = "com.apple.springboard";
/// The system shells that are not apps, per platform: SpringBoard (iOS / iPadOS), PineBoard (tvOS's SpringBoard:
/// alerts), Carousel (watchOS: the app grid, the time over apps), visionOS's SurfBoard; the homes that are apps of
/// their own: tvOS's HeadBoard (TVSystemUIService: its clock corner), a watch face (com.apple.clockface), visionOS's
/// Home View (RealityLauncher; RealityHUD: the Home / Control Center buttons)
const SHELLS: &[&str] = &[SPRINGBOARD, "com.apple.PineBoard", "com.apple.Carousel", "com.apple.SurfBoard"];
const TV_HOME: &str = "com.apple.HeadBoard";
const WATCH_FACE: &str = "com.apple.clockface";
const XR_HOME: &str = "com.apple.RealityLauncher";
const XR_HUD: &str = "com.apple.RealityHUD";
const TV_SYSTEM_UI: &str = "com.apple.TVSystemUIService";

fn is_shell(bundle: &str) -> bool {
    SHELLS.contains(&bundle) || [TV_HOME, TV_SYSTEM_UI, WATCH_FACE, XR_HOME, XR_HUD].contains(&bundle)
}

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_true(b: &bool) -> bool {
    *b
}
fn yes() -> bool {
    true
}
fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// One element on screen. Frames are in screen points (UIKit's, of the current interface orientation).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct El {
    #[serde(rename = "ref")]
    pub r: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub href: Option<String>,
    /// x, y, width, height
    pub frame: [f64; 4],
    #[serde(skip_serializing_if = "is_true", default = "yes")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    pub selected: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    pub focused: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    pub offscreen: bool,
    /// under another app's window (iPadOS windowed apps / Stage Manager): not visible, a touch there hits that window
    #[serde(skip_serializing_if = "is_false", default)]
    pub covered: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    pub web: bool,
    /// the app (bundle id) whose process draws it
    pub app: String,
    pub pid: u32,
    /// where it is: "app", "system" (SpringBoard over everything: alerts, banners, Control Center, app switcher),
    /// "window" (SpringBoard's chrome of iPadOS app windows: window controls, resize grabbers, the windows behind),
    /// "home" (home screen, dock, App Library), "status" (status bar)
    pub layer: String,
    /// the window it is in (class name)
    pub window: String,
    /// the process-local object (simax's oid): the same object keeps its ref
    pub oid: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub container: Option<String>,
    /// the inspector's extras (SnapOpts::full; simax `more`): language, custom actions, rotors, view / controller
    /// class, container path, text font / color / truncation (axinspect.rs, axaudit.rs)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub more: Option<Value>,
    /// what stays the same while the object behind it may not (simax): a table / collection cell's index path
    /// ("<list>:<section>.<row>/<role><n>"), a DOM node's identity ("dom:<document tag>.<n>"); refs follow it
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<String>,
    /// the interface orientation of its window as a turn of the panel (0, 90, 180, 270; hid::orientation_rot's
    /// convention): its frame is in that orientation's coordinates
    #[serde(skip_serializing_if = "is_zero", default)]
    pub rot: u32,
    /// web content of an iframe: the frame's URL
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub frame_url: Option<String>,
}

impl El {
    pub fn center(&self, screen: (f64, f64)) -> (f64, f64) {
        // the visible part's center (a cell half under the toolbar is tapped where it shows)
        let [x, y, w, h] = self.frame;
        let (x0, y0) = (x.max(0.0), y.max(0.0));
        let (x1, y1) = ((x + w).min(screen.0), (y + h).min(screen.1));
        if x1 > x0 && y1 > y0 {
            ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
        } else {
            (x + w / 2.0, y + h / 2.0)
        }
    }

    /// One line: `@e3 button "General" id=com.apple.settings.general (201,319)`
    pub fn line(&self, screen: (f64, f64)) -> String {
        let mut s = format!("@{} {}", self.r, self.role);
        if let Some(l) = &self.label {
            s += &format!(" {}", quote(l));
        }
        if let Some(v) = &self.value {
            if Some(v) != self.label.as_ref() {
                s += &format!(" value={}", quote(v));
            }
        }
        if let Some(i) = &self.id {
            if Some(i) != self.label.as_ref() {
                s += &format!(" id={i}");
            }
        }
        for (on, f) in [(!self.enabled, "disabled"), (self.selected, "selected"), (self.focused, "focused"), (self.offscreen, "offscreen"), (self.covered, "covered")] {
            if on {
                s += &format!(" [{f}]");
            }
        }
        let (cx, cy) = self.center(screen);
        s += &format!(" ({:.0},{:.0})", cx, cy);
        s
    }
}

fn quote(s: &str) -> String {
    let t: String = s.chars().take(160).collect();
    let t = t.replace('\n', " ").replace('"', "\\\"");
    if s.chars().count() > 160 {
        format!("\"{t}…\"")
    } else {
        format!("\"{t}\"")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub phone: String,
    /// ios, tvos, watchos, xros
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub platform: String,
    /// screen size in points (the interface orientation's)
    pub screen: (f64, f64),
    /// apps on screen (bundle ids), front-most first; SpringBoard means the home screen
    pub front: Vec<String>,
    /// SpringBoard shows something over the apps (an alert, a banner, Control Center, the app switcher)
    pub overlay: bool,
    pub elements: Vec<El>,
    /// processes on screen that did not answer in time (busy main thread)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub busy: Vec<String>,
    /// the front app's interface orientation as a turn of the panel (0 portrait, 90, 180, 270): `screen` and points
    /// given to tap / swipe are in it
    #[serde(skip_serializing_if = "is_zero", default)]
    pub rot: u32,
    /// a two-screen phone: which screen is in use ("inner" unfolded, "outer" folded)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display: Option<String>,
    /// how long the snapshot took
    pub ms: u64,
    /// an app was in the middle of an animation (a push, a sheet coming up) when it answered
    #[serde(skip_serializing_if = "is_false", default)]
    pub animating: bool,
    /// where the focus engine's focus is (tvOS: what the remote's select acts on), x, y, width, height
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub focus: Option<[f64; 4]>,
    /// a process turned accessibility on for this request (its elements may lack labels yet): snapshot() asks again
    #[serde(skip)]
    pub ax_fresh: bool,
    /// SnapOpts::full: interactive views VoiceOver cannot reach (simax `hidden`, with `app` and `pid` added)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hidden: Vec<Value>,
}

impl Snapshot {
    pub fn get(&self, r: &str) -> Option<&El> {
        let r = r.trim_start_matches('@');
        self.elements.iter().find(|e| e.r == r)
    }

    /// The element the focus engine's focus is on (tvOS): a focused element at the focus's place (a text field being
    /// edited is focused too, while the remote's focus is on a key); without a focus engine, the front app's first
    /// focused one
    pub fn focused_el(&self) -> Option<&El> {
        let overlap = |a: &[f64; 4], b: &[f64; 4]| {
            let (x0, y0, x1, y1) = (a[0].max(b[0]), a[1].max(b[1]), (a[0] + a[2]).min(b[0] + b[2]), (a[1] + a[3]).min(b[1] + b[3]));
            let i = if x1 > x0 && y1 > y0 { (x1 - x0) * (y1 - y0) } else { 0.0 };
            i / (a[2] * a[3] + b[2] * b[3] - i).max(1.0)
        };
        let fs = self.elements.iter().filter(|e| e.focused && !e.offscreen);
        if let Some(f) = self.focus {
            // none there: the focus is on something no element stands for (tvOS's keyboard), not on the text field
            return fs.clone().filter(|e| overlap(&e.frame, &f) > 0.5).max_by(|a, b| overlap(&a.frame, &f).total_cmp(&overlap(&b.frame, &f)));
        }
        fs.min_by_key(|x| self.front.iter().position(|a| *a == x.app).unwrap_or(usize::MAX))
    }

    /// The compact text form an agent reads.
    pub fn text(&self) -> String {
        let mut out = format!(
            "device {} · {}{}{} · front {}{}\n",
            self.phone,
            if self.platform == "xros" {
                // windows float in the room: no display coordinates (tap acts through accessibility)
                "Vision Pro: (x,y) are points in each app's own window".to_string()
            } else {
                format!("{:.0}x{:.0} pt", self.screen.0, self.screen.1)
            },
            self.display.as_deref().map(|d| format!(" · {d} screen")).unwrap_or_default(),
            match self.rot {
                0 => "",
                90 | 270 => " · landscape",
                _ => " · upside down",
            },
            if self.front.is_empty() { "?".into() } else { self.front.join(", ") },
            if self.overlay { " · system overlay on top" } else { "" }
        );
        let layer_rank = |l: &str| match l {
            "system" => 0,
            "window" => 1,
            "app" => 2,
            "home" => 3,
            _ => 4,
        };
        let mut groups: Vec<(String, String)> = Vec::new();
        for e in &self.elements {
            let k = (e.layer.clone(), e.app.clone());
            if !groups.contains(&k) {
                groups.push(k);
            }
        }
        // apps front-most first (iPadOS windows: the top window's app)
        let front_rank = |app: &str| self.front.iter().position(|f| f == app).unwrap_or(usize::MAX);
        groups.sort_by_key(|g| (layer_rank(&g.0), front_rank(&g.1)));
        for (layer, app) in groups {
            let els: Vec<&El> = self.elements.iter().filter(|e| e.layer == layer && e.app == app).collect();
            if layer == "status" {
                let parts: Vec<String> = els
                    .iter()
                    .map(|e| match (&e.label, &e.value) {
                        (Some(l), Some(v)) => format!("{l}: {v}"),
                        (Some(l), None) => l.clone(),
                        (None, Some(v)) => v.clone(),
                        _ => String::new(),
                    })
                    .filter(|s| !s.is_empty())
                    .collect();
                out += &format!("status bar: {}\n", parts.join(" | "));
                continue;
            }
            out += &match layer.as_str() {
                "system" => format!("system ({}) on top:\n", els.first().map(|e| e.window.as_str()).unwrap_or("")),
                "window" => "app windows (SpringBoard: window controls, windows behind the front one):\n".to_string(),
                "home" => "home screen:\n".to_string(),
                _ => format!("app {app}:\n"),
            };
            for e in els {
                out += &e.line(self.screen);
                out.push('\n');
            }
        }
        if !self.busy.is_empty() {
            out += &format!("busy (no answer): {}\n", self.busy.join(", "));
        }
        if let Some(f) = self.focus {
            // tvOS: name what has the focus (an element above shows [focused]; else where it is)
            match self.focused_el() {
                Some(e) => out += &format!("focus: @{} {} {}\n", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default()),
                None => out += &format!("focus: ({:.0},{:.0}) {:.0}x{:.0}, no element there\n", f[0] + f[2] / 2.0, f[1] + f[3] / 2.0, f[2], f[3]),
            }
        }
        out
    }

    /// What changed since `prev`: `+` new elements, `-` gone ones, `~` a changed label / value / state.
    pub fn diff(&self, prev: &Snapshot) -> String {
        let old: HashMap<&str, &El> = prev.elements.iter().map(|e| (e.r.as_str(), e)).collect();
        let new: HashSet<&str> = self.elements.iter().map(|e| e.r.as_str()).collect();
        let mut out = String::new();
        if self.front != prev.front {
            out += &format!("front {} -> {}\n", prev.front.join(", "), self.front.join(", "));
        }
        for e in &self.elements {
            match old.get(e.r.as_str()) {
                None => out += &format!("+ {}\n", e.line(self.screen)),
                Some(o) => {
                    let mut ch = Vec::new();
                    if o.label != e.label {
                        ch.push(format!("label {} -> {}", quote(o.label.as_deref().unwrap_or("")), quote(e.label.as_deref().unwrap_or(""))));
                    }
                    if o.value != e.value {
                        ch.push(format!("value {} -> {}", quote(o.value.as_deref().unwrap_or("")), quote(e.value.as_deref().unwrap_or(""))));
                    }
                    for (a, b, n) in [(o.enabled, e.enabled, "enabled"), (o.selected, e.selected, "selected"), (o.focused, e.focused, "focused"), (o.offscreen, e.offscreen, "offscreen"), (o.covered, e.covered, "covered")] {
                        if a != b {
                            ch.push(format!("{n} {a} -> {b}"));
                        }
                    }
                    if !ch.is_empty() {
                        out += &format!("~ @{} {} {}\n", e.r, e.role, ch.join(", "));
                    }
                }
            }
        }
        for e in &prev.elements {
            if !new.contains(e.r.as_str()) {
                out += &format!("- {}\n", e.line(prev.screen));
            }
        }
        if out.is_empty() {
            out = "no change\n".into();
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Talking to the guest

/// A process's reader: its socket and whether it is on screen (its .fg marker)
struct Endpoint {
    sock: PathBuf,
    fg: bool,
}

fn endpoints(inst: &Instance) -> Vec<Endpoint> {
    let dir = inst.ring.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/tmp"));
    let pre = format!("x86sim-ax-{}-", if inst.udid.is_empty() { "phone" } else { &inst.udid });
    let mut v = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if let Some(rest) = n.strip_prefix(&pre).and_then(|r| r.strip_suffix(".sock")) {
            let fg = dir.join(format!("{pre}{rest}.fg")).exists();
            v.push(Endpoint { sock: e.path(), fg });
        }
    }
    v
}

fn forget(sock: &Path) {
    let _ = std::fs::remove_file(sock);
    let _ = std::fs::remove_file(sock.with_extension("fg"));
}

/// connect() that never blocks: a suspended process never accepts, and once its backlog is full a blocking connect
/// would wait forever (EAGAIN here instead)
fn connect_nb(sock: &Path) -> std::io::Result<UnixStream> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let b = sock.as_os_str().as_bytes();
    let mut a: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if b.len() >= a.sun_path.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "socket path too long"));
    }
    a.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (d, s) in a.sun_path.iter_mut().zip(b) {
        *d = *s as libc::c_char;
    }
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let s = UnixStream::from_raw_fd(fd);
        if libc::connect(fd, &a as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        s.set_nonblocking(false)?;
        Ok(s)
    }
}

/// One request to one process; Ok(None): the process is gone (its socket removed)
fn ask(sock: &Path, req: &Value, timeout: Duration) -> Result<Option<Value>> {
    let mut s = match connect_nb(sock) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) => {
            forget(sock);
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut r = req.clone();
    // the guest answers "busy" a little before we give up
    r["timeout"] = json!((timeout.as_secs_f64() - 0.3).max(0.2));
    s.write_all(format!("{r}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).context("no answer")?;
    if line.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

/// The same request to every process on screen, in parallel
fn ask_fg(inst: &Instance, req: &Value, timeout: Duration, stuck: &HashSet<u32>) -> Vec<(PathBuf, Result<Value>)> {
    let eps: Vec<Endpoint> = endpoints(inst).into_iter().filter(|e| e.fg).collect();
    std::thread::scope(|sc| {
        let hs: Vec<_> = eps
            .iter()
            .map(|ep| {
                let req = req.clone();
                let t = if stuck.contains(&pid_of_sock(&ep.sock)) { Duration::from_millis(80) } else { timeout };
                sc.spawn(move || (ep.sock.clone(), ask(&ep.sock, &req, t)))
            })
            .collect();
        hs.into_iter()
            .filter_map(|h| h.join().ok())
            .filter_map(|(p, r)| match r {
                Ok(None) => None,
                Ok(Some(v)) => Some((p, Ok(v))),
                Err(e) => Some((p, Err(e))),
            })
            .collect()
    })
}

/// One request to one process of the phone (its reader's socket by pid): the inspector's actions
pub fn ask_pid(inst: &Instance, pid: u32, req: &Value, timeout: Duration) -> Result<Value> {
    let sock = sock_of_pid(inst, pid).ok_or_else(|| anyhow!("process {pid} is gone"))?;
    ask(&sock, req, timeout)?.ok_or_else(|| anyhow!("process {pid} did not answer"))
}

fn pid_of_sock(p: &Path) -> u32 {
    p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.rsplit('-').next()).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn sock_of_pid(inst: &Instance, pid: u32) -> Option<PathBuf> {
    endpoints(inst).into_iter().map(|e| e.sock).find(|s| pid_of_sock(s) == pid)
}

// ---------------------------------------------------------------------------------------------------------------------
// Refs

#[derive(Serialize, Deserialize, Default)]
struct RefState {
    next: u32,
    /// the phone's boot (refs of another boot mean nothing)
    #[serde(default)]
    boot: String,
    last: Snapshot,
    /// processes on screen that did not answer last time (an extension RunningBoard suspended keeps its .fg: its
    /// threads never run); they get a short timeout until they answer again
    #[serde(default)]
    stuck: HashSet<u32>,
}

fn refs_path(inst: &Instance) -> PathBuf {
    inst.state_dir.join("ax-refs.json")
}

fn boot_id(inst: &Instance) -> String {
    // the frame ring is made anew at every boot (its ctime moves with every frame: inode and birth time)
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(&inst.ring).map(|m| format!("{}.{:?}", m.ino(), m.created().ok())).unwrap_or_default()
}

fn load_refs(inst: &Instance) -> RefState {
    let st: RefState = std::fs::read(refs_path(inst)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if st.boot != boot_id(inst) {
        return RefState { next: 1, boot: boot_id(inst), ..Default::default() };
    }
    st
}

fn save_refs(inst: &Instance, st: &RefState) {
    let _ = std::fs::create_dir_all(&inst.state_dir);
    let tmp = refs_path(inst).with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_vec(st).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, refs_path(inst));
    }
}

/// Gives each element of `cur` the ref it had in `prev` when it is the same element:
/// 1. the same key in the same process (simax's `key`: a DOM node's identity, a cell's index path; a cell's element
///    also needs its label, identifier or object to be the same, else the rows moved), or the same object (pid + oid)
///    still with the same role and label or id;
/// 2. else the only previous element of the same app with the same role and id, else the nearest one with the same
///    role and label (recreated cells, a relaunched app). Two keyed elements of one process with different keys are
///    different elements: they never trade refs on a label they share ("by", "|" on every row of a web page) unless
///    the label is unique on both sides.
/// New elements get new numbers.
fn assign_refs(cur: &mut [El], prev: &[El], next: &mut u32) {
    let mut used: HashSet<String> = HashSet::new();
    let key_obj = |e: &El| format!("{}|{}", e.pid, e.oid);
    let key_key = |e: &El| e.key.as_ref().map(|k| format!("{}|{}|{}", e.pid, e.role, k));
    let by_obj: HashMap<String, &El> = prev.iter().filter(|e| !e.web).map(|e| (key_obj(e), e)).collect();
    let by_key: HashMap<String, &El> = prev.iter().filter_map(|e| key_key(e).map(|k| (k, e))).collect();
    let mut pending = Vec::new();
    for (i, e) in cur.iter_mut().enumerate() {
        let same_content = |p: &El| p.label == e.label || (p.id.is_some() && p.id == e.id);
        let hit = match key_key(e).and_then(|k| by_key.get(&k).copied()) {
            // a DOM node is itself whatever it says now; a cell's row only while it shows the same thing
            Some(p) if p.web == e.web && (e.web || same_content(p) || p.oid == e.oid) => Some(p),
            Some(_) => None,
            None => match by_obj.get(&key_obj(e)) {
                Some(p) if !e.web && p.role == e.role && same_content(p) && (p.key.is_none() || e.key.is_none() || p.key == e.key) => Some(*p),
                _ => None,
            },
        };
        match hit {
            Some(p) if !used.contains(&p.r) => {
                e.r = p.r.clone();
                used.insert(p.r.clone());
            }
            _ => pending.push(i),
        }
    }
    // labels that occur once among an app's keyed elements of a role (a keyed element may move to another key by one)
    let count = |els: &[El], e: &El| els.iter().filter(|o| o.key.is_some() && o.pid == e.pid && o.role == e.role && o.label == e.label).count();
    let unique = |cands: Vec<&El>| -> Option<String> { if cands.len() == 1 { Some(cands[0].r.clone()) } else { None } };
    let mut still = Vec::new();
    for i in pending {
        let e = &cur[i];
        let free: Vec<&El> = prev
            .iter()
            .filter(|p| !used.contains(&p.r) && p.app == e.app && p.role == e.role && p.web == e.web)
            .filter(|p| p.key.is_none() || e.key.is_none() || p.pid != e.pid || (count(prev, p) == 1 && count(cur, e) == 1))
            .collect();
        let r = e
            .id
            .as_ref()
            .and_then(|id| unique(free.iter().copied().filter(|p| p.id.as_ref() == Some(id)).collect()))
            .or_else(|| {
                // the same label; when several, the nearest one
                // (two identifiers that differ are two elements: a back button named after the cell it came from)
                let same: Vec<&El> = free.iter().copied().filter(|p| p.label == e.label && p.label.is_some() && (p.id.is_none() || e.id.is_none() || p.id == e.id) && p.href == e.href).collect();
                let d = |p: &El| (p.frame[0] - e.frame[0]).abs() + (p.frame[1] - e.frame[1]).abs();
                same.into_iter().min_by(|a, b| d(a).total_cmp(&d(b))).map(|p| p.r.clone())
            });
        match r {
            Some(r) => {
                used.insert(r.clone());
                cur[i].r = r;
            }
            None => still.push(i),
        }
    }
    for i in still {
        cur[i].r = format!("e{next}");
        *next += 1;
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Snapshot

#[derive(Clone, Copy, Default)]
pub struct SnapOpts {
    /// the inspector's view: El::more and Snapshot::hidden (slower: the audit and the inspector ask for it)
    pub full: bool,
    /// also elements scrolled out of view (in scroll views the app has loaded)
    pub all: bool,
    /// per process answer timeout
    pub timeout_ms: u64,
}

fn layer_of(bundle: &str, window: &str, class: &str, id: &str) -> &'static str {
    // (a watch's time over an app: Carousel's CSLStatusBarContextTimeLabel)
    if class.starts_with("STUIStatusBar") || class.starts_with("UIStatusBar") || class.contains("StatusBar") || window.contains("StatusBar") {
        return "status";
    }
    // visionOS: the Home View is RealityLauncher's; RealityHUD's Control Center / Home buttons are its status corner
    if bundle == XR_HOME {
        return "home";
    }
    if bundle == XR_HUD {
        return "status";
    }
    // watchOS: the watch face (com.apple.clockface) and Carousel's app grid / list (its CSLSecureWindow) are home
    if bundle == WATCH_FACE || (bundle == "com.apple.Carousel" && window == "CSLSecureWindow") {
        return "home";
    }
    // tvOS: the Home Screen is its own app; the clock / network / user corner is TVSystemUIService's
    if bundle == TV_HOME {
        return "home";
    }
    if bundle == TV_SYSTEM_UI {
        return if id.starts_with("com.apple.TVSystemUIService.status") { "status" } else { "system" };
    }
    if !SHELLS.contains(&bundle) {
        return "app";
    }
    if window.contains("HomeScreen") || window.contains("Wallpaper") || window.contains("FloatingDock") || window.contains("Dock") {
        "home"
    } else {
        "system"
    }
}

/// iPadOS windowed apps (Windowed Apps, Stage Manager): SpringBoard draws each window's chrome in SBMainSwitcherWindow
/// (the front window's "window-controls:<bundle>" and its resize grabber, a "card:<bundle>:..." over each window behind
/// it, back to front). Those are the "window" layer, not a system overlay (the app switcher has no window controls).
/// Answers each windowed app's stacking order (higher: nearer the front); empty when no app is in a window.
fn windows_z(snap: &mut Snapshot) -> HashMap<String, i32> {
    let mut z = HashMap::new();
    let chrome = |e: &El| e.app == SPRINGBOARD && e.window == "SBMainSwitcherWindow";
    let windowed = snap.elements.iter().any(|e| chrome(e) && e.id.as_deref().is_some_and(|i| i.starts_with("window-controls:") || i == "resize-grabber"));
    if !windowed {
        return z;
    }
    let mut n = 0;
    for e in snap.elements.iter_mut().filter(|e| chrome(e)) {
        e.layer = "window".into();
        let id = e.id.as_deref().unwrap_or("");
        if let Some(b) = id.strip_prefix("card:").and_then(|r| r.split(':').next()) {
            z.entry(b.to_string()).or_insert(n);
            n += 1;
        }
    }
    for e in snap.elements.iter().filter(|e| chrome(e)) {
        if let Some(b) = e.id.as_deref().and_then(|i| i.strip_prefix("window-controls:")) {
            z.insert(b.to_string(), 10_000);
        }
    }
    snap.overlay = snap.elements.iter().any(|e| e.layer == "system");
    z
}

/// Elements of a window behind another app's window: fully covered ones are marked, partly covered ones keep the
/// part that shows (so a tap lands on them, not on the window in front)
fn cover(els: &mut [El], rects: &HashMap<String, [f64; 4]>, z: &HashMap<String, i32>) {
    for e in els.iter_mut().filter(|e| e.layer == "app") {
        let mine = z.get(&e.app).copied().unwrap_or(-1);
        for (b, r) in rects {
            if *b == e.app || z.get(b).copied().unwrap_or(-1) <= mine {
                continue;
            }
            let [x, y, w, h] = e.frame;
            let (rx0, ry0, rx1, ry1) = (r[0], r[1], r[0] + r[2], r[1] + r[3]);
            let (x1, y1) = (x + w, y + h);
            if rx1 <= x || rx0 >= x1 || ry1 <= y || ry0 >= y1 {
                continue;
            }
            if rx0 <= x && rx1 >= x1 && ry0 <= y && ry1 >= y1 {
                e.covered = true;
                break;
            }
            // a cover across the whole height (or width) leaves one side
            if ry0 <= y && ry1 >= y1 {
                if rx0 > x {
                    e.frame[2] = rx0 - x;
                } else {
                    e.frame = [rx1, y, x1 - rx1, h];
                }
            } else if rx0 <= x && rx1 >= x1 {
                if ry0 > y {
                    e.frame[3] = ry0 - y;
                } else {
                    e.frame = [x, ry1, w, y1 - ry1];
                }
            }
        }
    }
}

/// The screen in use and how a process's coordinates sit on its panel (simax's `display`, `fixed`, `fixed_size`)
struct Geometry {
    /// simfb's name of the display in use (x86Sim-main-display, x86Sim-display-<id>); None on a one-screen phone
    display: Option<String>,
    /// the panel's turn as mounted (an inner screen's nativeRotation) and its framebuffer size in points
    native: u32,
    fb: (f64, f64),
}

impl Geometry {
    fn of(inst: &Instance) -> Geometry {
        let sc = inst.scale.max(1.0) as f64;
        match &inst.inner {
            Some(d) if inst.inner_active() => Geometry { display: Some(format!("x86Sim-display-{}", d.id)), native: d.rotation, fb: (d.size.0 as f64 / sc, d.size.1 as f64 / sc) },
            Some(_) => Geometry { display: Some("x86Sim-main-display".into()), native: 0, fb: panel(inst).1 },
            None => Geometry { display: None, native: 0, fb: panel(inst).1 },
        }
    }

    /// The space of a window (or of a process's primary window: the same keys at the top of its answer), None when it
    /// is on another display than the one in use. Its turn comes from where the screen's coordinate space puts the
    /// unit x vector in the screen's fixed space (`fixed`), else from the interface orientation (older readers).
    fn space(&self, w: &Value) -> Option<Space> {
        if let (Some(want), Some(d)) = (&self.display, w["display"].as_str()) {
            if want != d {
                return None;
            }
        }
        let sc = w["screen"].as_array()?;
        let size = (sc.first()?.as_f64()?, sc.get(1)?.as_f64()?);
        if size.0 <= 0.0 {
            return None;
        }
        let rot = match w["fixed"].as_array().map(|f| (f.first().and_then(Value::as_f64).unwrap_or(1.0), f.get(1).and_then(Value::as_f64).unwrap_or(0.0))) {
            Some(f) => {
                let r = fixed_rot(f);
                // a fixed space in the framebuffer's orientation (not the panel's upright picture): Hid adds the mount
                // turn, so take it off here
                let fixed = w["fixed_size"].as_array().map(|f| (f.first().and_then(Value::as_f64).unwrap_or(0.0), f.get(1).and_then(Value::as_f64).unwrap_or(0.0)));
                let fb_like = fixed.is_some_and(|f| (f.0 > f.1) == (self.fb.0 > self.fb.1));
                if self.native % 180 == 90 && fb_like {
                    (r + 360 - self.native) % 360
                } else {
                    r
                }
            }
            None => w["orient"].as_u64().map(interface_rot).unwrap_or(0),
        };
        Some(Space { size, rot })
    }
}

/// Where a screen's coordinate space puts the unit x vector in its fixed space, as the turn of the picture (hid's
/// convention: hid::unrotate(x, y, 90) = (1 - y, x), so x runs down the fixed space at 90)
pub fn fixed_rot(x: (f64, f64)) -> u32 {
    match (x.0.round() as i64, x.1.round() as i64) {
        (0, 1) => 90,
        (-1, 0) => 180,
        (0, -1) => 270,
        _ => 0,
    }
}

/// UIInterfaceOrientation (1 portrait, 2 upside down, 3 landscape left, 4 landscape right) as the panel's turn in
/// hid::orientation_rot's convention (measured: host/simhid's "landscapeLeft" turns Safari to interface 3, whose
/// picture stands upright with the viewer's 90)
pub fn interface_rot(o: u64) -> u32 {
    match o {
        2 => 180,
        3 => 90,
        4 => 270,
        _ => 0,
    }
}

/// The bundle id a process's `.fg` marker names (simax writes it), for messages about a process that did not answer
fn fg_bundle(sock: &Path) -> String {
    std::fs::read_to_string(sock.with_extension("fg")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "?".into())
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(String::from)
}

pub fn snapshot(inst: &Instance, opt: SnapOpts) -> Result<Snapshot> {
    let s = snapshot_once(inst, opt.clone())?;
    if !s.ax_fresh {
        return Ok(s);
    }
    // a process just turned accessibility on for this request: its answer came before the accessibility bundles
    // labelled anything (a fresh phone's first snapshot listed no home-screen icons). Once more after they load.
    settle(inst, Duration::from_millis(300), Duration::from_secs(2));
    snapshot_once(inst, opt)
}

fn snapshot_once(inst: &Instance, opt: SnapOpts) -> Result<Snapshot> {
    let t0 = Instant::now();
    let req = json!({"op": "tree", "fg": true, "all": opt.all, "full": opt.full});
    let timeout = Duration::from_millis(if opt.timeout_ms > 0 { opt.timeout_ms } else { 6000 });
    let mut st = load_refs(inst);
    let answers = ask_fg(inst, &req, timeout, &st.stuck);
    let was_stuck = std::mem::take(&mut st.stuck);
    let mut snap = Snapshot { phone: inst.name.clone(), platform: inst.platform.clone(), ..Default::default() };
    let mut fronts: Vec<(u32, String, bool)> = Vec::new();
    // each app's main window on the display (simax offsets an iPadOS window by its scene's place)
    let mut app_rects: HashMap<String, [f64; 4]> = HashMap::new();
    // the space of each process that answered, on the screen in use: (rank: 0 an app on screen, 1 SpringBoard, 2 other
    // (a helper such as the keyboard's, which can lag behind a rotation), its bundle, space)
    let mut spaces: Vec<(u8, String, Space)> = Vec::new();
    let geo = Geometry::of(inst);
    let mut silent: Vec<String> = Vec::new();
    let n_answers = answers.len();
    for (sock, r) in answers {
        let v = match r {
            Ok(v) => v,
            Err(e) => {
                // no answer at all: hung, or suspended (an extension RunningBoard suspended keeps its .fg: it draws
                // nothing either). Said once (the first time), then it gets a short timeout until it answers again.
                let pid = pid_of_sock(&sock);
                st.stuck.insert(pid);
                let who = format!("{} (pid {pid}: {})", fg_bundle(&sock), if was_stuck.contains(&pid) { "still no answer".to_string() } else { format!("no answer in {:.1} s", timeout.as_secs_f64()) });
                if std::env::var_os("X86SIM_AX_DEBUG").is_some() {
                    eprintln!("ax: {who}: {e:#}");
                }
                if !was_stuck.contains(&pid) {
                    snap.busy.push(who.clone());
                }
                silent.push(who);
                continue;
            }
        };
        let bundle = v["bundle"].as_str().unwrap_or("").to_string();
        if let Some(e) = v["error"].as_str() {
            snap.busy.push(format!("{bundle} (pid {}: {e})", v["pid"].as_u64().unwrap_or(0)));
            continue;
        }
        if let Some(n) = v["web_pending"].as_u64() {
            snap.busy.push(format!("{bundle} web content ({n} frame(s) did not answer in time; their elements are missing)"));
        }
        if v["animating"].as_bool().unwrap_or(false) {
            snap.animating = true;
        }
        if v["ax_fresh"].as_bool().unwrap_or(false) {
            snap.ax_fresh = true;
        }
        let state = v["state"].as_str().unwrap_or("");
        if state != "active" && state != "inactive" {
            continue;
        }
        let proc_space = geo.space(&v);
        let pid = v["pid"].as_u64().unwrap_or(0) as u32;
        // the focus (tvOS): the active process's focused item
        // (not a whole-screen item: a process whose focus sits on a window or a full-screen container)
        if let Some(f) = v["focus_item"]["frame"].as_array() {
            let f = [0, 1, 2, 3].map(|i| f.get(i).and_then(Value::as_f64).unwrap_or(0.0));
            let whole = v["screen"].as_array().map(|sc| f[2] * f[3] >= 0.9 * sc[0].as_f64().unwrap_or(1.0) * sc[1].as_f64().unwrap_or(1.0)).unwrap_or(false);
            if !whole && (snap.focus.is_none() || state == "active") {
                snap.focus = Some(f);
            }
        }
        if bundle != SPRINGBOARD {
            if let Some(f) = v["windows"].as_array().and_then(|a| a.first()).and_then(|w| w["frame"].as_array()) {
                app_rects.insert(bundle.clone(), [0, 1, 2, 3].map(|i| f.get(i).and_then(Value::as_f64).unwrap_or(0.0)));
            }
        }
        let wins: Vec<String> = v["windows"].as_array().map(|a| a.iter().map(|w| w["class"].as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
        // per window: its space, None when it is on a screen not in use (a two-screen phone's other screen)
        let win_space: Vec<Option<Space>> = v["windows"].as_array().map(|a| a.iter().map(|w| geo.space(w)).collect()).unwrap_or_default();
        let els = v["elements"].as_array().cloned().unwrap_or_default();
        let mut has_app_content = false;
        for e in els {
            let win = e["win"].as_u64().unwrap_or(0) as usize;
            let window = wins.get(win).cloned().unwrap_or_default();
            let rot = match win_space.get(win) {
                Some(Some(sp)) => sp.rot,
                Some(None) => continue,
                None => proc_space.map(|s| s.rot).unwrap_or(0),
            };
            let class = e["class"].as_str().unwrap_or("");
            let layer = layer_of(&bundle, &window, class, e["id"].as_str().unwrap_or("")).to_string();
            if layer == "app" || layer == "home" {
                has_app_content = true;
            }
            if layer == "system" {
                snap.overlay = true;
            }
            snap.elements.push(El {
                r: String::new(),
                role: e["role"].as_str().unwrap_or("element").to_string(),
                label: s(&e["label"]),
                value: s(&e["value"]),
                id: s(&e["id"]),
                hint: s(&e["hint"]),
                href: s(&e["href"]),
                frame: e["frame"].as_array().map(|f| [0, 1, 2, 3].map(|i| f.get(i).and_then(Value::as_f64).unwrap_or(0.0))).unwrap_or_default(),
                enabled: e["enabled"].as_bool().unwrap_or(true),
                selected: e["selected"].as_bool().unwrap_or(false),
                focused: e["focused"].as_bool().unwrap_or(false),
                offscreen: e["offscreen"].as_bool().unwrap_or(false),
                covered: false,
                web: e["web"].as_bool().unwrap_or(false),
                app: bundle.clone(),
                pid,
                layer,
                window,
                oid: e["oid"].as_str().unwrap_or("").to_string(),
                container: s(&e["in"]),
                more: e.get("more").cloned(),
                key: s(&e["key"]),
                rot,
                frame_url: s(&e["frame_url"]),
            });
        }
        for h in v["hidden"].as_array().into_iter().flatten() {
            let mut h = h.clone();
            h["app"] = json!(bundle);
            h["pid"] = json!(pid);
            h["window"] = json!(wins.get(h["win"].as_u64().unwrap_or(0) as usize).cloned().unwrap_or_default());
            snap.hidden.push(h);
        }
        if !is_shell(&bundle) && has_app_content {
            fronts.push((pid, bundle.clone(), state == "active"));
        }
        if let Some(sp) = proc_space {
            let rank = if bundle != SPRINGBOARD && has_app_content && state == "active" { 0 } else if bundle == SPRINGBOARD { 1 } else { 2 };
            spaces.push((rank, bundle.clone(), sp));
        }
    }
    if n_answers == 0 {
        bail!("{}: no app answers (is the device running and booted to the home screen? its reader is host/simax)", inst.name);
    }
    if snap.elements.is_empty() && !silent.is_empty() && spaces.is_empty() {
        bail!("{}: no process on screen answered: {}", inst.name, silent.join("; "));
    }
    if inst.inner.is_some() {
        snap.display = Some(if inst.inner_active() { "inner" } else { "outer" }.into());
    }
    // watchOS: Carousel's app grid covers the watch face (which stays active under it)
    if snap.elements.iter().any(|e| e.app == "com.apple.Carousel" && e.layer == "home") {
        snap.elements.retain(|e| e.app != WATCH_FACE);
    }
    // iPadOS windowed apps: SpringBoard's window chrome is not an overlay, and gives the windows' stacking order
    let z = windows_z(&mut snap);
    // top window first, then active before inactive (an app under an alert)
    fronts.sort_by_key(|f| (std::cmp::Reverse(z.get(&f.1).copied().unwrap_or(-1)), !f.2));
    if !z.is_empty() {
        // what shows of an element is inside its window (a web page wider than the window): taps land there
        for e in snap.elements.iter_mut().filter(|e| e.layer == "app" && !e.offscreen) {
            if let Some(r) = app_rects.get(&e.app) {
                let [x, y, w, h] = e.frame;
                let (x0, y0, x1, y1) = (x.max(r[0]), y.max(r[1]), (x + w).min(r[0] + r[2]), (y + h).min(r[1] + r[3]));
                if x1 > x0 && y1 > y0 {
                    e.frame = [x0, y0, x1 - x0, y1 - y0];
                }
            }
        }
        cover(&mut snap.elements, &app_rects, &z);
    }
    snap.front = fronts.into_iter().map(|f| f.1).collect();
    // the screen as the front app sees it (the top window's app on an iPad; a portrait-only app on a sideways phone
    // keeps its portrait points), else SpringBoard's
    let front_space = snap.front.first().and_then(|b| spaces.iter().find(|s| s.0 == 0 && s.1 == *b));
    if let Some((_, _, sp)) = front_space.or_else(|| spaces.iter().min_by_key(|s| s.0)) {
        snap.screen = sp.size;
        snap.rot = sp.rot;
    }
    if snap.front.is_empty() {
        // the home screen: the platform's (tvOS HeadBoard, a watch's Carousel, ...)
        let home = snap.elements.iter().find(|e| e.layer == "home").map(|e| e.app.clone());
        snap.front.push(home.unwrap_or_else(|| SPRINGBOARD.into()));
    }
    if snap.screen.0 <= 0.0 {
        snap.screen = panel(inst).1;
    }
    // reading order on the home screen: rows, then columns (SpringBoard lists icons by page model)
    let idx: Vec<usize> = (0..snap.elements.len()).filter(|&i| snap.elements[i].layer == "home").collect();
    let mut home: Vec<El> = idx.iter().map(|&i| snap.elements[i].clone()).collect();
    home.sort_by(|a, b| ((a.frame[1] / 20.0).round(), a.frame[0]).partial_cmp(&((b.frame[1] / 20.0).round(), b.frame[0])).unwrap_or(std::cmp::Ordering::Equal));
    for (i, e) in idx.into_iter().zip(home) {
        snap.elements[i] = e;
    }
    let mut next = st.next.max(1);
    assign_refs(&mut snap.elements, &st.last.elements, &mut next);
    snap.ms = t0.elapsed().as_millis() as u64;
    st.next = next;
    // the full picture is remembered (an --all snapshot's off-screen elements keep their refs too)
    st.last = snap.clone();
    save_refs(inst, &st);
    Ok(snap)
}

/// The last snapshot taken of the phone (for diffs and refs), if it is of this boot
pub fn last(inst: &Instance) -> Option<Snapshot> {
    let st = load_refs(inst);
    (!st.last.elements.is_empty()).then_some(st.last)
}

// ---------------------------------------------------------------------------------------------------------------------
// Finding elements

/// What an agent names: `@e3` / `e3` (a ref), `id:xyz` (an accessibility identifier), `x,y` / `x y` (a point), or
/// text (a label: exact, then prefix, then substring, case-insensitive; also matches values and identifiers)
pub enum Target {
    Ref(String),
    Id(String),
    Point(f64, f64),
    Text(String),
}

pub fn parse_target(t: &str) -> Target {
    let t = t.trim();
    if let Some(r) = t.strip_prefix('@') {
        return Target::Ref(r.to_string());
    }
    if t.len() > 1 && t.starts_with('e') && t[1..].chars().all(|c| c.is_ascii_digit()) {
        return Target::Ref(t.to_string());
    }
    if let Some(i) = t.strip_prefix("id:").or_else(|| t.strip_prefix("id=")) {
        return Target::Id(i.to_string());
    }
    let nums: Vec<f64> = t.split([',', ' ']).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
    if nums.len() == 2 && t.split([',', ' ']).filter(|s| !s.is_empty()).count() == 2 {
        return Target::Point(nums[0], nums[1]);
    }
    Target::Text(t.to_string())
}

fn layer_rank(l: &str) -> u8 {
    match l {
        "system" => 0,
        "app" => 1,
        "window" => 2,
        "home" => 3,
        _ => 4,
    }
}

fn actionable(role: &str) -> bool {
    !matches!(role, "text" | "heading" | "image" | "element" | "webview")
}

/// Every element matching a target, best first: on screen, top layer (SpringBoard's alerts over the app), how well
/// the text matches, controls before text.
pub fn find_all<'a>(snap: &'a Snapshot, target: &Target) -> Vec<&'a El> {
    find_all_q(snap, target, u8::MAX)
}

/// find_all, text matches no worse than `worst` (0 exact label, 1 exact id / value, 2 label prefix, 3 substring)
pub fn find_all_q<'a>(snap: &'a Snapshot, target: &Target, worst: u8) -> Vec<&'a El> {
    let mut v: Vec<(u8, &El)> = Vec::new();
    for e in &snap.elements {
        let q = match target {
            Target::Ref(r) => (e.r == *r).then_some(0),
            Target::Id(i) => (e.id.as_deref() == Some(i.as_str())).then_some(0),
            Target::Point(..) => None,
            Target::Text(t) => {
                // case and space blind: labels carry no-break spaces ("Apple\u{a0}TV", "8:14\u{202f}AM")
                let n = |s: &str| s.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect::<String>().to_lowercase();
                let t = n(t);
                let l = n(e.label.as_deref().unwrap_or(""));
                let val = n(e.value.as_deref().unwrap_or(""));
                let id = n(e.id.as_deref().unwrap_or(""));
                if l == t {
                    Some(0)
                } else if id == t || val == t {
                    Some(1)
                } else if !l.is_empty() && l.starts_with(&t) {
                    Some(2)
                } else if l.contains(&t) {
                    Some(3)
                } else if val.contains(&t) {
                    Some(4)
                } else {
                    None
                }
            }
        };
        if let Some(q) = q.filter(|q| *q <= worst) {
            v.push((q, e));
        }
    }
    // the front-most app's element wins a tie (iPadOS: two windows with a Back button each)
    let front_rank = |app: &str| snap.front.iter().position(|f| f == app).unwrap_or(usize::MAX);
    v.sort_by_key(|(q, e)| (e.offscreen || e.covered, layer_rank(&e.layer), *q, front_rank(&e.app), !actionable(&e.role), e.role == "webview"));
    v.into_iter().map(|x| x.1).collect()
}

pub fn find<'a>(snap: &'a Snapshot, target: &str) -> Result<&'a El> {
    let t = parse_target(target);
    find_all(snap, &t).into_iter().next().ok_or_else(|| {
        if matches!(t, Target::Ref(_)) {
            anyhow!("no element {target} on screen now (refs come from the last snapshot; take a new one)")
        } else {
            anyhow!("nothing on screen matches {target:?}")
        }
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// Acting

/// The panel in use: an unfolded two-screen phone's inner screen (its simfb display id and the turn it is mounted
/// at), else the main screen; and its upright size in points
fn panel(inst: &Instance) -> (Option<(u32, u32)>, (f64, f64)) {
    let sc = inst.scale.max(1.0) as f64;
    match inst.inner.as_ref().filter(|_| inst.inner_active()) {
        Some(d) => {
            let (w, h) = d.upright();
            (Some((d.id, d.rotation)), (w as f64 / sc, h as f64 / sc))
        }
        None => (None, (inst.screen.0 as f64 / sc, inst.screen.1 as f64 / sc)),
    }
}

/// Where points are: the screen size (points) and the interface orientation's turn of the panel they are given in.
/// UIKit's screen coordinates turn with the interface, not with the device: a portrait-only app on a phone held
/// landscape keeps portrait coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Space {
    pub size: (f64, f64),
    pub rot: u32,
}

impl Space {
    /// the same screen in another interface orientation
    pub fn turned(&self, rot: u32) -> Space {
        let d = (rot + 360 - self.rot % 360) % 180;
        Space { size: if d == 90 { (self.size.1, self.size.0) } else { self.size }, rot }
    }

    /// a point in this space -> normalized upright coordinates of the interface's picture (Hid turns them to the panel)
    pub fn norm(&self, x: f64, y: f64) -> (f32, f32) {
        ((x / self.size.0.max(1.0)) as f32, (y / self.size.1.max(1.0)) as f32)
    }
}

impl Snapshot {
    /// the space its points are in (the front app's)
    pub fn space(&self) -> Space {
        Space { size: self.screen, rot: self.rot }
    }

    /// the space an element's frame is in (its own window's orientation: SpringBoard's status bar can be turned
    /// another way than a portrait-only app under it)
    pub fn space_of(&self, e: &El) -> Space {
        self.space().turned(e.rot)
    }
}

/// The touch / key channel to the phone for points in a space: the panel in use (an unfolded phone's inner screen)
/// and the turn from the interface's picture to the panel
fn hid_for(inst: &Instance, sp: Space) -> Hid {
    let mut h = Hid::new(&inst.sock);
    match panel(inst).0 {
        Some((id, native)) => h.display = Some((id, (native + sp.rot) % 360)),
        None => h.main_rot = sp.rot % 360,
    }
    h
}

/// The front app's space now: a quick `info` of the processes on screen (no walk), else the last snapshot's, else the
/// device orientation's
pub fn front_space(inst: &Instance) -> Space {
    let answers = ask_fg(inst, &json!({"op": "info"}), Duration::from_millis(1500), &load_refs(inst).stuck);
    let geo = Geometry::of(inst);
    let front = last(inst).map(|s| s.front).unwrap_or_default();
    let mut best: Option<((u8, usize), Space)> = None;
    for (_, r) in answers {
        let Ok(v) = r else { continue };
        let st = v["state"].as_str().unwrap_or("");
        if !matches!(st, "active" | "inactive") || v["error"].is_string() {
            continue;
        }
        let Some(sp) = geo.space(&v) else { continue };
        // the app the last snapshot had in front (view services and the keyboard host are active too; of two iPad
        // windows, the top one), else SpringBoard
        let bundle = v["bundle"].as_str().unwrap_or("");
        let pos = front.iter().position(|f| f == bundle);
        let rank = match pos {
            Some(i) if bundle != SPRINGBOARD && st == "active" => (0, i),
            _ if bundle == SPRINGBOARD => (1, 0),
            _ => (2, 0),
        };
        if best.is_none_or(|b| rank < b.0) {
            best = Some((rank, sp));
        }
    }
    if let Some((_, sp)) = best {
        return sp;
    }
    if let Some(s) = last(inst).filter(|s| s.screen.0 > 0.0) {
        return s.space();
    }
    let rot = inst.orientation().as_deref().and_then(hid::orientation_rot).unwrap_or(0);
    Space { size: panel(inst).1, rot: 0 }.turned(rot)
}

/// The frame ring of the screen in use (an unfolded two-screen phone draws on its inner screen's ring)
pub fn screen_ring(inst: &Instance) -> PathBuf {
    match inst.inner.as_ref().filter(|_| inst.inner_active()) {
        Some(d) => d.ring.clone(),
        None => inst.ring.clone(),
    }
}

/// The newest frame's number, to settle on what a change draws (settle_since)
pub fn frame_seq(inst: &Instance) -> u64 {
    Ring::new(&screen_ring(inst)).seq()
}

/// After a change made when the newest frame was `seq0`: waits up to `first` for it to draw anything (a setting can
/// take a second before its first frame), then until the screen has been still for `quiet`, at most `max` in all
pub fn settle_since(inst: &Instance, seq0: u64, first: Duration, quiet: Duration, max: Duration) {
    let t0 = Instant::now();
    let mut ring = Ring::new(&screen_ring(inst));
    if ring.seq() == seq0 {
        ring.wait(seq0, first);
    }
    settle(inst, quiet, max.saturating_sub(t0.elapsed()));
}

/// Waits until the screen has been still for `quiet` (no new frame), at most `max`
pub fn settle(inst: &Instance, quiet: Duration, max: Duration) {
    let mut ring = Ring::new(&screen_ring(inst));
    let end = Instant::now() + max;
    let mut seq = ring.seq();
    // the first frame of a reaction can take a moment to come (a slow app under the translator)
    let mut q = quiet.max(Duration::from_millis(600));
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        let n = ring.wait(seq, q.min(left));
        if n == seq {
            return;
        }
        seq = n;
        q = quiet;
    }
}

/// A touch at a point (screen points of `sp`), held `hold_ms` (80: a tap; 600+: a long press)
pub fn touch_at(inst: &Instance, sp: Space, x: f64, y: f64, hold_ms: u64) {
    let (nx, ny) = sp.norm(x, y);
    let mut h = hid_for(inst, sp);
    h.touch("down", nx, ny, None);
    if hold_ms > 200 {
        // a still finger keeps reporting (a digitizer does at its scan rate)
        let end = Instant::now() + Duration::from_millis(hold_ms);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
            h.touch("move", nx, ny, None);
        }
    } else {
        std::thread::sleep(Duration::from_millis(hold_ms));
    }
    h.touch("up", nx, ny, None);
}

/// A drag from (x1, y1) to (x2, y2) in points over `ms`; `hold_ms` still at the end (no fling: a precise scroll)
pub fn drag(inst: &Instance, sp: Space, from: (f64, f64), to: (f64, f64), ms: u64, hold_ms: u64) {
    let scr = sp.size;
    let (a, b) = (sp.norm(from.0, from.1), sp.norm(to.0, to.1));
    let mut h = hid_for(inst, sp);
    let edge = hid::edge_at(a.0, a.1, scr.0 as u32, scr.1 as u32);
    let n = (ms / 16).max(2);
    h.touch("down", a.0, a.1, edge);
    for i in 1..=n {
        std::thread::sleep(Duration::from_millis(ms / n));
        // ease out into a hold, so UIKit sees the finger stop (no momentum)
        let f = i as f32 / n as f32;
        let f = if hold_ms > 0 { 1.0 - (1.0 - f).powi(3) } else { f };
        h.touch("move", a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, None);
    }
    let end = Instant::now() + Duration::from_millis(hold_ms);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(16));
        h.touch("move", b.0, b.1, None);
    }
    h.touch("up", b.0, b.1, None);
}

/// Resolves a target to an element of a fresh snapshot (a ref: of the last one, checked against the screen now)
pub fn resolve(inst: &Instance, target: &str) -> Result<(El, Snapshot)> {
    let snap = snapshot(inst, SnapOpts::default())?;
    let e = find(&snap, target)?.clone();
    Ok((e, snap))
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    /// what was done, one line
    pub did: String,
    /// the screen after it settled (None when the action asked for none)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen: Option<Snapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// What an action answers with: the screen after it (full), only the changes (diff), or nothing (none)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum After {
    Full,
    Diff,
    None,
}

impl After {
    pub fn parse(s: Option<&str>) -> After {
        match s {
            Some("diff") => After::Diff,
            Some("none") => After::None,
            _ => After::Full,
        }
    }
}

pub fn outcome(inst: &Instance, did: String, before: Option<Snapshot>, after: After) -> Outcome {
    if after == After::None {
        return Outcome { did, screen: None, diff: None };
    }
    let t0 = Instant::now();
    settle(inst, Duration::from_millis(350), Duration::from_millis(4000));
    let mut snap = snapshot(inst, SnapOpts::default());
    // still animating (a transition can pause drawing for longer than the quiet window): again, a few times
    while matches!(&snap, Ok(s) if s.animating) && t0.elapsed() < Duration::from_secs(6) {
        settle(inst, Duration::from_millis(200), Duration::from_millis(1500));
        snap = snapshot(inst, SnapOpts::default());
    }
    match snap {
        Ok(s) => {
            let diff = (after == After::Diff).then(|| before.map(|b| s.diff(&b)).unwrap_or_else(|| s.text()));
            Outcome { did, screen: Some(s), diff }
        }
        Err(e) => Outcome { did: format!("{did}; then no snapshot: {e:#}"), screen: None, diff: None },
    }
}

impl Outcome {
    pub fn text(&self) -> String {
        let mut s = self.did.clone();
        s.push('\n');
        if let Some(d) = &self.diff {
            s += d;
        } else if let Some(sc) = &self.screen {
            s += &sc.text();
        }
        s
    }
}

/// Tap an element (ref, id:, text) or a point ("x y"). On an Apple TV (no touchscreen: the Siri Remote moves a focus)
/// the focus is moved onto the element with the remote's arrows and select is pressed, as XCUIRemote does.
pub fn tap(inst: &Instance, target: &str, hold_ms: u64, after: After) -> Result<Outcome> {
    if inst.platform == "tvos" {
        return tv_select(inst, target, hold_ms, after);
    }
    if inst.platform == "xros" {
        return xr_activate(inst, target, hold_ms, after);
    }
    let (x, y, what, before, sp) = match parse_target(target) {
        Target::Point(x, y) => (x, y, format!("({x:.0},{y:.0})"), last(inst), front_space(inst)),
        _ => {
            let (mut e, mut snap) = resolve(inst, target)?;
            if e.offscreen {
                scroll_into_view(inst, &e)?;
                let s2 = snapshot(inst, SnapOpts::default())?;
                e = find(&s2, &format!("@{}", e.r)).cloned().or_else(|_| find(&s2, target).cloned())?;
                snap = s2;
            }
            if e.covered {
                // iPadOS: under another app's window; a touch there would hit that window
                let front = snap.front.first().cloned().unwrap_or_default();
                bail!("@{} is under {front}'s window: bring {} to the front first (tap a part of its window that shows, or its icon in the dock)", e.r, e.app);
            }
            let sp = snap.space_of(&e);
            let (x, y) = e.center(sp.size);
            (x, y, format!("@{} {} {}", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default()), Some(snap), sp)
        }
    };
    anyhow::ensure!(simhid_up(inst), "{}: its touch input (host/simhid) does not answer (is the device still starting?)", inst.name);
    touch_at(inst, sp, x, y, hold_ms);
    let verb = if hold_ms >= 500 { "long-pressed" } else { "tapped" };
    let mut did = format!("{verb} {what} at ({x:.0},{y:.0})");
    if hold_ms < 500 && what.contains(" icon ") && after != After::None {
        // an app icon: the answer is the app, once it is in front (its first frames can take seconds to come). On a
        // phone just booted the home screen can still be settling (icons flying in, a page laid out again) and the
        // touch lands between icons: when nothing opened, the icon is looked up again and tapped once more.
        let front0 = before.as_ref().map(|b| b.front.clone()).unwrap_or_default();
        if !wait_front_change(inst, front0.clone(), Duration::from_secs(6)) {
            if let Ok((e, snap)) = resolve(inst, target) {
                // only when the screen shows the same things (an icon that opened a folder or a menu did something)
                let items = |s: &Snapshot| {
                    let mut v: Vec<(String, String)> = s.elements.iter().filter(|e| e.layer != "status").map(|e| (e.role.clone(), e.label.clone().unwrap_or_default())).collect();
                    v.sort();
                    v
                };
                let same = before.as_ref().is_some_and(|b| items(b) == items(&snap));
                if e.role == "icon" && snap.front == front0 && same {
                    let sp = snap.space_of(&e);
                    let (x2, y2) = e.center(sp.size);
                    touch_at(inst, sp, x2, y2, hold_ms);
                    did += &format!("; nothing opened, tapped again at ({x2:.0},{y2:.0})");
                    wait_front_change(inst, front0, Duration::from_secs(10));
                }
            }
        }
    }
    Ok(outcome(inst, did, before, after))
}

// ---------------------------------------------------------------------------------------------------------------------
// tvOS: the focus engine

fn center_of(f: &[f64; 4]) -> (f64, f64) {
    (f[0] + f[2] / 2.0, f[1] + f[3] / 2.0)
}

/// The element a target names on an Apple TV: a point picks the element under it (front layer first)
fn tv_find(snap: &Snapshot, target: &str) -> Result<El> {
    if let Target::Point(x, y) = parse_target(target) {
        let mut hits: Vec<&El> = snap.elements.iter().filter(|e| !e.offscreen && e.frame[0] <= x && x <= e.frame[0] + e.frame[2] && e.frame[1] <= y && y <= e.frame[1] + e.frame[3]).collect();
        hits.sort_by(|a, b| (layer_rank(&a.layer), !actionable(&a.role), a.frame[2] * a.frame[3]).partial_cmp(&(layer_rank(&b.layer), !actionable(&b.role), b.frame[2] * b.frame[3])).unwrap_or(std::cmp::Ordering::Equal));
        return hits.first().map(|e| (*e).clone()).ok_or_else(|| anyhow!("no element at ({x:.0},{y:.0})"));
    }
    find(snap, target).cloned()
}

/// Moves the tvOS focus onto a target with the remote's arrows (keyboard arrows: the same focus moves), checking the
/// focus after every press: the larger distance first, the other axis when the focus does not move. Answers the
/// element (now focused) and the snapshot that shows it.
pub fn tv_focus(inst: &Instance, target: &str) -> Result<(El, Snapshot, u32)> {
    let mut snap = snapshot(inst, SnapOpts::default())?;
    let first = tv_find(&snap, target)?;
    let mut h = Hid::new(&inst.sock);   // remote keys: no touch space
    let mut presses = 0;
    let mut blocked: HashSet<u32> = HashSet::new();
    let mut last_focus: Option<[f64; 4]> = None;
    let mut last_key = 0;
    let mut same = 0;
    for _ in 0..60 {
        // the same element: its ref (refs follow the element as the focus scales it), else the target again
        let e = find(&snap, &format!("@{}", first.r)).cloned().or_else(|_| tv_find(&snap, target))?;
        if e.focused && snap.focused_el().map(|x| x.r == e.r).unwrap_or(true) {
            return Ok((e, snap, presses));
        }
        // the focused element (the front app's first), else where the focus engine says it is
        let fel = snap.focused_el().map(|x| x.frame);
        let Some(f) = fel.or(snap.focus) else {
            bail!("nothing has the focus on screen (an app that takes no remote focus?): cannot move it to @{}", e.r);
        };
        if last_focus == Some(f) && last_key != 0 {
            // that press did not move the focus: that way is closed from here. A focus on a container that moves its
            // own highlight (tvOS's keyboard: the focus is the whole keyboard) stays put for a few presses first.
            same += 1;
            if fel.is_some() || same >= 6 {
                blocked.insert(last_key);
            }
        } else {
            blocked.clear();
            same = 0;
        }
        last_focus = Some(f);
        let (fx, fy) = center_of(&f);
        let (tx, ty) = center_of(&e.frame);
        let (dx, dy) = (tx - fx, ty - fy);
        // in the same row / column: along it; else the larger distance first
        let same_row = (e.frame[1] < f[1] + f[3]) && (f[1] < e.frame[1] + e.frame[3]);
        let same_col = (e.frame[0] < f[0] + f[2]) && (f[0] < e.frame[0] + e.frame[2]);
        let horiz = if dx.abs() < 1.0 { 0 } else if dx > 0.0 { 0x4f } else { 0x50 };
        let vert = if dy.abs() < 1.0 { 0 } else if dy > 0.0 { 0x51 } else { 0x52 };
        let order = if same_row { [horiz, vert] } else if same_col { [vert, horiz] } else if dx.abs() > dy.abs() { [horiz, vert] } else { [vert, horiz] };
        if std::env::var_os("X86SIM_AX_DEBUG").is_some() {
            eprintln!("tv_focus: focus {f:?} target @{} {:?} blocked {blocked:?} order {order:?}", e.r, e.frame);
        }
        let Some(k) = order.into_iter().find(|k| *k != 0 && !blocked.contains(k)) else {
            bail!("the focus cannot reach @{} {} from where it is (tried {}): it may not be focusable", e.r, e.label.as_deref().map(quote).unwrap_or_default(), presses);
        };
        let seq = frame_seq(inst);
        h.key(true, k);
        std::thread::sleep(Duration::from_millis(40));
        h.key(false, k);
        presses += 1;
        last_key = k;
        // the focus animates (scale, parallax): wait for it to settle before reading where it is
        settle_since(inst, seq, Duration::from_millis(700), Duration::from_millis(150), Duration::from_millis(1500));
        snap = snapshot(inst, SnapOpts::default())?;
    }
    bail!("the focus did not reach {target:?} in {presses} presses")
}

fn tv_select(inst: &Instance, target: &str, hold_ms: u64, after: After) -> Result<Outcome> {
    let before = last(inst);
    let (e, _, presses) = tv_focus(inst, target)?;
    // select: the remote's click (held: a long press, e.g. to rearrange the Home Screen)
    Hid::new(&inst.sock).press_for("select", hold_ms.max(80));
    let verb = if hold_ms >= 500 { "long-pressed select on" } else { "selected" };
    let did = format!("{verb} @{} {} {} (focus moved with {presses} remote press{})", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default(), if presses == 1 { "" } else { "es" });
    Ok(outcome(inst, did, before, after))
}

// ---------------------------------------------------------------------------------------------------------------------
// visionOS: windows float in a rendered room. An element's frame is in its window's own points, and where that window
// is on the display depends on its place in 3D and the eyes' pose, so a point on the display cannot be computed from
// it. Elements are acted on through accessibility instead (what VoiceOver does): accessibilityActivate for a tap,
// the scroll view's offset for a scroll. sim_pointer still drives the system pointer by display position.

fn xr_activate(inst: &Instance, target: &str, hold_ms: u64, after: After) -> Result<Outcome> {
    if matches!(parse_target(target), Target::Point(..)) {
        bail!("Vision Pro: a point is in a window's own coordinates, not the display's: tap an element (@ref, id:, text), or move the system pointer with sim_pointer (at x y on the display) and click");
    }
    anyhow::ensure!(hold_ms < 500, "Vision Pro: no long press through accessibility (use sim_pointer down / up)");
    let (mut e, mut snap) = resolve(inst, target)?;
    if e.offscreen {
        scroll_into_view(inst, &e)?;
        snap = snapshot(inst, SnapOpts::default())?;
        e = find(&snap, &format!("@{}", e.r)).cloned().or_else(|_| find(&snap, target).cloned())?;
    }
    anyhow::ensure!(!e.web, "Vision Pro: web content cannot be activated through accessibility yet (sim_pointer)");
    let sock = sock_of_pid(inst, e.pid).ok_or_else(|| anyhow!("{} is gone", e.app))?;
    let r = ask(&sock, &json!({"op": "activate", "oid": e.oid, "tap": true}), Duration::from_secs(4))?.ok_or_else(|| anyhow!("{} is gone", e.app))?;
    if let Some(err) = r["error"].as_str() {
        bail!("cannot activate @{}: {err}", e.r);
    }
    let did = if r["tapped"].as_bool() == Some(true) {
        format!("activated @{} {} {} (Vision Pro: a tap made in the app at its activation point, as VoiceOver does when accessibilityActivate is not enough)", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default())
    } else if r["ok"].as_bool() == Some(true) {
        format!("activated @{} {} {} (Vision Pro: accessibilityActivate, as VoiceOver)", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default())
    } else {
        format!("@{} {} {} did not take accessibilityActivate (nothing happened; try sim_pointer)", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default())
    };
    let before = Some(snap);
    if did.starts_with("activated") && (e.role == "icon" || e.app == "com.apple.RealityLauncher") {
        wait_front_change(inst, before.as_ref().map(|b| b.front.clone()).unwrap_or_default(), Duration::from_secs(10));
    }
    Ok(outcome(inst, did, before, after))
}

/// visionOS: scroll the front app's main scroll view (simax scrollby)
fn xr_scroll(inst: &Instance, direction: &str, amount: f64, after: After) -> Result<Outcome> {
    let before = last(inst);
    let snap = snapshot(inst, SnapOpts::default())?;
    let front = snap.front.first().cloned().unwrap_or_default();
    let pid = snap.elements.iter().find(|e| e.app == front).map(|e| e.pid).ok_or_else(|| anyhow!("no app in front to scroll"))?;
    let sock = sock_of_pid(inst, pid).ok_or_else(|| anyhow!("{front} is gone"))?;
    let r = ask(&sock, &json!({"op": "scrollby", "dir": direction, "amount": amount.clamp(0.05, 2.0)}), Duration::from_secs(3))?.ok_or_else(|| anyhow!("{front} is gone"))?;
    if let Some(err) = r["error"].as_str() {
        bail!("cannot scroll {direction}: {err}");
    }
    let moved = r["moved"].as_bool().unwrap_or(false);
    Ok(outcome(inst, format!("scrolled {front}'s {} {direction}{}", r["view"].as_str().unwrap_or("scroll view"), if moved { "" } else { " (already at the end)" }), before, after))
}

/// Waits (on new frames) until `bundle` is in front, showing something
fn wait_front_is(inst: &Instance, bundle: &str, timeout: Duration) {
    let t0 = Instant::now();
    let mut ring = Ring::new(&screen_ring(inst));
    while t0.elapsed() < timeout {
        let seq = ring.seq();
        if let Ok(s) = snapshot(inst, SnapOpts::default()) {
            if s.front.first().map(|f| f.as_str()) == Some(bundle) && s.elements.iter().any(|e| e.app == bundle && e.layer == "app") {
                return;
            }
        }
        ring.wait(seq, Duration::from_millis(500));
    }
}

/// Waits (on new frames) until the apps in front differ from `front`; false when they did not in time
fn wait_front_change(inst: &Instance, front: Vec<String>, timeout: Duration) -> bool {
    let t0 = Instant::now();
    let mut ring = Ring::new(&screen_ring(inst));
    while t0.elapsed() < timeout {
        let seq = ring.seq();
        if let Ok(s) = snapshot(inst, SnapOpts::default()) {
            if s.front != front && s.elements.iter().any(|e| e.layer == "app") {
                return true;
            }
        }
        ring.wait(seq, Duration::from_millis(500));
    }
    false
}

fn scroll_into_view(inst: &Instance, e: &El) -> Result<()> {
    let sock = sock_of_pid(inst, e.pid).ok_or_else(|| anyhow!("{} is gone", e.app))?;
    let r = ask(&sock, &json!({"op": "scroll", "oid": e.oid}), Duration::from_secs(3))?.ok_or_else(|| anyhow!("{} is gone", e.app))?;
    if let Some(err) = r["error"].as_str() {
        bail!("cannot scroll to @{}: {err}", e.r);
    }
    settle(inst, Duration::from_millis(200), Duration::from_millis(1500));
    Ok(())
}

/// Types text into the focused field as the software keyboard does (insertText: of its UIKeyInput: any Unicode, no
/// lost keys); `clear` empties it first, `submit` ends with return. `keys`: hardware key presses instead (US layout).
pub fn type_text(inst: &Instance, text: &str, clear: bool, submit: bool, keys: bool, after: After) -> Result<Outcome> {
    let before = last(inst);
    if keys {
        // clear: the field emptied by the app first (as without keys), then the key presses
        if clear {
            type_in_app(inst, &json!({"op": "type", "text": "", "clear": true, "submit": false}))?;
        }
        let mut h = Hid::new(&inst.sock);
        for c in text.chars() {
            let (u, shift) = key_usage(c).ok_or_else(|| anyhow!("no US key for {c:?} (type without keys)"))?;
            if shift {
                h.key(true, 225);
            }
            h.key(true, u);
            h.key(false, u);
            if shift {
                h.key(false, 225);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if submit {
            h.key(true, 0x28);
            h.key(false, 0x28);
        }
        return Ok(outcome(inst, format!("typed {} keys", text.chars().count()), before, after));
    }
    let v = type_in_app(inst, &json!({"op": "type", "text": text, "clear": clear, "submit": submit}))?;
    let did = format!(
        "typed {} into {}{}",
        quote(text),
        v["field"].as_str().unwrap_or("the focused field"),
        v["value"].as_str().map(|s| format!(" (now {})", quote(s))).unwrap_or_default()
    );
    Ok(outcome(inst, did, before, after))
}

/// A type request to the process with the focused text field: apps first, SpringBoard (Spotlight, alerts' fields)
/// last. Answers that process's answer. No focused field yet: asked again for up to 3 s (a tap on an Apple Watch text
/// field brings up the input sheet, QuickboardViewService, a second later in its own process).
fn type_in_app(inst: &Instance, req: &Value) -> Result<Value> {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let mut eps: Vec<PathBuf> = endpoints(inst).into_iter().filter(|e| e.fg).map(|e| e.sock).collect();
        let sb_pid = last(inst).and_then(|s| s.elements.iter().find(|e| e.app == SPRINGBOARD).map(|e| e.pid));
        eps.sort_by_key(|p| Some(pid_of_sock(p)) == sb_pid);
        let mut why = String::from("no app on screen");
        for p in eps {
            match ask(&p, req, Duration::from_secs(4)) {
                Ok(Some(v)) if v["error"].is_null() => return Ok(v),
                Ok(Some(v)) => why = v["error"].as_str().unwrap_or("").to_string(),
                _ => {}
            }
        }
        if Instant::now() >= until || !why.starts_with("no focused text field") {
            bail!("cannot type: {why}")
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// HID usage (keyboard page) and shift for a character, US layout
pub fn key_usage(c: char) -> Option<(u32, bool)> {
    let lower = "abcdefghijklmnopqrstuvwxyz";
    if let Some(i) = lower.find(c) {
        return Some((4 + i as u32, false));
    }
    if let Some(i) = lower.find(c.to_ascii_lowercase()) {
        return Some((4 + i as u32, true));
    }
    if let Some(i) = "1234567890".find(c) {
        return Some((0x1e + i as u32, false));
    }
    if let Some(i) = "!@#$%^&*()".find(c) {
        return Some((0x1e + i as u32, true));
    }
    let plain = [(' ', 0x2c), ('\n', 0x28), ('\t', 0x2b), ('-', 0x2d), ('=', 0x2e), ('[', 0x2f), (']', 0x30), ('\\', 0x31), (';', 0x33), ('\'', 0x34), ('`', 0x35), (',', 0x36), ('.', 0x37), ('/', 0x38)];
    let shifted = [('_', 0x2d), ('+', 0x2e), ('{', 0x2f), ('}', 0x30), ('|', 0x31), (':', 0x33), ('"', 0x34), ('~', 0x35), ('<', 0x36), ('>', 0x37), ('?', 0x38)];
    plain.iter().find(|k| k.0 == c).map(|k| (k.1, false)).or_else(|| shifted.iter().find(|k| k.0 == c).map(|k| (k.1, true)))
}

/// A named key (return, tab, escape, delete, space, up/down/left/right, home/end/pageup/pagedown) or a usage number
pub fn key_named(name: &str) -> Option<u32> {
    let n = name.to_lowercase();
    Some(match n.as_str() {
        "return" | "enter" => 0x28,
        "escape" | "esc" => 0x29,
        "delete" | "backspace" => 0x2a,
        "tab" => 0x2b,
        "space" => 0x2c,
        "right" => 0x4f,
        "left" => 0x50,
        "down" => 0x51,
        "up" => 0x52,
        "home" => 0x4a,
        "end" => 0x4d,
        "pageup" => 0x4b,
        "pagedown" => 0x4e,
        "forwarddelete" => 0x4c,
        _ => {
            if n.chars().count() == 1 {
                return key_usage(n.chars().next()?).map(|k| k.0);
            }
            return n.strip_prefix("0x").map(|h| u32::from_str_radix(h, 16).ok()).unwrap_or_else(|| n.parse().ok());
        }
    })
}

/// A key press with modifiers ("cmd+a", "shift+tab", "return")
pub fn key(inst: &Instance, combo: &str, after: After) -> Result<Outcome> {
    let before = last(inst);
    let parts: Vec<&str> = combo.split('+').collect();
    let (mods, k) = parts.split_at(parts.len() - 1);
    let mut mu = Vec::new();
    for m in mods {
        mu.push(match m.to_lowercase().as_str() {
            "cmd" | "command" | "meta" => 0xe3,
            "shift" => 0xe1,
            "ctrl" | "control" => 0xe0,
            "alt" | "option" | "opt" => 0xe2,
            o => bail!("unknown modifier {o} (cmd shift ctrl alt)"),
        });
    }
    let u = key_named(k[0]).ok_or_else(|| anyhow!("unknown key {:?}", k[0]))?;
    let mut h = Hid::new(&inst.sock);
    for m in &mu {
        h.key(true, *m);
    }
    h.key(true, u);
    std::thread::sleep(Duration::from_millis(30));
    h.key(false, u);
    for m in mu.iter().rev() {
        h.key(false, *m);
    }
    Ok(outcome(inst, format!("pressed key {combo}"), before, after))
}

/// Scroll the content: `direction` is where the content goes into view (down = see what is below; the finger moves
/// up), `amount` in screens (1 = most of a screen), from the middle of the screen or of an element. No fling.
pub fn scroll(inst: &Instance, direction: &str, amount: f64, at: Option<&str>, after: After) -> Result<Outcome> {
    let before = last(inst);
    if inst.platform == "xros" {
        let _ = at;
        return xr_scroll(inst, direction, amount, after);
    }
    if inst.platform == "tvos" {
        // an Apple TV scrolls by moving the focus: arrow presses (about 4 per screen), from the focus, not a point
        let k = tv_arrow(direction)?;
        let n = (amount.clamp(0.05, 2.0) * 4.0).round().max(1.0) as u32;
        let mut h = Hid::new(&inst.sock);   // remote keys: no touch space
        for _ in 0..n {
            h.key(true, k);
            std::thread::sleep(Duration::from_millis(40));
            h.key(false, k);
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = at;
        return Ok(outcome(inst, format!("moved the focus {direction} {n} time(s) (Apple TV: the remote's arrows)"), before, after));
    }
    let (sp, cx, cy, area) = match at {
        Some(t) => {
            let (e, s) = resolve(inst, t)?;
            let sp = s.space_of(&e);
            let c = e.center(sp.size);
            (sp, c.0, c.1, (e.frame[2].min(sp.size.0), e.frame[3].min(sp.size.1)))
        }
        None => {
            let sp = front_space(inst);
            (sp, sp.size.0 / 2.0, sp.size.1 / 2.0, sp.size)
        }
    };
    let scr = sp.size;
    let amount = amount.clamp(0.05, 2.0);
    let (dx, dy) = match direction {
        "down" => (0.0, -area.1 * 0.6 * amount),
        "up" => (0.0, area.1 * 0.6 * amount),
        "right" => (-area.0 * 0.6 * amount, 0.0),
        "left" => (area.0 * 0.6 * amount, 0.0),
        o => bail!("direction {o:?}: up, down, left or right"),
    };
    // centred on the point, kept off the screen edges (an edge touch is a system gesture)
    let m = 0.06 * scr.0;
    let cl = |v: f64, hi: f64| v.clamp(m, hi - m);
    let from = (cl(cx - dx / 2.0, scr.0), cl(cy - dy / 2.0, scr.1));
    let to = (cl(cx + dx / 2.0, scr.0), cl(cy + dy / 2.0, scr.1));
    drag(inst, sp, from, to, 400, 150);
    Ok(outcome(inst, format!("scrolled {direction} {amount}"), before, after))
}

/// A swipe: a quick flick (momentum, page turns, dismissing) from one point to another, or in a direction from the
/// middle; a swipe that starts at a screen edge is a system gesture (bottom edge up: home)
pub fn swipe(inst: &Instance, from: (f64, f64), to: (f64, f64), ms: u64, after: After) -> Result<Outcome> {
    let before = last(inst);
    drag(inst, front_space(inst), from, to, ms.max(50), 0);
    Ok(outcome(inst, format!("swiped ({:.0},{:.0}) -> ({:.0},{:.0})", from.0, from.1, to.0, to.1), before, after))
}

fn tv_arrow(direction: &str) -> Result<u32> {
    Ok(match direction {
        "down" => 0x51,
        "up" => 0x52,
        "right" => 0x4f,
        "left" => 0x50,
        o => bail!("direction {o:?}: up, down, left or right"),
    })
}

pub fn swipe_dir(inst: &Instance, direction: &str, after: After) -> Result<Outcome> {
    if inst.platform == "xros" {
        // a flick moves the content with the finger: swipe up shows what is below
        let d = match direction {
            "up" => "down",
            "down" => "up",
            "left" => "right",
            "right" => "left",
            "home" => {
                let before = last(inst);
                Hid::new(&inst.sock).press_for("home", 100);
                return Ok(outcome(inst, "pressed the Digital Crown (Vision Pro: Home View)".into(), before, after));
            }
            o => bail!("direction {o:?} on a Vision Pro: up down left right home"),
        };
        return xr_scroll(inst, d, 0.8, after);
    }
    if inst.platform == "tvos" {
        // a swipe on the Siri Remote's touch surface moves the focus one step that way (content follows the focus)
        let before = last(inst);
        let k = match direction {
            "home" => {
                Hid::new(&inst.sock).press_for("tv", 100);
                return Ok(outcome(inst, "pressed the TV button (Apple TV: home)".into(), before, after));
            }
            "back" => {
                Hid::new(&inst.sock).press_for("menu", 100);
                return Ok(outcome(inst, "pressed back (menu) on the remote".into(), before, after));
            }
            // a swipe on the remote moves the focus the way the finger goes
            d => tv_arrow(d).map_err(|_| anyhow!("direction {d:?} on an Apple TV: up down left right home back"))?,
        };
        let mut h = Hid::new(&inst.sock);   // remote keys: no touch space
        h.key(true, k);
        std::thread::sleep(Duration::from_millis(40));
        h.key(false, k);
        return Ok(outcome(inst, format!("moved the focus with a remote swipe {direction}"), before, after));
    }
    let s = front_space(inst).size;
    let (cx, cy) = (s.0 / 2.0, s.1 / 2.0);
    let (from, to) = match direction {
        "up" => ((cx, s.1 * 0.75), (cx, s.1 * 0.25)),
        "down" => ((cx, s.1 * 0.25), (cx, s.1 * 0.75)),
        "left" => ((s.0 * 0.85, cy), (s.0 * 0.15, cy)),
        "right" => ((s.0 * 0.15, cy), (s.0 * 0.85, cy)),
        // system gestures from the edges
        "home" => ((cx, s.1 - 2.0), (cx, s.1 * 0.55)),
        "notifications" => ((s.0 * 0.25, 2.0), (s.0 * 0.25, s.1 * 0.6)),
        "controlcenter" => ((s.0 * 0.9, 2.0), (s.0 * 0.9, s.1 * 0.6)),
        "back" => ((2.0, cy), (s.0 * 0.7, cy)),
        o => bail!("direction {o:?}: up down left right, or home notifications controlcenter back"),
    };
    swipe(inst, from, to, 250, after)
}

/// Brings an element into view: scrolls its scroll view in the app (elements the app has loaded but are off
/// screen), else scrolls `direction` step by step until it shows or the content stops moving.
pub fn scroll_to(inst: &Instance, target: &str, direction: &str, max_steps: u32, after: After) -> Result<Outcome> {
    let before = last(inst);
    let t = parse_target(target);
    for step in 0..=max_steps {
        let snap = snapshot(inst, SnapOpts::default())?;
        if let Some(e) = find_all_q(&snap, &t, 2).into_iter().find(|e| !e.offscreen && !e.covered) {
            let did = format!("@{} {} {} is on screen after {step} scroll(s)", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default());
            return Ok(outcome(inst, did, before, after));
        }
        let all = snapshot(inst, SnapOpts { all: true, ..Default::default() })?;
        if let Some(e) = find_all_q(&all, &t, 2).into_iter().find(|e| e.offscreen && !e.web) {
            if scroll_into_view(inst, e).is_ok() {
                let s2 = snapshot(inst, SnapOpts::default())?;
                if find_all_q(&s2, &t, 2).into_iter().any(|e| !e.offscreen) {
                    let e = find_all_q(&s2, &t, 2)[0].clone();
                    return Ok(outcome(inst, format!("@{} {} {} scrolled into view", e.r, e.role, e.label.as_deref().map(quote).unwrap_or_default()), before, after));
                }
            }
        }
        if step == max_steps {
            break;
        }
        let seq = Ring::new(&screen_ring(inst)).seq();
        scroll(inst, direction, 0.8, None, After::None)?;
        settle(inst, Duration::from_millis(250), Duration::from_millis(2500));
        if Ring::new(&screen_ring(inst)).seq() == seq {
            bail!("{target:?} not found: the content stopped scrolling {direction}");
        }
    }
    bail!("{target:?} not found after {max_steps} scrolls {direction}")
}

/// Waits until a target is on screen (or, `gone`, no longer is). Wakes on new frames, not on a timer.
pub fn wait_for(inst: &Instance, target: &str, gone: bool, timeout: Duration) -> Result<Outcome> {
    let t0 = Instant::now();
    let t = parse_target(target);
    let mut ring = Ring::new(&screen_ring(inst));
    loop {
        let seq = ring.seq();
        let snap = snapshot(inst, SnapOpts::default());
        if let Ok(snap) = &snap {
            let hit = find_all_q(snap, &t, 2).into_iter().find(|e| !e.offscreen && !e.covered).cloned();
            if hit.is_some() != gone {
                let ms = t0.elapsed().as_millis();
                let did = match hit {
                    Some(e) => format!("{target:?} appeared after {ms} ms: {}", e.line(snap.screen)),
                    None => format!("{target:?} gone after {ms} ms"),
                };
                return Ok(Outcome { did, screen: Some(snap.clone()), diff: None });
            }
        }
        let left = timeout.saturating_sub(t0.elapsed());
        if left.is_zero() {
            let st = snap.map(|s| s.text()).unwrap_or_else(|e| format!("{e:#}"));
            bail!("timed out after {:.1} s waiting for {target:?}{}\n{st}", timeout.as_secs_f64(), if gone { " to go" } else { "" });
        }
        // the next frame (or a second: a change in a process that draws nothing new is rare but possible)
        ring.wait(seq, left.min(Duration::from_secs(1)));
        std::thread::sleep(Duration::from_millis(60).min(left));
    }
}

/// A hardware button (home lock side volup voldown siri action ...), held `ms`
pub fn press(inst: &Instance, button: &str, ms: u64, after: After) -> Result<Outcome> {
    let before = last(inst);
    Hid::new(&inst.sock).press_for(button, ms);
    Ok(outcome(inst, format!("pressed {button}"), before, after))
}

/// Opens a URL as `simctl openurl` (host/openurl's queue file in the device)
pub fn open_url(inst: &Instance, url: &str, after: After) -> Result<Outcome> {
    let before = last(inst);
    let q = inst.dir.join("data/tmp/x86sim-openurl");
    let staging = q.with_extension(format!("new.{}", std::process::id()));
    std::fs::write(&staging, format!("{url}\n"))?;
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
    if after != After::None {
        // web pages open in Safari; other schemes in whichever app claims them (the front changes, or not at all)
        let l = url.to_lowercase();
        // (no Safari on an Apple TV or a watch: there a web link opens what claims it, if anything)
        if (l.starts_with("http:") || l.starts_with("https:")) && !matches!(inst.platform.as_str(), "tvos" | "watchos") {
            wait_front_is(inst, "com.apple.mobilesafari", Duration::from_secs(20));
        } else {
            wait_front_change(inst, before.as_ref().map(|b| b.front.clone()).unwrap_or_default(), Duration::from_secs(5));
        }
    }
    Ok(outcome(inst, format!("opened {url}"), before, after))
}

pub fn launch(inst: &Instance, bundle: &str, args: &[String], after: After) -> Result<Outcome> {
    let before = last(inst);
    let a = crate::apps::launch(inst, bundle, args)?;
    if after != After::None {
        wait_front_is(inst, bundle, Duration::from_secs(20));
    }
    Ok(outcome(inst, format!("launched {a}"), before, after))
}

pub fn terminate(inst: &Instance, bundle: &str, after: After) -> Result<Outcome> {
    let before = last(inst);
    let a = crate::apps::terminate(inst, bundle)?;
    Ok(outcome(inst, format!("terminated {bundle}: {a}"), before, after))
}

// ---------------------------------------------------------------------------------------------------------------------
// Screenshots

/// The newest frame upright as PNG, scaled down by `div` (the device's scale gives a picture in points: the same
/// coordinates as the snapshot's)
pub fn screenshot(inst: &Instance, div: u32) -> Result<(Vec<u8>, u32, u32)> {
    let mut ring = Ring::new(&screen_ring(inst));
    let f = ring.latest().ok_or_else(|| anyhow!("{}: no frame (is the device running?)", inst.name))?;
    // upright as the front app's interface stands (its points are the snapshot's), on the panel in use
    let rot = (panel(inst).0.map(|p| p.1).unwrap_or(0) + front_space(inst).rot) % 360;
    let div = div.max(1) as usize;
    let (w, h, s) = (f.w as usize, f.h as usize, f.stride as usize);
    let (sw, sh) = (w / div, h / div);
    // box filter
    let mut small = vec![0u8; sw * sh * 3];
    for y in 0..sh {
        for x in 0..sw {
            let mut acc = [0u32; 3];
            for yy in 0..div {
                let row = (y * div + yy) * s;
                for xx in 0..div {
                    let o = row + (x * div + xx) * 4;
                    acc[0] += f.bgra[o + 2] as u32;
                    acc[1] += f.bgra[o + 1] as u32;
                    acc[2] += f.bgra[o] as u32;
                }
            }
            let n = (div * div) as u32;
            let o = (y * sw + x) * 3;
            small[o] = (acc[0] / n) as u8;
            small[o + 1] = (acc[1] / n) as u8;
            small[o + 2] = (acc[2] / n) as u8;
        }
    }
    // turn upright
    let (ow, oh) = if rot % 180 == 90 { (sh, sw) } else { (sw, sh) };
    let mut rgb = Vec::with_capacity(ow * oh * 3);
    for yo in 0..oh {
        for xo in 0..ow {
            let (x, y) = match rot % 360 {
                270 => (yo, sh - 1 - xo),
                90 => (sw - 1 - yo, xo),
                180 => (sw - 1 - xo, sh - 1 - yo),
                _ => (xo, yo),
            };
            let o = (y * sw + x) * 3;
            rgb.extend_from_slice(&small[o..o + 3]);
        }
    }
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, ow as u32, oh as u32);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()?.write_image_data(&rgb)?;
    }
    Ok((out, ow as u32, oh as u32))
}

pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(r: &str, pid: u32, oid: &str, role: &str, label: &str, y: f64) -> El {
        El { r: r.into(), pid, oid: oid.into(), role: role.into(), label: Some(label.into()), app: "a".into(), layer: "app".into(), frame: [0.0, y, 100.0, 40.0], enabled: true, ..Default::default() }
    }

    #[test]
    fn refs_stay_with_their_element() {
        let prev = vec![el("e1", 1, "10", "button", "General", 100.0), el("e2", 1, "20", "button", "Privacy", 150.0)];
        // the same objects, one relabelled cell (a different element now), a recreated element with the same label
        let mut cur = vec![el("", 1, "10", "button", "General", 90.0), el("", 1, "30", "button", "Privacy", 140.0), el("", 1, "40", "text", "New", 10.0)];
        let mut next = 3;
        assign_refs(&mut cur, &prev, &mut next);
        assert_eq!(cur[0].r, "e1");
        assert_eq!(cur[1].r, "e2");
        assert_eq!(cur[2].r, "e3");
        assert_eq!(next, 4);
        // a reused object with other content gets a new ref
        let mut cur = vec![el("", 1, "10", "button", "Sounds", 100.0)];
        assign_refs(&mut cur, &prev, &mut next);
        assert_eq!(cur[0].r, "e4");
    }

    #[test]
    fn targets() {
        assert!(matches!(parse_target("@e3"), Target::Ref(r) if r == "e3"));
        assert!(matches!(parse_target("e12"), Target::Ref(r) if r == "e12"));
        assert!(matches!(parse_target("id:com.x"), Target::Id(i) if i == "com.x"));
        assert!(matches!(parse_target("201 320"), Target::Point(x, y) if x == 201.0 && y == 320.0));
        assert!(matches!(parse_target("201,320"), Target::Point(..)));
        assert!(matches!(parse_target("General"), Target::Text(_)));
        assert!(matches!(parse_target("e mail"), Target::Text(_)));
        let snap = Snapshot {
            screen: (400.0, 800.0),
            elements: vec![
                El { layer: "app".into(), ..el("e1", 1, "1", "text", "General settings", 10.0) },
                El { layer: "app".into(), ..el("e2", 1, "2", "button", "General", 50.0) },
                El { layer: "system".into(), ..el("e3", 2, "3", "button", "Allow", 300.0) },
                El { layer: "app".into(), ..el("e4", 1, "4", "button", "Allow", 300.0) },
            ],
            ..Default::default()
        };
        assert_eq!(find(&snap, "general").unwrap().r, "e2");
        assert_eq!(find(&snap, "Allow").unwrap().r, "e3");
        assert_eq!(find(&snap, "@e4").unwrap().r, "e4");
        let nb = Snapshot { elements: vec![El { layer: "app".into(), ..el("e5", 1, "5", "button", "Apple\u{a0}TV Languages", 10.0) }], ..Default::default() };
        assert_eq!(find(&nb, "apple tv languages").unwrap().r, "e5");
        assert!(find(&snap, "nothing").is_err());
    }

    fn keyed(r: &str, oid: &str, role: &str, label: &str, key: &str, y: f64, web: bool) -> El {
        El { key: Some(key.into()), web, ..el(r, 1, oid, role, label, y) }
    }

    #[test]
    fn repeated_web_text_keeps_refs_by_dom_identity() {
        // Hacker News: every row has "by" and "|"; scrolling moves rows up, a new row comes in at the bottom
        let prev = vec![
            keyed("e1", "w#dom:a.1", "text", "by", "dom:a.1", 100.0, true),
            keyed("e2", "w#dom:a.2", "text", "|", "dom:a.2", 100.0, true),
            keyed("e3", "w#dom:a.3", "text", "by", "dom:a.3", 200.0, true),
            keyed("e4", "w#dom:a.4", "text", "|", "dom:a.4", 200.0, true),
        ];
        // scrolled by 100: row 1 is gone, row 2 moved to where row 1 was, row 3 (new) where row 2 was
        let mut cur = vec![
            keyed("", "w#dom:a.3", "text", "by", "dom:a.3", 0.0, true),
            keyed("", "w#dom:a.4", "text", "|", "dom:a.4", 0.0, true),
            keyed("", "w#dom:a.5", "text", "by", "dom:a.5", 100.0, true),
            keyed("", "w#dom:a.6", "text", "|", "dom:a.6", 100.0, true),
        ];
        let mut next = 5;
        assign_refs(&mut cur, &prev, &mut next);
        let r: Vec<&str> = cur.iter().map(|e| e.r.as_str()).collect();
        // the old position-based fallback gave the new row e1 / e2 (same label at the same place)
        assert_eq!(r, ["e3", "e4", "e5", "e6"]);
        // a DOM node keeps its ref when its text changes (a counter, a field's value)
        let mut cur2 = vec![keyed("", "w#dom:a.3", "text", "by someone", "dom:a.3", 0.0, true)];
        assign_refs(&mut cur2, &cur, &mut next);
        assert_eq!(cur2[0].r, "e3");
        // a node the page re-created with a label found once on both sides still finds its ref
        let prev = vec![keyed("e9", "w#dom:a.9", "link", "More", "dom:a.9", 700.0, true)];
        let mut cur3 = vec![keyed("", "w#dom:b.1", "link", "More", "dom:b.1", 690.0, true)];
        assign_refs(&mut cur3, &prev, &mut next);
        assert_eq!(cur3[0].r, "e9");
    }

    #[test]
    fn recycled_cells_keep_refs_by_index_path() {
        // two rows with a "Details" button each; after a scroll the cells (objects) are recycled: the object that showed
        // row 3 now shows row 5, and row 4's button sits in the object row 3 had
        let prev = vec![
            keyed("e1", "c1", "button", "Details", "t:0.3/button0", 100.0, false),
            keyed("e2", "c2", "button", "Details", "t:0.4/button0", 150.0, false),
        ];
        let mut cur = vec![
            keyed("", "c1", "button", "Details", "t:0.4/button0", 100.0, false),
            keyed("", "c2", "button", "Details", "t:0.5/button0", 150.0, false),
        ];
        let mut next = 3;
        assign_refs(&mut cur, &prev, &mut next);
        assert_eq!(cur[0].r, "e2", "row 4 keeps its ref in another cell object");
        assert_eq!(cur[1].r, "e3", "row 5 is new, even in the object (and at the place) row 4 had");
        // rows shifted by an insertion: the index path now shows another row (other label): not the same element
        let prev = vec![keyed("e7", "c1", "text", "Alice", "t:0.0/text0", 0.0, false), keyed("e8", "c2", "text", "Bob", "t:0.1/text0", 40.0, false)];
        let mut cur = vec![
            keyed("", "c3", "text", "Zed", "t:0.0/text0", 0.0, false),
            keyed("", "c1", "text", "Alice", "t:0.1/text0", 40.0, false),
            keyed("", "c2", "text", "Bob", "t:0.2/text0", 80.0, false),
        ];
        let mut next = 9;
        assign_refs(&mut cur, &prev, &mut next);
        let r: Vec<&str> = cur.iter().map(|e| e.r.as_str()).collect();
        assert_eq!(r, ["e9", "e7", "e8"]);
    }

    #[test]
    fn spaces_turn_with_the_interface() {
        assert_eq!(interface_rot(1), 0);
        assert_eq!(interface_rot(3), 90, "simhid's landscapeLeft: Safari's interface is UIKit's landscape left");
        assert_eq!(interface_rot(4), 270);
        assert_eq!(interface_rot(2), 180);
        let portrait = Space { size: (402.0, 874.0), rot: 0 };
        let land = portrait.turned(90);
        assert_eq!(land.size, (874.0, 402.0));
        assert_eq!(land.turned(270).size, (874.0, 402.0), "the other landscape");
        assert_eq!(land.turned(0), portrait);
        // a portrait-only app on a phone held landscape: its points are portrait ones and go to the panel as they are
        let mut h = hid_for_test(portrait);
        assert_eq!(h.main_rot, 0);
        let (x, y) = portrait.norm(100.5, 437.0);
        assert!((x - 0.25).abs() < 1e-3 && (y - 0.5).abs() < 1e-3);
        // Safari turned with the phone (landscape left): the top left of its picture is the panel's top right
        h = hid_for_test(land);
        assert_eq!(h.main_rot, 90);
        assert_eq!(hid::unrotate(0.0, 0.0, h.main_rot), (1.0, 0.0));
        // an element of a window turned another way than the front app (SpringBoard's) is in its own space
        let snap = Snapshot { screen: (874.0, 402.0), rot: 90, ..Default::default() };
        let e = El { rot: 0, ..Default::default() };
        assert_eq!(snap.space_of(&e).size, (402.0, 874.0));
    }

    #[test]
    fn geometry_from_the_fixed_space() {
        assert_eq!(fixed_rot((1.0, 0.0)), 0);
        assert_eq!(fixed_rot((0.0, 1.0)), 90);
        assert_eq!(fixed_rot((-1.0, 0.0)), 180);
        assert_eq!(fixed_rot((0.0, -1.0)), 270);
        // iPhone Duo unfolded: simfb's display 3, a 2007x2853 framebuffer mounted at 270
        let g = Geometry { display: Some("x86Sim-display-3".into()), native: 270, fb: (669.0, 951.0) };
        // SpringBoard there: a wide upright space, fixed = the same (no turn): Hid adds the mount turn
        let w = json!({"display": "x86Sim-display-3", "screen": [951, 669], "fixed": [1, 0], "fixed_size": [951, 669]});
        assert_eq!(g.space(&w), Some(Space { size: (951.0, 669.0), rot: 0 }));
        // a fixed space in the framebuffer's orientation: the turn to it already includes the mount
        let w = json!({"display": "x86Sim-display-3", "screen": [951, 669], "fixed": [0, -1], "fixed_size": [669, 951]});
        assert_eq!(g.space(&w).map(|s| s.rot), Some(0));
        // a window on the folded-away outer screen is not on screen
        let w = json!({"display": "x86Sim-main-display", "screen": [466, 678], "fixed": [1, 0], "fixed_size": [466, 678]});
        assert_eq!(g.space(&w), None);
        // an older reader: the interface orientation
        let one = Geometry { display: None, native: 0, fb: (402.0, 874.0) };
        assert_eq!(one.space(&json!({"screen": [874, 402], "orient": 3})).map(|s| s.rot), Some(90));
    }

    fn hid_for_test(sp: Space) -> Hid {
        let mut h = Hid::new(Path::new("/nonexistent"));
        h.main_rot = sp.rot % 360;
        h
    }

    #[test]
    fn ipad_windows() {
        // iPadOS windowed apps: Safari's window in front of Settings'; SpringBoard's chrome is the window layer
        let sb = |r: &str, id: &str| El { app: SPRINGBOARD.into(), window: "SBMainSwitcherWindow".into(), layer: "system".into(), id: Some(id.into()), ..el(r, 9, r, "element", "", 0.0) };
        let mut snap = Snapshot {
            screen: (820.0, 1180.0),
            elements: vec![
                sb("e1", "card:com.apple.Preferences:sceneID:x"),
                sb("e2", "window-controls:com.apple.mobilesafari"),
                El { app: "com.apple.Preferences".into(), frame: [17.0, 560.0, 358.0, 40.0], ..el("e3", 1, "3", "button", "General", 0.0) },
                El { app: "com.apple.Preferences".into(), frame: [250.0, 600.0, 100.0, 40.0], ..el("e4", 1, "4", "button", "Hidden", 0.0) },
            ],
            ..Default::default()
        };
        let z = windows_z(&mut snap);
        assert_eq!(z["com.apple.mobilesafari"], 10_000);
        assert_eq!(z["com.apple.Preferences"], 0);
        assert!(snap.elements[..2].iter().all(|e| e.layer == "window") && !snap.overlay);
        let rects = HashMap::from([("com.apple.mobilesafari".to_string(), [222.0, 50.0, 375.0, 965.0]), ("com.apple.Preferences".to_string(), [0.0, 290.0, 375.0, 486.0])]);
        cover(&mut snap.elements, &rects, &z);
        assert_eq!(snap.elements[2].frame, [17.0, 560.0, 205.0, 40.0]);
        assert!(!snap.elements[2].covered && snap.elements[3].covered);
    }

    #[test]
    fn b64() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi!"), "aGkh");
        assert_eq!(base64(b"h"), "aA==");
    }

    #[test]
    fn keys() {
        assert_eq!(key_usage('a'), Some((4, false)));
        assert_eq!(key_usage('Z'), Some((29, true)));
        assert_eq!(key_usage('@'), Some((0x1f, true)));
        assert_eq!(key_named("return"), Some(0x28));
        assert_eq!(key_named("0x2a"), Some(0x2a));
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Keyboard: Apple Simulator's I/O > Keyboard menu

/// The hardware keyboard's connection, as host/simhid remembers it (<data>/hwkeyboard; connected unless disconnected)
pub fn hw_keyboard_connected(inst: &Instance) -> bool {
    Hid::new(&inst.sock).hw_keyboard_connected()
}

/// host/simhid answers on the phone's input socket (the phone is up)
fn simhid_up(inst: &Instance) -> bool {
    let p = std::fs::canonicalize(&inst.sock).unwrap_or_else(|_| inst.sock.clone());
    std::os::unix::net::UnixStream::connect(p).is_ok()
}

/// The software keyboard is on screen: its keys are in the snapshot (role "key", UIKit's keyboard-key trait)
pub fn software_keyboard_up(s: &Snapshot) -> bool {
    s.elements.iter().filter(|e| e.role == "key" && !e.offscreen).count() >= 10
}

/// `keyboard software on|off|toggle` / `keyboard hardware connect|disconnect|toggle` / `keyboard status`, as Apple's
/// Toggle Software Keyboard (Cmd-K) and Connect Hardware Keyboard (Shift-Cmd-K)
pub fn keyboard(inst: &Instance, what: &str, how: &str, after: After) -> Result<Outcome> {
    let before = last(inst);
    let hw = hw_keyboard_connected(inst);
    let did = match (what, how) {
        ("status", _) | ("", _) => {
            let up = snapshot(inst, SnapOpts::default()).map(|s| software_keyboard_up(&s)).unwrap_or(false);
            format!("hardware keyboard {}; software keyboard {}", if hw { "connected" } else { "disconnected" }, if up { "shown" } else { "hidden" })
        }
        ("hardware" | "hw", "connect" | "on" | "disconnect" | "off" | "toggle") => {
            let on = match how {
                "connect" | "on" => true,
                "disconnect" | "off" => false,
                _ => !hw,
            };
            if simhid_up(inst) {
                Hid::new(&inst.sock).hw_keyboard(on);
                std::thread::sleep(Duration::from_millis(300));
            } else {
                // simhid applies it at the next boot
                std::fs::write(inst.sock.with_file_name("hwkeyboard"), if on { "connected\n" } else { "disconnected\n" })?;
            }
            format!("hardware keyboard {}", if on { "connected" } else { "disconnected" })
        }
        ("software" | "sw", _) if matches!(inst.platform.as_str(), "tvos" | "watchos") => {
            // Apple's Toggle Software Keyboard is an iPhone / iPad (and Vision Pro) thing
            bail!(
                "no software keyboard to toggle on {}: {}",
                if inst.platform == "tvos" { "an Apple TV" } else { "an Apple Watch" },
                if inst.platform == "tvos" {
                    "tvOS shows its own full-screen keyboard when a text field is selected (type writes into it; tap its keys or Done by moving the focus)"
                } else {
                    "a text field opens watchOS's input sheet (keyboard, Scribble, dictation); type writes into the field directly, keys with type keys=true"
                }
            )
        }
        ("software" | "sw", "on" | "off" | "toggle" | "show" | "hide") => {
            anyhow::ensure!(simhid_up(inst), "{} is not running", inst.name);
            let want = match how {
                "on" | "show" => Some(true),
                "off" | "hide" => Some(false),
                _ => None,
            };
            let up = || snapshot(inst, SnapOpts::default()).map(|s| software_keyboard_up(&s)).unwrap_or(false);
            if want.is_some_and(|w| w == up()) {
                format!("software keyboard already {}", if want == Some(true) { "shown" } else { "hidden" })
            } else {
                Hid::new(&inst.sock).soft_keyboard_toggle();
                std::thread::sleep(Duration::from_millis(600));
                let now = up();
                let mut d = format!("toggled the software keyboard: {}", if now { "shown" } else { "hidden" });
                if want.is_some_and(|w| w != now) {
                    d += if want == Some(true) {
                        " (it shows while a text field is focused: tap one first)"
                    } else {
                        " (still up: with the hardware keyboard disconnected it stays up while a field is focused)"
                    };
                }
                d
            }
        }
        _ => bail!("keyboard software on|off|toggle | hardware connect|disconnect|toggle | status"),
    };
    Ok(outcome(inst, did, before, after))
}
