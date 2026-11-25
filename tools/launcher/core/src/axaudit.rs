//! The accessibility audit (as Xcode's Accessibility Inspector audit / XCUIApplication.performAccessibilityAudit):
//! rules over the full snapshot (axinspect.rs, simax `tree full`) and the screen's pixels.
//!
//! Rules: missing-label (an interactive element or image with no label), label-not-readable (a label that is a file
//! name, an SF Symbol or an identifier), hit-region (a control under 44 x 44 pt; 60 on visionOS), contrast (WCAG ratio
//! of the text's color against what is behind it, sampled from the screenshot), text-clipped (a label cut off now),
//! dynamic-type (with `dynamic_type`: clipped or not growing at the largest accessibility text size, set with the
//! phone's content size for the check and put back), duplicate-label, hidden-interactive (a control or tap target
//! VoiceOver cannot reach), trait-mismatch (looks like a button, has no button trait).
use crate::ax::{self, El, Snapshot};
use crate::axinspect;
use crate::instance::Instance;
use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const RULES: &[&str] = &["missing-label", "label-not-readable", "hit-region", "contrast", "text-clipped", "dynamic-type", "duplicate-label", "hidden-interactive", "trait-mismatch"];

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub rule: String,
    /// error, warning or info
    pub severity: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub r: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// x, y, width, height in screen points
    pub frame: [f64; 4],
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    pub message: String,
    pub fix: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub phone: String,
    pub platform: String,
    /// the apps audited (bundle ids)
    pub apps: Vec<String>,
    pub screen: (f64, f64),
    pub checked: usize,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub findings: Vec<Finding>,
    pub rules: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct AuditOpts {
    /// re-check at the largest accessibility text size (changes the phone's content size for a few seconds)
    pub dynamic_type: bool,
    /// every layer (home screen, SpringBoard, status bar), not only the app in front
    pub all: bool,
    /// only these rules (default: all)
    pub rules: Vec<String>,
}

/// The screen in RGB, its size, and points -> pixels
pub struct Pixels {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
    pub sx: f64,
    pub sy: f64,
}

impl Pixels {
    pub fn from_png(png: &[u8], screen: (f64, f64)) -> Option<Pixels> {
        let mut d = png::Decoder::new(std::io::Cursor::new(png)).read_info().ok()?;
        let mut buf = vec![0; d.output_buffer_size()?];
        let info = d.next_frame(&mut buf).ok()?;
        let (w, h) = (info.width as usize, info.height as usize);
        let ch = match info.color_type {
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            _ => return None,
        };
        let mut rgb = Vec::with_capacity(w * h * 3);
        for p in buf[..w * h * ch].chunks(ch) {
            rgb.extend_from_slice(&p[..3]);
        }
        Some(Pixels { w, h, rgb, sx: w as f64 / screen.0.max(1.0), sy: h as f64 / screen.1.max(1.0) })
    }
}

/// WCAG 2 relative luminance
pub fn luminance(c: [u8; 3]) -> f64 {
    let f = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
}

