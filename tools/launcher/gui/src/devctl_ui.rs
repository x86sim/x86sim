//! Phone page, "Simulate" section: what Apple's Simulator offers in its Features / Device menus and simctl for one
//! phone: appearance (light / dark), text size (Dynamic Type), Increase Contrast, the fidelity preset (fast: a 3x
//! iPhone drawn at 2x plus Reduce Transparency / Motion; high: native), the keyboard (Connect Hardware Keyboard, Toggle
//! Software Keyboard), and Erase / Clone. Everything goes through
//! cli/x86sim (cli/libexec/devctl.py), on a thread: a running phone is changed at once through its helper, one that
//! is off at its next boot.
use crate::theme::*;
use crate::App;
use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use x86sim_core::instance::{self, Phase};

#[derive(Default, Clone)]
struct State {
    appearance: String,
    content_size: String,
    increase_contrast: String,
    fidelity: String,
}

#[derive(Default)]
pub struct DevctlUi {
    /// per phone id: its settings as last read (None while reading)
    state: Arc<Mutex<HashMap<String, Option<State>>>>,
    /// a command running for this phone
    busy: Arc<Mutex<HashMap<String, String>>>,
    /// finished commands' messages
    said: Arc<Mutex<Vec<String>>>,
    confirm_erase: Option<String>,
    clone_name: Option<(String, String)>,
    /// a clone / erase finished: the phone list changed
    changed: Arc<Mutex<bool>>,
}

const SIZES: [(&str, &str); 7] = [("extra-small", "XS"), ("small", "S"), ("medium", "M"), ("large", "L"), ("extra-large", "XL"), ("extra-extra-large", "XXL"), ("extra-extra-extra-large", "XXXL")];

