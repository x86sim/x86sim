//! The Develop page: build apps on Linux with omarchy-apple-dev (external/omarchy-apple-dev) and run them on a device.
//! The toolchain's pieces (Swift, xtool, the iOS SDK, device tools) with install / update / repair buttons and the
//! background setup job's progress; recent projects with Build and Run on a chosen device; the last task's output.
//! The logic is x86sim_core::dev (shared with `x86sim dev` and the MCP server's dev_* tools).
use crate::global::{list_row, note};
use crate::theme::*;
use crate::App;
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use x86sim_core::dev;

/// A build / run / new-project task the page started
#[derive(Default, Clone)]
pub struct Task {
    pub title: String,
    pub lines: Vec<String>,
    pub running: bool,
    /// Ok(summary) / Err(message) once finished
    pub result: Option<Result<String, String>>,
}

#[derive(Default)]
pub struct Develop {
    pub status: Arc<Mutex<Option<dev::Status>>>,
    pub recent: Arc<Mutex<Vec<dev::Project>>>,
    checking: Arc<Mutex<bool>>,
    last_poll: Option<Instant>,
    pub xip: String,
    pub new_name: String,
    pub add_path: String,
    /// the device builds run on (its folder id)
    pub phone: String,
    pub task: Arc<Mutex<Task>>,
    pub show_job_log: bool,
    loaded: bool,
}

impl Develop {
    pub fn refresh(&self, ctx: &egui::Context) {
        let mut c = self.checking.lock().unwrap();
        if *c {
            return;
        }
        *c = true;
        let (st, rec, chk, ctx) = (self.status.clone(), self.recent.clone(), self.checking.clone(), ctx.clone());
        std::thread::spawn(move || {
            let s = dev::status();
            let r = dev::recent();
            *st.lock().unwrap() = Some(s);
            *rec.lock().unwrap() = r;
            *chk.lock().unwrap() = false;
            ctx.request_repaint();
        });
    }

    fn start_task(&self, ctx: &egui::Context, title: String, f: impl FnOnce(&mut dyn FnMut(&str)) -> anyhow::Result<String> + Send + 'static) {
        let t = self.task.clone();
        *t.lock().unwrap() = Task { title, running: true, ..Default::default() };
        let ctx = ctx.clone();
        let (st, rec) = (self.status.clone(), self.recent.clone());
        std::thread::spawn(move || {
            let t2 = t.clone();
            let c2 = ctx.clone();
            let mut say = move |l: &str| {
                let mut g = t2.lock().unwrap();
                g.lines.push(l.to_string());
                if g.lines.len() > 2000 {
                    g.lines.drain(..500);
                }
                c2.request_repaint_after(Duration::from_millis(100));
            };
            let r = f(&mut say).map_err(|e| format!("{e:#}"));
            {
                let mut g = t.lock().unwrap();
                g.running = false;
                g.result = Some(r);
            }
            *rec.lock().unwrap() = dev::recent();
            let _ = st;
            ctx.request_repaint();
        });
    }
}

fn piece_icon(id: &str) -> &'static str {
    match id {
        "fork" => icon::PACKAGE,
        "packages" => icon::TERMINAL_WINDOW,
        "swift" => icon::CODE,
        "xtool" => icon::HAMMER,
        "sdk" => icon::APPLE_LOGO,
        _ => icon::DEVICE_MOBILE,
    }
}

fn state_badge(ui: &mut egui::Ui, s: &str) {
    let (t, c) = match s {
        "ok" => ("Ready", pal().good),
        "warn" => ("Optional", pal().warn),
        "stale" => ("Update", pal().warn),
        _ => ("Needed", pal().crit),
    };
    badge(ui, t, c);
}

fn log_box(ui: &mut egui::Ui, id: &str, lines: &[String], h: f32) {
    egui::Frame::new().fill(pal().raised).corner_radius(radius_sm()).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt(id).max_height(h).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
            let start = lines.len().saturating_sub(400);
            for l in &lines[start..] {
                // a download's \r progress: its last state
                let l = l.rsplit('\r').find(|s| !s.trim().is_empty()).unwrap_or("");
                ui.add(egui::Label::new(egui::RichText::new(l).font(egui::FontId::monospace(11.5)).color(pal().ink_2)).wrap());
            }
        });
    });
}

