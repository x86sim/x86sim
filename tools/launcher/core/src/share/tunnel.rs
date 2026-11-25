//! Share: reaching the server from elsewhere. Tailscale (`tailscale serve`: HTTPS on the tailnet only, with the
//! machine's ts.net certificate; `tailscale funnel`: the same, public) and cloudflared (a quick tunnel on
//! trycloudflare.com, or a named tunnel the user configured with its hostname). Both proxy to the loopback server, which
//! then requires a token from everything they bring (server.rs: proxied requests are never "local").
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tailscale,
    Funnel,
    Cloudflared,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "tailscale" | "ts" | "tailnet" => Some(Kind::Tailscale),
            "funnel" | "tailscale-funnel" => Some(Kind::Funnel),
            "cloudflared" | "cloudflare" | "cf" => Some(Kind::Cloudflared),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Tailscale => "tailscale",
            Kind::Funnel => "funnel",
            Kind::Cloudflared => "cloudflared",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub kind: Kind,
    /// starting, up, failed
    pub state: String,
    pub url: Option<String>,
    pub error: Option<String>,
    /// who can reach it: "tailnet" or "public"
    pub reach: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// The settings a named cloudflared tunnel needs (else a quick tunnel)
#[derive(Clone, Debug, Default)]
pub struct CfNamed {
    pub name: String,
    pub hostname: String,
}

pub struct Tunnel {
    pub info: Arc<Mutex<TunnelInfo>>,
    child: Option<Child>,
    /// tailscale: the HTTPS port we configured (turned off again at stop)
    ts_port: Option<u16>,
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let env = format!("X86SIM_{}", name.to_uppercase());
    if let Some(p) = std::env::var_os(env).map(std::path::PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// What can be used here: (installed, logged in / ready, detail)
#[derive(Clone, Debug, Serialize)]
pub struct Available {
    pub tailscale: bool,
    pub tailscale_ready: bool,
    pub tailscale_name: Option<String>,
    pub tailscale_detail: String,
    pub cloudflared: bool,
    pub cloudflared_detail: String,
}

pub fn available() -> Available {
    let mut a = Available { tailscale: false, tailscale_ready: false, tailscale_name: None, tailscale_detail: "not installed".into(), cloudflared: false, cloudflared_detail: "not installed".into() };
    if let Some(ts) = which("tailscale") {
        a.tailscale = true;
        match ts_status(&ts) {
            Ok(name) => {
                a.tailscale_ready = true;
                a.tailscale_detail = format!("logged in as {name}");
                a.tailscale_name = Some(name);
            }
            Err(e) => a.tailscale_detail = e,
        }
    }
    if let Some(cf) = which("cloudflared") {
        a.cloudflared = true;
        a.cloudflared_detail = Command::new(cf).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    }
    a
}

/// The machine's MagicDNS name when tailscale is up and logged in
fn ts_status(ts: &std::path::Path) -> Result<String, String> {
    let out = Command::new(ts).args(["status", "--json"]).stderr(Stdio::null()).output().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| "tailscaled not running".to_string())?;
    let st = v["BackendState"].as_str().unwrap_or("");
    if st != "Running" {
        return Err(format!("tailscale is {st} (run `tailscale up`)"));
    }
    v["Self"]["DNSName"].as_str().map(|s| s.trim_end_matches('.').to_string()).filter(|s| !s.is_empty()).ok_or_else(|| "no MagicDNS name (enable MagicDNS and HTTPS certificates)".into())
}

/// An HTTPS port `tailscale serve` has free: 443 unless something is served there, else 8443, else 10000 (the ports a
/// funnel may use)
fn ts_free_port(ts: &std::path::Path) -> u16 {
    let out = Command::new(ts).args(["serve", "status", "--json"]).stderr(Stdio::null()).output();
    let v: serde_json::Value = out.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default();
    let used = |p: u16| v["TCP"].get(p.to_string()).is_some();
    [443u16, 8443, 10000].into_iter().find(|p| !used(*p)).unwrap_or(8443)
}

impl Tunnel {
    pub fn start(kind: Kind, local_port: u16, ts_port: Option<u16>, named: Option<CfNamed>) -> Tunnel {
        let reach = if kind == Kind::Tailscale { "tailnet" } else { "public" };
        let info = Arc::new(Mutex::new(TunnelInfo { kind: kind.clone(), state: "starting".into(), url: None, error: None, reach: reach.into(), pid: None }));
        let mut t = Tunnel { info: info.clone(), child: None, ts_port: None };
        let fail = |e: String| {
            let mut i = info.lock().unwrap();
            i.state = "failed".into();
            i.error = Some(e);
        };
        match kind {
            Kind::Tailscale | Kind::Funnel => {
                let Some(ts) = which("tailscale") else {
                    fail("tailscale is not installed (https://tailscale.com/download)".into());
                    return t;
                };
                let name = match ts_status(&ts) {
                    Ok(n) => n,
                    Err(e) => {
                        fail(e);
                        return t;
                    }
                };
                let port = ts_port.unwrap_or_else(|| ts_free_port(&ts));
                let sub = if kind == Kind::Funnel { "funnel" } else { "serve" };
                // `tailscale serve` waits forever when Serve / Funnel is not enabled for the tailnet yet (it prints the
                // admin page to enable it at and polls): read its output with a deadline and hand that link on
                let child = Command::new(&ts)
                    .args([sub, "--bg", "--yes", &format!("--https={port}"), &format!("http://127.0.0.1:{local_port}")])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn();
                match child {
                    Ok(mut c) => {
                        let text = Arc::new(Mutex::new(String::new()));
                        for pipe in [c.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>), c.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)].into_iter().flatten() {
                            let text = text.clone();
                            std::thread::spawn(move || {
                                for l in BufReader::new(pipe).lines().map_while(Result::ok) {
                                    let mut t = text.lock().unwrap();
                                    t.push_str(&l);
                                    t.push('\n');
                                }
                            });
                        }
                        let end = Instant::now() + Duration::from_secs(20);
                        let status = loop {
                            if let Ok(Some(st)) = c.try_wait() {
                                break Some(st);
                            }
                            if Instant::now() > end || text.lock().unwrap().contains("To enable") {
                                std::thread::sleep(Duration::from_millis(300));
                                let _ = c.kill();
                                let _ = c.wait();
                                break None;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        };
                        std::thread::sleep(Duration::from_millis(50));
                        let msg: String = text.lock().unwrap().split_whitespace().collect::<Vec<_>>().join(" ");
                        match status {
                            Some(st) if st.success() => {
                                t.ts_port = Some(port);
                                let url = if port == 443 { format!("https://{name}") } else { format!("https://{name}:{port}") };
                                let mut i = info.lock().unwrap();
                                i.state = "up".into();
                                i.url = Some(url);
                            }
                            // the usual reasons: Serve / Funnel or HTTPS certificates not enabled for the tailnet, no
                            // operator rights (sudo tailscale set --operator=$USER)
                            _ => fail(format!("tailscale {sub} failed: {}", if msg.is_empty() { "no answer".to_string() } else { msg.chars().take(600).collect() })),
                        }
                    }
                    Err(e) => fail(format!("tailscale {sub}: {e}")),
                }
            }
            Kind::Cloudflared => {
                let Some(cf) = which("cloudflared") else {
                    fail("cloudflared is not installed (https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/)".into());
                    return t;
                };
                let mut cmd = Command::new(cf);
                cmd.args(["tunnel", "--no-autoupdate"]);
                let origin = format!("http://127.0.0.1:{local_port}");
                match &named {
                    Some(n) => {
                        cmd.args(["run", "--url", &origin, &n.name]);
                    }
                    None => {
                        cmd.args(["--url", &origin]);
                    }
                }
                cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
                match cmd.spawn() {
                    Ok(mut c) => {
                        info.lock().unwrap().pid = Some(c.id());
                        let err = c.stderr.take().unwrap();
                        let (info2, named2) = (info.clone(), named.clone());
                        // cloudflared logs to stderr: the quick tunnel's URL, then "Registered tunnel connection"
                        std::thread::spawn(move || {
                            let mut last = String::new();
                            for line in BufReader::new(err).lines().map_while(Result::ok) {
                                let mut i = info2.lock().unwrap();
                                if i.url.is_none() && named2.is_none() {
                                    if let Some(u) = quick_url(&line) {
                                        i.url = Some(u);
                                    }
                                }
                                if line.contains("Registered tunnel connection") {
                                    if let Some(n) = &named2 {
                                        i.url = Some(format!("https://{}", n.hostname));
                                    }
                                    if i.url.is_some() {
                                        i.state = "up".into();
                                    }
                                }
                                if line.contains(" ERR ") {
                                    last = line.chars().take(400).collect();
                                }
                                drop(i);
                            }
                            let mut i = info2.lock().unwrap();
                            if i.state != "stopped" {
                                i.state = "failed".into();
                                i.error = Some(if last.is_empty() { "cloudflared exited".into() } else { last });
                            }
                        });
                        t.child = Some(c);
                    }
                    Err(e) => fail(format!("cloudflared: {e}")),
                }
            }
        }
        t
    }

