//! Share: watch and drive phones from a browser, here or elsewhere (`x86sim serve`, the MCP share_* tools, the
//! launcher's Share button). One server process for every phone (x86sim-serve, server.rs) on loopback by default;
//! tailscale / cloudflared tunnels (tunnel.rs) for other machines. The server writes its state (pid, port, tokens,
//! tunnels and their URLs) to $XDG_RUNTIME_DIR/x86sim-serve.json (0600): the CLI, the MCP server and the launcher
//! find it there and talk to it over loopback with its control token.
/// the Accessibility Inspector's routes (/api/phones/<id>/ax/...)
pub mod axapi;
pub mod audio;
/// Web Inspector routes (control role): x86sim inspect, the viewer's Inspect button
pub mod inspect;
pub mod server;
pub mod tunnel;
pub mod video;
pub mod ws;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
pub use tunnel::{CfNamed, Kind};

pub const DEFAULT_PORT: u16 = 7380;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// watch and drive, start / stop phones, make links
    Control,
    /// watch only
    View,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenRec {
    pub token: String,
    pub role: Role,
    /// limited to this phone (instance id)
    pub phone: Option<String>,
    /// unix seconds
    pub expires: Option<u64>,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// address:port to listen on (default 127.0.0.1:7380; a busy port moves on to the next free one)
    pub listen: String,
    pub tunnels: Vec<Kind>,
    /// tailscale serve's HTTPS port (default: 443 if free, else 8443)
    pub ts_port: Option<u16>,
    pub cf_named: Option<CfNamed>,
    /// the main tokens expire after this many seconds (default: when the server stops)
    pub ttl_s: Option<u64>,
    /// a token even for a local browser
    pub require_token: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { listen: format!("127.0.0.1:{DEFAULT_PORT}"), tunnels: vec![], ts_port: None, cf_named: None, ttl_s: None, require_token: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct State {
    pub pid: u32,
    pub port: u16,
    pub listen: String,
    pub started: u64,
    pub local_url: String,
    pub control_token: String,
    pub view_token: String,
    pub expires: Option<u64>,
    pub tunnels: Vec<tunnel::TunnelInfo>,
    pub require_token: bool,
    /// links made (share links with their own tokens)
    pub links: usize,
}

pub fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 128 random bits, hex
pub fn random_token() -> String {
    let mut b = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_ok();
    assert!(ok, "no /dev/urandom");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn state_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    dir.join("x86sim-serve.json")
}

pub fn log_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/state"));
    base.join("x86sim/serve.log")
}

pub fn read_state() -> Option<State> {
    serde_json::from_str(&std::fs::read_to_string(state_path()).ok()?).ok()
}

fn alive(pid: u32) -> bool {
    pid > 0 && unsafe { libc::kill(pid as i32, 0) } == 0 && std::fs::read_to_string(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.contains("x86sim-serve"))
}

/// The running server's state; a state file left by a server that died is cleaned up (its tailscale ports turned off)
pub fn status() -> Option<State> {
    let st = read_state()?;
    if alive(st.pid) {
        return Some(st);
    }
    for t in &st.tunnels {
        if matches!(t.kind, Kind::Tailscale | Kind::Funnel) && t.state == "up" {
            if let Some(port) = t.url.as_deref().and_then(|u| u.rsplit_once(':')).and_then(|(_, p)| p.parse().ok()).or(Some(443)) {
                tunnel::ts_off(t.kind == Kind::Funnel, port);
            }
        }
    }
    let _ = std::fs::remove_file(state_path());
    None
}

/// The address URLs a token works at: tunnels that are up, then the listen address ("local", or the LAN addresses
/// for 0.0.0.0). (kind, reach, url)
pub fn urls(st: &State, token: &str, phone: Option<&str>) -> Vec<(String, String, String)> {
    let q = match phone {
        Some(p) => format!("?t={token}&phone={}", server::pct_encode(p)),
        None => format!("?t={token}"),
    };
    let mut v = vec![];
    for t in &st.tunnels {
        if let (Some(u), "up") = (&t.url, t.state.as_str()) {
            v.push((t.kind.name().to_string(), t.reach.to_string(), format!("{u}/{q}")));
        }
    }
    for (kind, base) in listen_urls(&st.listen, st.port) {
        let reach = if kind == "local" { "this machine" } else { "network" };
        v.push((kind.to_string(), reach.to_string(), format!("{base}{q}")));
    }
    v
}

/// "local" http://127.0.0.1:port/ always; "lan" URLs for an address other than loopback
pub fn listen_urls(listen: &str, port: u16) -> Vec<(&'static str, String)> {
    let ip: Option<std::net::IpAddr> = listen.rsplit_once(':').and_then(|(h, _)| h.trim_matches(|c| c == '[' || c == ']').parse().ok());
    let mut v = vec![("local", format!("http://127.0.0.1:{port}/"))];
    match ip {
        Some(ip) if ip.is_loopback() => {}
        Some(ip) if ip.is_unspecified() => {
            // the address this machine reaches the network from (a UDP "connect" sends nothing)
            if let Some(a) = std::net::UdpSocket::bind("0.0.0.0:0").ok().and_then(|s| s.connect("192.0.2.1:9").ok().and_then(|_| s.local_addr().ok())) {
                v.push(("lan", format!("http://{}:{port}/", a.ip())));
            }
        }
        Some(ip) => v.push(("lan", format!("http://{}/", std::net::SocketAddr::new(ip, port)))),
        None => {}
    }
    v
}

/// The x86sim-serve binary: beside this one (launcher, MCP server, AppImage), else the repo's build
pub fn serve_exe() -> PathBuf {
    if let Some(p) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("x86sim-serve"))).filter(|p| p.is_file()) {
        return p;
    }
    crate::aot::repo_root().join("tools/launcher/target/release/x86sim-serve")
}

