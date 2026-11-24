//! x86sim-mcp: debug_start / debug_command / debug_stop, LLDB on an app in a phone (cli/x86sim debug,
//! cli/libexec/lldbattach.py: a background lldb session per phone over the stub in libarm64jit). Kept apart from
//! mcp.rs like mcp_parts/simctl.rs.
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use x86sim_core::{aot::repo_root, instance};

pub fn tools(phone: &Value) -> Vec<Value> {
    vec![
        json!({"name": "debug_start", "description": "Attach LLDB to an app in a running device (as Xcode's Debug > Attach to Process): a debug session that stays open for debug_command until debug_stop. The process stops; answers the threads and the setup's output. launch: (re)launch the app stopped before dyld runs, so breakpoints in main, initializers and +load work (then debug_command 'continue'). symbols: a .dSYM or unstripped executable for source lines and variables. Uses the Swift toolchain's lldb when installed (set up on first use, or x86sim dev setup lldb): Swift values in frame variable, po and expression, as in Xcode; else upstream lldb (C / Objective-C only, Swift values as raw memory). The answer's 'swift' says which. One session per device.",
            "inputSchema": {"type": "object", "properties": {"phone": phone,
                "app": {"type": "string", "description": "bundle id of the app, or a pid (x86sim pidof)"},
                "launch": {"type": "boolean", "description": "relaunch it stopped before dyld (default false: attach to the running app)"},
                "wait": {"type": "boolean", "description": "wait until the app is started (by you or a tap), then attach"},
                "symbols": {"type": "string", "description": "path of a .dSYM (or its DWARF file) for the app's executable"}},
                "required": ["app"]}}),
        json!({"name": "debug_command", "description": "Run one lldb command in the device's debug session and answer its output and the process state (stopped / running / exited). Commands that resume (continue, next, step, finish) wait up to timeout_s for the next stop; 'process interrupt' stops a running process. Examples: 'breakpoint set -n viewDidLoad', 'bt all', 'frame variable', 'register read', 'memory read --size 8 --count 4 $sp', 'image lookup -a 0x1c0001234', 'expr (int)getpid()', 'po self', 'expression point.x + 1'.",
            "inputSchema": {"type": "object", "properties": {"phone": phone,
                "command": {"type": "string"},
                "timeout_s": {"type": "number", "description": "how long to wait for a stop after a resume (default 30)"}},
                "required": ["command"]}}),
        json!({"name": "debug_stop", "description": "Detach LLDB from the app (breakpoints removed, the app runs on) and end the device's debug session.",
            "inputSchema": {"type": "object", "properties": {"phone": phone}}}),
    ]
}

fn run(inst: &instance::Instance, args: &[String]) -> anyhow::Result<Value> {
    let out = Command::new(repo_root().join("cli/x86sim"))
        .arg("--phone").arg(&inst.dir).arg("debug").args(args)
        .current_dir(repo_root()).stdin(Stdio::null()).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    match serde_json::from_str::<Value>(text.trim().lines().last().unwrap_or("")) {
        Ok(v) => Ok(v),
        Err(_) => anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim()),
    }
}

/// Some(result) for the debug_* tools, None for other tools.
pub fn call(name: &str, a: &Value, phone: &dyn Fn() -> anyhow::Result<instance::Instance>) -> Option<anyhow::Result<Value>> {
    if !name.starts_with("debug_") {
        return None;
    }
    Some((|| {
        let inst = phone()?;
        let mut args: Vec<String> = Vec::new();
        match name {
            "debug_start" => {
                let app = a["app"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("app required"))?;
                args.extend(["--start".into(), app.into()]);
                if a["launch"].as_bool() == Some(true) { args.push("--launch".into()); }
                if a["wait"].as_bool() == Some(true) { args.push("--wait".into()); }
                if let Some(s) = a["symbols"].as_str().filter(|s| !s.is_empty()) { args.extend(["--symbols".into(), s.into()]); }
            }
            "debug_command" => {
                let c = a["command"].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| anyhow::anyhow!("command required"))?;
                args.extend(["--cmd".into(), c.into()]);
                if let Some(t) = a["timeout_s"].as_f64() { args.extend(["--timeout".into(), t.to_string()]); }
            }
            "debug_stop" => args.push("--stop".into()),
            _ => anyhow::bail!("unknown tool {name}"),
        }
        run(&inst, &args)
    })())
}
