//! Share: a phone in a browser, from the launcher (x86sim_core::share; the server is the x86sim-serve process the
//! CLI and the MCP server use too, found through its state file). The tool bar's globe opens the phone in the
//! default browser (starting the server on loopback); the side panel's Share section turns tunnels on and off,
//! copies control / view links and stops sharing. Server calls can take seconds (a tunnel coming up): they run on a
//! thread, the panel shows the last state it polled.
use crate::theme::*;
use crate::App;
use eframe::egui;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use x86sim_core::instance;
use x86sim_core::share::{self, Kind, Role};

#[derive(Default)]
struct Ui {
    state: Option<share::State>,
    avail: Option<share::tunnel::Available>,
    polled: Option<Instant>,
    busy: Option<String>,
    /// a result to show once (said in the launcher's message line)
    note: Option<String>,
}

fn ui_state() -> &'static Mutex<Ui> {
    static U: OnceLock<Mutex<Ui>> = OnceLock::new();
    U.get_or_init(Default::default)
}

/// Polls the server state every 2 s (on a thread: tailscale status can take a moment)
fn poll(ctx: &egui::Context) {
    let due = {
        let mut u = ui_state().lock().unwrap();
        let due = u.polled.is_none_or(|t| t.elapsed() > Duration::from_secs(2));
        if due {
            u.polled = Some(Instant::now());
        }
        due
    };
    if due {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let st = share::status();
            let need_avail = ui_state().lock().unwrap().avail.is_none();
            let avail = need_avail.then(share::tunnel::available);
            let mut u = ui_state().lock().unwrap();
            let changed = serde_json::to_string(&u.state).ok() != serde_json::to_string(&st).ok() || avail.is_some();
            u.state = st;
            if avail.is_some() {
                u.avail = avail;
            }
            if changed {
                ctx.request_repaint();
            }
        });
    }
}

/// Runs a server operation on a thread; `label` shows while it runs
fn run(ctx: &egui::Context, label: &str, f: impl FnOnce() -> anyhow::Result<String> + Send + 'static) {
    {
        let mut u = ui_state().lock().unwrap();
        if u.busy.is_some() {
            return;
        }
        u.busy = Some(label.to_string());
    }
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = f();
        let st = share::status();
        let mut u = ui_state().lock().unwrap();
        u.busy = None;
        u.state = st;
        u.polled = Some(Instant::now());
        u.note = Some(match r {
            Ok(m) => m,
            Err(e) => format!("Sharing: {e:#}"),
        });
        ctx.request_repaint();
    });
}

fn open_url(u: &str) -> anyhow::Result<()> {
    std::process::Command::new("xdg-open").arg(u).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
    Ok(())
}

fn copy(ctx: &egui::Context, text: &str) {
    ctx.copy_text(text.to_string());
}

/// Opens a phone (or the dashboard) in the default browser, starting the server if needed
pub fn open_in_browser(ctx: &egui::Context, phone: Option<String>) {
    run(ctx, "Starting the web server…", move || {
        let st = share::start(&share::Options::default())?;
        let u = share::share_urls(&st, Role::Control, phone.as_deref()).into_iter().find(|(k, _, _)| k == "local").map(|x| x.2).unwrap_or(st.local_url.clone());
        open_url(&u)?;
        Ok("Opened in your browser".into())
    });
}

impl App {
    /// Takes a finished operation's message into the launcher's message line
    pub fn share_note(&mut self) {
        let note = ui_state().lock().unwrap().note.take();
        if let Some(n) = note {
            self.say(n);
        }
    }

    /// The tool bar's button: the phone in a browser tab
    pub fn share_tool(&mut self, ui: &mut egui::Ui, inst: &instance::Instance) {
        poll(ui.ctx());
        let sharing = ui_state().lock().unwrap().state.as_ref().is_some_and(|s| s.tunnels.iter().any(|t| t.state == "up"));
        let b = if sharing { Btn::tinted("", pal().good) } else { Btn::ghost("") }.icon(egui_phosphor::regular::GLOBE);
        let tip = if sharing { "Open in a browser (shared through a tunnel: see Share in the side panel)" } else { "Open in a browser: watch and drive it from any browser tab (Share in the side panel for other machines)" };
        if ui.add(b).on_hover_text(tip).clicked() {
            open_in_browser(ui.ctx(), Some(inst.id.clone()));
        }
    }

