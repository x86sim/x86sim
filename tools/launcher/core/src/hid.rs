//! Input to a phone: host/simhid's socket (<device>/data/simhid.sock), one text command per line (touch, touch2,
//! key, button, crown, usage, hinge; Inputs 2: shake, orientation, rotate, biometric, gamepad). Edge touches are marked as Apple's Simulator does (simview.py's rule: within 4% of the width of an
//! edge; top and bottom win in the corners), so the home indicator and Control/Notification Center work.
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Hid {
    path: PathBuf,
    sock: Option<UnixStream>,
    /// Models 2: touches go to another display: (SFB display id, its native rotation); x, y given upright
    pub display: Option<(u32, u32)>,
    /// Inputs 2: the device orientation's turn of the main screen (0, 90 landscapeLeft, 180, 270 landscapeRight): the
    /// viewer shows it upright, as Apple's Simulator turns its window; x, y given upright
    pub main_rot: u32,
}

/// A preset of a two-screen phone's hinge, as Xcode's DeviceHub offers for one (CoreDevicePopDeviceKitExtension's pose
/// presets v68_Closed, v68_HalfOpen_Portrait, v68_HalfOpen_Landscape, v68_Open, beside its hinge slider)
pub struct HingePose {
    /// the name the CLI / MCP take
    pub key: &'static str,
    pub label: &'static str,
    pub angle: f64,
    /// the device orientation the pose sets (host/simhid "orientation"): every pose is held a way up (DeviceHub's
    /// Closed and Open are portrait poses; keeping a landscape turn showed the outer screen's portrait-only home
    /// screen sideways)
    pub orientation: Option<&'static str>,
}

pub const HINGE_POSES: [HingePose; 4] = [
    HingePose { key: "closed", label: "Closed", angle: 0.0, orientation: Some("portrait") },
    HingePose { key: "half-portrait", label: "Half open", angle: 90.0, orientation: Some("portrait") },
    HingePose { key: "half-landscape", label: "Half open, landscape", angle: 90.0, orientation: Some("landscapeLeft") },
    HingePose { key: "open", label: "Open", angle: 180.0, orientation: Some("portrait") },
];

/// The hinge state CoreMotion names for an angle (host/simhid's device state: closed at 5 degrees or less, flat from 170)
pub fn hinge_state(deg: f64) -> &'static str {
    if deg <= 5.0 {
        "Closed"
    } else if deg >= 170.0 {
        "Open flat"
    } else {
        "Partially open"
    }
}

/// Inputs 2: how far the panel's picture is turned to stand upright for a device orientation (host/simhid's names;
/// landscapeLeft: the device's top points left, so its framebuffer is shown a quarter counterclockwise); None for
/// faceUp / faceDown / unknown (the window keeps its last turn, as Apple's Simulator does)
pub fn orientation_rot(o: &str) -> Option<u32> {
    match o {
        "portrait" => Some(0),
        "landscapeLeft" => Some(90),
        "portraitUpsideDown" => Some(180),
        "landscapeRight" => Some(270),
        _ => None,
    }
}

/// Upright normalized (x, y) on a panel mounted at `rot` degrees -> the panel's framebuffer coordinates
/// (270: the framebuffer is shown turned a quarter clockwise, so its x runs down the picture and y right to left)
pub fn unrotate(x: f32, y: f32, rot: u32) -> (f32, f32) {
    match rot % 360 {
        270 => (y, 1.0 - x),
        90 => (1.0 - y, x),
        180 => (1.0 - x, 1.0 - y),
        _ => (x, y),
    }
}

fn unrotate_edge(e: &str, rot: u32) -> &str {
    const E: [&str; 4] = ["top", "right", "bottom", "left"];
    let Some(i) = E.iter().position(|n| *n == e) else { return e };
    // 270: upright top = panel left, upright right = panel top (the inverse of unrotate)
    E[(i + (rot % 360 / 90) as usize) % 4]
}

impl Hid {
    pub fn new(path: &Path) -> Hid {
        Hid { path: path.to_path_buf(), sock: None, display: None, main_rot: 0 }
    }

    pub fn send(&mut self, line: &str) {
        static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DEBUG.get_or_init(|| std::env::var_os("X86SIM_HID_DEBUG").is_some()) {
            eprintln!("hid {}: {line}", self.path.display());
        }
        for _ in 0..2 {
            if self.sock.is_none() {
                // a device path too long for sun_path is a symlink to a short socket (host/simhid listen_thread)
                let path = std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
                match UnixStream::connect(&path) {
                    Ok(mut s) => {
                        let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
                        // a human types with pauses: keep simhid's on-demand keyboard attached for 5 min after this
                        // client's last key (default 30 s, then the next key pays a re-attach + 1.5 s settle).
                        // X86SIM_VIEWER_KB_IDLE_S overrides; older simhids log it as an unknown command.
                        let idle = std::env::var("X86SIM_VIEWER_KB_IDLE_S").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(300);
                        let _ = s.write_all(format!("kbidle {idle}\n").as_bytes());
                        self.sock = Some(s);
                    }
                    Err(_) => return,
                }
            }
            if self.sock.as_mut().unwrap().write_all(format!("{line}\n").as_bytes()).is_ok() {
                return;
            }
            self.sock = None;
        }
    }

