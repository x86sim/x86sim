//! The Accessibility Inspector (as Xcode's): an element's properties from the full snapshot (simax `tree full`: the
//! extras in `El::more`), the element under a point, and accessibility actions (simax `action`): activate, increment /
//! decrement, scroll, escape, magic tap and custom actions by name. Used by the MCP tools (`ax_inspect`, `ax_action`),
//! `x86sim ax-inspect` / `ax-action`, the browser viewer's inspector (share/axapi.rs) and the launcher's phone page.
//! The audit is axaudit.rs. Protocol: docs/protocols/SIMAX.md.
use crate::ax::{self, After, El, Outcome, SnapOpts, Snapshot};
use crate::instance::Instance;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// The actions an accessibility action can be: activate, increment, decrement, scroll, escape, magic_tap, custom
pub const ACTIONS: &[&str] = &["activate", "increment", "decrement", "scroll", "escape", "magic_tap", "custom"];

/// A snapshot with the inspector's extras (refs are the snapshot's: they stay valid for tap and the others)
pub fn inspect(inst: &Instance, all: bool) -> Result<Snapshot> {
    ax::snapshot(inst, SnapOpts { full: true, all, ..Default::default() })
}

fn contains(f: &[f64; 4], x: f64, y: f64) -> bool {
    x >= f[0] && y >= f[1] && x <= f[0] + f[2] && y <= f[1] + f[3]
}

fn layer_rank(l: &str) -> u8 {
    match l {
        "system" => 0,
        "status" => 1,
        "window" => 2,
        "app" => 3,
        _ => 4,
    }
}

/// The element under a point (screen points): the top layer's smallest element there (what the inspector's pointer
/// picks), web views only when nothing of their page is there
pub fn element_at(snap: &Snapshot, x: f64, y: f64) -> Option<&El> {
    snap.elements
        .iter()
        .filter(|e| !e.offscreen && !e.covered && contains(&e.frame, x, y))
        .min_by(|a, b| {
            let k = |e: &El| (layer_rank(&e.layer), e.role == "webview");
            k(a).cmp(&k(b)).then((a.frame[2] * a.frame[3]).total_cmp(&(b.frame[2] * b.frame[3])))
        })
}

fn more<'a>(e: &'a El, k: &str) -> Option<&'a Value> {
    e.more.as_ref().and_then(|m| m.get(k)).filter(|v| !v.is_null())
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// What the element offers, as VoiceOver's actions rotor: activate, increment / decrement (adjustable), escape,
/// magic tap, scroll, and its custom actions ("custom:<name>")
pub fn actions_of(e: &El) -> Vec<String> {
    let mut v = Vec::new();
    if e.web {
        return vec!["activate (tap)".into()];
    }
    v.push("activate".to_string());
    if e.role == "adjustable" || strs(more(e, "traits_all")).iter().any(|t| t == "adjustable") {
        v.push("increment".into());
        v.push("decrement".into());
    }
    v.push("scroll".into());
    v.push("escape".into());
    v.push("magic_tap".into());
    for c in strs(more(e, "custom_actions")) {
        v.push(format!("custom:{c}"));
    }
    v
}

