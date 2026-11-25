//! The embedded screen of a phone: a thread per phone sleeps on its frame ring (futex) and converts new frames to
//! RGBA at the size the view needs (full for the open phone, a few pixels wide for a home-view thumbnail); the GUI
//! uploads them as a texture. Mouse on the screen = touch (edge touches marked as simview does), keyboard = the
//! phone's hardware keyboard, while the screen has focus.
use crate::theme::*;
use eframe::egui;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use x86sim_core::{hid, instance::Instance, ring::Ring};

pub const OFF: u8 = 0;
pub const THUMB: u8 = 1;
pub const FULL: u8 = 2;

pub struct Img {
    pub w: usize,
    pub h: usize,
    pub pixels: Vec<egui::Color32>,
    pub seq: u64,
}

#[derive(Default)]
pub struct Shared {
    pub mode: AtomicU8,
    pub target_w: AtomicU32,
    /// unix seconds of this boot: older frames are a past session's (f64 bits)
    pub boot_time: AtomicU64,
    pub frame: Mutex<Option<Img>>,
    pub dirty: AtomicBool,
    pub seq: AtomicU64,
    pub fps: Mutex<Vec<std::time::Instant>>,
    /// the frame ring this phone publishes to: a boot rewrites sim.env, so the path can change while the viewer lives
    pub ring: Mutex<std::path::PathBuf>,
    /// Models 2: degrees the shown panel is mounted at (the iPhone Duo's inner screen: 270), turned upright here
    pub rot: AtomicU32,
    /// SCALE_SMOOTH | SCALE_SHARP | SCALE_FULL
    pub scaling: AtomicU8,
}

/// Models 2: where a pointer position lands on a phone's screen: normalised (x, y) on the picture as drawn upright,
/// the screen's upright size in pixels (for edge gestures), and the touch target simhid needs for it (display id and
/// panel turn, the main screen's turn) when it is not the one the viewer is set to (`None`: as set)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub x: f32,
    pub y: f32,
    pub size: (u32, u32),
    pub target: Option<(Option<(u32, u32)>, u32)>,
    /// which surface (the 3D model's screen halves), for following a drag on it
    pub surface: u8,
}

/// Maps pointer positions to a phone's screens: the flat picture (`FlatMap`) or the 3D model (duo3d)
pub trait ScreenMap {
    /// The screen point under `p`; None off every screen
    fn hit(&self, p: egui::Pos2) -> Option<Hit>;
    /// Where `p` is on the surface `of` was on (a drag keeps to the screen it began on), unclamped
    fn follow(&self, p: egui::Pos2, of: &Hit) -> (f32, f32);
    /// Points the screen `of` is on spans across and down (wheel scrolls move a finger by that much)
    fn span(&self, of: &Hit) -> (f32, f32);
}

/// The flat picture drawn in `screen`
pub struct FlatMap {
    pub screen: egui::Rect,
    pub size: (u32, u32),
}

impl ScreenMap for FlatMap {
    fn hit(&self, p: egui::Pos2) -> Option<Hit> {
        self.screen.contains(p).then(|| {
            let (x, y) = self.follow(p, &Hit { x: 0.0, y: 0.0, size: self.size, target: None, surface: 0 });
            Hit { x, y, size: self.size, target: None, surface: 0 }
        })
    }
    fn follow(&self, p: egui::Pos2, _of: &Hit) -> (f32, f32) {
        ((p.x - self.screen.left()) / self.screen.width(), (p.y - self.screen.top()) / self.screen.height())
    }
    fn span(&self, _of: &Hit) -> (f32, f32) {
        (self.screen.width(), self.screen.height())
    }
}

pub struct Viewer {
    pub shared: Arc<Shared>,
    tex: Option<egui::TextureHandle>,
    tex_seq: u64,
    hid: hid::Hid,
    touching: bool,
    last_norm: (f32, f32),
    pub focused: bool,
    pub screen: (u32, u32),
    pub id: String,
    pinch: bool,
    pan2: bool,
    /// mouse wheel as a one-finger drag: the finger's position and the last wheel event
    wheel: Option<((f32, f32), std::time::Instant)>,
    /// the screen the current touch (or wheel scroll) began on: it carries on there
    touch_hit: Option<Hit>,
    wheel_hit: Option<Hit>,
    /// Shift as the phone knows it
    shift_held: bool,
    /// Platforms 1: an Apple Watch: the mouse wheel turns its Digital Crown (host/simhid "crown")
    pub crown: bool,
    /// Inputs 2: the device orientation's turn of the picture (0 / 90 / 180 / 270), kept while the device lies flat
    pub orient: u32,
    /// Inputs 2: an iPad's pointer capture (Apple's simulator: the Mac's pointer drives the iPad's mouse): hover moves
    /// the iPad's pointer, a click clicks, the wheel scrolls (host/simhid "pointer ..."), instead of touches
    pub pointer_mode: bool,
    last_hover: Option<egui::Pos2>,
    /// Platforms 1: an Apple TV: no touchscreen; a click is the remote's select, the wheel moves focus (the Siri
    /// Remote's touch surface), F1 the TV button
    pub remote: bool,
    /// the wheel distance not yet sent as an arrow key (remote)
    wheel_acc: egui::Vec2,
    /// Platforms 1: the screen's corner radius as a fraction of its width (a TV's is square, a watch's rounder)
    pub corner: f32,
}