impl App {
    pub fn develop_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.develop.loaded {
            self.develop.loaded = true;
            let s = dev::load_settings();
            self.develop.xip = s.xip;
            self.develop.phone = s.phone;
        }
        // status: at once, then every 1.5 s while a setup job runs (else every 20 s)
        let job_running = self.develop.status.lock().unwrap().as_ref().and_then(|s| s.job.as_ref()).is_some_and(|j| j.running);
        let every = if job_running { Duration::from_millis(1500) } else { Duration::from_secs(20) };
        if self.develop.last_poll.is_none_or(|t| t.elapsed() > every) {
            self.develop.last_poll = Some(Instant::now());
            self.develop.refresh(&ctx);
        }
        ctx.request_repaint_after(every);

        let mut recheck = false;
        page_header(ui, "Develop", "Build iPhone, iPad, Apple TV, Apple Watch and Vision Pro apps on Linux with omarchy-apple-dev, and run them on your devices.", |ui| {
            recheck = ui.add(Btn::secondary("Check again").icon(icon::ARROW_CLOCKWISE)).clicked();
        });
        if recheck {
            self.develop.refresh(&ctx);
        }
        let st = self.develop.status.lock().unwrap().clone();
        let Some(st) = st else {
            busy(ui, "Checking the toolchain…");
            return;
        };
        if st.pieces.iter().any(|p| p.id == "fork" && p.state != "ok") {
            empty_state(ui, icon::PACKAGE, "omarchy-apple-dev is missing", &format!("Expected in {}. In a checkout: git submodule update --init external/omarchy-apple-dev", st.fork.display()), |_| {});
            return;
        }