pub fn ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The contrast of text inside a frame: the background is the most common color there; the text's color is the one
/// the app set (fg, blended over the background by its alpha) or, without it, the strongest of the pixels that differ
/// from the background (90th percentile: anti-aliased edges do not count). None: nothing drawn there that differs.
pub fn contrast_in(px: &Pixels, frame: &[f64; 4], fg: Option<[f64; 4]>) -> Option<(f64, [u8; 3], [u8; 3])> {
    let x0 = ((frame[0] * px.sx).floor().max(0.0) as usize).min(px.w);
    let y0 = ((frame[1] * px.sy).floor().max(0.0) as usize).min(px.h);
    let x1 = (((frame[0] + frame[2]) * px.sx).ceil().max(0.0) as usize).min(px.w);
    let y1 = (((frame[1] + frame[3]) * px.sy).ceil().max(0.0) as usize).min(px.h);
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        return None;
    }
    let n = (x1 - x0) * (y1 - y0);
    let step = ((n as f64 / 40000.0).sqrt().ceil() as usize).max(1);
    let mut hist: HashMap<u16, (u32, [u32; 3])> = HashMap::new();
    let mut all = Vec::new();
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            let o = (y * px.w + x) * 3;
            let c = [px.rgb[o], px.rgb[o + 1], px.rgb[o + 2]];
            let k = (c[0] as u16 >> 4) << 8 | (c[1] as u16 >> 4) << 4 | c[2] as u16 >> 4;
            let e = hist.entry(k).or_insert((0, [0; 3]));
            e.0 += 1;
            for (acc, v) in e.1.iter_mut().zip(c) {
                *acc += v as u32;
            }
            all.push(c);
        }
    }
    let (cnt, sum) = hist.values().max_by_key(|v| v.0)?;
    let bg = [(sum[0] / cnt) as u8, (sum[1] / cnt) as u8, (sum[2] / cnt) as u8];
    let mut differ: Vec<(f64, [u8; 3])> = all.iter().map(|c| (ratio(*c, bg), *c)).filter(|(r, _)| *r > 1.15).collect();
    // a few samples of text in a wide frame are text too: a short label across an Apple TV's 1880 pt row covers
    // under 1% of it (the old floor); 0.25% and 8 samples still keep stray pixels out
    if differ.len() * 400 < all.len() || differ.len() < 8 {
        return None;
    }
    if let Some(f) = fg {
        let a = f[3].clamp(0.0, 1.0);
        let c = [0, 1, 2].map(|i| (f[i] * a + bg[i] as f64 * (1.0 - a)).round().clamp(0.0, 255.0) as u8);
        return Some((ratio(c, bg), c, bg));
    }
    differ.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (r, c) = differ[(differ.len() - 1) * 9 / 10];
    Some((r, c, bg))
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn more<'a>(e: &'a El, k: &str) -> Option<&'a Value> {
    e.more.as_ref().and_then(|m| m.get(k)).filter(|v| !v.is_null())
}

fn interactive(role: &str) -> bool {
    matches!(role, "button" | "link" | "switch" | "adjustable" | "textfield" | "searchfield" | "icon" | "popup" | "checkbox" | "radio")
}

/// A label that reads as a file name, an SF Symbol name or a developer's identifier rather than words
pub fn unreadable_label(l: &str) -> Option<&'static str> {
    let t = l.trim();
    if t.is_empty() || t.contains(' ') && !t.contains('_') {
        return None;
    }
    let lower = t.to_lowercase();
    for ext in [".png", ".jpg", ".jpeg", ".pdf", ".svg", ".heic", ".gif", ".webp", "@2x", "@3x"] {
        if lower.ends_with(ext) || lower.contains(&format!("{ext}.")) {
            return Some("a file name");
        }
    }
    let words: Vec<&str> = t.split('.').collect();
    if words.len() >= 2 && words.iter().all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())) && !t.contains(' ') {
        return Some("an SF Symbol or file name");
    }
    if t.contains('_') && !t.contains(' ') && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Some("an identifier (snake_case)");
    }
    if ["ic_", "icon_", "btn_", "img_", "image_", "button_", "ic-", "icon-", "btn-", "img-"].iter().any(|p| lower.starts_with(p)) {
        return Some("an image or identifier name");
    }
    // camelCase in one token with a lowercase word first: "settingsButton", "imgProfile" (not "iPhone", "macOS", "eBay")
    let first_up = t.find(|c: char| c.is_ascii_uppercase());
    let words_after = first_up.is_some_and(|i| t[i + 1..].chars().any(|c| c.is_ascii_lowercase()));
    if t.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && first_up.is_some_and(|i| i >= 3) && words_after && t.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some("an identifier (camelCase)");
    }
    None
}

fn finding(e: &El, rule: &str, sev: &str, message: String, fix: &str, detail: Value) -> Finding {
    Finding {
        rule: rule.into(),
        severity: sev.into(),
        r: Some(e.r.clone()),
        role: e.role.clone(),
        label: e.label.clone(),
        frame: e.frame,
        app: e.app.clone(),
        class: more(e, "class").and_then(Value::as_str).map(String::from),
        message,
        fix: fix.into(),
        detail,
    }
}

