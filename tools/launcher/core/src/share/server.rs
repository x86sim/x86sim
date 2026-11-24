//! Share: the HTTP + WebSocket server behind `x86sim serve`. A thread a connection (a handful of viewers), no TLS of its
//! own: remote access goes through tailscale / cloudflared (tunnel.rs), which bring HTTPS.
//!
//! Access: a token (control: watch and drive, start / stop phones; view: watch only), optionally limited to one phone
//! and expiring, from `?t=`, a cookie set from it, or `Authorization: Bearer`. Links (share_link, the page's Share
//! dialog) get their own tokens and can do only what their scope allows: the server-wide endpoints (status, the main
//! tokens' URLs, tunnels, making / listing / revoking links) are the owner's alone: the main control token, or a local
//! browser of the user who runs the server.
//! Without a token only that owner's local browser gets in: a loopback peer whose socket belongs to our own uid (looked
//! up in /proc/net/tcp: another user on the machine is not the owner), a loopback Host, no proxy headers, a same-origin
//! Origin. A tunnel's requests arrive from loopback too but carry forwarding headers and their own Host (tailscaled's
//! also another uid), and a web page elsewhere can neither name a loopback Host through DNS rebinding nor pass the
//! Origin check on the WebSocket.
//! Limits before any token is checked: request head 32 KiB / 100 headers, 20 s to send it, 128 connections at a time
//! (24 from one address other than loopback).
use super::audio;
use super::tunnel::{self, Kind, Tunnel};
use super::video::{self, Codec, Out, StreamKey};
use super::ws;
use super::{now_s, random_token, Options, Role, State, TokenRec};
use crate::hid::{self, Hid};
use crate::instance::{self, Instance};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const INDEX: &str = include_str!("web/index.html");
const APP_JS: &str = include_str!("web/app.js");
const APP_CSS: &str = include_str!("web/app.css");
// the Accessibility Inspector's panel (axapi.rs)
const AX_JS: &str = include_str!("web/axinspect.js");
const AX_CSS: &str = include_str!("web/axinspect.css");
const MIC_JS: &str = include_str!("web/mic.js");

pub struct Server {
    pub opts: Options,
    pub port: u16,
    tokens: Mutex<Vec<TokenRec>>,
    tunnels: Mutex<Vec<Tunnel>>,
    phones_cache: Mutex<(Option<Instant>, Value)>,
    thumbs: Mutex<HashMap<(String, u32), (Instant, Vec<u8>)>>,
    started: u64,
    pub stop: AtomicBool,
}

pub struct Req {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub peer: SocketAddr,
    /// our end of the connection (to find the peer's socket in /proc/net/tcp)
    pub local: SocketAddr,
}

#[derive(Clone, Debug)]
pub struct Grant {
    pub role: Role,
    pub phone: Option<String>,
    pub token: Option<String>,
    pub expires: Option<u64>,
    /// the server's owner (the main control token, or the user's own local browser): tunnels, links, status
    pub owner: bool,
}

/// Request head limits (before any token is checked)
const MAX_HEAD: u64 = 32 * 1024;
const MAX_HEADERS: usize = 100;
const HEAD_DEADLINE: Duration = Duration::from_secs(20);
/// Connections at a time: in all, and from one address other than loopback (a tunnel's all come from loopback)
const MAX_CONNS: usize = 128;
const MAX_CONNS_PEER: usize = 24;

/// A reader that gives up at a deadline (a socket read timeout alone is per read: a byte every 14 s kept a thread)
struct Deadline<'a> {
    s: &'a TcpStream,
    end: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "request too slow"));
        }
        self.s.set_read_timeout(Some(left))?;
        let mut s = self.s;
        s.read(buf)
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && hex(b[i + 1]).is_some() && hex(b[i + 2]).is_some() => {
                out.push(hex(b[i + 1]).unwrap() * 16 + hex(b[i + 2]).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn pct_encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn read_req(s: &mut TcpStream, peer: SocketAddr) -> Option<Req> {
    let local = s.local_addr().ok()?;
    let r = read_head(Deadline { s, end: Instant::now() + HEAD_DEADLINE }, peer, local);
    // what follows (a WebSocket session) sets its own timeouts; a plain request is answered at once
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    r
}

/// Parses a request head and body from `src`: at most MAX_HEAD bytes of head, MAX_HEADERS headers, a 256 KiB body
fn read_head(src: impl Read, peer: SocketAddr, local: SocketAddr) -> Option<Req> {
    let mut r = BufReader::new(src.take(MAX_HEAD));
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    if !line.ends_with('\n') {
        return None;
    }
    let mut it = line.split_whitespace();
    let method = it.next()?.to_string();
    let target = it.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let mut l = String::new();
        let n = r.read_line(&mut l).ok()?;
        // n == 0: the end, or the head limit reached; a line without its newline: cut off by the limit
        if n == 0 || !l.ends_with('\n') || headers.len() >= MAX_HEADERS {
            return None;
        }
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > 256 * 1024 {
        return None;
    }
    // the body is read past the head's limit (bytes the BufReader holds already, then the rest)
    let mut body = Vec::with_capacity(len);
    let have = r.buffer().len().min(len);
    body.extend_from_slice(&r.buffer()[..have]);
    r.consume(have);
    let mut rest = r.into_inner().into_inner();
    let mut tail = vec![0u8; len - have];
    rest.read_exact(&mut tail).ok()?;
    body.extend(tail);
    // the BufReader may hold bytes past the request only for a pipelining client; we answer one request a connection
    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    let query = q.split('&').filter(|p| !p.is_empty()).map(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        (pct_decode(k), pct_decode(v))
    });
    Some(Req { method, path: pct_decode(path), query: query.collect(), headers, body, peer, local })
}

fn respond(s: &mut TcpStream, code: u16, ctype: &str, extra: &[(&str, String)], body: &[u8]) {
    let reason = match code {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let mut h = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n",
        body.len()
    );
    for (k, v) in extra {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(body);
}

fn json_resp(s: &mut TcpStream, code: u16, v: &Value) {
    respond(s, code, "application/json", &[], v.to_string().as_bytes());
}

fn host_only(h: &str) -> &str {
    // "[::1]:7380" -> "[::1]", "localhost:7380" -> "localhost"
    if h.starts_with('[') {
        return h.split_inclusive(']').next().unwrap_or(h);
    }
    h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h)
}

fn is_loopback_name(h: &str) -> bool {
    let h = host_only(h).to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "[::1]" || h.ends_with(".localhost")
}

/// The uid owning the socket at `peer` connected to `local` (Linux: /proc/net/tcp and tcp6, both ends on this machine)
pub fn peer_uid(peer: SocketAddr, local: SocketAddr) -> Option<u32> {
    fn norm(a: SocketAddr) -> SocketAddr {
        match a {
            SocketAddr::V6(v) => match v.ip().to_ipv4_mapped() {
                Some(v4) => SocketAddr::new(v4.into(), v.port()),
                None => a,
            },
            a => a,
        }
    }
    fn parse(s: &str) -> Option<SocketAddr> {
        let (ip, port) = s.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let ip: std::net::IpAddr = match ip.len() {
            8 => std::net::Ipv4Addr::from(u32::from_str_radix(ip, 16).ok()?.to_ne_bytes()).into(),
            32 => {
                let mut b = [0u8; 16];
                for i in 0..4 {
                    b[i * 4..i * 4 + 4].copy_from_slice(&u32::from_str_radix(&ip[i * 8..i * 8 + 8], 16).ok()?.to_ne_bytes());
                }
                std::net::Ipv6Addr::from(b).into()
            }
            _ => return None,
        };
        Some(norm(SocketAddr::new(ip, port)))
    }
    let (peer, local) = (norm(peer), norm(local));
    for f in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(t) = std::fs::read_to_string(f) else { continue };
        for l in t.lines().skip(1) {
            let c: Vec<&str> = l.split_whitespace().collect();
            if c.len() > 7 && parse(c[1]) == Some(peer) && parse(c[2]) == Some(local) {
                return c[7].parse().ok();
            }
        }
    }
    None
}