/// How a frame is shrunk to the size it is drawn at (per phone: launcher.json "scaling", Advanced > Screen scaling)
pub const SCALE_SMOOTH: u8 = 0;
/// every step-th pixel: cheapest, but small text and edges shimmer and break up
pub const SCALE_SHARP: u8 = 1;
/// the whole frame to the GPU, which scales it (linear): the most upload and GPU work
pub const SCALE_FULL: u8 = 2;

pub fn scaling_of(s: &str) -> u8 {
    match s {
        "sharp" => SCALE_SHARP,
        "full" => SCALE_FULL,
        _ => SCALE_SMOOTH,
    }
}

/// The frame shrunk by `step` and turned upright. `smooth`: each output pixel is the average of its step x step block
/// (a box filter) instead of the block's first pixel, so a phone drawn small keeps its text and lines whole.
pub(crate) fn convert(f: &x86sim_core::ring::Frame, step: usize, rot: u32, smooth: bool) -> Img {
    let (w, h) = (f.w as usize / step, f.h as usize / step);
    let stride = f.stride as usize;
    let mut small = Vec::with_capacity(w * h);
    if smooth && step > 1 {
        let n = (step * step) as u32;
        let mut acc = vec![[0u32; 3]; w];
        for y in 0..h {
            acc.iter_mut().for_each(|a| *a = [0; 3]);
            for sy in 0..step {
                let row = &f.bgra[(y * step + sy) * stride..];
                for (x, a) in acc.iter_mut().enumerate() {
                    for p in row[x * step * 4..(x + 1) * step * 4].as_chunks::<4>().0 {
                        a[0] += p[2] as u32;
                        a[1] += p[1] as u32;
                        a[2] += p[0] as u32;
                    }
                }
            }
            small.extend(acc.iter().map(|a| egui::Color32::from_rgb(((a[0] + n / 2) / n) as u8, ((a[1] + n / 2) / n) as u8, ((a[2] + n / 2) / n) as u8)));
        }
    } else {
        for y in 0..h {
            for x in 0..w {
                let o = y * step * stride + x * step * 4;
                small.push(egui::Color32::from_rgb(f.bgra[o + 2], f.bgra[o + 1], f.bgra[o]));
            }
        }
    }
    let px_at = |x: usize, y: usize| small[y * w + x];
    match rot % 360 {
        // the panel's framebuffer turned upright: 270 = a quarter clockwise (upright (xo, yo) = framebuffer
        // (yo, h - 1 - xo)), 90 = counter-clockwise; hid::unrotate is the inverse for touches
        270 | 90 => {
            let mut px = Vec::with_capacity(w * h);
            for yo in 0..w {
                for xo in 0..h {
                    px.push(if rot % 360 == 270 { px_at(yo, h - 1 - xo) } else { px_at(w - 1 - yo, xo) });
                }
            }
            Img { w: h, h: w, pixels: px, seq: f.seq }
        }
        // Inputs 2: upside down (portraitUpsideDown)
        180 => Img { w, h, pixels: small.into_iter().rev().collect(), seq: f.seq },
        _ => Img { w, h, pixels: small, seq: f.seq },
    }
}

