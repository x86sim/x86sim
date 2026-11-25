//! Share: Web Inspector in the browser (`x86sim inspect`, the viewer's Inspect button). Control role only: an inspector
//! runs JavaScript in the phone's pages, so a view link never gets here.
//!
//! - `GET /inspect/<phone>`: the phone's inspectable pages (crate::rwi::targets), each a link to the inspector
//! - `GET /api/web/<phone>`: the same as JSON
//! - `GET /inspector/<path>`: WebKit's own Web Inspector frontend (WebInspectorUI: Main.html and its scripts), the files
//!   from X86SIM_INSPECTOR_UI (e.g. a WebKit checkout's Source/WebInspectorUI), else the copy x86Sim ships (release
//!   AppImages; build/webinspector.sh in a checkout: WebKitGTK's build of it, pinned), else the host WebKitGTK's
//!   GResources (`gresource extract`, once, into a cache dir). Its BrowserInspectorFrontendHost opens
//!   a WebSocket to `?ws=<host>/<path>` and speaks the WebKit inspector protocol over it, so no translation is needed;
//!   Main.js is served with `wss://` behind an HTTPS tunnel instead of its hardcoded `ws://`.
//! - `GET /inspect-ws/<phone>/<app id>/<page id>`: that WebSocket, bridged to an RWI session on the page
//!   (crate::rwi::Session): every text message both ways as is.
use super::server::{local_trusted, Grant, Req, Server};
use super::{ws, Role};
use crate::instance::{self, Instance};
use crate::rwi;
use serde_json::{json, Value};
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// Whether a request is one of ours (by its first path segment)
pub fn wants(path: &str) -> bool {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    matches!(first, "inspect" | "inspector" | "inspect-ws") || path.starts_with("/api/web/")
}

