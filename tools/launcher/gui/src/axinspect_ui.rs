//! The launcher's Accessibility Inspector (as Xcode's, and the browser viewer's axinspect.js): a tool button beside the
//! screen turns inspecting on (touches pause): the element under the mouse is outlined, a click picks it and the side
//! panel's Accessibility section shows its properties and actions; the audit runs from there and its findings outline
//! their frames. Data: x86sim_core::axinspect / axaudit (simax `tree full`, `action`). State lives in egui's temp
//! data per phone, so the App struct is untouched.
use crate::theme::*;
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use x86sim_core::ax::{After, El, Snapshot};
use x86sim_core::{axaudit, axinspect, hid, instance::Instance};

#[derive(Default)]
pub struct AxState {
    pub on: bool,
    snap: Option<Snapshot>,
    sel: Option<String>,
    hover: Option<String>,
    busy: bool,
    last: Option<Instant>,
    status: String,
    audit: Option<axaudit::Report>,
    auditing: bool,
    mark: Option<usize>,
    dynamic_type: bool,
    all_frames: bool,
    pending_select: Option<String>,
    pending_audit: bool,
}

type Shared = Arc<Mutex<AxState>>;

pub fn state(ctx: &egui::Context, id: &str) -> Shared {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(egui::Id::new(("axinspect", id)), || {
            // scripted screenshots: X86SIM_LAUNCHER_OPEN=ax starts inspecting; X86SIM_AX_SELECT=<label> picks that element,
            // X86SIM_AX_AUDIT=1 runs the audit
            let on = std::env::var("X86SIM_LAUNCHER_OPEN").is_ok_and(|v| v.split(',').any(|k| k == "ax"));
            Arc::new(Mutex::new(AxState { on, pending_select: std::env::var("X86SIM_AX_SELECT").ok(), pending_audit: std::env::var("X86SIM_AX_AUDIT").is_ok(), ..Default::default() }))
        })
        .clone()
    })
}

/// Inspecting (the screen takes no touches then)
pub fn is_on(ctx: &egui::Context, id: &str) -> bool {
    state(ctx, id).lock().unwrap().on
}

fn refresh(ctx: &egui::Context, inst: &Instance, st: &Shared) {
    {
        let mut s = st.lock().unwrap();
        if s.busy {
            return;
        }
        s.busy = true;
        s.last = Some(Instant::now());
    }
    let (inst, st, ctx) = (inst.clone(), st.clone(), ctx.clone());
    std::thread::spawn(move || {
        let r = axinspect::inspect(&inst, false);
        let mut s = st.lock().unwrap();
        s.busy = false;
        s.last = Some(Instant::now());
        match r {
            Ok(snap) => {
                if let Some(t) = s.pending_select.take() {
                    s.sel = x86sim_core::ax::find(&snap, &t).ok().map(|e| e.r.clone());
                }
                s.status = format!("{} elements · {} · {} ms{}", snap.elements.len(), snap.front.join(", "), snap.ms,
                    if snap.hidden.is_empty() { String::new() } else { format!(" · {} hidden from VoiceOver", snap.hidden.len()) });
                s.snap = Some(snap);
            }
            Err(e) => s.status = format!("{e:#}"),
        }
        ctx.request_repaint();
    });
}

/// The tool button beside the screen
pub fn tool(ui: &mut egui::Ui, inst: &Instance, live: bool) {
    let st = state(ui.ctx(), &inst.id);
    let on = st.lock().unwrap().on;
    let tip = if on { "Inspecting: the element under the mouse is outlined, a click shows its accessibility properties (Accessibility in the side panel). Click to touch the screen again" } else { "Accessibility Inspector: point at an element to see its label, traits, actions and more" };
    if ui.add_enabled(live, if on { Btn::tinted("", pal().active) } else { Btn::ghost("") }.icon(icon::PERSON_SIMPLE_CIRCLE)).on_hover_text(tip).on_disabled_hover_text("Not running").clicked() {
        let mut s = st.lock().unwrap();
        s.on = !on;
        if s.on {
            drop(s);
            refresh(ui.ctx(), inst, &st);
        } else {
            s.hover = None;
        }
    }
}

