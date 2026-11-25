//! x86sim-mcp: the Accessibility Inspector's tools (ax_inspect, ax_audit, ax_action; x86sim_core::axinspect /
//! axaudit). Kept apart from mcp.rs's own tools so the two merge easily; mcp.rs adds `tools()` to its list and asks
//! `call()` first.
use serde_json::{json, Value};
use x86sim_core::{ax, axaudit, axinspect, instance};

pub fn tools(phone: &Value) -> Vec<Value> {
    vec![
        json!({"name": "ax_inspect", "description": "Accessibility Inspector: every property of an element, as Xcode's inspector shows it: label, value, hint, traits, identifier, frame, activation point, actions and custom actions, custom rotors, input labels, language, respondsToUserInteraction, the element's class, its view and view controller, the accessibility hierarchy above it (container path), its text's font, color and whether it is cut off. target: a ref (@e3), id:..., label text; or x and y (points): the element there. Without either: the screen as the snapshot's lines plus the interactive views VoiceOver cannot reach. json=true: the records.",
               "inputSchema": {"type": "object", "properties": {"phone": phone, "target": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}, "json": {"type": "boolean"}}}}),
        json!({"name": "ax_audit", "description": "Accessibility audit of the app in front (as Xcode's Accessibility Inspector audit): missing labels on controls and images, labels that are file names / SF Symbol names / identifiers, touch targets under 44x44 pt (60 on Vision Pro), text contrast under WCAG 4.5:1 (3:1 for large text; text color from the app, background sampled from the screen), text cut off, Dynamic Type (dynamic_type=true: re-checks at the largest accessibility text size, then puts the device's text size back), duplicate labels, interactive views VoiceOver cannot reach, things that take taps without the button trait. Answers findings with ref, frame, severity (error / warning / info) and a fix hint; structured JSON too. all=true: every layer (home screen, alerts), not only the front app. rules: only these.",
               "inputSchema": {"type": "object", "properties": {"phone": phone, "dynamic_type": {"type": "boolean"}, "all": {"type": "boolean"},
                   "rules": {"type": "array", "items": {"type": "string", "enum": axaudit::RULES}}}}}),
        json!({"name": "ax_action", "description": "Perform an accessibility action on an element, as VoiceOver does: activate (accessibilityActivate, else a tap made in the app at the element's activation point), increment / decrement (sliders, steppers, pickers: anything adjustable), scroll (direction up / down / left / right / next / previous: VoiceOver's three-finger swipes, up shows what is below), escape (two-finger Z: back / dismiss), magic_tap (two-finger double tap: the app's main action, e.g. play / pause, answer a call), custom (one of the element's custom actions by name, see ax_inspect). Useful where touches are awkward (Apple TV focus, Vision Pro windows). target: @ref, id:..., or label text (escape and magic_tap may go without: the focused element / the app). Answers with the screen after it.",
               "inputSchema": {"type": "object", "properties": {"phone": phone, "target": {"type": "string"}, "action": {"type": "string", "enum": axinspect::ACTIONS},
                   "direction": {"type": "string", "enum": ["up", "down", "left", "right", "next", "previous"]}, "name": {"type": "string", "description": "the custom action's name"},
                   "after": {"type": "string", "enum": ["full", "diff", "none"]}}, "required": ["action"]}}),
    ]
}

fn is_ours(name: &str) -> bool {
    matches!(name, "ax_inspect" | "ax_audit" | "ax_action")
}

/// Some((text, structured)) for the tools of this module, None for others.
pub fn call(name: &str, a: &Value, phone: &dyn Fn() -> anyhow::Result<instance::Instance>) -> Option<anyhow::Result<(String, Option<Value>)>> {
    if !is_ours(name) {
        return None;
    }
    let inst = match phone() {
        Ok(i) => i,
        Err(e) => return Some(Err(e)),
    };
    Some((|| -> anyhow::Result<(String, Option<Value>)> {
        match name {
            "ax_inspect" => {
                let snap = axinspect::inspect(&inst, false)?;
                let el = match (a["target"].as_str().filter(|t| !t.is_empty()), a["x"].as_f64(), a["y"].as_f64()) {
                    (Some(t), _, _) => Some(ax::find(&snap, t)?.clone()),
                    (None, Some(x), Some(y)) => Some(axinspect::element_at(&snap, x, y).cloned().ok_or_else(|| anyhow::anyhow!("no element at ({x}, {y})"))?),
                    _ => None,
                };
                let json = a["json"].as_bool().unwrap_or(false);
                Ok(match el {
                    Some(e) => (axinspect::describe(&e), json.then(|| axinspect::element_json(&e))),
                    None => {
                        let mut t = snap.text();
                        if !snap.hidden.is_empty() {
                            t += "not reachable by VoiceOver (interactive views that are not accessibility elements):\n";
                            for h in &snap.hidden {
                                t += &format!("  {} {} {} {}\n", h["app"].as_str().unwrap_or(""), h["class"].as_str().unwrap_or(""), h["frame"], h["why"].as_str().unwrap_or(""));
                            }
                        }
                        (t, json.then(|| serde_json::to_value(&snap).unwrap_or_default()))
                    }
                })
            }
            "ax_audit" => {
                let opts = axaudit::AuditOpts {
                    dynamic_type: a["dynamic_type"].as_bool().unwrap_or(false),
                    all: a["all"].as_bool().unwrap_or(false),
                    rules: a["rules"].as_array().map(|r| r.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                };
                let r = axaudit::audit(&inst, &opts)?;
                Ok((r.text(), Some(serde_json::to_value(&r)?)))
            }
            "ax_action" => {
                let action = a["action"].as_str().unwrap_or("activate");
                let arg = a["direction"].as_str().or(a["name"].as_str());
                let after = ax::After::parse(a["after"].as_str());
                let o = axinspect::action(&inst, a["target"].as_str(), action, arg, after)?;
                Ok((o.text(), None))
            }
            _ => unreachable!(),
        }
    })())
}