fn respond(s: &mut TcpStream, code: u16, ctype: &str, extra: &[(&str, String)], body: &[u8]) {
    let reason = match code {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
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

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Serves one of our requests (`wants` said yes); the grant has been checked by the caller
pub fn handle(_srv: &Arc<Server>, mut s: TcpStream, req: Req, g: Grant) {
    if g.role != Role::Control {
        return json_resp(&mut s, 403, &json!({"error": "Web Inspector needs a control link (it runs code in the device's pages)"}));
    }
    let seg: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    let visible = |id: &str| g.phone.as_deref().is_none_or(|p| p == id);
    let phone = |id: &str| -> Option<Instance> { instance::find(id).filter(|i| visible(&i.id)) };
    match (req.method.as_str(), seg.as_slice()) {
        ("GET", ["inspector", rest @ ..]) if !rest.is_empty() => serve_ui(&mut s, &rest.join("/"), &req),
        ("GET", ["api", "web", id]) => {
            let Some(inst) = phone(id) else { return json_resp(&mut s, 404, &json!({"error": "no such device"})) };
            let host = req.headers.get("x-forwarded-host").or(req.headers.get("host")).cloned().unwrap_or_default();
            match rwi::targets(&inst, Duration::from_secs(3)) {
                Ok(apps) => {
                    // each page with the inspector's URL (the viewer's Inspect buttons)
                    let mut v = serde_json::to_value(&apps).unwrap_or_default();
                    for a in v.as_array_mut().into_iter().flatten() {
                        for p in a["pages"].as_array_mut().into_iter().flatten() {
                            let href = inspector_href(&inst, &host, p["app_id"].as_str().unwrap_or(""), p["page"].as_i64().unwrap_or(0));
                            p["href"] = json!(href);
                        }
                    }
                    let ui = ui_dir().map(|_| Value::Null).unwrap_or_else(|e| json!(format!("{e:#}")));
                    json_resp(&mut s, 200, &json!({"phone": inst.id, "apps": v, "ui_error": ui}))
                }
                Err(e) => json_resp(&mut s, 409, &json!({"error": format!("{e:#}")})),
            }
        }
        ("GET", ["inspect", id]) => {
            let Some(inst) = phone(id) else { return respond(&mut s, 404, "text/plain", &[], b"no such device") };
            let mut extra = vec![("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; img-src data:; frame-ancestors 'none'".to_string())];
            if let (Some(t), Some(_)) = (req.query.get("t"), g.token.as_ref()) {
                let secure = req.headers.contains_key("x-forwarded-proto") || req.headers.contains_key("cf-ray") || req.headers.contains_key("tailscale-user-login");
                extra.push(("Set-Cookie", format!("x86sim_t={t}; Path=/; HttpOnly; SameSite=Strict{}", if secure { "; Secure" } else { "" })));
            }
            let host = req.headers.get("x-forwarded-host").or(req.headers.get("host")).cloned().unwrap_or_default();
            // ?page=<app id>/<page id> (x86sim inspect --url): straight to the inspector, with the cookie set
            if let Some((app, page)) = req.query.get("page").and_then(|p| p.rsplit_once('/')) {
                extra.push(("Location", inspector_href(&inst, &host, app, page.parse().unwrap_or(0))));
                return respond(&mut s, 302, "text/plain", &extra, b"");
            }
            respond(&mut s, 200, "text/html; charset=utf-8", &extra, list_page(&inst, &host).as_bytes())
        }
        ("GET", ["inspect-ws", id, app, page]) => {
            let Some(inst) = phone(id) else { return json_resp(&mut s, 404, &json!({"error": "no such device"})) };
            // from our own origin only (as the viewer's socket): a page elsewhere must not drive the inspector
            let host = req.headers.get("host").cloned().unwrap_or_default();
            if let Some(o) = req.headers.get("origin") {
                let oh = o.split("://").nth(1).unwrap_or("");
                let fwd = req.headers.get("x-forwarded-host").cloned().unwrap_or_default();
                if !oh.eq_ignore_ascii_case(&host) && !oh.eq_ignore_ascii_case(&fwd) {
                    return json_resp(&mut s, 403, &json!({"error": "cross-origin socket"}));
                }
            } else if g.token.is_none() && !local_trusted(&req) {
                return json_resp(&mut s, 403, &json!({"error": "no origin"}));
            }
            let Some(key) = req.headers.get("sec-websocket-key") else { return json_resp(&mut s, 400, &json!({"error": "not a websocket"})) };
            let Ok(page_id) = page.parse::<i64>() else { return json_resp(&mut s, 400, &json!({"error": "bad page id"})) };
            let wp = rwi::WebPage {
                id: format!("{app}/{page_id}"),
                app_id: app.to_string(),
                page: page_id,
                title: String::new(),
                url: String::new(),
                kind: "web-page".into(),
                bundle: String::new(),
                app_name: String::new(),
            };
            let session = match rwi::Session::open(&inst, &wp) {
                Ok(x) => x,
                Err(e) => return json_resp(&mut s, 409, &json!({"error": format!("{e:#}")})),
            };
            let resp = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", ws::accept_key(key));
            if s.write_all(resp.as_bytes()).is_err() {
                return;
            }
            bridge(s, session);
        }
        _ => json_resp(&mut s, 404, &json!({"error": "not found"})),
    }
}

/// The frontend's socket <-> the RWI session, until either side goes
fn bridge(s: TcpStream, mut session: rwi::Session) {
    let Ok(mut r) = s.try_clone() else { return };
    let w = s;
    let _ = w.set_write_timeout(Some(Duration::from_secs(20)));
    let _ = r.set_read_timeout(Some(Duration::from_millis(1000)));
    let (tx, rx) = mpsc::channel::<String>();
    // frames from the browser; pings answered through the shared writer
    let w2 = Arc::new(Mutex::new(w.try_clone().ok()));
    let pong = w2.clone();
    let reader = std::thread::spawn(move || loop {
        match ws::read(&mut r) {
            Ok(ws::Msg::Text(t)) => {
                if tx.send(t).is_err() {
                    break;
                }
            }
            Ok(ws::Msg::Ping(p)) => {
                if let Some(w) = pong.lock().unwrap().as_mut() {
                    let _ = ws::write(w, 0xa, &p);
                }
            }
            Ok(ws::Msg::Close) => break,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    });
    let ok = loop {
        // to the page
        let mut gone = false;
        loop {
            match rx.try_recv() {
                Ok(m) => {
                    if session.send(&m).is_err() {
                        gone = true;
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        if gone {
            break true;
        }
        // from the page
        match session.recv(Duration::from_millis(30)) {
            Ok(Some(m)) => {
                let mut g = w2.lock().unwrap();
                let Some(w) = g.as_mut() else { break true };
                if ws::write(w, 1, m.as_bytes()).is_err() {
                    break true;
                }
            }
            Ok(None) => {}
            Err(_) => break false,
        }
    };
    if !ok {
        if let Some(w) = w2.lock().unwrap().as_mut() {
            let _ = ws::write(w, 8, &[]);
        }
    }
    session.close();
    let _ = w.shutdown(std::net::Shutdown::Both);
    let _ = reader.join();
}

/// The frontend's URL for a page: Main.html with its socket (and the platform for its "connected to" line)
fn inspector_href(inst: &Instance, host: &str, app: &str, page: i64) -> String {
    use super::server::pct_encode;
    let ws = format!("{host}/inspect-ws/{}/{}/{page}", pct_encode(&inst.id), pct_encode(app));
    let (platform, version) = inst.ios.split_once(' ').unwrap_or((&inst.ios, ""));
    format!("/inspector/UserInterface/Main.html?ws={}&platform={}&version={}&build={}", pct_encode(&ws), pct_encode(platform), pct_encode(version), pct_encode(&inst.build))
}

fn list_page(inst: &Instance, host: &str) -> String {
    let body = match rwi::targets(inst, Duration::from_secs(3)) {
        Err(e) => format!("<p class=err>{}</p>", esc(&format!("{e:#}"))),
        Ok(apps) => {
            let ui = ui_dir();
            let mut h = String::new();
            if let Err(e) = &ui {
                h.push_str(&format!("<p class=err>No Web Inspector frontend: {}</p>", esc(&format!("{e:#}"))));
            }
            let n: usize = apps.iter().map(|a| a.pages.len()).sum();
            if n == 0 {
                h.push_str("<p>Nothing to inspect yet: open a page in Safari, or an app whose WKWebView sets <code>isInspectable = true</code>. This page reloads every 5 s.</p>");
            }
            for a in apps.iter().filter(|a| !a.pages.is_empty()) {
                let name = if a.name.is_empty() { a.bundle.clone() } else { a.name.clone() };
                h.push_str(&format!("<h2>{} <small>{} · {}</small></h2><ul>", esc(&name), esc(&a.bundle), esc(&a.id)));
                for p in &a.pages {
                    let href = inspector_href(inst, host, &p.app_id, p.page);
                    let title = if p.title.is_empty() { "(untitled)" } else { &p.title };
                    let link = if ui.is_ok() { format!("<a href=\"{}\" target=_blank>Inspect</a>", esc(&href)) } else { String::new() };
                    h.push_str(&format!("<li>{link} <b>{}</b> <span class=k>{}</span><br><code>{}</code></li>", esc(title), esc(&p.kind), esc(&p.url)));
                }
                h.push_str("</ul>");
            }
            h
        }
    };
    format!(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><meta http-equiv=refresh content=5>\
         <title>Inspect · {name}</title><style>\
         :root{{color-scheme:light dark;--bg:#fff;--fg:#1d1d1f;--mut:#6e6e73;--acc:#0a66d6}}\
         @media (prefers-color-scheme:dark){{:root{{--bg:#1c1c1e;--fg:#f2f2f7;--mut:#98989d;--acc:#4ea1ff}}}}\
         body{{background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,sans-serif;margin:0 auto;max-width:900px;padding:16px}}\
         h1{{font-size:20px}}h2{{font-size:16px;margin:20px 0 6px}}small,.k{{color:var(--mut);font-weight:normal;font-size:12px}}\
         ul{{list-style:none;padding:0}}li{{padding:8px 0;border-bottom:1px solid #8883}}code{{word-break:break-all;color:var(--mut)}}\
         a{{color:var(--acc);font-weight:600;margin-right:8px}}.err{{color:#d70015}}</style></head>\
         <body><h1>Web Inspector · {name}</h1>{body}</body></html>",
        name = esc(&inst.name)
    )
}

// ---------------------------------------------------------------------------------------------------------------------
// x86sim inspect

pub const USAGE: &str = "usage: x86sim inspect [PHONE] [--url PATTERN] [--list] [--no-open]
  The phone's inspectable web pages (Safari's tabs, apps' WKWebViews with isInspectable = true), and WebKit's Web
  Inspector for them in your browser (served by x86sim serve, started if needed).
  --url PATTERN  open the inspector on the first page whose URL or title contains PATTERN (or an id: PID:123/1)
  --list         only list the pages
  --no-open      print the URL instead of opening a browser";

/// `x86sim inspect` (x86sim-serve inspect ...): exit status
pub fn cli(args: &[String]) -> i32 {
    let (mut phone, mut url, mut list, mut open) = (None::<String>, None::<String>, false, true);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--url" | "-u" if i + 1 < args.len() => {
                url = Some(args[i + 1].clone());
                i += 1;
            }
            "--phone" if i + 1 < args.len() => {
                phone = Some(args[i + 1].clone());
                i += 1;
            }
            "--list" | "-l" => list = true,
            "--no-open" => open = false,
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            a if !a.starts_with('-') && phone.is_none() => phone = Some(a.to_string()),
            a => {
                eprintln!("x86sim inspect: unknown argument {a}\n{USAGE}");
                return 2;
            }
        }
        i += 1;
    }
    let inst = match phone.as_deref() {
        Some(p) => instance::find(p),
        None => {
            let running: Vec<_> = instance::summaries(0).into_iter().filter(|s| s.state != "off").collect();
            match running.as_slice() {
                [one] => instance::find(&one.id),
                _ => std::env::var("X86SIM_DEVICE").ok().and_then(|d| instance::find(&d)),
            }
        }
    };
    let Some(inst) = inst else {
        eprintln!("x86sim inspect: which device? ({})", instance::list().iter().map(|i| i.name.clone()).collect::<Vec<_>>().join(", "));
        return 2;
    };
    let apps = match rwi::targets(&inst, Duration::from_secs(3)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("x86sim inspect: {e:#}");
            return 1;
        }
    };
    for a in &apps {
        for p in &a.pages {
            println!("{:14} {:24} {:14} {}  {}", p.id, if a.name.is_empty() { &a.bundle } else { &a.name }, p.kind, p.title, p.url);
        }
    }
    if list {
        return 0;
    }
    let page = match url.as_deref() {
        Some(u) => match rwi::pick(&apps, Some(u)) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("x86sim inspect: {e:#}");
                return 1;
            }
        },
        None => None,
    };
    if let Err(e) = ui_dir() {
        eprintln!("x86sim inspect: {e:#} (the web_* MCP tools work without it)");
        return 1;
    }
    let st = match super::status() {
        Some(st) => st,
        None => match super::start(&super::Options::default()) {
            Ok(st) => st,
            Err(e) => {
                eprintln!("x86sim inspect: starting x86sim serve: {e:#}");
                return 1;
            }
        },
    };
    let mut u = format!("{}inspect/{}?t={}", st.local_url, super::server::pct_encode(&inst.id), st.control_token);
    if let Some(p) = &page {
        u.push_str(&format!("&page={}", super::server::pct_encode(&p.id)));
    }
    println!("{u}");
    if open {
        let _ = std::process::Command::new("xdg-open").arg(&u).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    }
    0
}

// ---------------------------------------------------------------------------------------------------------------------
// The frontend files

const RES_PREFIX: &str = "/org/webkit/inspector/";

/// The host's WebKitGTK library that carries the inspector's GResources
fn webkit_lib() -> Option<PathBuf> {
    let names = ["libwebkit2gtk-4.1.so.0", "libwebkitgtk-6.0.so.4", "libwebkit2gtk-4.0.so.37"];
    let dirs = ["/usr/lib", "/usr/lib64", "/usr/lib/x86_64-linux-gnu", "/usr/local/lib", "/app/lib"];
    dirs.iter().flat_map(|d| names.iter().map(move |n| Path::new(d).join(n))).find(|p| p.exists())
}

/// The frontend copies shipped with x86Sim, first found wins: the AppImage's (AppRun copies usr/share/x86sim/webinspector
/// next to the payload, `<app>/webinspector`; `$APPDIR/usr/share/x86sim/webinspector` while the AppImage is mounted),
/// then a checkout's build output (build/webinspector.sh: tools/launcher/target/webinspector). Found from the
/// running binary (`<root>/tools/launcher/target/release/<bin>`), so a worktree's build finds its own.
fn bundled_dirs() -> Vec<(PathBuf, &'static str)> {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let up = |n: usize| exe.as_deref().and_then(|e| e.ancestors().nth(n));
    let mut v = Vec::new();
    v.extend(up(5).map(|root| (root.join("webinspector"), "AppImage")));
    v.extend(std::env::var_os("APPDIR").map(|a| (PathBuf::from(a).join("usr/share/x86sim/webinspector"), "AppImage")));
    v.extend(up(2).map(|target| (target.join("webinspector"), "build output")));
    v
}

/// A directory with UserInterface/Main.html (and Localizations/): X86SIM_INSPECTOR_UI, else the copy x86Sim ships
/// (bundled_dirs), else the host WebKitGTK's resources extracted once into ~/.cache/x86sim/webinspector/<library>-<size>.
/// The choice is logged once (stderr: x86sim serve's log).
pub fn ui_dir() -> anyhow::Result<PathBuf> {
    static CACHE: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap();
    if let Some(p) = c.as_ref() {
        return Ok(p.clone());
    }
    let (d, from) = find_ui_dir()?;
    let ver = std::fs::read_to_string(d.join("VERSION")).map(|v| format!(", {}", v.trim())).unwrap_or_default();
    eprintln!("x86sim inspect: Web Inspector frontend: {} ({from}{ver})", d.display());
    *c = Some(d.clone());
    Ok(d)
}

fn find_ui_dir() -> anyhow::Result<(PathBuf, &'static str)> {
    if let Some(d) = std::env::var_os("X86SIM_INSPECTOR_UI").filter(|v| !v.is_empty()).map(PathBuf::from) {
        anyhow::ensure!(d.join("UserInterface/Main.html").exists(), "X86SIM_INSPECTOR_UI={}: no UserInterface/Main.html there", d.display());
        return Ok((d, "X86SIM_INSPECTOR_UI"));
    }
    if let Some((d, from)) = bundled_dirs().into_iter().find(|(d, _)| d.join("UserInterface/Main.html").exists()) {
        return Ok((d, from));
    }
    let lib = webkit_lib().ok_or_else(|| {
        anyhow::anyhow!("no Web Inspector frontend: this build has none (build/webinspector.sh makes it) and WebKitGTK (webkit2gtk-4.1 or webkitgtk-6.0) is not installed; run build/webinspector.sh, install WebKitGTK, or set X86SIM_INSPECTOR_UI to a WebInspectorUI directory")
    })?;
    let real = std::fs::canonicalize(&lib).unwrap_or(lib.clone());
    let size = std::fs::metadata(&real).map(|m| m.len()).unwrap_or(0);
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or_else(|| anyhow::anyhow!("no cache directory"))?
        .join("x86sim/webinspector")
        .join(format!("{}-{size}", real.file_name().unwrap_or_default().to_string_lossy()));
    if !cache.join("UserInterface/Main.html").exists() {
        extract(&real, &cache)?;
    }
    Ok((cache, "host WebKitGTK"))
}

fn extract(lib: &Path, dest: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let out = std::process::Command::new("gresource").arg("list").arg(lib).output().context("running gresource (glib2's tools)")?;
    anyhow::ensure!(out.status.success(), "gresource list {}: {}", lib.display(), String::from_utf8_lossy(&out.stderr).trim());
    let list = String::from_utf8_lossy(&out.stdout);
    let tmp = dest.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut n = 0;
    for r in list.lines().filter(|l| l.starts_with(RES_PREFIX)) {
        let rel = &r[RES_PREFIX.len()..];
        if rel.split('/').any(|c| c == ".." || c.is_empty()) {
            continue;
        }
        let f = std::process::Command::new("gresource").arg("extract").arg(lib).arg(r).output()?;
        anyhow::ensure!(f.status.success(), "gresource extract {r}: {}", String::from_utf8_lossy(&f.stderr).trim());
        let p = tmp.join(rel);
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, &f.stdout)?;
        n += 1;
    }
    anyhow::ensure!(tmp.join("UserInterface/Main.html").exists(), "{}: no Web Inspector frontend in its resources ({n} files)", lib.display());
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest.parent().unwrap())?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

fn mime(p: &str) -> &'static str {
    match p.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Main.js as served: the socket's scheme follows the page's (behind an HTTPS tunnel a ws:// socket is mixed content),
/// and the debuggable is described as the simulator it is (the stub says "not a simulator", no platform)
pub fn patch_main_js(js: &str) -> String {
    js.replacen("\"ws://\"+queryParams.ws", "(location.protocol===\"https:\"?\"wss://\":\"ws://\")+queryParams.ws", 1)
        .replacen(
            "targetPlatformName:undefined,targetBuildVersion:undefined,targetProductVersion:undefined,targetIsSimulator:false,",
            "targetPlatformName:new URLSearchParams(location.search).get(\"platform\")||undefined,targetBuildVersion:new URLSearchParams(location.search).get(\"build\")||undefined,targetProductVersion:new URLSearchParams(location.search).get(\"version\")||undefined,targetIsSimulator:true,",
            1,
        )
}

fn serve_ui(s: &mut TcpStream, rel: &str, _req: &Req) {
    if rel.split('/').any(|c| c == ".." || c.is_empty() || c.starts_with('.')) {
        return respond(s, 400, "text/plain", &[], b"bad path");
    }
    let dir = match ui_dir() {
        Ok(d) => d,
        Err(e) => return respond(s, 404, "text/plain; charset=utf-8", &[], format!("{e:#}").as_bytes()),
    };
    let p = dir.join(rel);
    let Ok(body) = std::fs::read(&p) else { return respond(s, 404, "text/plain", &[], b"not found") };
    let body = if rel == "UserInterface/Main.js" { patch_main_js(&String::from_utf8_lossy(&body)).into_bytes() } else { body };
    // the frontend's own CSP is in Main.html; its files may be cached by the browser for the session
    respond(s, 200, mime(rel), &[], &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_and_patch() {
        assert!(wants("/inspect/device-a64-x"));
        assert!(wants("/inspector/UserInterface/Main.html"));
        assert!(wants("/inspect-ws/a/PID:1/1"));
        assert!(wants("/api/web/x"));
        assert!(!wants("/api/phones"));
        assert!(!wants("/inspectors"));
        let js = "let url=\"ws\"in queryParams?\"ws://\"+queryParams.ws:null;";
        assert!(patch_main_js(js).contains("wss://"));
    }
}
