//! x86sim-serve: watch and drive x86Sim's phones from a browser (x86sim_core::share). `x86sim serve` runs it.
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use x86sim_core::share::{self, Kind, Options, Role};

const USAGE: &str = "usage: x86sim serve [run] [--listen ADDR:PORT] [--tailscale | --funnel] [--cloudflared [--cf-tunnel NAME --cf-host HOST]]
                    [--ts-port 443|8443|10000] [--ttl 2h] [--require-token] [--phone NAME] [--open] [--bg]
       x86sim serve status | stop [tailscale|funnel|cloudflared]
       x86sim serve link [--phone NAME] [--control] [--ttl 1h]

Serves every phone to a browser: a dashboard (live thumbnails, start / stop, share links) and each phone's screen
(H.264 through WebCodecs, else JPEG) with touch, multi-touch, keyboard, paste and the hardware buttons.
  --listen      default 127.0.0.1:7380 (0.0.0.0:7380 for the LAN: plain HTTP, browsers there get the JPEG stream)
  --tailscale   HTTPS on your tailnet only (tailscale serve); --funnel: the same, public (tailscale funnel)
  --cloudflared a public https://*.trycloudflare.com quick tunnel; with --cf-tunnel/--cf-host a named tunnel you set up
  --ttl         the printed links stop working after this (s, m, h, d)
  --bg          run in the background (stop with `x86sim serve stop`)
Anything not from this machine's own browser needs the token in the link; view links (--view in `link`) only watch.";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// `redact`: the server's own output when it goes to a file (serve.log): tokens cut short (x86sim serve status prints them)
fn print_urls(st: &share::State, phone: Option<&str>) {
    print_urls_r(st, phone, false)
}

fn print_urls_r(st: &share::State, phone: Option<&str>, redact: bool) {
    let r = |v: Vec<(String, String, String)>| -> Vec<(String, String, String)> { v.into_iter().map(|(k, re, u)| (k, re, if redact { share::redact(&u) } else { u })).collect() };
    let ctl = r(share::share_urls(st, Role::Control, phone));
    let view = r(share::share_urls(st, Role::View, phone));
    println!("x86sim serve: pid {}, listening on {}", st.pid, st.listen);
    for t in &st.tunnels {
        match t.state.as_str() {
            "up" => println!("  {} tunnel up ({}): {}", t.kind.name(), t.reach, t.url.as_deref().unwrap_or("")),
            s => println!("  {} tunnel {s}: {}", t.kind.name(), t.error.as_deref().unwrap_or("")),
        }
    }
    println!("control (watch and drive):");
    for (k, _, u) in &ctl {
        println!("  {k:12} {u}");
    }
    println!("view only:");
    for (k, _, u) in &view {
        println!("  {k:12} {u}");
    }
    if let Some(e) = st.expires {
        println!("tokens expire in {} min", e.saturating_sub(share::now_s()) / 60);
    }
    // a QR code of the first remote control link (a phone's camera opens it)
    if let Some((_, _, u)) = ctl.iter().find(|(k, _, _)| k != "local" && !redact) {
        if let Some(q) = share::qr_text(u) {
            println!("{q}");
        }
    }
}

