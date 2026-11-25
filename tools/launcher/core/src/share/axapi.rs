//! Share: the Accessibility Inspector's routes for the browser viewer (web/axinspect.js), kept apart from server.rs so
//! the two merge easily. Under /api/phones/<id>/ax/:
//!   GET  tree                        the full snapshot (axinspect.rs): elements with their properties and actions,
//!                                    the interactive views VoiceOver cannot reach
//!   GET  audit[?dynamic_type=1&all=1] the audit (axaudit.rs); dynamic_type changes the phone's text size for a few
//!                                    seconds: control role only
//!   POST action {target, action, arg} an accessibility action (control role only)
use super::server::Req;
use crate::ax::After;
use crate::instance::Instance;
use crate::{axaudit, axinspect};
use serde_json::{json, Value};

/// (HTTP status, JSON body) for `what` (the segment after ax/)
pub fn handle(req: &Req, inst: &Instance, what: &str, control: bool) -> (u16, Value) {
    let flag = |k: &str| req.query.get(k).is_some_and(|v| v == "1" || v == "true");
    let r: anyhow::Result<Value> = match (req.method.as_str(), what) {
        ("GET", "tree") => axinspect::inspect(inst, false).map(|s| {
            json!({"phone": s.phone, "platform": s.platform, "screen": [s.screen.0, s.screen.1], "front": s.front, "focus": s.focus, "ms": s.ms,
                   "elements": s.elements.iter().map(axinspect::element_json).collect::<Vec<_>>(), "hidden": s.hidden})
        }),
        ("GET", "audit") => {
            if flag("dynamic_type") && !control {
                return (403, json!({"error": "the Dynamic Type check changes the device's text size: it needs a control link"}));
            }
            let o = axaudit::AuditOpts { dynamic_type: flag("dynamic_type"), all: flag("all"), rules: vec![] };
            axaudit::audit(inst, &o).and_then(|r| Ok(serde_json::to_value(r)?))
        }
        ("POST", "action") if control => {
            let b: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            axinspect::action(inst, b["target"].as_str(), b["action"].as_str().unwrap_or("activate"), b["arg"].as_str(), After::None).map(|o| json!({"did": o.did}))
        }
        ("POST", "action") => return (403, json!({"error": "actions need a control link"})),
        _ => return (404, json!({"error": "not found"})),
    };
    match r {
        Ok(v) => (200, v),
        Err(e) => (409, json!({"error": format!("{e:#}")})),
    }
}
