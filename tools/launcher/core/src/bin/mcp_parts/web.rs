//! x86sim-mcp: web_targets, web_eval, web_console, web_dom, web_network: Safari pages and apps' WKWebViews through the
//! phone's webinspectord (x86sim_core::rwi; the WebKit inspector protocol). Kept apart from mcp.rs like mcp_parts/simctl.rs.
use serde_json::{json, Value};
use std::time::Duration;
use x86sim_core::{instance, rwi};

pub fn tools(phone: &Value) -> Vec<Value> {
    let target = json!({"type": "string", "description": "the page: an id from web_targets (\"PID:1581/1\"), or text in its URL or title, or the app's bundle id. Default: the front app's first web page, else any"});
    let follow = |what: &str| json!({"type": "number", "description": format!("then keep collecting {what} for this many seconds (default 0, at most 60)")});
    let reload = |what: &str| json!({"type": "boolean", "description": format!("reload the page first and collect {what} from its load (at least 3 s)")});
    vec![
        json!({"name": "web_targets", "description": "Web content you can inspect on a running device, per app: Safari's pages (every tab), WKWebViews of apps that set isInspectable = true (as in Apple's simulator), JSContexts. Each page: id (for the other web_* tools), title, URL, kind, app. Starts the device's webinspectord if needed (the webinspector feature, on by default).",
            "inputSchema": {"type": "object", "properties": {"phone": phone}}}),
        json!({"name": "web_eval", "description": "Evaluate JavaScript in a web page (Runtime.evaluate, as in Web Inspector's console: $0, $$() and the other console helpers work; a promise is awaited). Answers the value (JSON) or the exception.",
            "inputSchema": {"type": "object", "properties": {"phone": phone, "target": target, "expression": {"type": "string"}, "timeout_s": {"type": "number", "description": "default 20"}}, "required": ["expression"]}}),
        json!({"name": "web_console", "description": "A web page's console messages, then new ones while following. Each: level, source, text, url, line, repeat count. WebKit keeps a page's messages only from its first inspection on (any web_* call or the inspector): what it logged at load before that is gone, so use reload to see a load's messages.",
            "inputSchema": {"type": "object", "properties": {"phone": phone, "target": target, "follow_s": follow("new messages"), "reload": reload("its messages"), "limit": {"type": "integer", "description": "at most this many, the newest (default 200)"}}}}),
        json!({"name": "web_dom", "description": "A web page's HTML: outerHTML of the first element matching a CSS selector (with the number of matches), or the whole document.",
            "inputSchema": {"type": "object", "properties": {"phone": phone, "target": target, "selector": {"type": "string"}, "max_chars": {"type": "integer", "description": "cut the HTML here (default 20000)"}}}}),
        json!({"name": "web_network", "description": "A web page's network requests: every request since it loaded (Resource Timing: URL, type, start, duration, size; no status), and with follow_s the live ones (method, URL, status, MIME type, failures) for that long; reload: the page's load, every request live with its status.",
            "inputSchema": {"type": "object", "properties": {"phone": phone, "target": target, "follow_s": follow("live requests"), "reload": reload("its requests"), "limit": {"type": "integer", "description": "at most this many past requests, the newest (default 200)"}}}}),
    ]
}

fn secs(a: &Value, k: &str, default: f64, max: f64) -> Duration {
    Duration::from_secs_f64(a[k].as_f64().unwrap_or(default).clamp(0.0, max))
}

/// Some(result) for the web_* tools, None for others.
pub fn call(name: &str, a: &Value, phone: &dyn Fn() -> anyhow::Result<instance::Instance>) -> Option<anyhow::Result<Value>> {
    if !name.starts_with("web_") {
        return None;
    }
    let target = a["target"].as_str();
    let limit = a["limit"].as_u64().unwrap_or(200).clamp(1, 5000) as usize;
    Some((|| {
        let inst = phone()?;
        match name {
            "web_targets" => {
                let apps = rwi::targets(&inst, Duration::from_secs(3))?;
                let n: usize = apps.iter().map(|a| a.pages.len()).sum();
                Ok(json!({"phone": inst.name, "pages": n, "apps": apps}))
            }
            "web_eval" => {
                let e = a["expression"].as_str().ok_or_else(|| anyhow::anyhow!("expression: the JavaScript to run"))?;
                rwi::eval(&inst, target, e, secs(a, "timeout_s", 20.0, 300.0))
            }
            "web_console" => rwi::console(&inst, target, secs(a, "follow_s", 0.0, 60.0), limit, a["reload"].as_bool() == Some(true)),
            "web_dom" => rwi::dom(&inst, target, a["selector"].as_str(), a["max_chars"].as_u64().unwrap_or(20000).clamp(100, 2_000_000) as usize),
            "web_network" => rwi::network(&inst, target, secs(a, "follow_s", 0.0, 60.0), limit, a["reload"].as_bool() == Some(true)),
            _ => anyhow::bail!("unknown tool {name}"),
        }
    })())
}
