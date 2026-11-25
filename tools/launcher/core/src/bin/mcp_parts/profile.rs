//! x86sim-mcp: profile_start / profile_stop, the CPU profiler (x86sim_core::profile: libarm64jit's sampler, started
//! and stopped at run time in one process). One profile per phone at a time; kept apart from mcp.rs like
//! mcp_parts/debug.rs.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use x86sim_core::{instance, profile};

static SESSIONS: Mutex<Option<HashMap<String, (profile::Session, std::path::PathBuf)>>> = Mutex::new(None);

pub fn tools(phone: &Value) -> Vec<Value> {
    vec![
        json!({"name": "profile_start", "description": "Start a CPU profile of an app in a running device (as Instruments' Time Profiler): the translator samples the app's threads while they run (one sample per period_us of CPU time) until profile_stop. Times are those of the translated x86-64 code, so relative costs differ from a device; stacks come from frame pointers.",
            "inputSchema": {"type": "object", "properties": {"phone": phone,
                "app": {"type": "string", "description": "bundle id of the app, or a pid (x86sim pidof)"},
                "period_us": {"type": "integer", "description": "CPU time per sample in microseconds (default 1000)"},
                "max_secs": {"type": "integer", "description": "the sampler stops by itself after this long (default 600)"}},
                "required": ["app"]}}),
        json!({"name": "profile_stop", "description": "Stop the device's CPU profile and answer the summary (top functions by self and total time, per thread; guest code vs native library functions vs the translator) and the path of the profile file (speedscope JSON for https://www.speedscope.app, or gecko for https://profiler.firefox.com, or text).",
            "inputSchema": {"type": "object", "properties": {"phone": phone,
                "format": {"type": "string", "enum": ["speedscope", "gecko", "text"], "description": "the file's format (default speedscope)"},
                "out": {"type": "string", "description": "where to write it (default under $X86SIM_SCRATCH/profiles)"},
                "top": {"type": "integer", "description": "functions per list in the summary (default 25)"}}}}),
    ]
}

/// Some(result) for the profile_* tools, None for other tools.
pub fn call(name: &str, a: &Value, phone: &dyn Fn() -> anyhow::Result<instance::Instance>) -> Option<anyhow::Result<(String, Option<Value>)>> {
    if !name.starts_with("profile_") {
        return None;
    }
    Some((|| {
        let inst = phone()?;
        let mut g = SESSIONS.lock().unwrap();
        let map = g.get_or_insert_with(HashMap::new);
        match name {
            "profile_start" => {
                if let Some((s, _)) = map.get(&inst.id) {
                    if profile::linux_pid(s.target.gpid, &s.target.udid) == Some(s.target.hpid) {
                        anyhow::bail!("{} already has a profile of {} running (profile_stop first)", inst.name, s.target.label);
                    }
                }
                let app = a["app"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("app required"))?;
                let t = profile::resolve(&inst, app)?;
                let period = a["period_us"].as_u64().unwrap_or(1000).clamp(100, 1_000_000) as u32;
                let max = a["max_secs"].as_u64().unwrap_or(600).clamp(1, 3600) as u32;
                let s = profile::start(&t, period, max)?;
                let v = json!({"app": app, "pid": t.gpid, "period_us": period, "max_secs": max, "raw": s.raw});
                map.insert(inst.id.clone(), (s, inst.runtime_root.clone()));
                Ok((format!("profiling {app} (pid {}) on {}; profile_stop to finish", t.gpid, inst.name), Some(v)))
            }
            "profile_stop" => {
                let (s, rt) = map.remove(&inst.id).ok_or_else(|| anyhow::anyhow!("no profile running on {} (profile_start first)", inst.name))?;
                drop(g);
                let fmt = a["format"].as_str().map(|f| profile::Format::parse(f).ok_or_else(|| anyhow::anyhow!("unknown format {f}"))).transpose()?.unwrap_or(profile::Format::Speedscope);
                let top = a["top"].as_u64().unwrap_or(25) as usize;
                let out = a["out"].as_str().filter(|s| !s.is_empty()).map(std::path::PathBuf::from);
                let raw = profile::stop(&s)?;
                let secs = s.started.elapsed().as_secs_f64();
                let (path, summary, p) = profile::report(&raw, Some(&rt), fmt, out.as_deref(), top)?;
                let n: usize = p.threads.iter().map(|t| t.samples.len()).sum();
                let v = json!({"file": path, "raw": raw, "samples": n, "secs": secs, "complete": p.complete});
                Ok((format!("{summary}\nprofile: {}\n", path.display()), Some(v)))
            }
            _ => anyhow::bail!("unknown tool {name}"),
        }
    })())
}