/// The elements an audit looks at: the app in front (or the home screen when no app is), every layer with `all`
pub fn in_scope(snap: &Snapshot, all: bool) -> Vec<&El> {
    let front = snap.front.first().cloned().unwrap_or_default();
    let has_app = snap.elements.iter().any(|e| e.app == front && e.layer == "app");
    snap.elements
        .iter()
        .filter(|e| !e.offscreen && !e.covered && e.role != "webview" && e.role != "key")
        .filter(|e| all || if has_app { e.app == front && e.layer == "app" } else { e.layer == "home" })
        .collect()
}

/// The rules on one snapshot (and its pixels for contrast). Pure: no phone needed (tests use it).
pub fn check(snap: &Snapshot, px: Option<&Pixels>, opts: &AuditOpts) -> (Vec<Finding>, usize) {
    let on = |r: &str| opts.rules.is_empty() || opts.rules.iter().any(|x| x == r);
    let els = in_scope(snap, opts.all);
    let mut out = Vec::new();
    let min_hit = match snap.platform.as_str() {
        "xros" => Some(60.0),
        "tvos" | "watchos" => None,
        _ => Some(44.0),
    };
    for e in &els {
        let label = e.label.as_deref().unwrap_or("").trim();
        let tap = more(e, "tap").and_then(Value::as_str);
        let class = more(e, "class").and_then(Value::as_str).unwrap_or("");
        // missing-label
        if on("missing-label") && label.is_empty() {
            if interactive(&e.role) {
                out.push(finding(e, "missing-label", "error", format!("{} has no accessibility label: VoiceOver says only \"{}\"", e.role, e.role),
                    "set accessibilityLabel (UIKit) / .accessibilityLabel(\"...\") (SwiftUI) to what it does, e.g. \"Add to Favorites\"", Value::Null));
            } else if e.role == "image" {
                out.push(finding(e, "missing-label", "warning", "image has no description".into(),
                    "describe it with accessibilityLabel, or hide a decorative image (isAccessibilityElement = false / .accessibilityHidden(true))", Value::Null));
            }
        }
        // label-not-readable
        if on("label-not-readable") && (interactive(&e.role) || e.role == "image") {
            if let Some(why) = unreadable_label(label) {
                out.push(finding(e, "label-not-readable", "warning", format!("label \"{label}\" looks like {why}: VoiceOver reads it as is"),
                    "give it a label in words (accessibilityLabel); an image named after its file gets its file name as the label", Value::Null));
            }
        }
        // hit-region
        if let (true, Some(min)) = (on("hit-region"), min_hit) {
            if interactive(&e.role) && e.role != "icon" && e.enabled && e.layer != "status" && (e.frame[2] < min - 0.5 || e.frame[3] < min - 0.5) {
                out.push(finding(e, "hit-region", "warning", format!("touch target {:.0} x {:.0} pt is smaller than {min:.0} x {min:.0} pt", e.frame[2], e.frame[3]),
                    "make the control (or its tappable area: contentEdgeInsets, a larger frame, .contentShape / .frame(minWidth:minHeight:)) at least 44 x 44 pt",
                    json!({"min": min})));
            }
        }
        // contrast
        if let (true, Some(px)) = (on("contrast"), px) {
            // controls only where the app's text color is known (a button's title): an image button's pixels are no text
            let fg_known = more(e, "fg").is_some();
            let texty = !label.is_empty() && (matches!(e.role.as_str(), "text" | "heading") || (fg_known && matches!(e.role.as_str(), "button" | "link" | "textfield" | "searchfield")));
            let area = e.frame[2] * e.frame[3];
            if texty && snap.platform != "xros" && area < snap.screen.0 * snap.screen.1 * 0.4 && e.layer != "status" {
                let fg = more(e, "fg").and_then(Value::as_array).map(|a| [0, 1, 2, 3].map(|i| a.get(i).and_then(Value::as_f64).unwrap_or(if i == 3 { 1.0 } else { 0.0 })));
                // text inside a cell: sample the cell where the label is (its frame comes with the label's)
                if let Some((r, c, bg)) = contrast_in(px, &e.frame, fg) {
                    let font = more(e, "font");
                    let size = font.and_then(|f| f["size"].as_f64());
                    let bold = font.and_then(|f| f["bold"].as_bool()).unwrap_or(false);
                    let large = size.map(|s| s >= 18.0 || (s >= 14.0 && bold)).unwrap_or(e.role == "heading");
                    let need = if large { 3.0 } else { 4.5 };
                    // the system tint (iOS 26+ blue #0088FF, older #007AFF) on white: Apple's own buttons, 3.5:1
                    let tint = fg.is_some() && matches!(hex(c).as_str(), "#0088FF" | "#007AFF");
                    if r < need {
                        out.push(finding(e, "contrast", if tint { "info" } else if r < 3.0 { "error" } else { "warning" },
                            format!("contrast {r:.2}:1 ({} on {}) is under {need}:1 for {} text", hex(c), hex(bg), if large { "large" } else { "normal" }),
                            "darken (or lighten) the text or its background; system colors (label, secondaryLabel) adapt and pass",
                            json!({"ratio": (r * 100.0).round() / 100.0, "fg": hex(c), "bg": hex(bg), "need": need, "fg_from": if fg.is_some() { "app" } else { "pixels" }, "font_size": size})));
                    }
                }
            }
        }
        // text-clipped (now)
        if on("text-clipped") {
            if let Some(t) = more(e, "truncated").and_then(Value::as_str) {
                out.push(finding(e, "text-clipped", "warning", format!("text is cut off: \"{}\"", t.chars().take(60).collect::<String>()),
                    "let the label wrap (numberOfLines = 0) and its container grow; avoid fixed heights", Value::Null));
            }
        }
        // trait-mismatch
        if on("trait-mismatch") && !interactive(&e.role) && e.enabled {
            let why = if class == "UIButton" || class.ends_with("Button") && tap.is_some() {
                Some(format!("a {class} without the button trait"))
            } else {
                tap.map(|t| format!("it takes taps ({}) but has no button trait", if t == "control" { "a UIControl" } else { "a tap gesture recognizer" }))
            };
            if let Some(w) = why {
                out.push(finding(e, "trait-mismatch", "warning", format!("looks like a button: {w}; VoiceOver says \"{}\" without \"button\"", if label.is_empty() { &e.role } else { label }),
                    "add the button trait (accessibilityTraits.insert(.button) / .accessibilityAddTraits(.isButton)) or use a Button", Value::Null));
            }
        }
    }
    // duplicate-label: interactive elements of one app that VoiceOver names the same
    if on("duplicate-label") {
        let mut groups: HashMap<(String, String, String), Vec<&El>> = HashMap::new();
        for e in els.iter().filter(|e| interactive(&e.role)) {
            if let Some(l) = e.label.as_deref().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()) {
                groups.entry((e.app.clone(), e.role.clone(), l)).or_default().push(e);
            }
        }
        let mut keys: Vec<_> = groups.keys().cloned().collect();
        keys.sort();
        for k in keys {
            let g = &groups[&k];
            if g.len() < 2 {
                continue;
            }
            let refs: Vec<String> = g.iter().map(|e| format!("@{}", e.r)).collect();
            // in different containers (a row's own Delete): still the same words to VoiceOver, but less of a problem
            let containers: std::collections::HashSet<_> = g.iter().map(|e| e.container.clone()).collect();
            let sev = if containers.len() == g.len() && containers.iter().all(|c| c.is_some()) { "info" } else { "warning" };
            for e in g {
                out.push(finding(e, "duplicate-label", sev, format!("{} {}s are labelled \"{}\" ({})", g.len(), k.1, e.label.as_deref().unwrap_or(""), refs.join(", ")),
                    "make each label say what is different (\"Delete Photo 3\"), or add a hint / value", json!({"same": refs})));
            }
        }
    }
    // hidden-interactive (simax's scan: interactive views out of VoiceOver's reach)
    if on("hidden-interactive") {
        let front = snap.front.first().cloned().unwrap_or_default();
        for h in &snap.hidden {
            let app = h["app"].as_str().unwrap_or("");
            if !opts.all && app != front {
                continue;
            }
            let f = h["frame"].as_array().map(|a| [0, 1, 2, 3].map(|i| a.get(i).and_then(Value::as_f64).unwrap_or(0.0))).unwrap_or_default();
            out.push(Finding {
                rule: "hidden-interactive".into(),
                severity: "error".into(),
                r: None,
                role: h["kind"].as_str().unwrap_or("view").into(),
                label: h["label"].as_str().map(String::from),
                frame: f,
                app: app.into(),
                class: h["class"].as_str().map(String::from),
                message: format!("{} {} at ({:.0},{:.0}) {:.0}x{:.0} takes taps but VoiceOver cannot reach it: {}",
                    if h["kind"] == "control" { "control" } else { "view with a tap gesture" }, h["class"].as_str().unwrap_or(""), f[0], f[1], f[2], f[3], h["why"].as_str().unwrap_or("")),
                fix: "make it an accessibility element with a label and the button trait (isAccessibilityElement = true), or remove accessibilityElementsHidden".into(),
                detail: json!({"controller": h["controller"], "oid": h["oid"]}),
            });
        }
    }
    let n = els.len();
    (out, n)
}