        // ---- the toolchain
        let job = st.job.clone();
        let busy_job = job.as_ref().is_some_and(|j| j.running);
        let mut start: Option<(&'static str, Vec<String>)> = None;
        section(ui, "Toolchain", |ui| {
            if ui.add_enabled(!busy_job && st.ready_sim, Btn::ghost("Verify").icon(icon::CHECK_CIRCLE).small()).on_hover_text("Build a new app end to end").clicked() {
                start = Some(("verify", vec![]));
            }
            if ui.add_enabled(!busy_job, Btn::ghost("Repair").icon(icon::WRENCH).small()).on_hover_text("Re-register the SDK and its tools (after a Swift update)").clicked() {
                start = Some(("repair", vec![]));
            }
        });
        list_card(ui, |ui| {
            let pieces: Vec<_> = st.pieces.iter().filter(|p| p.id != "fork").collect();
            for (k, p) in pieces.iter().enumerate() {
                if k > 0 {
                    hline(ui);
                }
                list_row(ui, piece_icon(p.id), p.title, &p.detail, |ui| {
                    state_badge(ui, p.state);
                    if busy_job {
                        return;
                    }
                    match (p.id, p.state) {
                        ("swift", "missing" | "stale") => {
                            if ui.add(Btn::primary("Install").icon(icon::DOWNLOAD_SIMPLE)).on_hover_text(format!("Swift {} from swift.org, into your home folder (1.1 GB download)", dev::SWIFT_VERSION)).clicked() {
                                start = Some(("swift", vec![]));
                            }
                        }
                        ("xtool", s) if s != "ok" => {
                            if ui.add(Btn::primary(if s == "missing" { "Install" } else { "Update" }).icon(icon::DOWNLOAD_SIMPLE)).on_hover_text("xtool (built from source, about 7 minutes), rcodesign, ipsw, pymobiledevice3").clicked() {
                                start = Some(("toolchain", vec![]));
                            }
                        }
                        ("packages", "missing") => {
                            if ui.add(Btn::secondary("Copy command").icon(icon::COPY)).clicked() {
                                ui.ctx().copy_text(p.fix.clone());
                            }
                        }
                        _ => {}
                    }
                });
                if p.id == "sdk" && p.state != "ok" {
                    egui::Frame::new().inner_margin(egui::Margin { left: 60, right: 16, top: 0, bottom: 12 }).show(ui, |ui| {
                        ui.label(muted("Download Xcode 27 (.xip) from developer.apple.com/download/all with a free Apple ID, then build the SDK from it (about 3 GB used):"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.develop.xip).hint_text("/path/to/Xcode_27.xip").desired_width(420.0));
                            let ok = std::path::Path::new(self.develop.xip.trim()).is_file();
                            if ui.add_enabled(ok && !busy_job, Btn::primary("Build SDK")).clicked() {
                                start = Some(("sdk", vec!["--xip".into(), self.develop.xip.trim().into()]));
                            }
                        });
                    });
                } else if !p.fix.is_empty() && p.state != "ok" {
                    note(ui, &p.fix);
                }
            }
        });
        if let Some((kind, args)) = start {
            match dev::start_job(kind, &args) {
                Ok(_) => {
                    self.develop.show_job_log = true;
                    self.say(format!("Setup started: {kind}"));
                }
                Err(e) => self.say(format!("Couldn't start: {e:#}")),
            }
            self.develop.last_poll = None;
        }
        // the setup job
        if let Some(j) = &job {
            ui.add_space(S3);
            let mut cancel = false;
            list_card(ui, |ui| {
                let title = match (j.running, j.rc) {
                    (true, _) => format!("Installing: {}", j.kind),
                    (false, Some(0)) => format!("Finished: {}", j.kind),
                    (false, rc) => format!("Stopped: {} (exit {})", j.kind, rc.unwrap_or(-1)),
                };
                let last = j.last.trim_matches(|c| c == '=' || c == ' ').to_string();
                let detail = if j.step.is_empty() || j.step == last { last } else { format!("{} · {last}", j.step) };
                list_row(ui, if j.running { icon::HOURGLASS } else { icon::TERMINAL_WINDOW }, &title, &detail, |ui| {
                    if j.running {
                        cancel = ui.add(Btn::secondary("Cancel")).clicked();
                    }
                    let t = if self.develop.show_job_log { "Hide log" } else { "Show log" };
                    if ui.add(Btn::ghost(t).small()).clicked() {
                        self.develop.show_job_log = !self.develop.show_job_log;
                    }
                });
                if j.running {
                    egui::Frame::new().inner_margin(egui::Margin { left: 60, right: 16, top: 0, bottom: 10 }).show(ui, |ui| {
                        progress(ui, Some(j.progress.max(0.03)), pal().accent);
                    });
                }
                if self.develop.show_job_log {
                    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 14, top: 0, bottom: 12 }).show(ui, |ui| {
                        let text = dev::tail_bytes(&j.log, 64 * 1024);
                        let lines: Vec<String> = text.lines().map(String::from).collect();
                        log_box(ui, "dev-job-log", &lines, 220.0);
                    });
                }
            });
            if cancel {
                if let Err(e) = dev::cancel_job() {
                    self.say(format!("{e:#}"));
                }
                self.develop.last_poll = None;
            }
        }

        // ---- the device to run on
        ui.add_space(S4);
        if self.develop.phone.is_empty() || !self.insts.iter().any(|i| i.id == self.develop.phone) {
            // the running one, else the first
            let running = self.insts.iter().find(|i| self.phase(&i.id) != x86sim_core::instance::Phase::Off).or(self.insts.first());
            self.develop.phone = running.map(|i| i.id.clone()).unwrap_or_default();
        }
        section(ui, "Run on", |_| {});
        list_card(ui, |ui| {
            let cur = self.insts.iter().find(|i| i.id == self.develop.phone).cloned();
            let detail = match &cur {
                Some(i) => format!("{} · {} · {}", i.model, i.ios, format!("{:?}", self.phase(&i.id)).to_lowercase()),
                None => "No device yet: add one on the Devices page".into(),
            };
            list_row(ui, icon::DEVICE_MOBILE, cur.as_ref().map(|i| i.name.as_str()).unwrap_or("Device"), &detail, |ui| {
                egui::ComboBox::from_id_salt("dev-phone").selected_text(cur.as_ref().map(|i| i.name.clone()).unwrap_or_default()).width(200.0).show_ui(ui, |ui| {
                    for i in &self.insts {
                        ui.selectable_value(&mut self.develop.phone, i.id.clone(), format!("{} · {}", i.name, i.ios));
                    }
                });
            });
            note(ui, "Run builds the app for this device's platform, starts the device if it is off, then installs and opens the app.");
        });

        // ---- projects
        ui.add_space(S4);
        section(ui, "Projects", |_| {});
        let recent = self.develop.recent.lock().unwrap().clone();
        let task_running = self.develop.task.lock().unwrap().running;
        let mut act: Option<(String, PathBuf)> = None;
        list_card(ui, |ui| {
            for (k, p) in recent.iter().enumerate() {
                if k > 0 {
                    hline(ui);
                }
                let detail = format!("{} · {} · {}", p.kind, if p.bundle_id.is_empty() { "-" } else { &p.bundle_id }, p.dir.display());
                list_row(ui, icon::FILE_CODE, &p.name, &detail, |ui| {
                    let can = st.ready_sim && p.kind == "xtool" && !task_running;
                    if ui.add_enabled(can && !self.develop.phone.is_empty(), Btn::primary("Run").icon(icon::PLAY)).on_hover_text("Build, install and open it on the device above").clicked() {
                        act = Some(("run".into(), p.dir.clone()));
                    }
                    if ui.add_enabled(can, Btn::secondary("Build").icon(icon::HAMMER)).clicked() {
                        act = Some(("build".into(), p.dir.clone()));
                    }
                });
            }
            if !recent.is_empty() {
                hline(ui);
            }
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.develop.new_name).hint_text("NewApp").desired_width(180.0));
                    let ok = !self.develop.new_name.trim().is_empty() && st.pieces.iter().any(|p| p.id == "xtool" && p.state != "missing");
                    if ui.add_enabled(ok && !task_running, Btn::secondary("New SwiftUI app").icon(icon::FOLDER_PLUS)).on_hover_text("xtool new, in ~/x86sim-apps").clicked() {
                        act = Some(("new".into(), PathBuf::from(self.develop.new_name.trim())));
                    }
                    ui.add_space(S4);
                    ui.add(egui::TextEdit::singleline(&mut self.develop.add_path).hint_text("/path/to/project").desired_width(260.0));
                    if ui.add_enabled(!self.develop.add_path.trim().is_empty(), Btn::secondary("Add folder").icon(icon::FOLDER_OPEN)).clicked() {
                        act = Some(("add".into(), PathBuf::from(self.develop.add_path.trim())));
                    }
                });
            });
        });
        if let Some((what, path)) = act {
            let phone = self.develop.phone.clone();
            match what.as_str() {
                "add" => match dev::project(&path) {
                    Ok(p) => {
                        dev::remember(&p.dir, None);
                        self.develop.add_path.clear();
                        *self.develop.recent.lock().unwrap() = dev::recent();
                    }
                    Err(e) => self.say(format!("{e:#}")),
                },
                "new" => {
                    let name = path.to_string_lossy().into_owned();
                    self.develop.new_name.clear();
                    self.develop.start_task(&ctx, format!("New app {name}"), move |say| {
                        let parent = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("x86sim-apps");
                        let d = dev::new_project(&name, &parent, say)?;
                        dev::remember(&d, None);
                        Ok(format!("{}", d.display()))
                    });
                }
                "build" => {
                    let platform = self.insts.iter().find(|i| i.id == phone).map(|i| i.platform.clone()).unwrap_or_else(|| "ios".into());
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.develop.start_task(&ctx, format!("Build {name} ({})", x86sim_core::sim::platform_name(&platform)), move |say| {
                        let app = dev::build(&path, &dev::BuildOpts { platform, release: false }, say)?;
                        Ok(format!("{}", app.display()))
                    });
                }
                _ => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let pname = self.insts.iter().find(|i| i.id == phone).map(|i| i.name.clone()).unwrap_or_default();
                    self.develop.start_task(&ctx, format!("Run {name} on {pname}"), move |say| {
                        let inst = dev::pick_phone(Some(&phone))?;
                        let r = dev::run(&path, &inst, &dev::RunOpts { release: false, start: true, no_build: false, args: vec![] }, say)?;
                        Ok(format!("{} running on {} ({:.0} s)", r.bundle_id, r.phone, r.secs))
                    });
                }
            }
        }

        // ---- the last task's output
        let task = self.develop.task.lock().unwrap().clone();
        if !task.title.is_empty() {
            ui.add_space(S4);
            section(ui, "Output", |_| {});
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(strong(&task.title));
                    ui.add_space(S2);
                    match &task.result {
                        None => busy(ui, "working…"),
                        Some(Ok(s)) => {
                            badge(ui, "Done", pal().good);
                            ui.label(muted(s));
                        }
                        Some(Err(_)) => {
                            badge(ui, "Failed", pal().crit);
                        }
                    }
                });
                if let Some(Err(e)) = &task.result {
                    ui.add(egui::Label::new(egui::RichText::new(e).color(pal().crit)).wrap());
                }
                ui.add_space(S2);
                log_box(ui, "dev-task-log", &task.lines, 260.0);
            });
        }
        ui.add_space(S4);
        ui.label(muted(format!("omarchy-apple-dev {} · {} · also: x86sim dev, MCP dev_* tools", st.fork_version, st.fork.display())));
    }
}