    /// x, y normalized 0..1; screen (w, h) in device pixels for the edge rule
    pub fn touch(&mut self, phase: &str, x: f32, y: f32, edge: Option<&str>) {
        let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
        let (pre, (x, y), edge) = match self.display {
            Some((id, rot)) => (format!("@{id} "), unrotate(x, y, rot), edge.map(|e| unrotate_edge(e, rot))),
            None => (String::new(), unrotate(x, y, self.main_rot), edge.map(|e| unrotate_edge(e, self.main_rot))),
        };
        match edge {
            Some(e) => self.send(&format!("{pre}touch {phase} {x:.5} {y:.5} {e}")),
            None => self.send(&format!("{pre}touch {phase} {x:.5} {y:.5}")),
        }
    }

    pub fn touch2(&mut self, phase: &str, x1: f32, y1: f32, x2: f32, y2: f32) {
        let c = |v: f32| v.clamp(0.0, 1.0);
        let (pre, (x1, y1), (x2, y2)) = match self.display {
            Some((id, rot)) => (format!("@{id} "), unrotate(c(x1), c(y1), rot), unrotate(c(x2), c(y2), rot)),
            None => (String::new(), unrotate(c(x1), c(y1), self.main_rot), unrotate(c(x2), c(y2), self.main_rot)),
        };
        self.send(&format!("{pre}touch2 {phase} {x1:.5} {y1:.5} {x2:.5} {y2:.5}"));
    }

    /// Models 2: the hinge of a two-screen phone, degrees (0 folded .. 180 flat), moved over `ms`
    pub fn hinge(&mut self, deg: f64, ms: u32) {
        self.send(&format!("hinge {deg:.1} {ms}"));
    }

    /// The hinge poses of a two-screen phone (`HINGE_POSES`): the angle, and the orientation the pose holds it in, if any
    pub fn hinge_pose(&mut self, pose: &HingePose) {
        if let Some(o) = pose.orientation {
            self.orientation(o);
        }
        self.hinge(pose.angle, 600);
    }

    /// Platforms 1: turns an Apple Watch's Digital Crown by `delta` (positive: towards the bottom), host/simhid "crown"
    pub fn crown(&mut self, delta: f32) {
        self.send(&format!("crown {delta:.2} y"));
    }

    pub fn key(&mut self, down: bool, usage: u32) {
        self.send(&format!("key {} {usage}", if down { "down" } else { "up" }));
    }

    /// Keyboard: Apple's Toggle Software Keyboard (Cmd-K): the on-screen keyboard key (Consumer 0x1ae) on the keyboard
    pub fn soft_keyboard_toggle(&mut self) {
        self.send("key down 0x1ae 0x0c");
        self.send("key up 0x1ae 0x0c");
    }

    /// Keyboard: Apple's Connect Hardware Keyboard (Shift-Cmd-K): host/simhid's hwkb (GraphicsServices' attached state)
    pub fn hw_keyboard(&mut self, on: bool) {
        self.send(if on { "hwkb connect" } else { "hwkb disconnect" });
    }

    /// The hardware keyboard's connection as host/simhid remembers it (<data>/hwkeyboard next to the socket)
    pub fn hw_keyboard_connected(&self) -> bool {
        let f = self.path.parent().map(|d| d.join("hwkeyboard")).unwrap_or_default();
        !std::fs::read_to_string(f).is_ok_and(|s| s.trim() == "disconnected")
    }

    /// Platforms 1: any page / usage on the buttons service (host/simhid "usage"), for buttons simhid has no name for
    pub fn usage(&mut self, page: u32, usage: u32, down: bool) {
        self.send(&format!("usage {page:#x} {usage:#x} {}", if down { "down" } else { "up" }));
    }

    pub fn button(&mut self, name: &str, down: bool) {
        self.send(&format!("button {name} {}", if down { "down" } else { "up" }));
    }

    /// Inputs 2: a hardware button held `ms` (Apple's usages per platform: home lock side volup voldown siri action wallet
    /// playpause tv select menu back mute; host/simhid button_usage)
    pub fn press_for(&mut self, name: &str, ms: u64) {
        self.button(name, true);
        std::thread::sleep(Duration::from_millis(ms));
        self.button(name, false);
    }

    /// Inputs 2: Device > Shake (UIKit's motion event)
    pub fn shake(&mut self) {
        self.send("shake");
    }

    /// Inputs 2: the device orientation (portrait portraitUpsideDown landscapeLeft landscapeRight faceUp faceDown), or ""
    /// to have simhid write the current one to <device>/data/orientation
    pub fn orientation(&mut self, name: &str) {
        let l = format!("orientation {name}");
        self.send(l.trim_end());
    }

    /// Inputs 2: Rotate Left (counterclockwise) / Right
    pub fn rotate(&mut self, left: bool) {
        self.send(if left { "rotate left" } else { "rotate right" });
    }