fn open_browser(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn main() {
    // printing into a closed pipe (`| head`) ends quietly; the server itself ignores SIGPIPE (run)
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    // x86sim profile: CPU profile of an app (profile.rs)
    if args.first().map(String::as_str) == Some("profile") {
        std::process::exit(x86sim_core::profile::cli(&args[1..]));
    }
    // x86sim inspect: Web Inspector for a phone's pages (share/inspect.rs)
    if args.first().map(String::as_str) == Some("inspect") {
        std::process::exit(share::inspect::cli(&args[1..]));
    }
    let mut opts = Options::default();
    let (mut bg, mut open, mut phone, mut sub, mut role, mut link_ttl) = (false, false, None::<String>, None::<String>, Role::View, 3600u64);
    let mut stop_what = None;
    let mut i = 0;
    let need = |i: usize| -> String {
        args.get(i + 1).cloned().unwrap_or_else(|| {
            eprintln!("{USAGE}");
            std::process::exit(2)
        })
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "run" | "status" | "stop" | "link" if sub.is_none() => sub = Some(a.to_string()),
            "tailscale" | "funnel" | "cloudflared" if sub.as_deref() == Some("stop") => stop_what = Kind::parse(a),
            "--listen" | "-l" => {
                let mut l = need(i);
                if !l.contains(':') {
                    l = format!("{l}:{}", share::DEFAULT_PORT);
                } else if l.starts_with(':') {
                    l = format!("0.0.0.0{l}");
                }
                opts.listen = l;
                i += 1;
            }
            "--port" => {
                let p = need(i);
                let host = opts.listen.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or("127.0.0.1".into());
                opts.listen = format!("{host}:{p}");
                i += 1;
            }
            "--tailscale" => opts.tunnels.push(Kind::Tailscale),
            "--funnel" => opts.tunnels.push(Kind::Funnel),
            "--cloudflared" => opts.tunnels.push(Kind::Cloudflared),
            "--cf-tunnel" => {
                opts.cf_named.get_or_insert_with(Default::default).name = need(i);
                i += 1;
            }
            "--cf-host" => {
                opts.cf_named.get_or_insert_with(Default::default).hostname = need(i);
                i += 1;
            }
            "--ts-port" => {
                opts.ts_port = need(i).parse().ok();
                i += 1;
            }
            "--ttl" => {
                let t = share::parse_duration(&need(i));
                opts.ttl_s = t;
                link_ttl = t.unwrap_or(3600);
                i += 1;
            }
            "--require-token" => opts.require_token = true,
            "--phone" => {
                phone = Some(need(i));
                i += 1;
            }
            "--control" => role = Role::Control,
            "--view" => role = Role::View,
            "--bg" | "--background" => bg = true,
            "--open" => open = true,
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return;
            }
            o => {
                eprintln!("x86sim serve: unknown argument {o}\n{USAGE}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    if opts.tunnels.contains(&Kind::Funnel) {
        opts.tunnels.retain(|k| *k != Kind::Tailscale);
    }
    if let Some(n) = &opts.cf_named {
        if n.name.is_empty() || n.hostname.is_empty() {
            eprintln!("x86sim serve: a named cloudflared tunnel needs --cf-tunnel NAME and --cf-host HOST");
            std::process::exit(2);
        }
        if !opts.tunnels.contains(&Kind::Cloudflared) {
            opts.tunnels.push(Kind::Cloudflared);
        }
    }
    let phone_id = phone.as_deref().map(|p| match x86sim_core::instance::find(p) {
        Some(i) => i.id,
        None => {
            eprintln!("x86sim serve: no device {p:?}");
            std::process::exit(2)
        }
    });
    match sub.as_deref() {
        Some("status") => match share::status() {
            Some(st) => print_urls(&st, phone_id.as_deref()),
            None => {
                println!("x86sim serve: not running");
                let a = share::tunnel::available();
                println!("  tailscale: {}", a.tailscale_detail);
                println!("  cloudflared: {}", a.cloudflared_detail);
                std::process::exit(1);
            }
        },
        Some("stop") => {
            let r = match stop_what {
                Some(k) => share::status().ok_or_else(|| anyhow::anyhow!("not running")).and_then(|st| share::api(&st, "POST", "/api/tunnel", Some(serde_json::json!({"kind": k.name(), "on": false})))).map(|_| true),
                None => share::stop(),
            };
            match r {
                Ok(true) => println!("x86sim serve: stopped"),
                Ok(false) => println!("x86sim serve: not running"),
                Err(e) => {
                    eprintln!("x86sim serve: {e:#}");
                    std::process::exit(1)
                }
            }
        }
        Some("link") => match share::link(phone_id.as_deref(), role, link_ttl) {
            Ok(v) => {
                for u in v["urls"].as_array().into_iter().flatten() {
                    println!("{:12} {}", u["kind"].as_str().unwrap_or(""), u["url"].as_str().unwrap_or(""));
                }
                println!("{} link, expires in {} min", if role == Role::Control { "control" } else { "view-only" }, link_ttl / 60);
            }
            Err(e) => {
                eprintln!("x86sim serve: {e:#}");
                std::process::exit(1)
            }
        },
        _ if bg || share::status().is_some() => match share::start(&opts) {
            Ok(st) => {
                print_urls(&st, phone_id.as_deref());
                if open {
                    if let Some((_, _, u)) = share::share_urls(&st, Role::Control, phone_id.as_deref()).into_iter().find(|(k, _, _)| k == "local") {
                        open_browser(&u);
                    }
                }
            }
            Err(e) => {
                eprintln!("x86sim serve: {e:#}");
                std::process::exit(1)
            }
        },
        _ => run(opts, phone_id, open),
    }
}

/// The server in this process (foreground; `--bg` and the other tools start it with `run` under setsid)
fn run(opts: Options, phone: Option<String>, open: bool) {
    // a busy port moves on to the next one (up to 20 further)
    let (host, port) = opts.listen.rsplit_once(':').map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap_or(share::DEFAULT_PORT))).unwrap_or(("127.0.0.1".into(), share::DEFAULT_PORT));
    let host = host.trim_matches(|c| c == '[' || c == ']').to_string();
    let mut bound = None;
    for p in port..port.saturating_add(20) {
        if let Ok(l) = std::net::TcpListener::bind((host.as_str(), p)) {
            bound = Some((l, p));
            break;
        }
    }
    let Some((listener, port)) = bound else {
        eprintln!("x86sim serve: cannot listen on {host}:{port}..{}", port.saturating_add(19));
        std::process::exit(1)
    };
    let mut opts = opts;
    opts.listen = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let tunnels = std::mem::take(&mut opts.tunnels);
    let srv = share::server::Server::new(opts, port);
    srv.write_state();
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let s2 = srv.clone();
    std::thread::spawn(move || share::server::run(s2, listener));
    for k in tunnels {
        srv.add_tunnel(k);
    }
    srv.write_state();
    let st = srv.state();
    print_urls_r(&st, phone.as_deref(), unsafe { libc::isatty(1) } == 0);
    if unsafe { libc::isatty(1) } == 0 {
        println!("(tokens cut short here: x86sim serve status prints the full links)");
    }
    let _ = std::io::stdout().flush();
    if open {
        if let Some((_, _, u)) = share::share_urls(&st, Role::Control, phone.as_deref()).into_iter().find(|(k, _, _)| k == "local") {
            open_browser(&u);
        }
    }
    let mut n = 0u64;
    while !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        n += 1;
        // the state file is how the other tools find us: put it back if something removed it
        if n % 25 == 0 && share::status().is_none() {
            srv.write_state();
        }
        if let Some(e) = st.expires {
            if share::now_s() >= e {
                println!("x86sim serve: tokens expired, stopping");
                break;
            }
        }
    }
    srv.shutdown();
    println!("x86sim serve: stopped");
}