/// upright points -> where they are on the picture (the panel's own orientation: landscape content is drawn sideways)
fn to_screen(inst: &Instance, snap: &Snapshot, rect: egui::Rect, x: f64, y: f64) -> egui::Pos2 {
    let rot = inst.orientation().as_deref().and_then(hid::orientation_rot).unwrap_or(0);
    let (u, v) = ((x / snap.screen.0.max(1.0)) as f32, (y / snap.screen.1.max(1.0)) as f32);
    let (pu, pv) = hid::unrotate(u, v, rot);
    egui::pos2(rect.left() + pu * rect.width(), rect.top() + pv * rect.height())
}

fn from_screen(inst: &Instance, snap: &Snapshot, rect: egui::Rect, p: egui::Pos2) -> (f64, f64) {
    let rot = inst.orientation().as_deref().and_then(hid::orientation_rot).unwrap_or(0);
    let (pu, pv) = ((p.x - rect.left()) / rect.width(), (p.y - rect.top()) / rect.height());
    let (u, v) = match rot % 360 {
        270 => (1.0 - pv, pu),
        90 => (pv, 1.0 - pu),
        180 => (1.0 - pu, 1.0 - pv),
        _ => (pu, pv),
    };
    (u as f64 * snap.screen.0, v as f64 * snap.screen.1)
}

fn frame_rect(inst: &Instance, snap: &Snapshot, rect: egui::Rect, f: &[f64; 4]) -> egui::Rect {
    egui::Rect::from_two_pos(to_screen(inst, snap, rect, f[0], f[1]), to_screen(inst, snap, rect, f[0] + f[2], f[1] + f[3]))
}