impl Viewer {
    pub fn new(inst: &Instance, ctx: egui::Context) -> Viewer {
        let shared = Arc::new(Shared::default());
        shared.target_w.store(inst.screen.0, Ordering::Relaxed);
        shared.scaling.store(scaling_of(&x86sim_core::instance::load_settings(&inst.dir).scaling), Ordering::Relaxed);
        *shared.ring.lock().unwrap() = inst.ring.clone();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name(format!("ring {}", inst.id))
            .spawn(move || {
                let mut ring_path = sh.ring.lock().unwrap().clone();
                let mut ring = Ring::new(&ring_path);
                let mut last = (0u64, 0usize, 0u8, 0u32);
                loop {
                    let want = sh.ring.lock().unwrap().clone();
                    if want != ring_path {
                        ring = Ring::new(&want);
                        ring_path = want;
                        last = (0, 0, 0, 0);
                    }
                    let mode = sh.mode.load(Ordering::Relaxed);
                    if mode == OFF {
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    let seq = ring.seq();
                    let tw = sh.target_w.load(Ordering::Relaxed).max(8) as usize;
                    if seq != 0 {
                        if let Some(f) = ring.latest() {
                            let rot = sh.rot.load(Ordering::Relaxed);
                            let src_w = if rot % 180 == 90 { f.h } else { f.w } as usize;
                            let sc = sh.scaling.load(Ordering::Relaxed);
                            // smooth: the largest step that keeps the image at least as wide as it is drawn (the GPU
                            // then shrinks it by less than 2x, which linear filtering does well); sharp: at most as wide
                            let step = match sc {
                                SCALE_FULL => 1,
                                SCALE_SHARP => src_w.div_ceil(tw),
                                _ => src_w / tw,
                            }
                            .max(1);
                            if (f.seq, step, sc, rot) != last {
                                let boot = f64::from_bits(sh.boot_time.load(Ordering::Relaxed));
                                if (f.t_ns as f64) / 1e9 >= boot - 2.0 {
                                    let img = convert(&f, step, rot, sc == SCALE_SMOOTH);
                                    *sh.frame.lock().unwrap() = Some(img);
                                    sh.dirty.store(true, Ordering::Release);
                                    sh.seq.store(f.seq, Ordering::Relaxed);
                                    let now = std::time::Instant::now();
                                    let mut fps = sh.fps.lock().unwrap();
                                    fps.retain(|t| now.duration_since(*t) < Duration::from_secs(1));
                                    fps.push(now);
                                    ctx.request_repaint();
                                }
                                last = (f.seq, step, sc, rot);
                            }
                        }
                    }
                    if mode == THUMB || seq == 0 {
                        // thumbnails: one frame a second is enough for a card
                        std::thread::sleep(Duration::from_millis(1000));
                    } else {
                        ring.wait(seq, Duration::from_secs(1));
                    }
                }
            })
            .expect("ring thread");
        Viewer { shared, tex: None, tex_seq: 0, hid: hid::Hid::new(&inst.sock), touching: false, last_norm: (0.0, 0.0), focused: false, screen: inst.screen, id: inst.id.clone(), pinch: false, pan2: false, wheel: None, touch_hit: None, wheel_hit: None, shift_held: false,
            crown: inst.model_id.starts_with("Watch"),
            remote: inst.model_id.starts_with("AppleTV"),
            wheel_acc: egui::Vec2::ZERO,
            orient: 0,
            pointer_mode: false,
            last_hover: None,
            corner: if inst.model_id.starts_with("AppleTV") { 0.0 } else if inst.model_id.starts_with("Watch") { 0.22 } else { 0.11 } }
    }

    /// Follow the phone's current ring (sim.env after its last boot); a change drops the old ring's picture.
    pub fn set_ring(&mut self, p: &std::path::Path) {
        let mut r = self.shared.ring.lock().unwrap();
        if r.as_path() != p {
            *r = p.to_path_buf();
            drop(r);
            *self.shared.frame.lock().unwrap() = None;
            self.clear();
        }
    }

    /// Models 2: the screen shown and touched: its upright size and, for a second display, (SFB id, rotation).
    /// Inputs 2: `orient`, the device orientation's turn (hid::orientation_rot), turns the picture and the touches
    /// further, as Apple's Simulator turns its window when the device rotates; `screen` is the size before it.
    pub fn set_display(&mut self, screen: (u32, u32), display: Option<(u32, u32)>, orient: u32) {
        self.screen = if orient % 180 == 90 { (screen.1, screen.0) } else { screen };
        let panel = display.map(|d| d.1).unwrap_or(0);
        self.hid.display = display.map(|(id, r)| (id, (r + orient) % 360));
        self.hid.main_rot = orient;
        self.shared.rot.store((panel + orient) % 360, Ordering::Relaxed);
    }

    /// Models 2: move the hinge of a two-screen phone (degrees, 0 folded .. 180 flat) over `ms`
    pub fn hinge(&mut self, deg: f64, ms: u32) {
        self.hid.hinge(deg, ms);
    }

    /// Models 2: a hinge preset (x86sim_core::hid::HINGE_POSES): its angle and orientation
    pub fn hinge_pose(&mut self, pose: &x86sim_core::hid::HingePose) {
        self.hid.hinge_pose(pose);
    }

    pub fn set_boot_time(&self, t: Option<f64>) {
        self.shared.boot_time.store(t.unwrap_or(f64::MAX).to_bits(), Ordering::Relaxed);
    }

    pub fn fps(&self) -> usize {
        let now = std::time::Instant::now();
        self.shared.fps.lock().unwrap().iter().filter(|t| now.duration_since(**t) < Duration::from_secs(1)).count()
    }

    fn upload(&mut self, ctx: &egui::Context) {
        if !self.shared.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let img = self.shared.frame.lock().unwrap().take();
        if let Some(img) = img {
            let ci = egui::ColorImage { size: [img.w, img.h], source_size: egui::vec2(img.w as f32, img.h as f32), pixels: img.pixels };
            let opts = egui::TextureOptions::LINEAR;
            match &mut self.tex {
                Some(t) => t.set(ci, opts),
                None => self.tex = Some(ctx.load_texture(format!("screen-{}", self.id), ci, opts)),
            }
            self.tex_seq = img.seq;
        }
    }

    pub fn clear(&mut self) {
        self.tex = None;
        self.tex_seq = 0;
    }

    /// Draws the screen into `avail` (keeping the phone's aspect), with a bezel. `live`: frames count as the screen.
    /// `interactive`: mouse and keyboard go to the phone. Returns the screen rect.
    pub fn show(&mut self, ui: &mut egui::Ui, avail: egui::Vec2, live: bool, placeholder: &str, interactive: bool, mode: u8) -> egui::Rect {
        self.shared.mode.store(if live { mode } else { OFF }, Ordering::Relaxed);
        if !live {
            self.clear();
        } else {
            self.upload(ui.ctx());
        }
        let (sw, sh) = (self.screen.0 as f32, self.screen.1 as f32);
        let bezel = if mode == FULL { 10.0 } else { 5.0 };
        let inner = avail - egui::vec2(2.0 * bezel, 2.0 * bezel);
        // Platforms 1: a watch's small screen is not blown up past 1.5 points a pixel (the rest of the area stays free)
        let mut scale = (inner.x / sw).min(inner.y / sh).max(0.01);
        if self.crown && mode == FULL {
            scale = scale.min(1.5);
        }
        let size = egui::vec2(sw * scale, sh * scale);
        // in physical pixels: on a HiDPI screen (2 pixels a point) a frame shrunk to the size in points was blown up 2x
        self.shared.target_w.store((size.x * ui.ctx().pixels_per_point()).ceil() as u32, Ordering::Relaxed);
        let outer = size + egui::vec2(2.0 * bezel, 2.0 * bezel);
        let (rect, resp) = ui.allocate_exact_size(outer, if interactive { egui::Sense::click_and_drag() } else { egui::Sense::hover() });
        let screen = rect.shrink(bezel);
        let p = ui.painter();
        let radius = (size.x * self.corner).max(4.0);
        p.rect_filled(rect, radius + bezel * 0.7, pal().bezel);
        p.rect_stroke(rect, radius + bezel * 0.7, egui::Stroke::new(1.0_f32, pal().border_strong), egui::StrokeKind::Inside);
        match (&self.tex, live) {
            (Some(t), true) => {
                egui::Image::new(egui::load::SizedTexture::new(t.id(), size)).corner_radius(radius).paint_at(ui, screen);
            }
            _ => {
                p.rect_filled(screen, radius, pal().well);
                if !placeholder.is_empty() {
                    let fs = if mode == FULL { T_LG } else { T_SMALL };
                    let ic = if placeholder == "Off" { icon::POWER } else { icon::CIRCLE_NOTCH };
                    p.text(screen.center() - egui::vec2(0.0, fs * 0.9), egui::Align2::CENTER_CENTER, ic, ifont(fs * 1.6), pal().ink_3);
                    p.text(screen.center() + egui::vec2(0.0, fs * 0.9), egui::Align2::CENTER_CENTER, placeholder, font_m(fs), pal().ink_3);
                }
            }
        }
        if interactive {
            let map = FlatMap { screen, size: self.screen };
            self.input(ui, &resp, screen, &map, live);
            if self.focused {
                ui.painter().rect_stroke(rect, radius + bezel * 0.7, egui::Stroke::new(1.0_f32, pal().active), egui::StrokeKind::Outside);
            }
        }
        rect
    }

    /// Inputs 2: pointer capture on or off (off takes the iPad's mouse away again, as Apple's simulator does)
    pub fn set_pointer_mode(&mut self, on: bool) {
        if self.pointer_mode && !on {
            self.hid.send("pointer remove");
        }
        self.pointer_mode = on;
        self.last_hover = None;
    }

    /// Inputs 2: the host mouse as the iPad's mouse: the pointer kept under the cursor, buttons, the wheel as scroll
    fn pointer_input(&mut self, ctx: &egui::Context, resp: &egui::Response, screen: egui::Rect) {
        let (pressed, released, hover, delta) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.hover_pos(), i.raw_scroll_delta));
        let Some(p) = hover.filter(|p| screen.contains(*p)) else {
            self.last_hover = None;
            return;
        };
        // the pointer placed under the host's cursor (host/simhid "pointer at": an absolute pointer event, in the
        // interface's orientation as the picture shows it; relative moves are sped up by the iPad and drift)
        if self.last_hover != Some(p) {
            let (x, y) = (((p.x - screen.left()) / screen.width()).clamp(0.0, 1.0), ((p.y - screen.top()) / screen.height()).clamp(0.0, 1.0));
            self.hid.send(&format!("pointer at {x:.5} {y:.5}"));
        }
        self.last_hover = Some(p);
        // device points a screen pixel of the picture (the device's pixels are 2 a point), for the wheel
        let k = self.screen.0 as f32 / screen.width() / 2.0;
        if pressed && resp.rect.contains(p) {
            self.hid.send("pointer button down");
        }
        if released {
            self.hid.send("pointer button up");
        }
        if delta != egui::Vec2::ZERO {
            // the wheel turned down (egui: negative y) scrolls the page down: simhid's positive dy
            self.hid.send(&format!("pointer scroll {:.2} {:.2}", -delta.x * k, -delta.y * k));
        }
        ctx.output_mut(|o| o.cursor_icon = egui::CursorIcon::None);
    }

    /// Models 2: touches and keys through another view of the screens (the 3D model): `resp` is its area
    pub fn input_mapped(&mut self, ui: &mut egui::Ui, resp: &egui::Response, map: &dyn ScreenMap, live: bool) {
        self.input(ui, resp, resp.rect, map, live);
    }

    /// Sends the next touch event to the screen `h` is on
    fn aim(&mut self, h: Option<Hit>) {
        if let Some((display, main_rot)) = h.and_then(|h| h.target) {
            self.hid.display = display;
            self.hid.main_rot = main_rot;
        }
    }

    fn input(&mut self, ui: &mut egui::Ui, resp: &egui::Response, screen: egui::Rect, map: &dyn ScreenMap, live: bool) {
        let ctx = ui.ctx().clone();
        if live && self.pointer_mode {
            if ctx.input(|i| i.pointer.primary_pressed()) {
                let p = ctx.input(|i| i.pointer.interact_pos());
                self.focused = p.is_some_and(|p| resp.rect.contains(p));
                if self.focused {
                    ui.memory_mut(|m| m.request_focus(resp.id));
                }
            }
            self.pointer_input(&ctx, resp, screen);
        }
        let (pressed, released, pos, moving, shift, ctrl) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.interact_pos().or(i.pointer.latest_pos()), i.pointer.is_moving(), i.modifiers.shift, i.modifiers.ctrl));
        let pressed = pressed && !(live && self.pointer_mode);
        if pressed {
            if let Some(p) = pos {
                if resp.rect.contains(p) {
                    self.focused = true;
                    ui.memory_mut(|m| m.request_focus(resp.id));
                    if live && self.remote {
                        self.hid.key(true, 0x28);
                        self.touching = true;
                    } else if let (true, Some(h)) = (live, map.hit(p)) {
                        let (x, y) = (h.x, h.y);
                        self.last_norm = (x, y);
                        self.touching = true;
                        self.touch_hit = Some(h);
                        self.aim(Some(h));
                        if ctrl || shift {
                            self.two_finger("down", x, y, ctrl);
                        } else {
                            let edge = hid::edge_at(x, y, h.size.0, h.size.1);
                            self.hid.touch("down", x, y, edge);
                        }
                        self.pinch = ctrl;
                        self.pan2 = shift;
                    }
                } else {
                    self.focused = false;
                }
            }
        }
        if self.remote {
            if self.touching && released {
                self.touching = false;
                self.hid.key(false, 0x28);
            }
            // the wheel / touchpad as the remote's touch surface: an arrow key a notch (40 points of scroll)
            if live {
                let (delta, hover) = ctx.input(|i| (i.raw_scroll_delta, i.pointer.hover_pos()));
                if delta != egui::Vec2::ZERO && hover.is_some_and(|p| map.hit(p).is_some()) {
                    self.wheel_acc += delta;
                    const NOTCH: f32 = 40.0;
                    while self.wheel_acc.y.abs() >= NOTCH || self.wheel_acc.x.abs() >= NOTCH {
                        // content follows the wheel: wheel down (negative y) = focus down
                        let u = if self.wheel_acc.y <= -NOTCH { self.wheel_acc.y += NOTCH; 0x51 }
                            else if self.wheel_acc.y >= NOTCH { self.wheel_acc.y -= NOTCH; 0x52 }
                            else if self.wheel_acc.x <= -NOTCH { self.wheel_acc.x += NOTCH; 0x4f }
                            else { self.wheel_acc.x -= NOTCH; 0x50 };
                        self.hid.key(true, u);
                        self.hid.key(false, u);
                    }
                }
            }
        }
        if !self.remote && self.touching && moving {
            if let (Some(p), Some(h)) = (pos, self.touch_hit) {
                let (x, y) = map.follow(p, &h);
                let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
                if (x, y) != self.last_norm {
                    self.last_norm = (x, y);
                    self.aim(Some(h));
                    if self.pinch || self.pan2 {
                        self.two_finger("move", x, y, self.pinch);
                    } else {
                        self.hid.touch("move", x, y, None);
                    }
                }
            }
        }
        if !self.remote && self.touching && released {
            self.touching = false;
            let (x, y) = self.last_norm;
            let h = self.touch_hit.take();
            self.aim(h);
            if self.pinch || self.pan2 {
                self.two_finger("up", x, y, self.pinch);
            } else {
                self.hid.touch("up", x, y, None);
            }
            self.pinch = false;
            self.pan2 = false;
        }
        // Platforms 1: on an Apple Watch the wheel turns the Digital Crown (as Apple's Simulator and simview.py do):
        // wheel down / content up = crown towards the bottom (positive)
        if live && self.crown {
            let (delta, hover) = ctx.input(|i| (i.raw_scroll_delta, i.pointer.hover_pos()));
            if delta.y != 0.0 && hover.is_some_and(|p| map.hit(p).is_some()) {
                // ~60 a notch: half a Settings row (a step of ~120 scrolls one; 0.16 a pixel barely moved)
                self.hid.crown(-delta.y * 1.2);
            }
        }
        // mouse wheel / touchpad scroll over the screen: a finger dragging the content (lifted after a short pause)
        if live && !self.touching && !self.crown && !self.remote && !self.pointer_mode {
            let (delta, hover) = ctx.input(|i| (i.raw_scroll_delta, i.pointer.hover_pos()));
            let over = hover.and_then(|p| map.hit(p));
            if delta != egui::Vec2::ZERO && (over.is_some() || self.wheel.is_some()) {
                // a scroll carries on on the screen it began on
                let h = self.wheel_hit.or(over).unwrap();
                self.wheel_hit = Some(h);
                self.aim(Some(h));
                let start = over.map(|o| (o.x, o.y)).unwrap_or((0.5, 0.5));
                let ((mut x, mut y), _) = self.wheel.unwrap_or_else(|| {
                    // never start on an edge (that would be a system gesture)
                    let s = (start.0.clamp(0.1, 0.9), start.1.clamp(0.15, 0.85));
                    self.hid.touch("down", s.0, s.1, None);
                    (s, std::time::Instant::now())
                });
                let span = map.span(&h);
                x += delta.x / span.0.max(1.0);
                y += delta.y / span.1.max(1.0);
                if !(0.05..=0.95).contains(&x) || !(0.08..=0.92).contains(&y) {
                    // ran out of screen: lift and grab again where the scroll began
                    self.hid.touch("up", x.clamp(0.0, 1.0), y.clamp(0.0, 1.0), None);
                    let s = (start.0.clamp(0.1, 0.9), start.1.clamp(0.15, 0.85));
                    self.hid.touch("down", s.0, s.1, None);
                    (x, y) = s;
                } else {
                    self.hid.touch("move", x, y, None);
                }
                self.wheel = Some(((x, y), std::time::Instant::now()));
                ctx.request_repaint_after(Duration::from_millis(150));
            }
        }
        if let Some(((x, y), at)) = self.wheel {
            if at.elapsed() >= Duration::from_millis(140) {
                let h = self.wheel_hit.take();
                self.aim(h);
                self.hid.touch("up", x, y, None);
                self.wheel = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(150) - at.elapsed());
            }
        }
        if self.focused && live {
            if resp.hovered() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
            }
            // (down, usage, needs shift, with Command, host Shift at that event)
            let mut sends: Vec<(bool, u32, bool, bool, bool)> = vec![];
            let mut buttons: Vec<(&'static str, bool)> = vec![];
            let mut paste = String::new();
            let shift_now = ctx.input(|i| i.modifiers.shift);
            let remote = self.remote;
            ctx.input_mut(|i| {
                i.events.retain(|e| match e {
                    egui::Event::Key { key, pressed, repeat, modifiers, .. } => {
                        if *repeat {
                            return false;
                        }
                        match *key {
                            // Platforms 1: an Apple TV's F1 is the TV button, F2 play/pause (its power button only
                            // sleeps the TV); a watch's F1 presses the crown, F2 sleep / wake (power, 0x30; its side button,
                            // Apple's 0x95, is the button beside the screen)
                            egui::Key::F1 if remote => buttons.push(("tv", *pressed)),
                            egui::Key::F2 if remote => buttons.push(("playpause", *pressed)),
                            egui::Key::F1 => buttons.push(("home", *pressed)),
                            egui::Key::F2 => buttons.push(("lock", *pressed)),
                            egui::Key::F3 => buttons.push(("volup", *pressed)),
                            egui::Key::F4 => buttons.push(("voldown", *pressed)),
                            egui::Key::H if modifiers.ctrl => buttons.push(("home", *pressed)),
                            // Apple's Cmd-K / Shift-Cmd-K: Toggle Software Keyboard / Connect Hardware Keyboard
                            egui::Key::K if modifiers.ctrl && *pressed => buttons.push((if modifiers.shift { "hwkb" } else { "softkb" }, true)),
                            egui::Key::K if modifiers.ctrl => {}
                            k => {
                                if let Some((u, sh)) = usage(k) {
                                    sends.push((*pressed, u, sh, modifiers.ctrl || modifiers.mac_cmd, modifiers.shift));
                                }
                            }
                        }
                        false
                    }
                    // Ctrl+V: the host clipboard is typed into the phone; Ctrl+C / Ctrl+X act inside the phone
                    egui::Event::Paste(t) => {
                        paste.push_str(t);
                        false
                    }
                    egui::Event::Copy => {
                        sends.push((true, 0x06, false, true, false));
                        sends.push((false, 0x06, false, true, false));
                        false
                    }
                    egui::Event::Cut => {
                        sends.push((true, 0x1b, false, true, false));
                        sends.push((false, 0x1b, false, true, false));
                        false
                    }
                    egui::Event::Text(_) => false,
                    _ => true,
                });
            });
            for (b, down) in buttons {
                match b {
                    "softkb" => self.hid.soft_keyboard_toggle(),
                    "hwkb" => {
                        let on = !self.hid.hw_keyboard_connected();
                        self.hid.hw_keyboard(on);
                    }
                    _ => self.hid.button(b, down),
                }
            }
            // Shift mirrors the host's Shift key: set from each key press, then from the frame's final state (a key's
            // release can come without the modifier: sending Shift with each key left it held on the phone)
            for (down, u, sh, cmd, ev_shift) in sends {
                if down && ev_shift != self.shift_held {
                    self.hid.key(ev_shift, 225);
                    self.shift_held = ev_shift;
                }
                let extra_shift = sh && !self.shift_held;
                if down {
                    if cmd {
                        self.hid.key(true, 0xe3);
                    }
                    if extra_shift {
                        self.hid.key(true, 225);
                    }
                    self.hid.key(true, u);
                    if extra_shift {
                        self.hid.key(false, 225);
                    }
                } else {
                    self.hid.key(false, u);
                    if cmd {
                        self.hid.key(false, 0xe3);
                    }
                }
            }
            if shift_now != self.shift_held {
                self.hid.key(shift_now, 225);
                self.shift_held = shift_now;
            }
            for c in paste.chars().take(2000) {
                if let Some((u, sh)) = char_usage(c) {
                    if sh {
                        self.hid.key(true, 225);
                    }
                    self.hid.key(true, u);
                    self.hid.key(false, u);
                    if sh {
                        self.hid.key(false, 225);
                    }
                }
            }
        } else if self.shift_held {
            self.hid.key(false, 225);
            self.shift_held = false;
        }
    }

    fn two_finger(&mut self, phase: &str, x: f32, y: f32, pinch: bool) {
        if pinch {
            self.hid.touch2(phase, x, y, 1.0 - x, 1.0 - y);
        } else {
            let dx = 50.0 / self.screen.0 as f32;
            self.hid.touch2(phase, x - dx, y, x + dx, y);
        }
    }

    pub fn press(&mut self, button: &str) {
        self.hid.press(button);
    }

    pub fn hold(&self, button: &str, ms: u64) {
        self.hid.hold(button, ms);
    }

    /// Inputs 2: Device > Shake (host/simhid "shake")
    pub fn shake(&mut self) {
        self.hid.shake();
    }

    /// Inputs 2: Rotate Left / Right (host/simhid "rotate"): the device orientation; the frame stays the panel's own
    /// (landscape content is drawn sideways in it, as in Apple's framebuffer)
    pub fn rotate(&mut self, left: bool) {
        self.hid.rotate(left);
    }

    /// Platforms 1: the Digital Crown turned by `delta` (host/simhid "crown"; positive scrolls towards the bottom)
    pub fn crown(&mut self, delta: f32) {
        self.hid.crown(delta);
    }

    /// Platforms 1: a keyboard key held (the TV remote's clickpad: arrows and select repeat while held, as on a remote)
    pub fn key(&mut self, down: bool, usage: u32) {
        self.hid.key(down, usage);
    }

    /// Platforms 1: a consumer-page button pressed and let go by its HID usage (host/simhid "usage"): the Siri Remote's
    /// buttons simhid has no name for (mute 0xe2)
    pub fn tap_usage(&mut self, page: u32, usage: u32) {
        self.hid.usage(page, usage, true);
        std::thread::sleep(std::time::Duration::from_millis(60));
        self.hid.usage(page, usage, false);
    }

    /// Saves the screen at full resolution, upright, as Simulator's File > Save Screen does: "<name> <date> at
    /// <time>.png" in the Pictures folder (xdg-user-dir PICTURES, else ~/Pictures). Returns the file.
    pub fn screenshot(&self, name: &str) -> Result<std::path::PathBuf, String> {
        let path = self.shared.ring.lock().unwrap().clone();
        let f = Ring::new(&path).latest().ok_or("no picture from the device yet")?;
        let png = f.png(self.shared.rot.load(Ordering::Relaxed)).map_err(|e| e.to_string())?;
        let run = |c: &str, a: &[&str]| std::process::Command::new(c).args(a).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty());
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        let dir = run("xdg-user-dir", &["PICTURES"]).map(std::path::PathBuf::from).filter(|d| d != &home).unwrap_or_else(|| home.join("Pictures"));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let when = run("date", &["+%Y-%m-%d at %H.%M.%S"]).unwrap_or_else(|| f.seq.to_string());
        let file = dir.join(format!("{} {when}.png", name.replace('/', "-")));
        std::fs::write(&file, png).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(file)
    }
}