    /// Inputs 2: Face ID / Touch ID / Optic ID: enroll, unenroll, match, nomatch; kind face|touch|optic or "" (the device's)
    pub fn biometric(&mut self, action: &str, kind: &str) {
        let l = format!("biometric {action} {kind}");
        self.send(l.trim_end());
    }

    /// Inputs 2: the simulated game controller: press / down / up a button (up down left right a b x y l1 r1 l2 r2)
    pub fn gamepad(&mut self, button: &str, action: &str) {
        match action {
            "down" | "up" => self.send(&format!("gamepad {button} {action}")),
            _ => self.send(&format!("gamepad press {button}")),
        }
    }

    /// Inputs 2: a thumbstick (-1..1, y up)
    pub fn gamepad_stick(&mut self, right: bool, x: f32, y: f32) {
        self.send(&format!("gamepad stick {} {x:.3} {y:.3}", if right { "right" } else { "left" }));
    }

    pub fn press(&mut self, name: &str) {
        self.button(name, true);
        std::thread::sleep(Duration::from_millis(80));
        self.button(name, false);
    }

    /// Holds a button for `ms` without blocking the caller: its own connection on a thread (the side button held
    /// 2.5 s is Siri, as tests/arm64/gate/gate.sh presses it).
    pub fn hold(&self, name: &str, ms: u64) {
        let (path, name) = (self.path.clone(), name.to_string());
        std::thread::spawn(move || {
            let mut h = Hid::new(&path);
            h.button(&name, true);
            std::thread::sleep(Duration::from_millis(ms));
            h.button(&name, false);
        });
    }
}

/// The edge a touch at normalized (x, y) begins on, for a screen of w x h device pixels.
pub fn edge_at(x: f32, y: f32, w: u32, h: u32) -> Option<&'static str> {
    let mx = 0.04;
    let my = 0.04 * w as f32 / h.max(1) as f32;
    if y >= 1.0 - my {
        Some("bottom")
    } else if y <= my {
        Some("top")
    } else if x <= mx {
        Some("left")
    } else if x >= 1.0 - mx {
        Some("right")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_screen_touch_mapping() {
        // iPhone Duo inner screen (nativeRotation 270): Settings drawn upright at (0.803, 0.291) was tapped with
        // "@3 touch ... 0.291 0.197" on the framebuffer (Models 2)
        let (x, y) = unrotate(0.803, 0.291, 270);
        assert!((x - 0.291).abs() < 1e-6 && (y - 0.197).abs() < 1e-3);
        assert_eq!(unrotate(0.3, 0.4, 0), (0.3, 0.4));
        // upright top / right / bottom / left of a 270 panel = its left / top / right / bottom
        assert_eq!(unrotate_edge("top", 270), "left");
        assert_eq!(unrotate_edge("right", 270), "top");
        assert_eq!(unrotate_edge("bottom", 270), "right");
        assert_eq!(unrotate_edge("left", 270), "bottom");
        assert_eq!(unrotate_edge("bottom", 0), "bottom");
    }

    #[test]
    fn orientation_turns() {
        // landscapeLeft: the framebuffer is shown a quarter counterclockwise (the viewer's 90): the upright picture's
        // top left is the framebuffer's top right, its top edge the framebuffer's right edge
        let r = orientation_rot("landscapeLeft").unwrap();
        assert_eq!(unrotate(0.0, 0.0, r), (1.0, 0.0));
        assert_eq!(unrotate_edge("top", r), "right");
        assert_eq!(unrotate_edge("bottom", r), "left");
        let r = orientation_rot("landscapeRight").unwrap();
        assert_eq!(unrotate(0.0, 0.0, r), (0.0, 1.0));
        assert_eq!(unrotate_edge("bottom", r), "right");
        assert_eq!(unrotate(0.25, 0.1, 180), (0.75, 0.9));
        assert_eq!(unrotate_edge("top", 180), "bottom");
        assert_eq!(orientation_rot("faceUp"), None);
    }

    #[test]
    fn long_socket_path_is_followed() {
        // simhid links a device path too long for sun_path to a short socket: connect through the link
        use std::io::BufRead;
        let t = std::env::temp_dir().join(format!("x86sim-hid-{}", std::process::id()));
        let dir = t.join("d".repeat(60)).join("e".repeat(60)).join("data");
        std::fs::create_dir_all(&dir).unwrap();
        let short = t.join("s.sock");
        let link = dir.join("simhid.sock");
        assert!(link.as_os_str().len() > 108);
        let l = std::os::unix::net::UnixListener::bind(&short).unwrap();
        std::os::unix::fs::symlink(&short, &link).unwrap();
        Hid::new(&link).send("touch down 0.5 0.5");
        let (c, _) = l.accept().unwrap();
        let lines: Vec<String> = std::io::BufReader::new(c).lines().take(2).map(|l| l.unwrap()).collect();
        assert!(lines[0].starts_with("kbidle "));
        assert_eq!(lines[1], "touch down 0.5 0.5");
        std::fs::remove_dir_all(&t).unwrap();
    }
}