fn content_size(inst: &Instance, value: Option<&str>) -> Result<String> {
    let mut c = std::process::Command::new(crate::aot::repo_root().join("cli/x86sim"));
    c.arg("ui").arg(&inst.dir).arg("content_size");
    if let Some(v) = value {
        c.arg(v);
    }
    let o = c.current_dir(crate::aot::repo_root()).stdin(std::process::Stdio::null()).output()?;
    anyhow::ensure!(o.status.success(), "content_size: {}", String::from_utf8_lossy(&o.stderr).trim());
    Ok(String::from_utf8_lossy(&o.stdout).trim().lines().last().unwrap_or("").to_string())
}

pub const LARGEST: &str = "accessibility-extra-extra-extra-large";

/// The audit of the phone's screen now
pub fn audit(inst: &Instance, opts: &AuditOpts) -> Result<Report> {
    let t0 = Instant::now();
    let snap = axinspect::inspect(inst, false)?;
    let mut notes = Vec::new();
    let on = |r: &str| opts.rules.is_empty() || opts.rules.iter().any(|x| x == r);
    let px = if on("contrast") && snap.platform != "xros" {
        ax::settle(inst, Duration::from_millis(150), Duration::from_millis(1500));
        let div = inst.scale.round().max(1.0) as u32;
        ax::screenshot(inst, div).ok().and_then(|(png, _, _)| Pixels::from_png(&png, snap.screen))
    } else {
        None
    };
    if snap.platform == "xros" {
        notes.push("Vision Pro: frames are window points (no contrast check: the window's place on the display is not known)".into());
    }
    if matches!(snap.platform.as_str(), "tvos" | "watchos") {
        notes.push(format!("{}: no hit-region check (the focus engine / the watch's own sizes)", snap.platform));
    }
    let (mut findings, checked) = check(&snap, px.as_ref(), opts);
    if opts.dynamic_type && on("dynamic-type") {
        match dynamic_type(inst, &snap, opts) {
            Ok(mut f) => findings.append(&mut f),
            Err(e) => notes.push(format!("dynamic type check failed: {e:#}")),
        }
    } else if on("dynamic-type") {
        notes.push("dynamic type: only text cut off now was checked (dynamic_type=true re-checks at the largest accessibility text size)".into());
    }
    if snap.elements.iter().any(|e| e.more.as_ref().and_then(|m| m.get("class")).and_then(Value::as_str).is_some_and(|c| c.contains("SwiftUI"))) {
        notes.push("SwiftUI views: text color, font and truncation are read from UIKit labels only (contrast from pixels)".into());
    }
    let rank = |s: &str| match s {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    };
    findings.sort_by(|a, b| rank(&a.severity).cmp(&rank(&b.severity)).then(a.frame[1].total_cmp(&b.frame[1])).then(a.frame[0].total_cmp(&b.frame[0])));
    let mut apps: Vec<String> = in_scope(&snap, opts.all).iter().map(|e| e.app.clone()).collect();
    apps.dedup();
    apps.sort();
    apps.dedup();
    let count = |s: &str| findings.iter().filter(|f| f.severity == s).count();
    Ok(Report {
        phone: inst.name.clone(),
        platform: snap.platform.clone(),
        apps,
        screen: snap.screen,
        checked,
        errors: count("error"),
        warnings: count("warning"),
        infos: count("info"),
        rules: RULES.iter().filter(|r| on(r)).map(|r| r.to_string()).collect(),
        findings,
        notes,
        ms: t0.elapsed().as_millis() as u64,
    })
}