pub fn args_of(opts: &Options) -> Vec<String> {
    let mut a = vec!["run".to_string(), "--listen".into(), opts.listen.clone()];
    for t in &opts.tunnels {
        a.push(format!("--{}", t.name()));
    }
    if let Some(p) = opts.ts_port {
        a.extend(["--ts-port".into(), p.to_string()]);
    }
    if let Some(n) = &opts.cf_named {
        a.extend(["--cf-tunnel".into(), n.name.clone(), "--cf-host".into(), n.hostname.clone()]);
    }
    if let Some(t) = opts.ttl_s {
        a.extend(["--ttl".into(), format!("{t}s")]);
    }
    if opts.require_token {
        a.push("--require-token".into());
    }
    a
}

/// Starts the server in the background (or, when one runs, starts the tunnels asked for in it) and returns its state
/// once it listens and its tunnels are up or failed.
pub fn start(opts: &Options) -> Result<State> {
    if let Some(st) = status() {
        for k in &opts.tunnels {
            if !st.tunnels.iter().any(|t| t.kind == *k && t.state == "up") {
                api(&st, "POST", "/api/tunnel", Some(json!({"kind": k.name(), "on": true})))?;
            }
        }
        return status().context("the server went away");
    }
    let exe = serve_exe();
    anyhow::ensure!(exe.is_file(), "{} is not built (cd tools/launcher && cargo build --release)", exe.display());
    let log = log_path();
    if let Some(d) = log.parent() {
        std::fs::create_dir_all(d)?;
    }
    // the log is the user's alone (an older one made 0644 is narrowed): the server prints no full token into it, but it
    // names the tunnels and the phones
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let lf = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&log)?;
    lf.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    // A user service of its own when systemd is there: started from a terminal, an editor or an agent, the server would
    // otherwise live in that caller's cgroup and die with it (systemd-oomd killing an agent's scope took the user's
    // shared server and its tunnel down). Without systemd (or a caller with a private XDG_RUNTIME_DIR, which cannot
    // reach the user manager): its own session, as before.
    let pid = match crate::sim::user_systemd().then(|| start_unit(&exe, opts, &log)).transpose()? {
        Some(pid) => pid,
        None => {
            let mut cmd = std::process::Command::new("setsid");
            cmd.arg(&exe).args(args_of(opts)).stdin(std::process::Stdio::null()).stdout(lf.try_clone()?).stderr(lf);
            let mut child = cmd.spawn().context("starting x86sim-serve")?;
            // setsid execs in place (no fork when we are not a group leader): the child's pid is the server's
            let pid = child.id();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            pid
        }
    };
    let end = Instant::now() + Duration::from_secs(40);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(st) = read_state().filter(|s| s.pid == pid || alive(s.pid)) {
            let pending = st.tunnels.iter().any(|t| t.state == "starting") || st.tunnels.len() < opts.tunnels.len();
            if !pending {
                return Ok(st);
            }
        }
        if !alive(pid) && read_state().is_none() {
            let tail: String = std::fs::read_to_string(&log).unwrap_or_default().lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            bail!("x86sim-serve exited: {tail}");
        }
    }
    read_state().context("x86sim-serve did not come up (see ~/.local/state/x86sim/serve.log)")
}