    /// Waits up to `d` for the tunnel to come up or fail
    pub fn wait(&self, d: Duration) {
        let end = Instant::now() + d;
        while Instant::now() < end {
            if self.info.lock().unwrap().state != "starting" {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn stop(&mut self) {
        self.info.lock().unwrap().state = "stopped".into();
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(port) = self.ts_port.take() {
            let kind = self.info.lock().unwrap().kind.clone();
            ts_off(kind == Kind::Funnel, port);
        }
    }
}

/// Turns our `tailscale serve` / `funnel` port off again (also used to clean up after a server that died)
pub fn ts_off(funnel: bool, port: u16) {
    if let Some(ts) = which("tailscale") {
        let sub = if funnel { "funnel" } else { "serve" };
        let _ = Command::new(ts).args([sub, "--yes", &format!("--https={port}"), "off"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The https://<words>.trycloudflare.com URL in a cloudflared log line
fn quick_url(line: &str) -> Option<String> {
    let i = line.find("https://")?;
    let rest = &line[i..];
    let end = rest.find(|c: char| c.is_whitespace() || c == '|' || c == '"').unwrap_or(rest.len());
    let u = &rest[..end];
    (u.ends_with(".trycloudflare.com") && u != "https://api.trycloudflare.com").then(|| u.to_string())
}

impl Tunnel {
    pub fn ts_port(&self) -> Option<u16> {
        self.ts_port
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_the_quick_tunnel_url() {
        let l = "2026-11-24T10:00:00Z INF |  https://brave-lion-river-cloud.trycloudflare.com                                   |";
        assert_eq!(super::quick_url(l).as_deref(), Some("https://brave-lion-river-cloud.trycloudflare.com"));
        assert_eq!(super::quick_url("INF Requesting new quick Tunnel on trycloudflare.com..."), None);
        assert_eq!(super::quick_url("https://api.trycloudflare.com"), None);
    }
}