    /// The side panel's Share section
    pub fn share_section(&mut self, ui: &mut egui::Ui, inst: &instance::Instance) {
        poll(ui.ctx());
        let (st, avail, busy) = {
            let u = ui_state().lock().unwrap();
            (u.state.clone(), u.avail.clone(), u.busy.clone())
        };
        let ctx = ui.ctx().clone();
        let id = inst.id.clone();
        ui.label(muted("Watch and drive this device from a browser: here, on your tailnet or anywhere. Every link carries a token."));
        ui.add_space(S2);
        if let Some(b) = &busy {
            busy_line(ui, b);
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(busy.is_none(), Btn::primary("Open in browser").small().icon(egui_phosphor::regular::GLOBE)).clicked() {
                open_in_browser(&ctx, Some(id.clone()));
            }
            if let Some(st) = &st {
                if ui.add_enabled(busy.is_none(), Btn::danger("Stop sharing").small()).on_hover_text("Stops the web server, its tunnels and every link").clicked() {
                    run(&ctx, "Stopping…", || share::stop().map(|_| "Sharing stopped".into()));
                }
                let _ = st;
            }
        });
        ui.add_space(S2);
        // tunnels: one row each, with what it needs when it can't be used
        let a = avail.clone();
        let rows: [(Kind, &str, bool, String); 3] = [
            (Kind::Tailscale, "Tailscale", a.as_ref().is_some_and(|a| a.tailscale_ready), a.as_ref().map(|a| a.tailscale_detail.clone()).unwrap_or_default()),
            (Kind::Funnel, "Funnel (public)", a.as_ref().is_some_and(|a| a.tailscale_ready), "through Tailscale".into()),
            (Kind::Cloudflared, "Cloudflare", a.as_ref().is_some_and(|a| a.cloudflared), a.as_ref().map(|a| if a.cloudflared { "trycloudflare.com quick tunnel".into() } else { "cloudflared not installed".into() }).unwrap_or_default()),
        ];
        for (kind, label, ok, detail) in rows {
            let t = st.as_ref().and_then(|s| s.tunnels.iter().find(|t| t.kind == kind).cloned());
            let on = t.as_ref().is_some_and(|t| t.state == "up" || t.state == "starting");
            ui.horizontal(|ui| {
                let w = ui.label(body(label)).rect.width();
                ui.add_space((110.0 - w).max(0.0));
                let r = ui.add_enabled(busy.is_none() && (ok || on), if on { Btn::secondary("Turn off") } else { Btn::secondary("Turn on") }.small());
                if r.clicked() {
                    let k = kind.clone();
                    if on {
                        run(&ctx, "Stopping the tunnel…", move || {
                            let st = share::status().ok_or_else(|| anyhow::anyhow!("not running"))?;
                            share::api(&st, "POST", "/api/tunnel", Some(serde_json::json!({"kind": k.name(), "on": false})))?;
                            Ok(format!("{} tunnel off", k.name()))
                        });
                    } else {
                        run(&ctx, "Starting the tunnel…", move || {
                            let st = share::start(&share::Options { tunnels: vec![k.clone()], ..Default::default() })?;
                            match st.tunnels.iter().find(|t| t.kind == k) {
                                Some(t) if t.state == "up" => Ok(format!("Shared: {}", t.url.clone().unwrap_or_default())),
                                Some(t) => anyhow::bail!("{} tunnel {}: {}", k.name(), t.state, t.error.clone().unwrap_or_default()),
                                None => anyhow::bail!("the tunnel did not start"),
                            }
                        });
                    }
                }
            });
            let line = match &t {
                Some(t) if t.state == "up" => t.url.clone().unwrap_or_default(),
                Some(t) if t.state == "failed" => format!("failed: {}", t.error.clone().unwrap_or_default()),
                Some(t) => t.state.clone(),
                None => detail,
            };
            ui.label(muted(line)).on_hover_text("Tailscale: HTTPS for your tailnet only. Funnel and Cloudflare: a public HTTPS address (the link's token still guards it).");
            ui.add_space(2.0);
        }
        // the links for this phone: control (watch and drive) and view only, the remote one first
        if let Some(st) = &st {
            ui.add_space(S2);
            for (role, label) in [(Role::Control, "Control link"), (Role::View, "View-only link")] {
                let urls = share::share_urls(st, role, Some(&id));
                if let Some((kind, _, u)) = urls.first() {
                    ui.horizontal(|ui| {
                        if ui.add(Btn::secondary(format!("Copy {label}")).small().icon(egui_phosphor::regular::COPY)).on_hover_text(u.as_str()).clicked() {
                            copy(&ctx, u);
                            self.say(format!("Copied the {} {} ({kind})", label.to_lowercase(), if kind == "local" { "for this machine" } else { "link" }));
                        }
                    });
                }
            }
            if let Some(e) = st.expires {
                ui.label(muted(format!("Links expire in {} min", e.saturating_sub(share::now_s()) / 60)));
            }
        }
    }
}

/// The folded section's summary: whether and how the phones are shared
pub fn summary() -> String {
    let u = ui_state().lock().unwrap();
    match &u.state {
        None => String::new(),
        Some(st) => {
            let up: Vec<&str> = st.tunnels.iter().filter(|t| t.state == "up").map(|t| t.kind.name()).collect();
            if up.is_empty() { "on this machine".into() } else { up.join(", ") }
        }
    }
}

fn busy_line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(12.0));
        ui.label(muted(text));
    });
}