/// The unit of the server for this XDG_RUNTIME_DIR (one server per user and runtime dir, as its state file)
fn unit_name() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::env::var("XDG_RUNTIME_DIR").unwrap_or_default().hash(&mut h);
    format!("x86sim-serve-{:08x}", h.finish() as u32)
}

/// x86sim-serve as a transient systemd user service (`systemd-run --user`), with this process's environment and its
/// output appended to the log; returns the server's pid (the unit's main process)
fn start_unit(exe: &std::path::Path, opts: &Options, log: &std::path::Path) -> Result<u32> {
    let unit = unit_name();
    let sctl = |args: &[&str]| {
        std::process::Command::new("systemctl").arg("--user").args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()
    };
    // a unit left from a server that lost its state file: ended first (no state = no server to keep)
    let _ = sctl(&["stop", &format!("{unit}.service")]);
    let _ = sctl(&["reset-failed", &format!("{unit}.service")]);
    let mut cmd = std::process::Command::new("systemd-run");
    cmd.args(["--user", "--collect", "--quiet", "--unit", &unit, "-p", "KillMode=mixed"])
        .arg("-p")
        .arg(format!("StandardOutput=append:{}", log.display()))
        .arg("-p")
        .arg(format!("StandardError=append:{}", log.display()));
    if let Ok(d) = std::env::current_dir() {
        cmd.arg("-p").arg(format!("WorkingDirectory={}", d.display()));
    }
    for (k, v) in std::env::vars() {
        if !v.contains('\n') && !k.starts_with("SYSTEMD_") && k != "_" {
            cmd.arg("-E").arg(format!("{k}={v}"));
        }
    }
    cmd.arg(exe).args(args_of(opts)).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    let out = cmd.output().context("systemd-run x86sim-serve")?;
    anyhow::ensure!(out.status.success(), "systemd-run x86sim-serve: {}", String::from_utf8_lossy(&out.stderr).trim());
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let o = std::process::Command::new("systemctl").args(["--user", "show", "-p", "MainPID", "--value", &format!("{unit}.service")]).output()?;
        if let Ok(pid) = String::from_utf8_lossy(&o.stdout).trim().parse::<u32>() {
            if pid > 0 {
                return Ok(pid);
            }
        }
        anyhow::ensure!(Instant::now() < end, "x86sim-serve: no main process in {unit}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stops the server (its tunnels with it). False if none ran.
pub fn stop() -> Result<bool> {
    let Some(st) = status() else { return Ok(false) };
    unsafe { libc::kill(st.pid as i32, libc::SIGTERM) };
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(100));
        if !alive(st.pid) {
            let _ = status();
            return Ok(true);
        }
    }
    unsafe { libc::kill(st.pid as i32, libc::SIGKILL) };
    std::thread::sleep(Duration::from_millis(200));
    let _ = status();
    Ok(true)
}

/// A call to the running server's API over loopback with its control token
pub fn api(st: &State, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", st.port)).context("connecting to x86sim-serve")?;
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nX-X86sim: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        st.port,
        st.control_token,
        b.len()
    )?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let code: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if code != 200 {
        bail!("x86sim-serve {path}: {code} {}", v["error"].as_str().unwrap_or(body));
    }
    Ok(v)
}

/// A new link: role view or control, limited to a phone (or every phone), expiring after ttl_s
pub fn link(phone: Option<&str>, role: Role, ttl_s: u64) -> Result<Value> {
    let st = status().context("x86sim serve is not running (x86sim serve --bg, or the MCP tool share_start)")?;
    api(&st, "POST", "/api/links", Some(json!({"phone": phone, "role": role, "ttl_s": ttl_s})))
}

/// The QR code as an SVG (dark modules on white, a 4-module quiet zone)
pub fn qr_svg(q: &qrcode::QrCode) -> String {
    let w = q.width();
    let c = q.to_colors();
    let n = w + 8;
    let mut p = String::new();
    for y in 0..w {
        for x in 0..w {
            if c[y * w + x] == qrcode::Color::Dark {
                p.push_str(&format!("M{} {}h1v1h-1z", x + 4, y + 4));
            }
        }
    }
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {n} {n}\" shape-rendering=\"crispEdges\"><rect width=\"{n}\" height=\"{n}\" fill=\"#fff\"/><path d=\"{p}\" fill=\"#000\"/></svg>")
}