/// egui key -> (HID usage, needs shift)
fn usage(k: egui::Key) -> Option<(u32, bool)> {
    use egui::Key as K;
    let n = k.name();
    if n.len() == 1 {
        let c = n.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some((4 + (c.to_ascii_lowercase() as u32 - 'a' as u32), false));
        }
    }
    Some(match k {
        K::Num0 => (0x27, false),
        K::Num1 => (0x1e, false),
        K::Num2 => (0x1f, false),
        K::Num3 => (0x20, false),
        K::Num4 => (0x21, false),
        K::Num5 => (0x22, false),
        K::Num6 => (0x23, false),
        K::Num7 => (0x24, false),
        K::Num8 => (0x25, false),
        K::Num9 => (0x26, false),
        K::Space => (0x2c, false),
        K::Enter => (0x28, false),
        K::Escape => (0x29, false),
        K::Backspace => (0x2a, false),
        K::Tab => (0x2b, false),
        K::Minus => (0x2d, false),
        K::Equals => (0x2e, false),
        K::Plus => (0x2e, true),
        K::OpenBracket => (0x2f, false),
        K::CloseBracket => (0x30, false),
        K::OpenCurlyBracket => (0x2f, true),
        K::CloseCurlyBracket => (0x30, true),
        K::Backslash => (0x31, false),
        K::Pipe => (0x31, true),
        K::Semicolon => (0x33, false),
        K::Colon => (0x33, true),
        K::Quote => (0x34, false),
        K::Backtick => (0x35, false),
        K::Comma => (0x36, false),
        K::Period => (0x37, false),
        K::Slash => (0x38, false),
        K::Questionmark => (0x38, true),
        K::Exclamationmark => (0x1e, true),
        K::ArrowRight => (0x4f, false),
        K::ArrowLeft => (0x50, false),
        K::ArrowDown => (0x51, false),
        K::ArrowUp => (0x52, false),
        K::Delete => (0x4c, false),
        K::Home => (0x4a, false),
        K::End => (0x4d, false),
        K::PageUp => (0x4b, false),
        K::PageDown => (0x4e, false),
        _ => return None,
    })
}