/// Dynamic Type: the screen again at the largest accessibility size (the phone's setting, put back after)
fn dynamic_type(inst: &Instance, before: &Snapshot, opts: &AuditOpts) -> Result<Vec<Finding>> {
    let was = content_size(inst, None)?;
    let was = if was.is_empty() { "large".to_string() } else { was };
    content_size(inst, Some(LARGEST))?;
    // apps relayout on the notification: wait for the screen to settle
    ax::settle(inst, Duration::from_millis(600), Duration::from_secs(6));
    let big = axinspect::inspect(inst, false);
    let restore = content_size(inst, Some(&was));
    ax::settle(inst, Duration::from_millis(400), Duration::from_secs(4));
    let big = big?;
    restore?;
    let old: HashMap<&str, &El> = before.elements.iter().map(|e| (e.r.as_str(), e)).collect();
    let mut out = Vec::new();
    for e in in_scope(&big, opts.all) {
        let prev = old.get(e.r.as_str());
        if let Some(t) = more(e, "truncated").and_then(Value::as_str) {
            if prev.is_none_or(|p| more(p, "truncated").is_none()) {
                out.push(finding(e, "dynamic-type", "warning", format!("text is cut off at the largest text size: \"{}\"", t.chars().take(60).collect::<String>()),
                    "let the label wrap (numberOfLines = 0), grow its container (self-sizing cells, no fixed heights) and stack side-by-side content vertically at accessibility sizes",
                    json!({"content_size": LARGEST})));
            }
        }
        // the font did not grow
        if let (Some(p), Some(f1)) = (prev, more(e, "font").and_then(|f| f["size"].as_f64())) {
            if let Some(f0) = more(p, "font").and_then(|f| f["size"].as_f64()) {
                if (f1 - f0).abs() < 0.5 && more(e, "dynamic_type").and_then(Value::as_bool) == Some(false) {
                    out.push(finding(e, "dynamic-type", "warning", format!("text stays {f0:.0} pt at the largest text size (Dynamic Type not supported)"),
                        "use a text style font (UIFont.preferredFont(forTextStyle:) / UIFontMetrics) and adjustsFontForContentSizeCategory = true; SwiftUI: .font(.body)",
                        json!({"size": f0, "content_size": LARGEST})));
                }
            }
        }
    }
    Ok(out)
}