/// The QR code for a terminal: two rows a line with half blocks (light modules drawn, so it scans on dark terminals)
pub fn qr_text(text: &str) -> Option<String> {
    let q = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let w = q.width() as i32;
    let c = q.to_colors();
    let dark = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < w && c[(y * w + x) as usize] == qrcode::Color::Dark;
    let mut s = String::new();
    let mut y = -2;
    while y < w + 2 {
        for x in -2..w + 2 {
            s.push(match (!dark(x, y), !dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        s.push('\n');
        y += 2;
    }
    Some(s)
}

/// The longest TTL taken (a year): "1e30" saturated to u64::MAX and then wrapped `now + ttl` into the past
pub const MAX_TTL_S: u64 = 365 * 86400;

pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    let (n, mul) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1),
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        'd' => (&s[..s.len() - 1], 86400),
        _ => (s, 1),
    };
    n.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0).map(|v| ((v * mul as f64) as u64).clamp(1, MAX_TTL_S))
}

/// A URL with its token cut to the first 6 characters (for logs: enough to tell links apart, useless to get in)
pub fn redact(url: &str) -> String {
    match url.split_once("t=") {
        Some((a, b)) => {
            let end = b.find('&').unwrap_or(b.len());
            format!("{a}t={}…{}", &b[..end.min(6)], &b[end..])
        }
        None => url.to_string(),
    }
}

/// The URLs to hand out: every way in for a role's token, the remote ones first
pub fn share_urls(st: &State, role: Role, phone: Option<&str>) -> Vec<(String, String, String)> {
    let tok = if role == Role::Control { &st.control_token } else { &st.view_token };
    urls(st, tok, phone)
}

// ---- MCP (x86sim-mcp): share_* tools, kept here so the server's file only gains a hook ----

pub fn mcp_tools() -> Vec<Value> {
    let phone = json!({"type": "string", "description": "a phone (name, folder or path): the link opens it directly and, for share_link, is limited to it"});
    vec![
        json!({"name": "share_start", "description": "Watch and drive phones from a browser: starts x86sim's web server (every phone: a dashboard with live thumbnails, start / stop; a phone: its live screen as H.264 (JPEG where the browser lacks WebCodecs), touches, multi-touch, keyboard, paste, hardware buttons, rotate, shake, screenshot, recording). Listens on loopback unless listen says otherwise; tailscale=true adds HTTPS on the tailnet (tailscale serve), funnel=true makes that public (tailscale funnel), cloudflared=true a public trycloudflare.com quick tunnel. Anything not from this machine needs the token in the URL. Returns the URLs (control: watch and drive; view: watch only). If it runs already, adds the tunnels asked for.",
               "inputSchema": {"type": "object", "properties": {"phone": phone, "tailscale": {"type": "boolean"}, "funnel": {"type": "boolean"}, "cloudflared": {"type": "boolean"}, "listen": {"type": "string", "description": "address:port, default 127.0.0.1:7380 (0.0.0.0:7380: the LAN, plain HTTP: browsers then fall back to JPEG)"}, "ttl": {"type": "string", "description": "the tokens expire after this (e.g. 2h, 30m); default when the server stops"}}}}),
        json!({"name": "share_stop", "description": "Stop sharing: the web server, its tunnels and every link.", "inputSchema": {"type": "object", "properties": {"tunnel": {"type": "string", "enum": ["tailscale", "funnel", "cloudflared"], "description": "stop only this tunnel, keep the server"}}}}),
        json!({"name": "share_status", "description": "Whether phones are shared: server, tunnels with their URLs and state, which tunnels are available here (tailscale logged in, cloudflared installed), open streams and viewers.", "inputSchema": {"type": "object", "properties": {}}}),
        json!({"name": "share_link", "description": "A new link with its own expiring token: role view (watch only, the default) or control, limited to one phone or not. Returns its URLs (every tunnel that is up, then local).",
               "inputSchema": {"type": "object", "properties": {"phone": phone, "role": {"type": "string", "enum": ["view", "control"]}, "ttl": {"type": "string", "description": "default 1h, at most 30d"}}}}),
    ]
}

fn phone_id(a: &Value) -> Result<Option<String>> {
    match a["phone"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => Ok(Some(crate::instance::find(k).with_context(|| format!("no phone {k:?}"))?.id)),
        None => Ok(None),
    }
}