/// Over the screen (`screen`: the picture's rect): outlines, hover and click while inspecting
pub fn overlay(ui: &mut egui::Ui, inst: &Instance, screen: egui::Rect) {
    let st = state(ui.ctx(), &inst.id);
    let (on, stale) = {
        let s = st.lock().unwrap();
        (s.on, s.last.is_none_or(|t| t.elapsed() > Duration::from_millis(2500)))
    };
    let marked = st.lock().unwrap().mark.is_some();
    if !on && !marked {
        return;
    }
    if on && stale {
        refresh(ui.ctx(), inst, &st);
    }
    if on {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
    let resp = ui.interact(screen, egui::Id::new(("axinspect-screen", &inst.id)), if on { egui::Sense::click() } else { egui::Sense::hover() });
    let mut s = st.lock().unwrap();
    let Some(snap) = s.snap.clone() else { return };
    if snap.platform == "xros" {
        return; // window points: no place on the display (pick from the list in the side panel)
    }
    if on {
        let hov = resp.hover_pos().map(|p| from_screen(inst, &snap, screen, p)).and_then(|(x, y)| axinspect::element_at(&snap, x, y)).map(|e| e.r.clone());
        s.hover = hov.clone();
        if resp.clicked() {
            s.sel = hov;
            s.mark = None;
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }
    let p = ui.painter_at(screen);
    let blue = egui::Color32::from_rgb(76, 141, 255);
    let orange = egui::Color32::from_rgb(255, 159, 10);
    if s.all_frames {
        for e in snap.elements.iter().filter(|e| !e.offscreen && e.role != "webview") {
            p.rect_stroke(frame_rect(inst, &snap, screen, &e.frame), 2.0, egui::Stroke::new(1.0_f32, blue.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        }
    }
    let label = |p: &egui::Painter, r: egui::Rect, text: String, col: egui::Color32| {
        let g = p.layout_no_wrap(text, egui::FontId::proportional(11.0), egui::Color32::WHITE);
        let at = egui::pos2(r.left(), (r.top() - g.size().y - 2.0).max(screen.top()));
        p.rect_filled(egui::Rect::from_min_size(at, g.size() + egui::vec2(8.0, 2.0)), 3.0, col);
        p.galley(at + egui::vec2(4.0, 1.0), g, egui::Color32::WHITE);
    };
    if let (Some(i), Some(a)) = (s.mark, s.audit.as_ref()) {
        if let Some(f) = a.findings.get(i) {
            let col = if f.severity == "error" { pal().crit_ink } else { egui::Color32::from_rgb(224, 164, 58) };
            let r = frame_rect(inst, &snap, screen, &f.frame);
            p.rect_filled(r, 2.0, col.gamma_multiply(0.2));
            p.rect_stroke(r, 2.0, egui::Stroke::new(2.0_f32, col), egui::StrokeKind::Inside);
            label(&p, r, f.rule.clone(), col);
        }
    }
    let find = |r: &Option<String>| r.as_ref().and_then(|r| snap.elements.iter().find(|e| &e.r == r));
    if let Some(e) = find(&s.hover).filter(|_| s.hover != s.sel) {
        let r = frame_rect(inst, &snap, screen, &e.frame);
        p.rect_filled(r, 2.0, blue.gamma_multiply(0.15));
        p.rect_stroke(r, 2.0, egui::Stroke::new(2.0_f32, blue), egui::StrokeKind::Inside);
        label(&p, r, format!("{} {}", e.role, e.label.as_deref().unwrap_or("").chars().take(40).collect::<String>()), blue);
    }
    if let Some(e) = find(&s.sel) {
        let r = frame_rect(inst, &snap, screen, &e.frame);
        p.rect_filled(r, 2.0, orange.gamma_multiply(0.18));
        p.rect_stroke(r, 2.0, egui::Stroke::new(2.0_f32, orange), egui::StrokeKind::Inside);
        label(&p, r, format!("@{}", e.r), orange);
    }
}

fn run_action(ctx: &egui::Context, inst: &Instance, st: &Shared, target: Option<String>, action: &str, arg: Option<String>) {
    st.lock().unwrap().status = format!("{action}…");
    let (inst, st, ctx, action) = (inst.clone(), st.clone(), ctx.clone(), action.to_string());
    std::thread::spawn(move || {
        let r = axinspect::action(&inst, target.as_deref(), &action, arg.as_deref(), After::None);
        {
            let mut s = st.lock().unwrap();
            s.status = match r {
                Ok(o) => o.did,
                Err(e) => format!("{e:#}"),
            };
            s.last = None; // refresh at the next frame
        }
        ctx.request_repaint();
    });
}

fn run_audit(ctx: &egui::Context, inst: &Instance, st: &Shared) {
    let dynamic_type = {
        let mut s = st.lock().unwrap();
        s.auditing = true;
        s.dynamic_type
    };
    let (inst, st, ctx) = (inst.clone(), st.clone(), ctx.clone());
    std::thread::spawn(move || {
        let r = axaudit::audit(&inst, &axaudit::AuditOpts { dynamic_type, ..Default::default() });
        let snap = axinspect::inspect(&inst, false).ok();
        let mut s = st.lock().unwrap();
        s.auditing = false;
        s.mark = None;
        match r {
            Ok(r) => s.audit = Some(r),
            Err(e) => s.status = format!("audit: {e:#}"),
        }
        if snap.is_some() {
            s.snap = snap;
        }
        ctx.request_repaint();
    });
}

/// The side panel's Accessibility section
pub fn section(ui: &mut egui::Ui, inst: &Instance, live: bool) {
    if !live {
        ui.label(muted("Start the device to inspect its screen."));
        ui.add_space(S3);
        return;
    }
    let st = state(ui.ctx(), &inst.id);
    let ctx = ui.ctx().clone();
    let (on, auditing, mut dynamic_type, mut all_frames) = {
        let s = st.lock().unwrap();
        (s.on, s.auditing, s.dynamic_type, s.all_frames)
    };
    ui.horizontal_wrapped(|ui| {
        if ui.add(if on { Btn::tinted("Inspecting", pal().active) } else { Btn::secondary("Inspect") }.icon(icon::PERSON_SIMPLE_CIRCLE).small())
            .on_hover_text("Point at an element on the screen to see its accessibility properties (touches pause while inspecting)").clicked() {
            st.lock().unwrap().on = !on;
            if !on {
                refresh(&ctx, inst, &st);
            }
        }
        if ui.add_enabled(!auditing, Btn::secondary(if auditing { "Auditing…" } else { "Run audit" }).icon(icon::LIST_CHECKS).small())
            .on_hover_text("Accessibility audit of the app in front: labels, touch targets, contrast, Dynamic Type, duplicates, hidden controls, traits").clicked() {
            run_audit(&ctx, inst, &st);
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.checkbox(&mut all_frames, "All frames").changed() {
            st.lock().unwrap().all_frames = all_frames;
        }
        if ui.checkbox(&mut dynamic_type, "Dynamic Type").on_hover_text("The audit also re-checks at the largest accessibility text size (the device's text size changes for a few seconds, then goes back)").changed() {
            st.lock().unwrap().dynamic_type = dynamic_type;
        }
    });
    let audit_now = std::mem::take(&mut st.lock().unwrap().pending_audit);
    if audit_now {
        run_audit(&ctx, inst, &st);
    }
    let s = st.lock().unwrap();
    if !s.status.is_empty() {
        ui.label(muted(s.status.clone()));
    }
    let snap = s.snap.clone();
    let sel = s.sel.clone().and_then(|r| snap.as_ref().and_then(|sn| sn.elements.iter().find(|e| e.r == r).cloned()));
    let audit = s.audit.clone();
    let mark = s.mark;
    drop(s);
    ui.add_space(S1);
    if let Some(e) = sel {
        element_pane(ui, inst, &st, &e);
    } else if on {
        let xr = snap.as_ref().is_some_and(|s| s.platform == "xros");
        ui.label(muted(if xr { "Vision Pro: pick an element below (its frame is in its window's own points)." } else { "Point at an element on the screen." }));
        if let (true, Some(sn)) = (xr, &snap) {
            for e in sn.elements.iter().filter(|e| e.role != "webview").take(200) {
                if ui.selectable_label(false, format!("@{} {} {}", e.r, e.role, e.label.as_deref().unwrap_or(""))).clicked() {
                    st.lock().unwrap().sel = Some(e.r.clone());
                }
            }
        }
    }
    if let Some(a) = audit {
        ui.add_space(S2);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(format!("{} errors · {} warnings · {} info", a.errors, a.warnings, a.infos)).strong());
            ui.label(muted(format!("{} elements of {}", a.checked, a.apps.join(", "))));
            if ui.small_button("Copy JSON").clicked() {
                ui.ctx().copy_text(serde_json::to_string_pretty(&a).unwrap_or_default());
            }
            if ui.small_button("Close").clicked() {
                let mut s = st.lock().unwrap();
                s.audit = None;
                s.mark = None;
            }
        });
        for (i, f) in a.findings.iter().enumerate() {
            let col = match f.severity.as_str() {
                "error" => pal().crit_ink,
                "warning" => egui::Color32::from_rgb(224, 164, 58),
                _ => pal().ink_3,
            };
            let fr = egui::Frame::new().fill(if mark == Some(i) { pal().hover } else { pal().card }).stroke(egui::Stroke::new(1.0_f32, pal().border)).corner_radius(radius()).inner_margin(egui::Margin::symmetric(8, 5));
            let resp = fr.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(&f.severity).color(col).strong());
                    ui.label(egui::RichText::new(&f.rule).monospace());
                    if let Some(r) = &f.r {
                        ui.label(egui::RichText::new(format!("@{r}")).monospace());
                    }
                });
                ui.label(&f.message);
                ui.label(muted(format!("Fix: {}", f.fix)));
            });
            if resp.response.interact(egui::Sense::click()).clicked() {
                let mut s = st.lock().unwrap();
                s.mark = Some(i);
                if f.r.is_some() {
                    s.sel = f.r.clone();
                }
            }
        }
        for n in &a.notes {
            ui.label(muted(n.clone()));
        }
    }
    ui.add_space(S3);
}

