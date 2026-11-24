//! The hub's Pool section (x86sim_core::pool: phones kept booted for agents; `x86sim pool up N` makes them) and app
//! installs by drag and drop: an .app, .ipa or .xcarchive dropped on the window installs on the open phone (or the only
//! running one), device builds converted first (x86sim_core::devapp).
use crate::theme::*;
use crate::{App, View};
use eframe::egui;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use x86sim_core::{apps, instance, pool};

/// Pool statuses, refreshed by a thread every few seconds while the Devices page shows (reading them touches files and
/// samples the phones: never on the UI thread)
static POOLS: Mutex<(Option<Instant>, Vec<pool::PoolStatus>, bool)> = Mutex::new((None, Vec::new(), false));
/// Messages from background work (installs, releases) for the toast
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(s: String) {
    NOTES.lock().unwrap().push(s);
}

impl App {
    /// Each frame: dropped files install on a phone; finished background work shows as a toast
    pub fn pool_frame(&mut self, ctx: &egui::Context) {
        if let Some(m) = NOTES.lock().unwrap().pop() {
            self.say(m);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        let target = match &self.view {
            View::Phone(id) => Some(id.clone()),
            _ => {
                let on: Vec<&String> = self.status.values().filter(|s| s.phase == instance::Phase::Running).map(|s| &s.id).collect();
                (on.len() == 1).then(|| on[0].clone())
            }
        };
        if hovering {
            let text = match target.as_ref().and_then(|id| self.inst(id)) {
                Some(i) => format!("Drop an .app, .ipa or .xcarchive to install it on {}", i.name),
                None => "Open a running device to install an app on it".into(),
            };
            egui::Area::new("drop-hint".into()).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                card(ui, |ui| {
                    ui.label(strong(text));
                });
            });
        }
        for p in dropped {
            if !x86sim_core::devapp::accepts(&p) {
                self.say(format!("{} is not an .app, .ipa or .xcarchive", p.display()));
                continue;
            }
            let Some(inst) = target.as_ref().and_then(|id| self.inst(id)).cloned() else {
                self.say("Open a running device first, then drop the app on it");
                continue;
            };
            if self.phase(&inst.id) != instance::Phase::Running {
                self.say(format!("Start {} first", inst.name));
                continue;
            }
            self.say(format!("Installing {} on {}…", p.file_name().unwrap_or_default().to_string_lossy(), inst.name));
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let m = match apps::install(&inst, &p) {
                    Ok(r) => format!(
                        "Installed {}{} on {}",
                        if r.bundle_id.is_empty() { p.file_name().unwrap_or_default().to_string_lossy().into_owned() } else { r.bundle_id },
                        r.converted.as_ref().map(|c| format!(" (a device build: {} converted for the simulator)", c.kind)).unwrap_or_default(),
                        inst.name
                    ),
                    Err(e) => format!("Couldn't install on {}: {e:#}", inst.name),
                };
                note(m);
                ctx.request_repaint();
            });
        }
    }

    /// The Devices page's Pool section: shown when a pool exists
    pub fn pool_section(&mut self, ui: &mut egui::Ui) {
        let (stale, list) = {
            let mut g = POOLS.lock().unwrap();
            let stale = !g.2 && g.0.is_none_or(|t| t.elapsed() > Duration::from_secs(5));
            if stale {
                g.2 = true;
            }
            (stale, g.1.clone())
        };
        if stale {
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let v: Vec<pool::PoolStatus> = pool::pools().iter().filter_map(|p| pool::status(p).ok()).collect();
                *POOLS.lock().unwrap() = (Some(Instant::now()), v, false);
                ctx.request_repaint();
            });
        }
        ui.ctx().request_repaint_after(Duration::from_secs(5));
        if list.is_empty() {
            return;
        }
        ui.add_space(S3);
        for s in &list {
            section(ui, &format!("Pool “{}”", s.pool), |_| {});
            let mut release = None;
            list_card(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(muted(format!(
                        "{} · {} of {} free · ~{:.0} MB per device · room for {} more · KSM {} · clones by {}",
                        if s.model.is_empty() { "no model yet" } else { &s.model },
                        s.free,
                        s.members.len(),
                        s.cost_mb,
                        s.room_for,
                        if s.ksm.running { format!("on ({:.0} MB shared)", s.ksm.saved_mb) } else { "off".into() },
                        if s.reflink { "reflink" } else { "full copy" }
                    )));
                    for m in &s.members {
                        ui.horizontal(|ui| {
                            let col = match (m.state.as_str(), m.ready) {
                                ("free", true) => pal().good,
                                ("leased", _) => pal().accent,
                                _ => pal().ink_3,
                            };
                            dot_label(ui, &m.phone, col);
                            let what = match (&m.holder, m.expires_in_s) {
                                (Some(h), Some(e)) => format!("held by {h} · {}:{:02} left · reset {}", e.max(0) / 60, e.max(0) % 60, m.reset.clone().unwrap_or_default()),
                                _ if m.state == "free" && m.ready => "free".into(),
                                _ if m.state == "free" => m.phase.clone(),
                                _ => m.state.clone(),
                            };
                            ui.label(muted(what));
                            if m.state == "leased" {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add(Btn::ghost("Release")).on_hover_text("Give it back now (reset as the holder asked)").clicked() {
                                        release = Some((s.pool.clone(), m.id.clone()));
                                    }
                                });
                            }
                        });
                    }
                });
            });
            if let Some((p, id)) = release {
                std::thread::spawn(move || {
                    note(match pool::release(&p, &id, None) {
                        Ok(m) => format!("Released {id} (reset: {m})"),
                        Err(e) => format!("Couldn't release {id}: {e:#}"),
                    });
                    POOLS.lock().unwrap().0 = None;
                });
            }
        }
    }
}