fn urls_json(st: &State, role: Role, phone: Option<&str>) -> Vec<Value> {
    share_urls(st, role, phone).into_iter().map(|(k, r, u)| json!({"kind": k, "reach": r, "url": u})).collect()
}

/// An MCP call for a share_* tool, None for any other name
pub fn mcp_call(name: &str, a: &Value) -> Option<Result<Value>> {
    let r = match name {
        "share_start" => (|| {
            let mut o = Options::default();
            if let Some(l) = a["listen"].as_str() {
                o.listen = l.to_string();
            }
            if a["tailscale"].as_bool() == Some(true) {
                o.tunnels.push(Kind::Tailscale);
            }
            if a["funnel"].as_bool() == Some(true) {
                o.tunnels.retain(|k| *k != Kind::Tailscale);
                o.tunnels.push(Kind::Funnel);
            }
            if a["cloudflared"].as_bool() == Some(true) {
                o.tunnels.push(Kind::Cloudflared);
            }
            o.ttl_s = a["ttl"].as_str().and_then(parse_duration);
            let phone = phone_id(a)?;
            let st = start(&o)?;
            let failed: Vec<Value> = st.tunnels.iter().filter(|t| t.state == "failed").map(|t| json!({"tunnel": t.kind.name(), "error": t.error})).collect();
            Ok(json!({"running": true, "pid": st.pid, "control": urls_json(&st, Role::Control, phone.as_deref()), "view": urls_json(&st, Role::View, phone.as_deref()),
                      "tunnels": st.tunnels, "failed": failed, "expires": st.expires,
                      "note": "control URLs let anyone holding them drive the phones: share view URLs (or share_link) with spectators"}))
        })(),
        "share_stop" => (|| match a["tunnel"].as_str().and_then(Kind::parse) {
            Some(k) => {
                let st = status().context("not running")?;
                api(&st, "POST", "/api/tunnel", Some(json!({"kind": k.name(), "on": false})))?;
                Ok(json!({"tunnel": k.name(), "stopped": true}))
            }
            None => Ok(json!({"stopped": stop()?})),
        })(),
        "share_status" => (|| match status() {
            Some(st) => {
                let detail = api(&st, "GET", "/api/status", None).unwrap_or(Value::Null);
                Ok(json!({"running": true, "pid": st.pid, "control": urls_json(&st, Role::Control, None), "view": urls_json(&st, Role::View, None), "tunnels": st.tunnels, "expires": st.expires,
                          "streams": detail["streams"], "available": detail["available"], "log": log_path()}))
            }
            None => Ok(json!({"running": false, "available": tunnel::available()})),
        })(),
        "share_link" => (|| {
            let role = if a["role"].as_str() == Some("control") { Role::Control } else { Role::View };
            let ttl = a["ttl"].as_str().and_then(parse_duration).unwrap_or(3600);
            link(phone_id(a)?.as_deref(), role, ttl)
        })(),
        _ => return None,
    };
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_urls() {
        assert_eq!(parse_duration("90"), Some(90));
        assert_eq!(parse_duration("30m"), Some(1800));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("1.5d"), Some(129600));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(parse_duration("1e30"), Some(MAX_TTL_S));
        assert_eq!(parse_duration("inf"), None);
        assert_eq!(parse_duration("NaN"), None);
        assert!(redact("http://x/?t=0123456789abcdef&phone=p").contains("?t=012345…&phone=p"));
        let st = State {
            port: 7380,
            listen: "127.0.0.1:7380".into(),
            tunnels: vec![tunnel::TunnelInfo { kind: Kind::Tailscale, state: "up".into(), url: Some("https://box.tail.ts.net".into()), error: None, reach: "tailnet".into(), pid: None }],
            ..Default::default()
        };
        let u = urls(&st, "abc", Some("device-a64-x"));
        assert_eq!(u[0].2, "https://box.tail.ts.net/?t=abc&phone=device-a64-x");
        assert_eq!(u[1].2, "http://127.0.0.1:7380/?t=abc&phone=device-a64-x");
        assert_eq!(listen_urls("[::1]:7380", 7380).len(), 1);
        assert_eq!(listen_urls("192.168.1.5:7380", 7380)[1].1, "http://192.168.1.5:7380/");
        assert!(qr_text("https://example.com").unwrap().lines().count() > 10);
    }
}