/// A typed character -> (HID usage, needs shift), US layout
fn char_usage(c: char) -> Option<(u32, bool)> {
    Some(match c {
        'a'..='z' => (4 + (c as u32 - 'a' as u32), false),
        'A'..='Z' => (4 + (c as u32 - 'A' as u32), true),
        '1'..='9' => (0x1e + (c as u32 - '1' as u32), false),
        '0' => (0x27, false),
        '\n' => (0x28, false),
        '\t' => (0x2b, false),
        ' ' => (0x2c, false),
        '-' => (0x2d, false),
        '_' => (0x2d, true),
        '=' => (0x2e, false),
        '+' => (0x2e, true),
        '[' => (0x2f, false),
        '{' => (0x2f, true),
        ']' => (0x30, false),
        '}' => (0x30, true),
        '\\' => (0x31, false),
        '|' => (0x31, true),
        ';' => (0x33, false),
        ':' => (0x33, true),
        '\'' => (0x34, false),
        '"' => (0x34, true),
        '`' => (0x35, false),
        '~' => (0x35, true),
        ',' => (0x36, false),
        '<' => (0x36, true),
        '.' => (0x37, false),
        '>' => (0x37, true),
        '/' => (0x38, false),
        '?' => (0x38, true),
        '!' => (0x1e, true),
        '@' => (0x1f, true),
        '#' => (0x20, true),
        '$' => (0x21, true),
        '%' => (0x22, true),
        '^' => (0x23, true),
        '&' => (0x24, true),
        '*' => (0x25, true),
        '(' => (0x26, true),
        ')' => (0x27, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, px: impl Fn(u32, u32) -> u8) -> x86sim_core::ring::Frame {
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let v = px(x, y);
                let o = ((y * w + x) * 4) as usize;
                bgra[o..o + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        x86sim_core::ring::Frame { w, h, stride: w * 4, seq: 1, t_ns: 0, bgra }
    }

    #[test]
    fn smooth_scaling_averages() {
        // one-pixel black/white checkerboard (fine text, hairlines): every 2nd pixel is all black, the average grey
        let f = frame(8, 6, |x, y| if (x + y) % 2 == 0 { 0 } else { 255 });
        let sharp = convert(&f, 2, 0, false);
        assert!(sharp.pixels.iter().all(|p| p.r() == 0));
        let smooth = convert(&f, 2, 0, true);
        assert_eq!((smooth.w, smooth.h), (4, 3));
        assert!(smooth.pixels.iter().all(|p| p.r() == 128 && p.g() == 128 && p.b() == 128));
        // turned upright like the sharp path: a left-to-right ramp becomes top-to-bottom
        let ramp = frame(6, 4, |x, _| (x * 40) as u8);
        let r = convert(&ramp, 2, 270, true);
        assert_eq!((r.w, r.h), (2, 3));
        assert_eq!([r.pixels[0].r(), r.pixels[2].r(), r.pixels[4].r()], [20, 100, 180]);
        assert_eq!(convert(&ramp, 2, 270, false).pixels.iter().map(|p| p.r()).collect::<Vec<_>>(), [0, 0, 80, 80, 160, 160]);
    }

    #[test]
    fn turned_picture_matches_touches() {
        // Inputs 2: for every turn the viewer shows, the pixel at an upright point is the framebuffer pixel that
        // hid::unrotate sends a touch at that point to (so a tap lands on what is drawn under the pointer)
        let (w, h) = (6usize, 4usize);
        let f = frame(w as u32, h as u32, |x, y| (y * 6 + x) as u8 * 10);
        for rot in [0u32, 90, 180, 270] {
            let img = convert(&f, 1, rot, false);
            for yo in 0..img.h {
                for xo in 0..img.w {
                    let (fx, fy) = x86sim_core::hid::unrotate((xo as f32 + 0.5) / img.w as f32, (yo as f32 + 0.5) / img.h as f32, rot);
                    let (fx, fy) = ((fx * w as f32) as usize, (fy * h as f32) as usize);
                    assert_eq!(img.pixels[yo * img.w + xo].r(), ((fy * 6 + fx) * 10) as u8, "rot {rot} at {xo},{yo}");
                }
            }
        }
    }

    #[test]
    fn smooth_scaling_cost() {
        // an iPhone 18 Pro frame into a ~450 px wide panel (step 2): the per-frame cost on the ring thread
        let f = frame(1206, 2622, |x, y| (x ^ y) as u8);
        let t = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(convert(&f, 2, 0, true));
        }
        let smooth = t.elapsed() / 10;
        let t = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(convert(&f, 2, 0, false));
        }
        eprintln!("1206x2622 step 2: smooth {smooth:?}, sharp {:?} a frame", t.elapsed() / 10);
    }
}