fn element_pane(ui: &mut egui::Ui, inst: &Instance, st: &Shared, e: &El) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(e.label.as_deref().unwrap_or("(no label)")).strong());
        ui.label(muted(e.role.clone()));
    });
    egui::Grid::new(("axprops", &e.r)).num_columns(2).spacing([10.0, 3.0]).striped(true).show(ui, |ui| {
        for (k, v) in axinspect::properties(e) {
            ui.label(muted(k));
            ui.add(egui::Label::new(v).wrap());
            ui.end_row();
        }
    });
    ui.add_space(S1);
    ui.label(muted("Actions"));
    let ctx = ui.ctx().clone();
    ui.horizontal_wrapped(|ui| {
        let target = Some(format!("@{}", e.r));
        for a in axinspect::actions_of(e) {
            if let Some(name) = a.strip_prefix("custom:") {
                if ui.small_button(name).on_hover_text("Custom action").clicked() {
                    run_action(&ctx, inst, st, target.clone(), "custom", Some(name.to_string()));
                }
            } else if a == "scroll" {
                for (d, t) in [("up", "Scroll ↑"), ("down", "Scroll ↓"), ("left", "Scroll ←"), ("right", "Scroll →")] {
                    if ui.small_button(t).on_hover_text(format!("VoiceOver three-finger swipe {d}")).clicked() {
                        run_action(&ctx, inst, st, target.clone(), "scroll", Some(d.to_string()));
                    }
                }
            } else if !a.contains(' ') && ui.small_button(a.replace('_', " ")).clicked() {
                run_action(&ctx, inst, st, target.clone(), &a, None);
            }
        }
    });
}