/// The properties pane: (name, value) pairs as Xcode's inspector groups them (Basic, Actions, Element, Hierarchy)
pub fn properties(e: &El) -> Vec<(String, String)> {
    let mut p: Vec<(String, String)> = Vec::new();
    let mut add = |k: &str, v: String| {
        if !v.is_empty() {
            p.push((k.to_string(), v));
        }
    };
    add("Ref", format!("@{}", e.r));
    add("Role", e.role.clone());
    add("Label", e.label.clone().unwrap_or_else(|| "(none)".into()));
    add("Value", e.value.clone().unwrap_or_default());
    add("Hint", e.hint.clone().unwrap_or_default());
    add("Identifier", e.id.clone().unwrap_or_default());
    let traits = strs(more(e, "traits_all"));
    add("Traits", if traits.is_empty() { "none".into() } else { traits.join(", ") });
    let [x, y, w, h] = e.frame;
    add("Frame", format!("x {x:.1}  y {y:.1}  w {w:.1}  h {h:.1}"));
    if let Some(a) = more(e, "activation").and_then(Value::as_array) {
        add("Activation point", format!("{:.1}, {:.1}", a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)));
    }
    let mut st = Vec::new();
    if !e.enabled {
        st.push("disabled");
    }
    if e.selected {
        st.push("selected");
    }
    if e.focused {
        st.push("focused");
    }
    if e.offscreen {
        st.push("offscreen");
    }
    if e.covered {
        st.push("covered");
    }
    add("State", if st.is_empty() { "enabled".into() } else { st.join(", ") });
    add("Actions", actions_of(e).into_iter().filter(|a| !a.starts_with("custom:")).collect::<Vec<_>>().join(", "));
    add("Custom actions", strs(more(e, "custom_actions")).join(", "));
    add("Custom rotors", strs(more(e, "rotors")).join(", "));
    add("Input labels", strs(more(e, "input_labels")).join(", "));
    add("Language", more(e, "lang").and_then(Value::as_str).unwrap_or("").to_string());
    if let Some(b) = more(e, "interactive").and_then(Value::as_bool) {
        add("Responds to user interaction", if b { "yes" } else { "no" }.into());
    }
    if more(e, "modal").is_some() {
        add("View is modal", "yes".into());
    }
    if let Some(f) = more(e, "font") {
        add("Font", format!("{} {:.1} pt{}", f["name"].as_str().unwrap_or(""), f["size"].as_f64().unwrap_or(0.0), if f["bold"].as_bool() == Some(true) { " bold" } else { "" }));
    }
    if let Some(d) = more(e, "dynamic_type").and_then(Value::as_bool) {
        add("Dynamic Type", if d { "adjusts (adjustsFontForContentSizeCategory)" } else { "fixed size" }.into());
    }
    if let Some(c) = more(e, "fg").and_then(Value::as_array) {
        add("Text color", rgb_hex(c));
    }
    if let Some(t) = more(e, "truncated").and_then(Value::as_str) {
        add("Truncated", format!("yes: \"{}\"", t.chars().take(60).collect::<String>()));
    }
    let cls = |k: &str| short_class(more(e, k).and_then(Value::as_str).unwrap_or(""));
    add("Element class", cls("class"));
    add("View", cls("view"));
    add("Controller", cls("controller"));
    add("Container", e.container.clone().unwrap_or_default());
    add("Hierarchy", strs(more(e, "path")).iter().map(|p| match p.split_once(' ') {
        Some((c, rest)) => format!("{} {rest}", short_class(c)),
        None => short_class(p),
    }).collect::<Vec<_>>().join(" › "));
    add("App", format!("{} (pid {})", e.app, e.pid));
    add("Layer", format!("{} · window {}", e.layer, e.window));
    if e.web {
        add("Web", e.href.clone().unwrap_or_else(|| "DOM element".into()));
    }
    p
}

/// A Swift class's mangled name made readable: `_TtGC7SwiftUI15CellHostingView...` -> `SwiftUI.CellHostingView<…>`
pub fn short_class(c: &str) -> String {
    if !c.starts_with("_Tt") {
        return c.to_string();
    }
    let b = c.as_bytes();
    let mut parts = Vec::new();
    let mut i = 3;
    while i < b.len() && parts.len() < 2 {
        if b[i].is_ascii_digit() {
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n: usize = c[st..i].parse().unwrap_or(0);
            if n == 0 || i + n > b.len() {
                break;
            }
            parts.push(&c[i..i + n]);
            i += n;
        } else {
            i += 1;
        }
    }
    match parts.as_slice() {
        [m, n] => format!("{m}.{n}{}", if c.starts_with("_TtG") { "<…>" } else { "" }),
        _ => c.to_string(),
    }
}

#[cfg(test)]
#[test]
fn swift_names() {
    assert_eq!(short_class("_TtGC7SwiftUI15CellHostingViewGVS_15ModifiedContent__"), "SwiftUI.CellHostingView<…>");
    assert_eq!(short_class("_TtC10x86a11ybad5BadVC"), "x86a11ybad.BadVC");
    assert_eq!(short_class("UIButton"), "UIButton");
}

pub fn rgb_hex(c: &[Value]) -> String {
    let n = |i: usize| c.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    let a = n(3);
    format!("#{:02X}{:02X}{:02X}{}", n(0) as u8, n(1) as u8, n(2) as u8, if a < 0.995 { format!(" @ {:.0}%", a * 100.0) } else { String::new() })
}