fn x86sim(dev: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut c = std::process::Command::new(x86sim_core::aot::repo_root().join("cli/x86sim"));
    c.arg(args[0]).arg(dev).args(&args[1..]).stdin(std::process::Stdio::null());
    match c.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn read_state(dev: &std::path::Path) -> State {
    let mut s = State::default();
    if let Ok(t) = x86sim(dev, &["ui"]) {
        for l in t.lines() {
            if let Some((k, v)) = l.split_once(": ") {
                match k {
                    "appearance" => s.appearance = v.into(),
                    "content_size" => s.content_size = v.into(),
                    "increase_contrast" => s.increase_contrast = v.into(),
                    _ => {}
                }
            }
        }
    }
    s.fidelity = x86sim(dev, &["fidelity"]).map(|t| t.split_whitespace().next().unwrap_or("high").to_string()).unwrap_or_else(|_| "high".into());
    s
}

impl DevctlUi {
    fn refresh(&self, id: &str, dev: &std::path::Path, ctx: &egui::Context) {
        self.state.lock().unwrap().insert(id.to_string(), None);
        let (st, id, dev, ctx) = (self.state.clone(), id.to_string(), dev.to_path_buf(), ctx.clone());
        std::thread::spawn(move || {
            let s = read_state(&dev);
            st.lock().unwrap().insert(id, Some(s));
            ctx.request_repaint();
        });
    }

    /// Runs `args` for the phone on a thread; `done` names it in the message; the settings are read again after.
    fn run(&self, id: &str, dev: &std::path::Path, args: Vec<String>, done: String, ctx: &egui::Context) {
        self.busy.lock().unwrap().insert(id.to_string(), done.clone());
        let (busy, said, st, changed, id, dev, ctx) = (self.busy.clone(), self.said.clone(), self.state.clone(), self.changed.clone(), id.to_string(), dev.to_path_buf(), ctx.clone());
        std::thread::spawn(move || {
            let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let r = x86sim(&dev, &a);
            let msg = match &r {
                Ok(_) => done,
                Err(e) => format!("Couldn't do it: {}", e.lines().last().unwrap_or(e)),
            };
            if matches!(a[0], "erase" | "clone") {
                *changed.lock().unwrap() = true;
            }
            let s = read_state(&dev);
            st.lock().unwrap().insert(id.clone(), Some(s));
            busy.lock().unwrap().remove(&id);
            said.lock().unwrap().push(msg);
            ctx.request_repaint();
        });
    }
}

impl DevctlUi {
    /// The keyboard (x86sim_core::ax::keyboard, as the CLI's `x86sim keyboard` and MCP sim_keyboard) on a thread
    fn keyboard(&self, inst: &instance::Instance, what: &'static str, how: &'static str, ctx: &egui::Context) {
        let id = inst.id.clone();
        self.busy.lock().unwrap().insert(id.clone(), format!("Keyboard: {what} {how}…"));
        let (busy, said, inst, ctx) = (self.busy.clone(), self.said.clone(), inst.clone(), ctx.clone());
        std::thread::spawn(move || {
            let msg = match x86sim_core::ax::keyboard(&inst, what, how, x86sim_core::ax::After::None) {
                Ok(o) => format!("{}: {}", inst.name, o.did),
                Err(e) => format!("Couldn't do it: {e}"),
            };
            busy.lock().unwrap().remove(&id);
            said.lock().unwrap().push(msg);
            ctx.request_repaint();
        });
    }
}

impl App {
    /// Messages of finished commands, the phone list after an erase / clone (each frame)
    pub fn devctl_poll(&mut self) {
        let msgs: Vec<String> = std::mem::take(&mut *self.devctl.said.lock().unwrap());
        for m in msgs {
            self.say(m);
        }
        let ch = std::mem::take(&mut *self.devctl.changed.lock().unwrap());
        if ch {
            self.refresh_instances();
        }
    }

    pub fn simulate_section(&mut self, ui: &mut egui::Ui, inst: &instance::Instance, phase: Phase) {
        let id = inst.id.clone();
        let dev = inst.dir.clone();
        let ctx = ui.ctx().clone();
        let known = self.devctl.state.lock().unwrap().get(&id).cloned();
        let Some(st) = known else {
            self.devctl.refresh(&id, &dev, &ctx);
            busy(ui, "Reading…");
            return;
        };
        let Some(st) = st else {
            busy(ui, "Reading…");
            return;
        };
        let running = phase == Phase::Running;
        let off = phase == Phase::Off;
        let working = self.devctl.busy.lock().unwrap().get(&id).cloned();
        let idle = working.is_none() && (running || off);
        let field = |ui: &mut egui::Ui, label: &str, tip: &str| {
            ui.add_space(S1);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new(label).size(T_SMALL).family(medium()).color(pal().ink_2));
                ui.label(egui::RichText::new(icon::INFO).family(icons()).size(12.0).color(pal().ink_3)).on_hover_text(tip);
            });
        };
        let mut cmd: Option<(Vec<String>, String)> = None;
        ui.label(muted(if running { "Changes apply at once." } else if off { "The device is off: changes apply at its next start." } else { "Wait until it has started or stopped." }));
        field(ui, "Appearance", "Light or Dark, as Features > Toggle Appearance / simctl ui appearance");
        let opts = [("light", "Light"), ("dark", "Dark")];
        if let Some(i) = segmented(ui, &opts.map(|o| o.1), opts.iter().position(|o| o.0 == st.appearance), idle) {
            cmd = Some((vec!["ui".into(), "appearance".into(), opts[i].0.into()], format!("{}: {} appearance", inst.name, opts[i].1)));
        }
        field(ui, "Text size", "Dynamic Type (Settings > Display & Brightness > Text Size; simctl ui content_size). The larger accessibility sizes: x86sim ui content_size accessibility-medium …");
        let sel = SIZES.iter().position(|o| o.0 == st.content_size);
        if let Some(i) = segmented(ui, &SIZES.map(|o| o.1), sel, idle) {
            cmd = Some((vec!["ui".into(), "content_size".into(), SIZES[i].0.into()], format!("{}: text size {}", inst.name, SIZES[i].1)));
        }
        if sel.is_none() && st.content_size.starts_with("accessibility") {
            ui.label(muted(format!("Now {}", st.content_size)));
        }
        ui.add_space(S1);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new("Increase Contrast").size(T_SMALL).family(medium()).color(pal().ink_2));
            ui.label(egui::RichText::new(icon::INFO).family(icons()).size(12.0).color(pal().ink_3)).on_hover_text("Settings > Accessibility > Display & Text Size (simctl ui increase_contrast)");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut on = st.increase_contrast == "enabled";
                if ui.add_enabled_ui(idle, |ui| toggle(ui, &mut on)).inner.changed() {
                    cmd = Some((vec!["ui".into(), "increase_contrast".into(), if on { "enabled" } else { "disabled" }.into()], format!("{}: Increase Contrast {}", inst.name, if on { "on" } else { "off" })));
                }
            });
        });
        field(ui, "Fidelity", "Fast: a 3x iPhone drawn at 2x (same layout, 4/9 of the pixels) with Reduce Transparency and Reduce Motion on: smoother here. High: the model's own scale and every effect. The scale changes at the next start.");
        let opts = [("fast", "Fast"), ("high", "High")];
        if let Some(i) = segmented(ui, &opts.map(|o| o.1), opts.iter().position(|o| o.0 == st.fidelity), idle) {
            cmd = Some((vec!["fidelity".into(), opts[i].0.into()], format!("{}: fidelity {} (the scale at the next start)", inst.name, opts[i].1)));
        }
        // Keyboard: Apple's I/O > Keyboard (Connect Hardware Keyboard is kept across boots; the software keyboard's
        // toggle needs the phone up and a focused text field)
        let hw = x86sim_core::ax::hw_keyboard_connected(inst);
        let mut kb: Option<(&'static str, &'static str)> = None;
        ui.add_space(S1);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new("Connect Hardware Keyboard").size(T_SMALL).family(medium()).color(pal().ink_2));
            ui.label(egui::RichText::new(icon::INFO).family(icons()).size(12.0).color(pal().ink_3)).on_hover_text("I/O > Keyboard > Connect Hardware Keyboard (Shift-Cmd-K; Ctrl+Shift+K in the viewer). Off: the software keyboard comes up on every text field, as on a device without a keyboard; typing still works.");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut on = hw;
                if ui.add_enabled_ui(idle, |ui| toggle(ui, &mut on)).inner.changed() {
                    kb = Some(("hardware", if on { "connect" } else { "disconnect" }));
                }
            });
        });
        ui.add_space(S1);
        let r = ui.add_enabled(running && working.is_none(), Btn::secondary("Toggle Software Keyboard").icon(icon::KEYBOARD).small());
        if r.on_hover_text("I/O > Keyboard > Toggle Software Keyboard (Cmd-K; Ctrl+K in the viewer): the on-screen keyboard while a text field is focused and a hardware keyboard is connected").clicked() {
            kb = Some(("software", "toggle"));
        }
        if let Some(w) = &working {
            ui.add_space(S1);
            busy(ui, w);
        }
        ui.add_space(S3);
        // ---- erase / clone (as simctl: only while off)
        if self.devctl.confirm_erase.as_deref() == Some(id.as_str()) {
            ui.label(body(format!("Erase all content and settings of {}? Its apps and data go; it starts as new.", inst.name)));
            ui.horizontal(|ui| {
                if ui.add_enabled(off && working.is_none(), Btn::danger("Erase").small()).clicked() {
                    cmd = Some((vec!["erase".into()], format!("Erased {}", inst.name)));
                    self.devctl.confirm_erase = None;
                }
                if ui.add(Btn::secondary("Cancel").small()).clicked() {
                    self.devctl.confirm_erase = None;
                }
            });
        } else if let Some((cid, mut name)) = self.devctl.clone_name.take().filter(|c| c.0 == id) {
            ui.label(body("Name of the copy"));
            let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(ui.available_width()).margin(egui::Margin::symmetric(8, 6)));
            let mut keep = true;
            ui.horizontal(|ui| {
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(off && working.is_none() && !name.trim().is_empty(), Btn::primary("Clone").small()).clicked() || enter) && !name.trim().is_empty() {
                    cmd = Some((vec!["clone".into(), name.trim().to_string()], format!("Cloned {} as {}", inst.name, name.trim())));
                    keep = false;
                }
                if ui.add(Btn::secondary("Cancel").small()).clicked() {
                    keep = false;
                }
            });
            if keep {
                self.devctl.clone_name = Some((cid, name));
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                let tip = if off { "" } else { "Stop it first" };
                let r = ui.add_enabled(off && working.is_none() && !inst.is_default, Btn::secondary("Erase…").icon(icon::ERASER).small());
                if r.on_disabled_hover_text(if inst.is_default { "The main device: x86sim erase --force" } else { tip }).clicked() {
                    self.devctl.confirm_erase = Some(id.clone());
                }
                let r = ui.add_enabled(off && working.is_none(), Btn::secondary("Clone…").icon(icon::COPY).small());
                if r.on_disabled_hover_text(tip).clicked() {
                    self.devctl.clone_name = Some((id.clone(), format!("{} copy", inst.name)));
                }
            });
        }
        if let Some((args, done)) = cmd {
            self.devctl.run(&id, &dev, args, done, &ctx);
        }
        if let Some((what, how)) = kb {
            self.devctl.keyboard(inst, what, how, &ctx);
        }
    }
}