/// A plain local browser (see the module doc); who runs it is checked apart (peer_uid)
pub fn local_trusted(req: &Req) -> bool {
    if !req.peer.ip().is_loopback() {
        return false;
    }
    const PROXY: [&str; 7] = ["x-forwarded-for", "forwarded", "x-real-ip", "cf-connecting-ip", "cf-ray", "tailscale-user-login", "x-forwarded-host"];
    if PROXY.iter().any(|h| req.headers.contains_key(*h)) {
        return false;
    }
    let host = req.headers.get("host").map(String::as_str).unwrap_or("");
    if !is_loopback_name(host) {
        return false;
    }
    match req.headers.get("origin") {
        None => true,
        Some(o) => o.strip_prefix("http://").or(o.strip_prefix("https://")).is_some_and(|oh| oh.eq_ignore_ascii_case(host)),
    }
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn cookie(req: &Req, name: &str) -> Option<String> {
    req.headers.get("cookie")?.split(';').find_map(|c| c.trim().strip_prefix(&format!("{name}=")).map(String::from))
}

impl Server {
    pub fn new(opts: Options, port: u16) -> Arc<Server> {
        let exp = opts.ttl_s.map(|t| now_s().saturating_add(t));
        let tokens = vec![
            TokenRec { token: random_token(), role: Role::Control, phone: None, expires: exp, label: "control".into() },
            TokenRec { token: random_token(), role: Role::View, phone: None, expires: exp, label: "view".into() },
        ];
        Arc::new(Server {
            opts,
            port,
            tokens: Mutex::new(tokens),
            tunnels: Mutex::new(vec![]),
            phones_cache: Mutex::new((None, Value::Null)),
            thumbs: Mutex::new(HashMap::new()),
            started: now_s(),
            stop: AtomicBool::new(false),
        })
    }

    pub fn grant(&self, req: &Req) -> Option<Grant> {
        let given = req.query.get("t").cloned().or_else(|| req.headers.get("authorization").and_then(|a| a.strip_prefix("Bearer ").map(|s| s.trim().to_string())));
        // a cookie left by an earlier server is no reason to turn a local browser away
        let from_cookie = given.is_none();
        if let Some(t) = given.or_else(|| cookie(req, "x86sim_t")) {
            let now = now_s();
            let mut toks = self.tokens.lock().unwrap();
            toks.retain(|r| r.expires.is_none_or(|e| e > now) || r.label == "control" || r.label == "view");
            match toks.iter().find(|r| ct_eq(&r.token, &t)) {
                Some(r) if r.expires.is_none_or(|e| e > now) => return Some(Grant { role: r.role, phone: r.phone.clone(), token: Some(t), expires: r.expires, owner: r.label == "control" }),
                _ if !from_cookie => return None,
                _ => {}
            }
        }
        // only the user running the server: another user's process on this machine is refused
        if !self.opts.require_token && local_trusted(req) && peer_uid(req.peer, req.local) == Some(unsafe { libc::getuid() }) {
            return Some(Grant { role: Role::Control, phone: None, token: None, expires: None, owner: true });
        }
        None
    }

    /// The grant's token still works (not revoked, not expired): an open WebSocket ends when it doesn't
    pub fn still_valid(&self, g: &Grant) -> bool {
        if g.expires.is_some_and(|e| e <= now_s()) {
            return false;
        }
        match &g.token {
            None => true,
            Some(t) => self.tokens.lock().unwrap().iter().any(|r| ct_eq(&r.token, t) && r.expires.is_none_or(|e| e > now_s())),
        }
    }

    pub fn state(&self) -> State {
        let toks = self.tokens.lock().unwrap();
        let tok = |l: &str| toks.iter().find(|r| r.label == l).map(|r| r.token.clone()).unwrap_or_default();
        State {
            pid: std::process::id(),
            port: self.port,
            listen: self.opts.listen.clone(),
            started: self.started,
            local_url: format!("http://127.0.0.1:{}/", self.port),
            control_token: tok("control"),
            view_token: tok("view"),
            expires: toks.iter().find(|r| r.label == "control").and_then(|r| r.expires),
            tunnels: self.tunnels.lock().unwrap().iter().map(|t| t.info.lock().unwrap().clone()).collect(),
            require_token: self.opts.require_token,
            links: toks.iter().filter(|r| r.label != "control" && r.label != "view").count(),
        }
    }

    pub fn write_state(&self) {
        if self.port == 0 {
            return;
        }
        let p = super::state_path();
        let tmp = p.with_extension("tmp");
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode_0600().open(&tmp) {
            // a tmp file left over keeps its old mode: 0600 in any case
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            let _ = f.write_all(serde_json::to_string_pretty(&self.state()).unwrap_or_default().as_bytes());
            let _ = std::fs::rename(&tmp, &p);
        }
    }

    pub fn add_tunnel(self: &Arc<Self>, kind: Kind) -> Value {
        {
            let mut ts = self.tunnels.lock().unwrap();
            // tailscale serve and funnel share the port mapping: one of them at a time
            let same = |k: &Kind| *k == kind || (matches!(kind, Kind::Tailscale | Kind::Funnel) && matches!(k, Kind::Tailscale | Kind::Funnel));
            ts.retain_mut(|t| {
                let i = t.info.lock().unwrap().clone();
                if same(&i.kind) && (i.state == "failed" || i.kind != kind) {
                    t.stop();
                    return false;
                }
                true
            });
            if let Some(t) = ts.iter().find(|t| t.info.lock().unwrap().kind == kind) {
                return serde_json::to_value(t.info.lock().unwrap().clone()).unwrap_or_default();
            }
        }
        let named = if kind == Kind::Cloudflared { self.opts.cf_named.clone() } else { None };
        let t = Tunnel::start(kind, self.port, self.opts.ts_port, named);
        t.wait(Duration::from_secs(25));
        let info = t.info.lock().unwrap().clone();
        self.tunnels.lock().unwrap().push(t);
        self.write_state();
        // cloudflared may print its URL after we stop waiting: keep the state file current
        let me = self.clone();
        std::thread::spawn(move || {
            for _ in 0..60 {
                std::thread::sleep(Duration::from_secs(1));
                me.write_state();
            }
        });
        serde_json::to_value(info).unwrap_or_default()
    }

    pub fn remove_tunnel(&self, kind: &Kind) {
        self.tunnels.lock().unwrap().retain_mut(|t| {
            if t.info.lock().unwrap().kind == *kind {
                t.stop();
                false
            } else {
                true
            }
        });
        self.write_state();
    }

    pub fn shutdown(&self) {
        for mut t in self.tunnels.lock().unwrap().drain(..) {
            t.stop();
        }
        let p = super::state_path();
        if super::read_state().is_some_and(|s| s.pid == std::process::id()) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Every phone for the page: the hub's summary plus what the page needs to draw controls
    fn phones(&self) -> Value {
        let mut c = self.phones_cache.lock().unwrap();
        if c.0.is_some_and(|t| t.elapsed() < Duration::from_millis(1500)) {
            return c.1.clone();
        }
        let insts = instance::list();
        let sums = instance::summaries(0);
        let v: Vec<Value> = insts
            .iter()
            .map(|i| {
                let s = sums.iter().find(|s| s.id == i.id);
                let mut o = 0;
                let t = video::target(i, &mut o);
                json!({
                    "id": i.id, "name": i.name, "model": i.model, "os": i.ios, "platform": i.platform,
                    "installed": i.runtime_present,
                    "state": s.map(|s| s.state.clone()).unwrap_or_else(|| "off".into()),
                    "home": s.is_some_and(|s| s.home),
                    "memory_mb": s.map(|s| s.memory_mb), "cpu_pct": s.map(|s| s.cpu_pct), "uptime_s": s.and_then(|s| s.uptime_s),
                    "screen": [t.size.0, t.size.1], "two_screens": i.inner.is_some(), "inner_active": i.inner_active(),
                    "orientation": i.orientation(), "hinge": i.hinge(),
                    "ipad": i.model.contains("iPad"),
                })
            })
            .collect();
        *c = (Some(Instant::now()), Value::Array(v));
        c.1.clone()
    }

    fn visible(&self, g: &Grant, id: &str) -> bool {
        g.phone.as_deref().is_none_or(|p| p == id)
    }

    fn mint(&self, role: Role, phone: Option<String>, ttl_s: Option<u64>, label: &str) -> TokenRec {
        let r = TokenRec { token: random_token(), role, phone, expires: ttl_s.map(|t| now_s().saturating_add(t)), label: format!("link:{label}") };
        self.tokens.lock().unwrap().push(r.clone());
        self.write_state();
        r
    }

    fn urls_for(&self, token: &str, phone: Option<&str>) -> Vec<Value> {
        let st = self.state();
        super::urls(&st, token, phone).into_iter().map(|(kind, reach, url)| json!({"kind": kind, "reach": reach, "url": url})).collect()
    }
}

trait Mode0600 {
    fn mode_0600(&mut self) -> &mut Self;
}
impl Mode0600 for std::fs::OpenOptions {
    fn mode_0600(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(0o600)
    }
}

/// Open connections: in all and per address (MAX_CONNS, MAX_CONNS_PEER)
#[derive(Default)]
pub struct Conns {
    total: AtomicUsize,
    peers: Mutex<HashMap<std::net::IpAddr, usize>>,
}

/// One connection's slot, given back when dropped
pub struct Slot<'a>(&'a Conns, std::net::IpAddr);

impl Conns {
    pub fn enter(&self, ip: std::net::IpAddr) -> Option<Slot<'_>> {
        let mut p = self.peers.lock().unwrap();
        let n = p.get(&ip).copied().unwrap_or(0);
        if self.total.load(Ordering::Relaxed) >= MAX_CONNS || (!ip.is_loopback() && n >= MAX_CONNS_PEER) {
            return None;
        }
        self.total.fetch_add(1, Ordering::Relaxed);
        p.insert(ip, n + 1);
        Some(Slot(self, ip))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut p = self.0.peers.lock().unwrap();
        if let Some(n) = p.get_mut(&self.1) {
            *n -= 1;
            if *n == 0 {
                p.remove(&self.1);
            }
        }
        self.0.total.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn run(srv: Arc<Server>, listener: TcpListener) {
    let conns: &'static Conns = Box::leak(Box::default());
    for c in listener.incoming() {
        if srv.stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(s) = c else { continue };
        let peer = s.peer_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
        // over the limit: closed at once (dropped), no thread
        let Some(slot) = conns.enter(peer.ip()) else { continue };
        let srv = srv.clone();
        std::thread::spawn(move || {
            let _slot = slot;
            handle(&srv, s, peer);
        });
    }
}

fn handle(srv: &Arc<Server>, mut s: TcpStream, peer: SocketAddr) {
    let _ = s.set_nodelay(true);
    let Some(req) = read_req(&mut s, peer) else { return };
    let p = req.path.clone();
    // static files: the page itself carries no data, the token check comes with the API / socket
    match p.as_str() {
        "/" | "/index.html" => {
            let mut extra = vec![("Content-Security-Policy", "default-src 'self'; img-src 'self' blob: data:; media-src 'self' blob:; connect-src 'self' ws: wss:; style-src 'self'; script-src 'self'; frame-ancestors 'none'".to_string())];
            // the token moves into a cookie (SameSite=Strict, HttpOnly) so the page's later requests carry it
            if let (Some(t), Some(_)) = (req.query.get("t"), srv.grant(&req)) {
                let secure = req.headers.contains_key("x-forwarded-proto") || req.headers.contains_key("cf-ray") || req.headers.contains_key("tailscale-user-login");
                extra.push(("Set-Cookie", format!("x86sim_t={t}; Path=/; HttpOnly; SameSite=Strict{}", if secure { "; Secure" } else { "" })));
            }
            return respond(&mut s, 200, "text/html; charset=utf-8", &extra, INDEX.as_bytes());
        }
        "/app.js" => return respond(&mut s, 200, "text/javascript; charset=utf-8", &[], APP_JS.as_bytes()),
        "/app.css" => return respond(&mut s, 200, "text/css; charset=utf-8", &[], APP_CSS.as_bytes()),
        "/axinspect.js" => return respond(&mut s, 200, "text/javascript; charset=utf-8", &[], AX_JS.as_bytes()),
        "/axinspect.css" => return respond(&mut s, 200, "text/css; charset=utf-8", &[], AX_CSS.as_bytes()),
        "/mic.js" => return respond(&mut s, 200, "text/javascript; charset=utf-8", &[], MIC_JS.as_bytes()),
        "/favicon.ico" => return respond(&mut s, 204, "image/x-icon", &[], b""),
        _ => {}
    }
    let Some(g) = srv.grant(&req) else {
        return json_resp(&mut s, 401, &json!({"error": "a token is needed: open the link x86sim serve printed (…?t=…)"}));
    };
    // Web Inspector (inspect.rs): its pages, frontend files and sockets, control role only
    if super::inspect::wants(&p) {
        return super::inspect::handle(srv, s, req, g);
    }
    // a page elsewhere can't post here: state changes need our header (a cross-origin request with it is preflighted
    // and refused) or a WebSocket from our own origin
    if req.method == "POST" && req.headers.get("x-x86sim").is_none() {
        return json_resp(&mut s, 403, &json!({"error": "missing X-X86sim header"}));
    }
    let seg: Vec<&str> = p.trim_matches('/').split('/').collect();
    let control = g.role == Role::Control;
    // server-wide: tunnels, the main tokens, links. A link (however broad) never manages the server or other links
    let owner = control && g.owner;
    match (req.method.as_str(), seg.as_slice()) {
        ("GET", ["api", "me"]) => json_resp(&mut s, 200, &json!({"role": g.role, "phone": g.phone, "expires": g.expires, "local": g.token.is_none(), "owner": owner, "server": {"started": srv.started, "version": env!("CARGO_PKG_VERSION")}})),
        ("GET", ["api", "phones"]) => {
            let all = srv.phones();
            let v: Vec<Value> = all.as_array().into_iter().flatten().filter(|p| srv.visible(&g, p["id"].as_str().unwrap_or(""))).cloned().collect();
            json_resp(&mut s, 200, &Value::Array(v))
        }
        ("GET", ["api", "status"]) if owner => {
            let streams: Vec<Value> = video::streams().into_iter().map(|(k, st)| json!({"phone": k.phone, "codec": format!("{:?}", k.codec), "width": k.width, "quality": k.quality, "fps": k.fps, "stats": st})).collect();
            let mut st = serde_json::to_value(srv.state()).unwrap_or_default();
            st["control_token"] = Value::Null;
            st["view_token"] = Value::Null;
            json_resp(&mut s, 200, &json!({"server": st, "streams": streams, "available": tunnel::available()}))
        }
        ("GET", ["api", "phones", id, what]) if srv.visible(&g, id) => {
            let Some(inst) = instance::find(id) else { return json_resp(&mut s, 404, &json!({"error": "no such device"})) };
            match *what {
                "thumb.jpg" => {
                    let w: u32 = req.query.get("w").and_then(|v| v.parse().ok()).unwrap_or(240).clamp(64, 720);
                    let key = (inst.id.clone(), w);
                    let cached = srv.thumbs.lock().unwrap().get(&key).filter(|(t, _)| t.elapsed() < Duration::from_millis(900)).map(|(_, b)| b.clone());
                    let jpg = cached.or_else(|| {
                        let j = video::snapshot_jpeg(&inst, w, 70)?;
                        let mut t = srv.thumbs.lock().unwrap();
                        // stale ones go (a widths sweep otherwise kept hundreds of JPEGs a phone)
                        t.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(10));
                        t.insert(key, (Instant::now(), j.clone()));
                        Some(j)
                    });
                    match jpg {
                        Some(j) => respond(&mut s, 200, "image/jpeg", &[], &j),
                        None => respond(&mut s, 204, "image/jpeg", &[], b""),
                    }
                }
                "screenshot.png" => match video::snapshot_png(&inst) {
                    Some(png) => {
                        let name = format!("{}-{}.png", inst.name.replace(|c: char| !c.is_ascii_alphanumeric(), "-"), now_s());
                        respond(&mut s, 200, "image/png", &[("Content-Disposition", format!("attachment; filename=\"{name}\""))], &png)
                    }
                    None => json_resp(&mut s, 409, &json!({"error": "no frame: is the device running?"})),
                },
                // x86sim profile of the front app (or ?app=BUNDLEID) for ?secs (default 5), as a download
                "profile.json" if control => {
                    let secs = req.query.get("secs").and_then(|v| v.parse().ok()).unwrap_or(5.0f64);
                    let fmt = req.query.get("format").and_then(|f| crate::profile::Format::parse(f)).unwrap_or(crate::profile::Format::Speedscope);
                    match crate::profile::take(&inst, req.query.get("app").map(String::as_str), secs, fmt) {
                        Ok((name, body)) => respond(&mut s, 200, "application/json", &[("Content-Disposition", format!("attachment; filename=\"{name}\""))], &body),
                        Err(e) => json_resp(&mut s, 409, &json!({"error": format!("{e:#}")})),
                    }
                }
                _ => json_resp(&mut s, 404, &json!({"error": "not found"})),
            }
        }
        ("POST", ["api", "phones", id, act]) if control && srv.visible(&g, id) => {
            let Some(inst) = instance::find(id) else { return json_resp(&mut s, 404, &json!({"error": "no such device"})) };
            let r = match *act {
                "start" => crate::sim::start(&inst.dir).map(|pid| json!({"starting": true, "pid": pid})),
                "stop" => {
                    // a stop waits for the phone to go down: answer at once
                    let dir = inst.dir.clone();
                    std::thread::spawn(move || {
                        let _ = crate::sim::stop(&dir);
                    });
                    Ok(json!({"stopping": true}))
                }
                _ => Err(anyhow::anyhow!("unknown action")),
            };
            srv.phones_cache.lock().unwrap().0 = None;
            match r {
                Ok(v) => json_resp(&mut s, 200, &v),
                Err(e) => json_resp(&mut s, 400, &json!({"error": format!("{e:#}")})),
            }
        }
        ("GET", ["api", "links"]) if owner => {
            let toks = srv.tokens.lock().unwrap();
            let v: Vec<Value> = toks.iter().filter(|r| r.label.starts_with("link:")).map(|r| json!({"id": &r.token[..6], "role": r.role, "phone": r.phone, "expires": r.expires, "label": r.label})).collect();
            json_resp(&mut s, 200, &Value::Array(v))
        }
        ("POST", ["api", "links"]) if owner => {
            let b: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let role = if b["role"].as_str() == Some("control") { Role::Control } else { Role::View };
            // a phone named by its name or folder is stored as its id (what visible() compares with)
            let phone = match g.phone.clone().or_else(|| b["phone"].as_str().filter(|p| !p.is_empty()).map(String::from)) {
                Some(p) => match instance::find(&p) {
                    Some(i) => Some(i.id),
                    None => return json_resp(&mut s, 404, &json!({"error": "no such device"})),
                },
                None => None,
            };
            let ttl = b["ttl_s"].as_u64().filter(|t| *t > 0).or(Some(3600)).map(|t| t.min(30 * 86400));
            // a link never outlives the token that made it
            let ttl = match (ttl, g.expires) {
                (Some(t), Some(e)) => Some(t.min(e.saturating_sub(now_s()))),
                (t, _) => t,
            };
            let r = srv.mint(role, phone.clone(), ttl, b["label"].as_str().unwrap_or(""));
            json_resp(&mut s, 200, &json!({"role": r.role, "phone": r.phone, "expires": r.expires, "token": r.token, "urls": srv.urls_for(&r.token, phone.as_deref())}))
        }
        ("POST", ["api", "links", "revoke"]) if owner => {
            let b: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let id = b["id"].as_str().unwrap_or("");
            // a link's id (its token's first 6 characters, as GET /api/links lists it) or "all": "" matched every link
            if id != "all" && !(id.len() >= 6 && id.bytes().all(|c| c.is_ascii_hexdigit())) {
                return json_resp(&mut s, 400, &json!({"error": "id: a link id (6+ hex characters) or \"all\""}));
            }
            let mut toks = srv.tokens.lock().unwrap();
            let before = toks.len();
            toks.retain(|r| !(r.label.starts_with("link:") && (r.token.starts_with(id) || id == "all")));
            let n = before - toks.len();
            drop(toks);
            srv.write_state();
            json_resp(&mut s, 200, &json!({"revoked": n}))
        }
        ("GET", ["api", "urls"]) if owner => {
            let st = srv.state();
            let tok = match req.query.get("role").map(String::as_str) {
                Some("view") => st.view_token,
                _ => st.control_token,
            };
            json_resp(&mut s, 200, &json!({"urls": srv.urls_for(&tok, req.query.get("phone").map(String::as_str)), "tunnels": st.tunnels, "available": tunnel::available()}))
        }
        ("POST", ["api", "tunnel"]) if owner => {
            let b: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let Some(kind) = b["kind"].as_str().and_then(Kind::parse) else { return json_resp(&mut s, 400, &json!({"error": "kind: tailscale, funnel or cloudflared"})) };
            if b["on"].as_bool() == Some(false) {
                srv.remove_tunnel(&kind);
                json_resp(&mut s, 200, &json!({"kind": kind, "state": "stopped"}))
            } else {
                let info = srv.add_tunnel(kind);
                json_resp(&mut s, 200, &info)
            }
        }
        ("GET", ["api", "qr.svg"]) => {
            let text = req.query.get("text").cloned().unwrap_or_default();
            match qrcode::QrCode::new(text.as_bytes()) {
                Ok(q) => respond(&mut s, 200, "image/svg+xml", &[], super::qr_svg(&q).as_bytes()),
                Err(_) => json_resp(&mut s, 400, &json!({"error": "too long for a QR code"})),
            }
        }
        ("GET", ["ws", id]) if srv.visible(&g, id) => {
            let Some(inst) = instance::find(id) else { return json_resp(&mut s, 404, &json!({"error": "no such device"})) };
            // the socket must come from this page's own origin (a token in the URL could be replayed by another site's
            // script otherwise, a local browser has no token at all)
            let host = req.headers.get("host").cloned().unwrap_or_default();
            if let Some(o) = req.headers.get("origin") {
                let oh = o.split("://").nth(1).unwrap_or("");
                let fwd = req.headers.get("x-forwarded-host").cloned().unwrap_or_default();
                if !oh.eq_ignore_ascii_case(&host) && !oh.eq_ignore_ascii_case(&fwd) {
                    return json_resp(&mut s, 403, &json!({"error": "cross-origin socket"}));
                }
            }
            let Some(key) = req.headers.get("sec-websocket-key") else { return json_resp(&mut s, 400, &json!({"error": "not a websocket"})) };
            let resp = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", ws::accept_key(key));
            if s.write_all(resp.as_bytes()).is_err() {
                return;
            }
            session(srv, s, inst, g, &req.query);
        }
        // the Accessibility Inspector (axapi.rs)
        (_, ["api", "phones", id, "ax", what]) if srv.visible(&g, id) => {
            let Some(inst) = instance::find(id) else { return json_resp(&mut s, 404, &json!({"error": "no such device"})) };
            let (code, v) = super::axapi::handle(&req, &inst, what, control);
            json_resp(&mut s, code, &v)
        }
        _ => json_resp(&mut s, if owner { 404 } else { 403 }, &json!({"error": "not found or not allowed"})),
    }
}

/// One viewer's socket: video out (from the shared stream), input in (control role only), phone state changes out
fn session(srv: &Arc<Server>, s: TcpStream, inst: Instance, g: Grant, q: &HashMap<String, String>) {
    let codec = if q.get("codec").map(String::as_str) == Some("jpeg") { Codec::Jpeg } else { Codec::H264 };
    let width = q.get("w").and_then(|v| v.parse::<u32>().ok()).unwrap_or(720).clamp(160, 2048) / 16 * 16;
    let quality = q.get("q").and_then(|v| v.parse::<u8>().ok()).unwrap_or(1).min(2);
    let fps = q.get("fps").and_then(|v| v.parse::<u32>().ok()).unwrap_or(30).clamp(5, 60);
    let (tx, rx) = mpsc::sync_channel::<Out>(6);
    let w = match s.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let _ = w.set_write_timeout(Some(Duration::from_secs(10)));
    // sound has its own queue (100 ms of 10 ms packets) and writer: it goes out between video frames, never behind a
    // queue of them, and a slow socket loses sound packets on their own
    let w = Arc::new(Mutex::new(w));
    let (atx, arx) = mpsc::sync_channel::<Out>(10);
    let aw = w.clone();
    let audio_writer = std::thread::spawn(move || {
        for m in arx {
            if let Out::Bin(b) = m {
                if ws::write(&mut *aw.lock().unwrap(), 2, &b).is_err() {
                    break;
                }
            }
        }
    });
    let writer = std::thread::spawn(move || {
        for m in rx {
            let mut g = w.lock().unwrap();
            let mut w: &mut TcpStream = &mut g;
            let r = match m {
                Out::Video(p, _) => ws::write(&mut w, 2, &p),
                Out::Bin(b) => ws::write(&mut w, 2, &b),
                Out::Text(t) => ws::write(&mut w, 1, t.as_bytes()),
                Out::Pong(p) => ws::write(&mut w, 0xa, &p),
                Out::Close => {
                    let _ = ws::write(&mut w, 8, &[]);
                    break;
                }
            };
            if r.is_err() {
                break;
            }
        }
        let _ = w.lock().unwrap().shutdown(std::net::Shutdown::Both);
    });
    let control = g.role == Role::Control;
    let state = |inst: &Instance| json!({"t": "state", "orientation": inst.orientation(), "hinge": inst.hinge(), "inner_active": inst.inner_active(), "running": crate::ring::Ring::new(&inst.ring).seq() != 0});
    let _ = tx.send(Out::Text(
        json!({"t": "hello", "role": g.role, "phone": inst.id, "name": inst.name, "model": inst.model, "platform": inst.platform, "two_screens": inst.inner.is_some(),
               "codec": if codec == Codec::H264 { "h264" } else { "jpeg" }, "expires": g.expires, "audio": true, "poses": hid::HINGE_POSES.iter().map(|p| json!({"key": p.key, "label": p.label})).collect::<Vec<_>>()})
        .to_string(),
    ));
    let (streamer, sub) = video::join(&inst, StreamKey { phone: inst.id.clone(), codec, width, quality, fps }, tx.clone());
    let mut r = s;
    let _ = r.set_read_timeout(Some(Duration::from_millis(1000)));
    let mut input = Input::new(&inst);
    // the phone's sound once the page asks for it ({"t": "audio", "on": true, "codec": "opus" | "pcm"}), the viewer's
    // microphone once it sends one (control role)
    let mut sound: Option<(Arc<audio::Audio>, u64)> = None;
    let mut mic: Option<(Arc<audio::Audio>, u64)> = None;
    static MIC_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);
    let mut last_state = Value::Null;
    let mut last_check = Instant::now() - Duration::from_secs(5);
    loop {
        if last_check.elapsed() >= Duration::from_millis(900) {
            last_check = Instant::now();
            let st = state(&inst);
            if st != last_state {
                last_state = st.clone();
                if tx.send(Out::Text(st.to_string())).is_err() {
                    break;
                }
            }
            // expired, or revoked since the socket opened: the session ends
            if !srv.still_valid(&g) {
                let _ = tx.send(Out::Text(json!({"t": if g.expires.is_some_and(|e| e <= now_s()) { "expired" } else { "revoked" }}).to_string()));
                break;
            }
        }
        match ws::read(&mut r) {
            Ok(ws::Msg::Text(t)) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                match v["t"].as_str() {
                    Some("key-frame") => streamer.request_key(),
                    Some("ping") => {
                        let _ = tx.send(Out::Text(json!({"t": "pong", "c": v["c"]}).to_string()));
                    }
                    Some("audio") => {
                        if let Some((a, id)) = sound.take() {
                            a.leave(id);
                        }
                        if v["on"].as_bool() == Some(true) {
                            let c = if v["codec"].as_str() == Some("pcm") { audio::Codec::Pcm } else { audio::Codec::Opus };
                            sound = Some(audio::join(&inst, atx.clone(), c));
                        }
                    }
                    Some("mic") if control => {
                        if v["on"].as_bool() != Some(true) {
                            if let Some((a, id)) = mic.take() {
                                a.mic_off(id);
                            }
                        }
                    }
                    _ if control => {
                        if let Err(e) = input.handle(&v) {
                            let _ = tx.send(Out::Text(json!({"t": "error", "error": e}).to_string()));
                        }
                    }
                    _ => {
                        let _ = tx.send(Out::Text(json!({"t": "error", "error": "this link can only watch"}).to_string()));
                    }
                }
            }
            Ok(ws::Msg::Binary(b)) if control && b.first() == Some(&audio::KIND_MIC) => {
                let (a, id) = mic.get_or_insert_with(|| (audio::get(&inst), MIC_IDS.fetch_add(1, Ordering::Relaxed)));
                a.mic(*id, &b[1..]);
            }
            Ok(ws::Msg::Ping(p)) => {
                let _ = tx.send(Out::Pong(p));
            }
            Ok(ws::Msg::Close) => break,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    input.release();
    streamer.leave(sub);
    if let Some((a, id)) = sound.take() {
        a.leave(id);
    }
    if let Some((a, id)) = mic.take() {
        a.mic_off(id);
    }
    drop(atx);
    let _ = audio_writer.join();
    let _ = tx.send(Out::Close);
    drop(tx);
    let _ = writer.join();
}

/// Input from a browser to the phone (the launcher viewer's rules: touches upright, turned to the panel; the edge rule)
struct Input {
    inst: Instance,
    hid: Hid,
    orient: u32,
    size: (u32, u32),
    down_buttons: Vec<String>,
    down_keys: Vec<u32>,
}

const BUTTONS: [&str; 14] = ["home", "lock", "side", "volup", "voldown", "siri", "action", "wallet", "playpause", "tv", "select", "menu", "back", "mute"];

impl Input {
    fn new(inst: &Instance) -> Input {
        let mut i = Input { inst: inst.clone(), hid: Hid::new(&inst.sock), orient: 0, size: inst.screen, down_buttons: vec![], down_keys: vec![] };
        i.aim();
        i
    }

    fn aim(&mut self) {
        let t = video::target(&self.inst, &mut self.orient);
        self.hid.display = t.display;
        self.hid.main_rot = t.main_rot;
        self.size = t.size;
    }

    /// lets go of anything held when the viewer goes away (a closed tab must not leave a button pressed)
    fn release(&mut self) {
        for b in std::mem::take(&mut self.down_buttons) {
            self.hid.button(&b, false);
        }
        for k in std::mem::take(&mut self.down_keys) {
            self.hid.key(false, k);
        }
    }

    fn handle(&mut self, v: &Value) -> Result<(), String> {
        let f = |k: &str| v[k].as_f64().unwrap_or(0.0) as f32;
        let phase = |v: &Value| -> Result<&'static str, String> {
            match v["p"].as_str() {
                Some("down") => Ok("down"),
                Some("move") => Ok("move"),
                Some("up") => Ok("up"),
                _ => Err("p: down, move or up".into()),
            }
        };
        match v["t"].as_str().unwrap_or("") {
            "touch" => {
                let p = phase(v)?;
                if p == "down" {
                    self.aim();
                }
                let (x, y) = (f("x"), f("y"));
                let edge = if p == "down" && v["edge"].as_bool() != Some(false) { hid::edge_at(x, y, self.size.0, self.size.1) } else { None };
                self.hid.touch(p, x, y, edge);
            }
            "touch2" => {
                let p = phase(v)?;
                if p == "down" {
                    self.aim();
                }
                self.hid.touch2(p, f("x1"), f("y1"), f("x2"), f("y2"));
            }
            "key" => {
                let u = v["u"].as_u64().ok_or("u: HID usage")? as u32;
                if !(4..=0xe7).contains(&u) {
                    return Err("usage out of range".into());
                }
                let down = v["d"].as_bool().unwrap_or(true);
                self.down_keys.retain(|k| *k != u);
                if down {
                    self.down_keys.push(u);
                }
                self.hid.key(down, u);
            }
            "text" => {
                let s = v["s"].as_str().unwrap_or("");
                let mut skipped = 0;
                for c in s.chars().take(4000) {
                    match char_usage(c) {
                        Some((u, sh)) => {
                            if sh {
                                self.hid.key(true, 225);
                            }
                            self.hid.key(true, u);
                            self.hid.key(false, u);
                            if sh {
                                self.hid.key(false, 225);
                            }
                        }
                        None => skipped += 1,
                    }
                }
                if skipped > 0 {
                    return Err(format!("{skipped} characters have no key on a US keyboard and were left out"));
                }
            }
            "button" => {
                let n = v["n"].as_str().unwrap_or("");
                if !BUTTONS.contains(&n) {
                    return Err(format!("unknown button {n}"));
                }
                match v["d"].as_bool() {
                    Some(d) => {
                        self.down_buttons.retain(|b| b != n);
                        if d {
                            self.down_buttons.push(n.to_string());
                        }
                        self.hid.button(n, d);
                    }
                    None => self.hid.hold(n, v["ms"].as_u64().unwrap_or(100).min(10_000)),
                }
            }
            "crown" => self.hid.crown(f("d").clamp(-2000.0, 2000.0)),
            // the system pointer (Vision Pro: the mouse is the pointer there, touches reach nothing; an iPad's mouse):
            // a place on the screen as shown (0..1), the button, a scroll
            "pointer" => {
                let line = match v["a"].as_str().unwrap_or("") {
                    "at" => format!("pointer at {:.5} {:.5}", f("x").clamp(0.0, 1.0), f("y").clamp(0.0, 1.0)),
                    "down" => "pointer button down".into(),
                    "up" => "pointer button up".into(),
                    "click" => "pointer click".into(),
                    "scroll" => format!("pointer scroll {:.2} {:.2}", f("dx").clamp(-2000.0, 2000.0), f("dy").clamp(-2000.0, 2000.0)),
                    a => return Err(format!("pointer: at, down, up, click or scroll, not {a:?}")),
                };
                self.hid.send(&line);
            }
            "rotate" => self.hid.rotate(v["dir"].as_str() == Some("left")),
            "shake" => self.hid.shake(),
            "hinge" => {
                self.inst.inner.as_ref().ok_or("this device has one screen")?;
                let k = v["pose"].as_str().unwrap_or("");
                let pose = hid::HINGE_POSES.iter().find(|p| p.key == k).ok_or("unknown pose")?;
                self.hid.hinge_pose(pose);
            }
            "sys" => {
                let line = match v["a"].as_str().unwrap_or("") {
                    "app_switcher" => "appswitcher",
                    "memory_warning" => {
                        self.inst.memory_warning().map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    "soft_keyboard" => {
                        self.hid.soft_keyboard_toggle();
                        return Ok(());
                    }
                    "hw_keyboard" => {
                        let on = !self.hid.hw_keyboard_connected();
                        self.hid.hw_keyboard(on);
                        return Ok(());
                    }
                    "double_tap" => "gesture doubletap",
                    "wrist_flick" => "gesture wristflick",
                    a => return Err(format!("unknown action {a}")),
                };
                self.hid.send(line);
            }
            "usage" => {
                // consumer-page buttons the Siri Remote has (page 0x0c only)
                let u = v["u"].as_u64().unwrap_or(0) as u32;
                self.hid.usage(0x0c, u, true);
                self.hid.usage(0x0c, u, false);
            }
            t => return Err(format!("unknown input {t}")),
        }
        Ok(())
    }
}

/// A typed character -> (HID usage, needs shift), US layout (as the launcher viewer types a paste)
pub fn char_usage(c: char) -> Option<(u32, bool)> {
    const SHIFTED: &str = "!@#$%^&*()";
    Some(match c {
        'a'..='z' => (4 + (c as u32 - 'a' as u32), false),
        'A'..='Z' => (4 + (c as u32 - 'A' as u32), true),
        '1'..='9' => (0x1e + (c as u32 - '1' as u32), false),
        '0' => (0x27, false),
        '\n' => (0x28, false),
        '\t' => (0x2b, false),
        ' ' => (0x2c, false),
        '-' => (0x2d, false),
        '_' => (0x2d, true),
        '=' => (0x2e, false),
        '+' => (0x2e, true),
        '[' => (0x2f, false),
        '{' => (0x2f, true),
        ']' => (0x30, false),
        '}' => (0x30, true),
        '\\' => (0x31, false),
        '|' => (0x31, true),
        ';' => (0x33, false),
        ':' => (0x33, true),
        '\'' => (0x34, false),
        '"' => (0x34, true),
        '`' => (0x35, false),
        '~' => (0x35, true),
        ',' => (0x36, false),
        '<' => (0x36, true),
        '.' => (0x37, false),
        '>' => (0x37, true),
        '/' => (0x38, false),
        '?' => (0x38, true),
        c if SHIFTED.contains(c) => (0x1e + SHIFTED.find(c).unwrap() as u32, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(peer: &str, headers: &[(&str, &str)]) -> Req {
        Req {
            method: "GET".into(),
            path: "/".into(),
            query: HashMap::new(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: vec![],
            peer: peer.parse().unwrap(),
            local: "127.0.0.1:7380".parse().unwrap(),
        }
    }

    /// A request to a running test server: (status, body)
    fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, Value) {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        write!(c, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}X-X86sim: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        let (head, b) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        (head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0), serde_json::from_str(b).unwrap_or(Value::Null))
    }

    /// A server on a free loopback port (its state file is not written: Server::new gets port 0)
    fn test_server() -> (Arc<Server>, u16) {
        let srv = Server::new(Options { require_token: false, ..Options::default() }, 0);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let s2 = srv.clone();
        std::thread::spawn(move || run(s2, l));
        (srv, port)
    }

    #[test]
    fn links_are_scoped_to_what_they_allow() {
        let (srv, port) = test_server();
        let main = srv.state().control_token;
        let view = srv.state().view_token;
        // a control link limited to a phone and expiring: no server-wide endpoint, no main token
        let l = srv.mint(Role::Control, Some("device-a64-x".into()), Some(600), "t");
        let other = srv.mint(Role::View, None, Some(600), "o");
        for (m, path, body) in [("GET", "/api/urls", ""), ("GET", "/api/urls?role=view", ""), ("GET", "/api/status", ""), ("GET", "/api/links", ""),
                                ("POST", "/api/tunnel", r#"{"kind":"funnel"}"#), ("POST", "/api/links", r#"{"role":"control"}"#),
                                ("POST", "/api/links/revoke", r#"{"id":""}"#), ("POST", "/api/links/revoke", r#"{"id":"all"}"#)] {
            let (code, v) = http(port, m, path, Some(&l.token), body);
            assert!(code == 403 || code == 404, "{m} {path} with a limited link: {code} {v}");
            assert!(!v.to_string().contains(&main), "{m} {path} handed out the main token");
        }
        // an unlimited control link is no owner either
        let wide = srv.mint(Role::Control, None, Some(600), "w");
        assert_eq!(http(port, "GET", "/api/urls", Some(&wide.token), "").0, 403);
        assert_eq!(http(port, "POST", "/api/tunnel", Some(&wide.token), r#"{"kind":"funnel"}"#).0, 403);
        // the view token neither
        assert_eq!(http(port, "GET", "/api/links", Some(&view), "").0, 403);
        let (code, me) = http(port, "GET", "/api/me", Some(&l.token), "");
        assert_eq!((code, me["owner"].as_bool()), (200, Some(false)));
        // the owner (main control token) does it all; revoke wants a real id
        let (code, me) = http(port, "GET", "/api/me", Some(&main), "");
        assert_eq!((code, me["owner"].as_bool()), (200, Some(true)));
        assert_eq!(http(port, "GET", "/api/urls", Some(&main), "").0, 200);
        assert_eq!(http(port, "GET", "/api/links", Some(&main), "").1.as_array().map(|a| a.len()), Some(3));
        assert_eq!(http(port, "POST", "/api/links/revoke", Some(&main), r#"{"id":""}"#).0, 400);
        assert_eq!(http(port, "POST", "/api/links/revoke", Some(&main), r#"{"id":"a"}"#).0, 400);
        assert!(srv.still_valid(&srv.grant(&{
            let mut r = req("10.0.0.2:1", &[("host", "x")]);
            r.query.insert("t".into(), other.token.clone());
            r
        }).unwrap()));
        let g = Grant { role: Role::View, phone: None, token: Some(other.token.clone()), expires: other.expires, owner: false };
        let (code, v) = http(port, "POST", "/api/links/revoke", Some(&main), &json!({"id": &other.token[..6]}).to_string());
        assert_eq!((code, v["revoked"].as_u64()), (200, Some(1)));
        // an open session of the revoked link ends at its next check (session() polls still_valid)
        assert!(!srv.still_valid(&g));
        assert_eq!(http(port, "GET", "/api/me", Some(&other.token), "").0, 401);
        srv.stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn the_local_owner_is_this_user_only() {
        let (srv, port) = test_server();
        // this test process is the server's user: no token needed, and it is the owner
        let (code, me) = http(port, "GET", "/api/me", None, "");
        assert_eq!((code, me["owner"].as_bool(), me["local"].as_bool()), (200, Some(true), Some(true)));
        // peer_uid finds our socket's uid; a pair that is no connection has none
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (a, _) = l.accept().unwrap();
        assert_eq!(peer_uid(a.peer_addr().unwrap(), a.local_addr().unwrap()), Some(unsafe { libc::getuid() }));
        assert_eq!(peer_uid("127.0.0.1:1".parse().unwrap(), a.local_addr().unwrap()), None);
        drop(c);
        // a peer that is not ours (no such socket: e.g. another user's, which /proc/net/tcp lists with its uid) is refused
        let mut r = req("127.0.0.1:1", &[("host", "127.0.0.1:7380")]);
        r.local = "127.0.0.1:2".parse().unwrap();
        assert!(srv.grant(&r).is_none());
        srv.stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn request_heads_are_bounded() {
        let peer: SocketAddr = "10.0.0.2:1".parse().unwrap();
        // an endless header line without a newline: refused at the head limit, not read to the end
        let endless = std::io::repeat(b'a');
        let t = Instant::now();
        assert!(read_head(std::io::Cursor::new(b"GET / HTTP/1.1\r\nX: ".to_vec()).chain(endless), peer, peer).is_none());
        assert!(t.elapsed() < Duration::from_secs(5));
        // too many headers
        let mut h = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..200 {
            h.extend(format!("X-{i}: 1\r\n").as_bytes());
        }
        h.extend(b"\r\n");
        assert!(read_head(std::io::Cursor::new(h), peer, peer).is_none());
        // a normal one with a body
        let r = read_head(std::io::Cursor::new(b"POST /api/x?a=1 HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody".to_vec()), peer, peer).unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.body.as_slice()), ("POST", "/api/x", &b"body"[..]));
        // a slow client: the deadline ends it however it trickles
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (a, _) = l.accept().unwrap();
        c.write_all(b"GET / HT").unwrap();
        let t = Instant::now();
        assert!(read_head(Deadline { s: &a, end: Instant::now() + Duration::from_millis(400) }, peer, peer).is_none());
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn connections_are_capped() {
        let c = Conns::default();
        let lan: std::net::IpAddr = "192.168.1.9".parse().unwrap();
        let held: Vec<_> = (0..MAX_CONNS_PEER).map(|_| c.enter(lan).unwrap()).collect();
        assert!(c.enter(lan).is_none(), "one address is capped");
        let lo: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let more: Vec<_> = (0..MAX_CONNS - MAX_CONNS_PEER).map(|_| c.enter(lo).unwrap()).collect();
        assert!(c.enter(lo).is_none(), "the total is capped");
        drop(held);
        drop(more);
        assert!(c.enter(lan).is_some(), "slots come back");
    }

    #[test]
    fn only_a_plain_local_browser_goes_without_a_token() {
        assert!(local_trusted(&req("127.0.0.1:5000", &[("host", "127.0.0.1:7380")])));
        assert!(local_trusted(&req("127.0.0.1:5000", &[("host", "localhost:7380"), ("origin", "http://localhost:7380")])));
        // a LAN peer, a tunnel (loopback peer with forwarding headers), DNS rebinding (foreign Host), another origin
        assert!(!local_trusted(&req("192.168.1.9:5000", &[("host", "127.0.0.1:7380")])));
        assert!(!local_trusted(&req("127.0.0.1:5000", &[("host", "desktop.tail.ts.net"), ("x-forwarded-for", "100.1.2.3")])));
        assert!(!local_trusted(&req("127.0.0.1:5000", &[("host", "abc.trycloudflare.com"), ("cf-connecting-ip", "1.2.3.4")])));
        assert!(!local_trusted(&req("127.0.0.1:5000", &[("host", "127.0.0.1:7380"), ("cf-ray", "x")])));
        assert!(!local_trusted(&req("127.0.0.1:5000", &[("host", "evil.example:7380")])));
        assert!(!local_trusted(&req("127.0.0.1:5000", &[("host", "127.0.0.1:7380"), ("origin", "http://evil.example")])));
        assert!(!local_trusted(&req("127.0.0.1:5000", &[])));
    }

    #[test]
    fn tokens_roles_and_expiry() {
        let srv = Server::new(Options::default(), 0);
        let st = srv.state();
        let with = |t: &str| {
            let mut r = req("10.0.0.2:1", &[("host", "x")]);
            r.query.insert("t".into(), t.into());
            r
        };
        assert_eq!(srv.grant(&with(&st.control_token)).unwrap().role, Role::Control);
        assert_eq!(srv.grant(&with(&st.view_token)).unwrap().role, Role::View);
        assert!(srv.grant(&with("nope")).is_none());
        assert!(srv.grant(&req("10.0.0.2:1", &[("host", "x")])).is_none());
        let l = srv.mint(Role::View, Some("device-a64-x".into()), Some(0), "t");
        assert!(srv.grant(&with(&l.token)).is_none(), "a link with no time left is refused");
        let l = srv.mint(Role::View, Some("device-a64-x".into()), Some(60), "t");
        let g = srv.grant(&with(&l.token)).unwrap();
        assert_eq!(g.phone.as_deref(), Some("device-a64-x"));
        assert!(!srv.visible(&g, "device-a64-y"));
    }

    #[test]
    fn typing_maps_us_keys() {
        assert_eq!(char_usage('a'), Some((4, false)));
        assert_eq!(char_usage('Z'), Some((29, true)));
        assert_eq!(char_usage('!'), Some((0x1e, true)));
        assert_eq!(char_usage(')'), Some((0x27, true)));
        assert_eq!(char_usage('@'), Some((0x1f, true)));
        assert_eq!(char_usage('é'), None);
        assert_eq!(pct_decode("a%20b+c%2F"), "a b c/");
        assert_eq!(host_only("[::1]:7380"), "[::1]");
        assert!(is_loopback_name("localhost:1"));
    }
}