/// The properties as text (CLI / MCP)
pub fn describe(e: &El) -> String {
    let p = properties(e);
    let w = p.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    p.iter().map(|(k, v)| format!("{k:w$}  {v}\n")).collect()
}

/// An element as JSON for the inspector panes: the El plus its properties and actions
pub fn element_json(e: &El) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or_default();
    v["properties"] = json!(properties(e).into_iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>());
    v["actions"] = json!(actions_of(e));
    v
}

/// Performs an accessibility action on an element (target as tap: @ref, id:..., label text). `arg`: the scroll
/// direction (up down left right next previous: VoiceOver's three-finger swipe directions, up shows what is below)
/// or the custom action's name. activate falls back to a tap made in the app at the element's activation point (what
/// VoiceOver does when accessibilityActivate is not handled). escape and magic_tap without a target act on the focused
/// element / the front app.
pub fn action(inst: &Instance, target: Option<&str>, name: &str, arg: Option<&str>, after: After) -> Result<Outcome> {
    let (name, arg) = match name.strip_prefix("custom:") {
        Some(n) => ("custom", Some(n)),
        None => (name, arg),
    };
    if !ACTIONS.contains(&name) {
        bail!("action {name:?}: one of {}", ACTIONS.join(", "));
    }
    let (e, snap) = match target.filter(|t| !t.is_empty()) {
        Some(t) => {
            let (e, s) = ax::resolve(inst, t)?;
            (Some(e), s)
        }
        None if matches!(name, "escape" | "magic_tap") => (None, ax::snapshot(inst, SnapOpts::default())?),
        None => bail!("{name} needs a target (@ref, id:..., or label text)"),
    };
    if e.as_ref().is_some_and(|e| e.web) {
        if name == "activate" {
            return ax::tap(inst, target.unwrap_or_default(), 80, after);
        }
        bail!("web content has no accessibility actions here (the page is read through its DOM): tap it");
    }
    let pid = match &e {
        Some(e) => e.pid,
        None => {
            let front = snap.front.first().cloned().unwrap_or_default();
            snap.elements.iter().find(|x| x.app == front).map(|x| x.pid).ok_or_else(|| anyhow::anyhow!("no app in front"))?
        }
    };
    let oid = e.as_ref().map(|e| e.oid.clone()).unwrap_or_default();
    let req = match name {
        "activate" => json!({"op": "activate", "oid": oid, "tap": true}),
        _ => {
            let mut r = json!({"op": "action", "name": name, "oid": oid});
            match name {
                "scroll" => r["dir"] = json!(arg.unwrap_or("up")),
                "custom" => r["custom"] = json!(arg.ok_or_else(|| anyhow::anyhow!("custom needs the action's name"))?),
                _ => {}
            }
            r
        }
    };
    let r = ax::ask_pid(inst, pid, &req, Duration::from_secs(5))?;
    if let Some(err) = r["error"].as_str() {
        bail!("{name}: {err}");
    }
    let what = match &e {
        Some(e) => format!("@{} {} {}", e.r, e.role, e.label.as_deref().map(|l| format!("\"{l}\"")).unwrap_or_default()),
        None => "the focused element".into(),
    };
    let ok = r["ok"].as_bool() == Some(true);
    let mut did = match (name, ok) {
        ("activate", true) if r["tapped"].as_bool() == Some(true) => format!("activated {what} (accessibilityActivate not handled: a tap in the app at its activation point, as VoiceOver)"),
        ("activate", true) => format!("activated {what} (accessibilityActivate)"),
        (_, true) => format!("{name}{} on {what}: handled by {}", arg.map(|a| format!(" {a}")).unwrap_or_default(), r["by"].as_str().unwrap_or("?")),
        (_, false) => format!("{name}{} on {what}: nothing took it (no element or container handles it)", arg.map(|a| format!(" {a}")).unwrap_or_default()),
    };
    if let Some(v) = r["value"].as_str() {
        did += &format!("; value now \"{v}\"");
    }
    if let Some(n) = r["note"].as_str() {
        did += &format!(" ({n})");
    }
    Ok(ax::outcome(inst, did, Some(snap), after))
}