impl Report {
    pub fn text(&self) -> String {
        let mut s = format!(
            "accessibility audit of {} on {}: {} error{}, {} warning{}, {} info ({} elements, {} ms)\n",
            if self.apps.is_empty() { "?".into() } else { self.apps.join(", ") },
            self.phone,
            self.errors,
            if self.errors == 1 { "" } else { "s" },
            self.warnings,
            if self.warnings == 1 { "" } else { "s" },
            self.infos,
            self.checked,
            self.ms
        );
        for f in &self.findings {
            let at = format!("({:.0},{:.0} {:.0}x{:.0})", f.frame[0], f.frame[1], f.frame[2], f.frame[3]);
            let who = match &f.r {
                Some(r) => format!("@{r} {}", f.role),
                None => f.class.clone().unwrap_or_else(|| f.role.clone()),
            };
            s += &format!("{:7} {:18} {who} {at}: {}\n        fix: {}\n", f.severity, f.rule, f.message, f.fix);
        }
        for n in &self.notes {
            s += &format!("note: {n}\n");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(r: &str, role: &str, label: Option<&str>, frame: [f64; 4]) -> El {
        El { r: r.into(), role: role.into(), label: label.map(String::from), frame, enabled: true, app: "a".into(), layer: "app".into(), ..Default::default() }
    }

    #[test]
    fn labels_that_are_names() {
        assert!(unreadable_label("ic_star_fill").is_some());
        assert!(unreadable_label("star.fill").is_some());
        assert!(unreadable_label("profile@2x.png").is_some());
        assert!(unreadable_label("settingsButton").is_some());
        assert!(unreadable_label("Settings").is_none());
        assert!(unreadable_label("Add to Favorites").is_none());
        assert!(unreadable_label("Wi-Fi").is_none());
        assert!(unreadable_label("iPhone").is_none());
        assert!(unreadable_label("macOS").is_none());
        assert!(unreadable_label("imgProfile").is_some());
    }

    #[test]
    fn wcag_ratio() {
        assert!((ratio([0, 0, 0], [255, 255, 255]) - 21.0).abs() < 0.01);
        assert!((ratio([118, 118, 118], [255, 255, 255]) - 4.54).abs() < 0.05);
    }

    #[test]
    fn rules_on_a_snapshot() {
        let mut snap = Snapshot { platform: "ios".into(), screen: (390.0, 844.0), front: vec!["a".into()], ..Default::default() };
        snap.elements = vec![
            el("e1", "button", None, [10.0, 10.0, 30.0, 30.0]),
            el("e2", "button", Some("ic_close"), [100.0, 10.0, 50.0, 50.0]),
            el("e3", "button", Some("Save"), [10.0, 100.0, 100.0, 44.0]),
            el("e4", "button", Some("Save"), [10.0, 200.0, 100.0, 44.0]),
            El { more: Some(json!({"tap": "gesture", "class": "UIImageView"})), ..el("e5", "image", Some("Profile"), [10.0, 300.0, 60.0, 60.0]) },
        ];
        snap.hidden = vec![json!({"app": "a", "kind": "gesture", "class": "UIImageView", "frame": [0, 400, 50, 50], "why": "x"})];
        let (f, n) = check(&snap, None, &AuditOpts::default());
        assert_eq!(n, 5);
        let has = |rule: &str, r: Option<&str>| f.iter().any(|x| x.rule == rule && x.r.as_deref() == r);
        assert!(has("missing-label", Some("e1")));
        assert!(has("hit-region", Some("e1")));
        assert!(has("label-not-readable", Some("e2")));
        assert!(has("duplicate-label", Some("e3")) && has("duplicate-label", Some("e4")));
        assert!(has("trait-mismatch", Some("e5")));
        assert!(has("hidden-interactive", None));
        assert!(!has("hit-region", Some("e3")));
    }

    #[test]
    fn contrast_from_pixels() {
        // white background, a grey bar of "text" (#AAAAAA: 2.3:1)
        let (w, h) = (100usize, 40usize);
        let mut rgb = vec![255u8; w * h * 3];
        for y in 15..25 {
            for x in 10..60 {
                let o = (y * w + x) * 3;
                rgb[o..o + 3].copy_from_slice(&[0xAA, 0xAA, 0xAA]);
            }
        }
        let px = Pixels { w, h, rgb, sx: 1.0, sy: 1.0 };
        let (r, fg, bg) = contrast_in(&px, &[0.0, 0.0, 100.0, 40.0], None).unwrap();
        assert_eq!(bg, [255, 255, 255]);
        assert_eq!(fg, [0xAA, 0xAA, 0xAA]);
        assert!((r - 2.32).abs() < 0.05, "{r}");
    }

    #[test]
    fn contrast_short_text_in_wide_frame() {
        // "Low contrast text" on an Apple TV: #BBBBBB glyphs over 120 of the frame's 1880 points (0.7% of its pixels)
        let (w, h) = (1880usize, 24usize);
        let mut rgb = vec![255u8; w * h * 3];
        for y in 10..14 {
            for x in (0..120).filter(|x| x % 3 != 0) {
                let o = (y * w + x) * 3;
                rgb[o..o + 3].copy_from_slice(&[0xBB, 0xBB, 0xBB]);
            }
        }
        let px = Pixels { w, h, rgb, sx: 1.0, sy: 1.0 };
        let (r, _, bg) = contrast_in(&px, &[0.0, 0.0, 1880.0, 24.0], Some([187.0, 187.0, 187.0, 1.0])).unwrap();
        assert_eq!(bg, [255, 255, 255]);
        assert!(r < 2.0, "{r}");
    }
}
