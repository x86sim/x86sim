//! Versions page: Delete for an installed OS version (core rtdelete.rs). The row's Delete opens a confirmation in the
//! row (as Discard does) that measures first, then offers "Delete the version only" (the runtime files; its
//! pre-translated code stays, so the same build downloaded again starts fast) and "Delete everything" (also that code,
//! its files in shared caches and its download log), with the size of each. Phones on the version are named; running
//! ones block the delete and can be stopped from there. Measuring and deleting run on a thread.
use crate::theme::*;
use eframe::egui;
use std::sync::{Arc, Mutex};
use x86sim_core::{host, rtdelete};

#[derive(Default)]
pub struct RtDeleteUi {
    /// the build being confirmed, and its plan once measured
    build: Option<String>,
    plan: Arc<Mutex<Option<rtdelete::Plan>>>,
    /// a delete in progress (build) and its outcome once done
    running: Option<String>,
    done: Arc<Mutex<Option<Result<(String, bool, u64), String>>>>,
}

pub enum Action {
    None,
    /// stop these phones (ids)
    Stop(Vec<String>),
    /// a delete finished: message, refresh the versions
    Finished(String),
}

impl RtDeleteUi {
    pub fn busy(&self, build: &str) -> bool {
        self.running.as_deref() == Some(build)
    }

    pub fn is_open(&self, build: &str) -> bool {
        self.build.as_deref() == Some(build)
    }

    /// Opens the confirmation for `r` and starts measuring it.
    pub fn open(&mut self, r: &host::Runtime, ctx: &egui::Context) {
        self.build = Some(r.build.clone());
        self.measure(r, ctx);
    }

    fn measure(&mut self, r: &host::Runtime, ctx: &egui::Context) {
        *self.plan.lock().unwrap() = None;
        let (slot, r, ctx) = (self.plan.clone(), r.clone(), ctx.clone());
        std::thread::spawn(move || {
            let p = rtdelete::plan(&r);
            *slot.lock().unwrap() = Some(p);
            ctx.request_repaint();
        });
    }

    /// Per frame: a finished delete.
    pub fn poll(&mut self) -> Action {
        let Some(res) = self.done.lock().unwrap().take() else { return Action::None };
        self.running = None;
        Action::Finished(match res {
            Ok((label, all, freed)) => format!("Deleted {label} ({}): {} freed", if all { "everything" } else { "the version only" }, rtdelete::gb(freed)),
            Err(e) => format!("Couldn't delete it: {e}"),
        })
    }

    /// The confirmation under the row of `r` (when open for it).
    pub fn panel(&mut self, ui: &mut egui::Ui, r: &host::Runtime) -> Action {
        if !self.is_open(&r.build) {
            return Action::None;
        }
        let mut act = Action::None;
        let plan = self.plan.lock().unwrap().clone();
        egui::Frame::new().inner_margin(egui::Margin { left: 60, right: 16, top: 0, bottom: 12 }).show(ui, |ui| {
            egui::Frame::new().fill(pal().raised).stroke(egui::Stroke::new(1.0_f32, pal().border)).corner_radius(radius()).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let Some(p) = plan else {
                    busy(ui, "Measuring what it takes on disk…");
                    return;
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = S2;
                    ui.label(egui::RichText::new(icon::WARNING_CIRCLE).family(icons()).size(14.0).color(pal().warn));
                    ui.label(body(format!("Delete {} ({})?", p.label, p.build)));
                });
                ui.add_space(S2);
                ui.label(sub(format!(
                    "The version only: {}. Its pre-translated code stays, so downloading it again starts fast.",
                    rtdelete::gb(p.runtime_bytes)
                )));
                ui.label(sub(format!(
                    "Everything: {} ({} more: its pre-translated code and caches). Data other versions use is kept.",
                    rtdelete::gb(p.runtime_bytes + p.caches_bytes),
                    rtdelete::gb(p.caches_bytes)
                )));
                if !p.devices.is_empty() {
                    let names: Vec<String> = p.devices.iter().map(|d| if d.state == "off" { d.name.clone() } else { format!("{} ({})", d.name, d.state) }).collect();
                    ui.add_space(S1);
                    ui.label(sub(format!(
                        "Used by {} device{}: {}. They can't start until this version is downloaded again.",
                        p.devices.len(),
                        if p.devices.len() == 1 { "" } else { "s" },
                        names.join(", ")
                    )));
                }
                let blocker = p.blocker();
                if let Some(b) = &blocker {
                    ui.add_space(S1);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(b).color(pal().warn));
                        let run: Vec<String> = p.running_devices().iter().map(|d| d.id.clone()).collect();
                        if !run.is_empty() && ui.add(Btn::secondary(if run.len() == 1 { "Stop it" } else { "Stop them" }).small().icon(icon::STOP)).clicked() {
                            act = Action::Stop(run);
                        }
                    });
                }
                ui.add_space(S2);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = S2;
                    let ok = blocker.is_none() && self.running.is_none();
                    let mut go = None;
                    if ui.add_enabled(ok, Btn::danger("Delete the version only").small()).on_hover_text("The runtime files; pre-translated code stays").clicked() {
                        go = Some(false);
                    }
                    if ui.add_enabled(ok, Btn::danger("Delete everything").small()).on_hover_text("Also its pre-translated code and caches").clicked() {
                        go = Some(true);
                    }
                    if ui.add(Btn::secondary("Cancel").small()).clicked() {
                        self.build = None;
                    }
                    if blocker.is_some() && ui.add(Btn::ghost("Check again").small()).clicked() {
                        self.measure(r, ui.ctx());
                    }
                    if let Some(all) = go {
                        self.build = None;
                        self.running = Some(p.build.clone());
                        let (slot, ctx) = (self.done.clone(), ui.ctx().clone());
                        std::thread::spawn(move || {
                            // measured a moment ago: plan again right before deleting (a phone may have started)
                            let res = rtdelete::find(&p.build).map(|r| rtdelete::plan(&r)).and_then(|p| rtdelete::delete(&p, all).map(|f| (p.label.clone(), all, f)));
                            *slot.lock().unwrap() = Some(res.map_err(|e| format!("{e:#}")));
                            ctx.request_repaint();
                        });
                    }
                });
            });
        });
        act
    }
}
