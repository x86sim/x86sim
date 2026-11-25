//! x86sim-mcp: a small MCP server (JSON-RPC 2.0 over stdio, one message per line) for the x86Sim arm64 simulator:
//! feature toggles (list / get / set / preset), sim control (status / start / stop / restart to apply) and the AOT
//! image status. Uses x86sim-core like the CLI and the GUI.
//!
//! Register it with an MCP client, e.g. `claude mcp add x86sim -- /path/to/x86sim-mcp`.
#![recursion_limit = "512"]
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use x86sim_core::{aot, apps, ax, features, instance, sim};
// ---- simctl parity (feat/ap-simctl): runtime_delete, sim_ui, sim_status_bar, ... in their own module ----
#[path = "mcp_parts/simctl.rs"]
mod mcp_simctl;
// ---- XCTest (feat/ap-xctest): xctest_run ----
#[path = "mcp_parts/xctest.rs"]
mod mcp_xctest;
// ---- Accessibility Inspector (feat/r2-axinspect): ax_inspect, ax_audit, ax_action ----
#[path = "mcp_parts/axinspect.rs"]
mod mcp_axinspect;
// ---- Web Inspector (feat/r2-webinspect): web_targets, web_eval, web_console, web_dom, web_network ----
#[path = "mcp_parts/web.rs"]
mod mcp_web;
// ---- LLDB (feat/r2-lldb): debug_start, debug_command, debug_stop ----
#[path = "mcp_parts/debug.rs"]
mod mcp_debug;
// ---- CPU profiler (feat/r2-profiler): profile_start, profile_stop ----
#[path = "mcp_parts/profile.rs"]
mod mcp_profile;

fn tools() -> Value {
    let phone = json!({"type": "string", "description": "the device: its name (as in the hub, e.g. \"QA\"), folder (device-a64-qa) or path. Default: the only running device, else the configured one"});
    let after = json!({"type": "string", "enum": ["full", "diff", "none"], "description": "what to answer with after the action settled: the whole screen (default), only what changed, or nothing"});
    let mut t = json!([
        {"name": "phones_list", "description": "Every device, as the hub shows it: name, model, iOS version and whether it is installed, state (off / starting / running / stopping), home screen reached, memory (MB), CPU %, processes, uptime.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "sim_status", "description": "Same as phones_list (kept for older clients).",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "features_list", "description": "List a device's optional features: id, title, on/off, default, measured memory cost (MB) and whether it is paid every boot or only when used. Changes apply at the next boot.",
         "inputSchema": {"type": "object", "properties": {"phone": phone}}},
        {"name": "feature_get", "description": "Whether one feature is on.",
         "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}, "phone": phone}, "required": ["id"]}},
        {"name": "feature_set", "description": "Turn a feature on or off (wallpaper, spotlight, keyboard, siri, maps, photos, files, wallet, health, widgets, xctest). Applies at the next boot: call sim_restart.",
         "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}, "enabled": {"type": "boolean"}, "phone": phone}, "required": ["id", "enabled"]}},
        {"name": "features_preset", "description": "Apply a preset: full (the default set), lean, minimal. Applies at the next boot.",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "enum": ["full", "lean", "minimal"]}, "phone": phone}, "required": ["name"]}},
        {"name": "sim_devices", "description": "Devices (folder, label with device model, iOS), the device models a new device can be and the installed iOS versions.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "sim_add_device", "description": "Make a new device of a model (see sim_devices models), optional name and version (ios, e.g. \"27.2\": iOS for an iPhone or iPad, tvOS for an Apple TV, watchOS for an Apple Watch, or a build such as \"24B91016s\"; see sim_devices runtimes; a model with a runtime of its own, the iPhone Duo's 24B91016s, gets that one). Start it with sim_start phone=<name>. Its first start can take a few minutes.",
         "inputSchema": {"type": "object", "properties": {"model": {"type": "string"}, "name": {"type": "string"}, "ios": {"type": "string"}}, "required": ["model"]}},
        {"name": "sim_start", "description": "Start a device (its own container, so other devices keep running). Returns at once; poll phones_list for state running.", "inputSchema": {"type": "object", "properties": {"phone": phone}}},
        {"name": "sim_fold", "description": "Fold or unfold a two-screen device (iPhone Duo): open=true opens it flat (the large inner screen takes over), open=false closes it (the outer screen); or angle in degrees (0 closed .. 180 flat; SpringBoard swaps screens at 85); or pose, a preset as in Xcode's DeviceHub: closed, half-portrait (90 degrees, portrait), half-landscape (90 degrees, landscape), open. The hinge moves over ms (default 600) as a hand would.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "open": {"type": "boolean"}, "angle": {"type": "number"}, "pose": {"type": "string", "enum": ["closed", "half-portrait", "half-landscape", "open"]}, "ms": {"type": "integer"}}}},
        {"name": "sim_button", "description": "Press a hardware button as Apple's simulator does (DeviceHub's Home, Lock, Side Button, Volume, Siri, Action Button, Apple Pay; an Apple TV's Siri Remote: select, menu (back), tv, playpause, siri). Per platform: on a watch home is the Digital Crown's press and side its side button. ms: how long it is held (default 100; e.g. 2500 on lock = Siri on an iPhone).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "name": {"type": "string", "enum": ["home", "lock", "side", "volup", "voldown", "siri", "action", "wallet", "playpause", "tv", "select", "menu", "back", "mute"]}, "ms": {"type": "integer"}}, "required": ["name"]}},
        {"name": "sim_crown", "description": "Turn the Digital Crown (Apple Watch, Vision Pro): delta per step (positive = down / towards the bottom; ~120 scrolls a watch list one row), steps (default 8, 30 ms apart).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "delta": {"type": "number"}, "steps": {"type": "integer"}}, "required": ["delta"]}},
        {"name": "sim_shake", "description": "Device > Shake: the app in front gets a shake motion event (e.g. Undo Typing).", "inputSchema": {"type": "object", "properties": {"phone": phone}}},
        {"name": "sim_orientation", "description": "The device orientation (iPhone, iPad), as Rotate Left / Right or devicectl device orientation: set one (portrait, portraitUpsideDown, landscapeLeft, landscapeRight, faceUp, faceDown) or rotate left / right; with neither, just reports it. The screen image stays in the panel's own orientation (as Apple's framebuffer): landscape content is drawn sideways.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "set": {"type": "string", "enum": ["portrait", "portraitUpsideDown", "landscapeLeft", "landscapeRight", "faceUp", "faceDown"]}, "rotate": {"type": "string", "enum": ["left", "right"]}}}},
        {"name": "sim_biometric", "description": "Face ID / Touch ID / Optic ID as Apple's simulator: enroll or unenroll (Features > Face ID > Enrolled), then match / nomatch to answer an authentication the app asked for (LAContext.evaluatePolicy). kind: face, touch or optic (default: the device's).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "action": {"type": "string", "enum": ["enroll", "unenroll", "match", "nomatch"]}, "kind": {"type": "string", "enum": ["face", "touch", "optic"]}}, "required": ["action"]}},
        {"name": "sim_gamepad", "description": "The simulated game controller (\"Gamepad\", an extended gamepad; iPhone, iPad, Apple TV, Vision Pro; GCController.controllers() lists it): press a button (or hold it down / let it up), or set a thumbstick (stick left|right, x, y in -1..1, y up).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "button": {"type": "string", "enum": ["up", "down", "left", "right", "a", "b", "x", "y", "l1", "r1", "l2", "r2"]}, "action": {"type": "string", "enum": ["press", "down", "up"]}, "stick": {"type": "string", "enum": ["left", "right"]}, "x": {"type": "number"}, "y": {"type": "number"}}}},
        {"name": "sim_pointer", "description": "An iPad's mouse, as Apple's simulator's pointer capture (the iPad gets a mouse at the first call and draws its pointer; remove takes it away), or a Vision Pro's pointer: move by dx, dy points (the iPad speeds large moves up), at: put it at x, y (0..1 of the screen as shown, exact), click, button down / up, scroll by dx, dy (positive dy: down the page).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "action": {"type": "string", "enum": ["move", "at", "click", "down", "up", "scroll", "remove"]}, "dx": {"type": "number"}, "dy": {"type": "number"}, "x": {"type": "number"}, "y": {"type": "number"}}, "required": ["action"]}},
        {"name": "sim_system", "description": "Apple simulator controls without a button of their own: app_switcher (iPhone / iPad: the home button pressed twice), standby_show / standby_hide (iPhone StandBy; SpringBoard shows it when the device is locked and in landscape: sim_orientation, sim_button lock then home), double_tap / wrist_flick (Apple Watch hand gestures: the Smart Stack opens / closes on the watch face), trackpad_on / trackpad_off (an iPad's trackpad device, as Apple's Simulate Trackpad; its gestures are not simulated).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "action": {"type": "string", "enum": ["app_switcher", "standby_show", "standby_hide", "double_tap", "wrist_flick", "trackpad_on", "trackpad_off"]}}, "required": ["action"]}},
        {"name": "sim_memory_warning", "description": "Simulate Memory Warning: every process gets the memory-pressure event (apps: didReceiveMemoryWarning).", "inputSchema": {"type": "object", "properties": {"phone": phone}}},
        {"name": "sim_stop", "description": "Stop a device (only that device).", "inputSchema": {"type": "object", "properties": {"phone": phone}}},
        {"name": "sim_restart", "description": "Restart a device (stop, then start with its current features and the pre-translated code). Returns at once; poll phones_list.",
         "inputSchema": {"type": "object", "properties": {"phone": phone}}},
        {"name": "app_install", "description": "Install an iOS Simulator app (.app bundle built for arm64 iphonesimulator) on a running device, as simctl install: installd installs it and the home screen shows its icon. Returns the bundle id.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "path": {"type": "string", "description": "the .app directory on this machine"}}, "required": ["path"]}},
        {"name": "app_uninstall", "description": "Uninstall an app from a running device (simctl uninstall).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "bundle_id": {"type": "string"}}, "required": ["bundle_id"]}},
        {"name": "app_launch", "description": "Open an app on a running device by bundle id (simctl launch); args are passed to the app. Returns the pid.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "bundle_id": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}}, "required": ["bundle_id"]}},
        {"name": "app_list", "description": "Installed apps on a running device (simctl listapps).", "inputSchema": {"type": "object", "properties": {"phone": phone}}},
        // agents: the screen as text and actions on its elements (x86sim_core::ax, host/simax)
        {"name": "snapshot", "description": "The device's screen as text: the accessibility elements of the app(s) in front, SpringBoard (alerts, home screen icons, status bar, Control Center) and web pages (DOM: links, buttons, fields, headings, text). One line per element: @ref role \"label\" value= id= [states] (center x,y in points). Refs stay the same for the same element across snapshots, so act on them (tap ref=@e3). diff=true: only what changed since the last snapshot. all=true: also elements scrolled out of view. json=true: full records (frames, app, layer, window).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "diff": {"type": "boolean"}, "all": {"type": "boolean"}, "json": {"type": "boolean"}}}},
        {"name": "tap", "description": "Tap an element and answer with the screen after it settled. target: a ref from the snapshot (@e3), id:<accessibility identifier>, or label text (exact, then prefix, then substring; alerts over the app win); or x and y in points. An element scrolled out of view is scrolled in first. hold_ms >= 500 is a long press.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "target": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}, "hold_ms": {"type": "integer"}, "after": after}}},
        {"name": "long_press", "description": "Touch and hold an element (target as tap) or a point, ms (default 800): context menus, icon editing.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "target": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}, "ms": {"type": "integer"}, "after": after}}},
        {"name": "type", "description": "Type text into the focused field (tap the field first, or pass target to tap it first), as the software keyboard does: any characters. clear: empty the field first; submit: press return after. keys=true: hardware key presses instead (US layout; for games / key handlers).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "text": {"type": "string"}, "target": {"type": "string"}, "clear": {"type": "boolean"}, "submit": {"type": "boolean"}, "keys": {"type": "boolean"}, "after": after}, "required": ["text"]}},
        {"name": "key", "description": "Press a key on the hardware keyboard: return, tab, escape, delete, space, up, down, left, right, home, end, pageup, pagedown, a character, or a combination such as cmd+a, shift+tab.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "key": {"type": "string"}, "after": after}, "required": ["key"]}},
        {"name": "scroll", "description": "Scroll content precisely (no fling): direction is where you want to see more (down = content below comes up), amount in screens (default 0.5), optionally inside an element (target).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "direction": {"type": "string", "enum": ["up", "down", "left", "right"]}, "amount": {"type": "number"}, "target": {"type": "string"}, "after": after}, "required": ["direction"]}},
        {"name": "scroll_to", "description": "Scroll until an element (target as tap) is on screen: an element the app already has is scrolled into view directly, else the content is scrolled in direction (default down) until it shows or stops moving.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "target": {"type": "string"}, "direction": {"type": "string", "enum": ["up", "down", "left", "right"]}, "max_steps": {"type": "integer"}, "after": after}, "required": ["target"]}},
        {"name": "swipe", "description": "A quick swipe (with momentum): direction up/down/left/right from the middle of the screen, or a system gesture: home (up from the bottom edge), notifications, controlcenter, back (from the left edge); or from x1,y1 to x2,y2 in points over ms.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "direction": {"type": "string", "enum": ["up", "down", "left", "right", "home", "notifications", "controlcenter", "back"]}, "x1": {"type": "number"}, "y1": {"type": "number"}, "x2": {"type": "number"}, "y2": {"type": "number"}, "ms": {"type": "integer"}, "after": after}}},
        {"name": "wait_for", "description": "Wait until an element (target as tap: text, @ref, id:...) is on screen, or with gone=true until it is not; timeout seconds (default 20). Wakes on every new frame (no fixed polling). Answers with the screen.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "target": {"type": "string"}, "gone": {"type": "boolean"}, "timeout": {"type": "number"}}, "required": ["target"]}},
        {"name": "press", "description": "Press a hardware button (home, lock, side, volup, voldown, siri, action, ...; as sim_button) and answer with the screen after it.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "button": {"type": "string"}, "ms": {"type": "integer"}, "after": after}, "required": ["button"]}},
        {"name": "sim_keyboard", "description": "Apple Simulator's I/O > Keyboard. software: on / off / toggle the on-screen keyboard while a hardware keyboard is connected (Toggle Software Keyboard, Cmd-K; it shows while a text field is focused). hardware: connect / disconnect / toggle the simulated hardware keyboard (Connect Hardware Keyboard, Shift-Cmd-K; disconnected, the software keyboard shows on every focus as on a device; kept across boots, also for a device that is off). status: both states. Typing (type, key) works either way; snapshot lists the keyboard's keys (role key) while it is up. Answers with the screen.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "keyboard": {"type": "string", "enum": ["software", "hardware", "status"]}, "action": {"type": "string", "enum": ["on", "off", "toggle", "connect", "disconnect"]}, "after": after}, "required": ["keyboard"]}},
        {"name": "screenshot", "description": "The screen as an image (PNG), upright. Default scale: one pixel per point, so positions match the snapshot's coordinates; scale=1 for full device pixels, 2 for half, etc. (a divisor).",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "scale": {"type": "integer"}}}},
        {"name": "open_url", "description": "Open a URL (web page in Safari, deep link, prefs:/settings link) as simctl openurl, and answer with the screen.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "url": {"type": "string"}, "after": after}, "required": ["url"]}},
        {"name": "app_terminate", "description": "Stop a running app by bundle id (simctl terminate) and answer with the screen.",
         "inputSchema": {"type": "object", "properties": {"phone": phone, "bundle_id": {"type": "string"}, "after": after}, "required": ["bundle_id"]}},
        {"name": "aot_status", "description": "Pre-translated code of a device's iOS version: parts up to date / stale (with reasons) / missing, and the translator's codegen version.",
         "inputSchema": {"type": "object", "properties": {"phone": phone}}}
    ]);
    // ---- Share (browser streaming): share_start / share_stop / share_status / share_link, x86sim_core::share ----
    if let Value::Array(v) = &mut t {
        v.extend(x86sim_core::share::mcp_tools());
    }
    // ---- end Share ----
    // ---- simctl parity (feat/ap-simctl) ----
    if let Value::Array(v) = &mut t {
        v.extend(mcp_simctl::tools(&phone));
    }
    // ---- XCTest (feat/ap-xctest) ----
    if let Value::Array(v) = &mut t {
        v.extend(mcp_xctest::tools(&phone));
    }
    // ---- Accessibility Inspector ----
    if let Value::Array(v) = &mut t {
        v.extend(mcp_axinspect::tools(&phone));
    }
    // ---- Web Inspector (feat/r2-webinspect) ----
    if let Value::Array(v) = &mut t {
        v.extend(mcp_web::tools(&phone));
    }
    // ---- LLDB (feat/r2-lldb) ----
    if let Value::Array(v) = &mut t {
        v.extend(mcp_debug::tools(&phone));
    }
    // ---- CPU profiler (feat/r2-profiler) ----
    if let Value::Array(v) = &mut t {
        v.extend(mcp_profile::tools(&phone));
    }
    t
}

/// The phone a call is about: phone / device argument, else the only running phone, else the configured one.
fn phone_of(a: &Value, cfg: &aot::Config) -> anyhow::Result<instance::Instance> {
    let key = a["phone"].as_str().or(a["device"].as_str());
    if let Some(k) = key {
        return instance::find(k).ok_or_else(|| {
            anyhow::anyhow!("no device {k:?} (devices: {})", instance::list().iter().map(|i| i.name.clone()).collect::<Vec<_>>().join(", "))
        });
    }
    let running: Vec<_> = instance::summaries(0).into_iter().filter(|s| s.state != "off").collect();
    if running.len() == 1 {
        return instance::find(&running[0].id).ok_or_else(|| anyhow::anyhow!("device vanished"));
    }
    let d = cfg.device.clone().ok_or_else(|| anyhow::anyhow!("say which device (phone: name)"))?;
    Ok(instance::load(&d))
}

/// What a tool answers: JSON (shown as text too), text for an agent to read (with optional structured content), or an
/// image
enum Answer {
    Json(Value),
    Text(String, Option<Value>),
    Image(String, String),
}

fn ax_answer(o: ax::Outcome) -> Answer {
    Answer::Text(o.text(), None)
}

fn call(name: &str, a: &Value) -> anyhow::Result<Answer> {
    // tests/mcp/panic.sh: a tool that panics, to prove the server survives one (only with this variable set)
    if name == "__test_panic" && std::env::var_os("X86SIM_MCP_TEST_PANIC").is_some() {
        panic!("test panic");
    }
    call_json(name, a)
}

fn call_json(name: &str, a: &Value) -> anyhow::Result<Answer> {
    // ---- Develop (omarchy-apple-dev): dev_* tools, x86sim_core::dev ----
    if name.starts_with("dev_") {
        return x86sim_core::dev::mcp_call(name, a).map(Answer::Json);
    }
    // ---- end Develop ----
    // ---- Pool ----
    if let Some(r) = pool_mcp::call(name, a) {
        return r.map(Answer::Json);
    }
    // ---- end Pool ----
    // ---- Share (browser streaming) ----
    if let Some(r) = x86sim_core::share::mcp_call(name, a) {
        return r.map(Answer::Json);
    }
    // ---- end Share ----
    let after = ax::After::parse(a["after"].as_str());
    let cfg = aot::Paths::new(&aot::Paths::default_jc()).load_config();
    // ---- simctl parity (feat/ap-simctl) ----
    if let Some(r) = mcp_simctl::call(name, a, &|| phone_of(a, &cfg)) {
        return r.map(Answer::Json);
    }
    if let Some(r) = mcp_xctest::call(name, a, &|| phone_of(a, &cfg)) {
        return r.map(Answer::Json);
    }
    if let Some(r) = mcp_axinspect::call(name, a, &|| phone_of(a, &cfg)) {
        return r.map(|(t, v)| Answer::Text(t, v));
    }
    if let Some(r) = mcp_web::call(name, a, &|| phone_of(a, &cfg)) {
        return r.map(Answer::Json);
    }
    if let Some(r) = mcp_debug::call(name, a, &|| phone_of(a, &cfg)) {
        return r.map(Answer::Json);
    }
    if let Some(r) = mcp_profile::call(name, a, &|| phone_of(a, &cfg)) {
        return r.map(|(t, v)| Answer::Text(t, v));
    }
    if matches!(name, "phones_list" | "sim_status") {
        return Ok(Answer::Json(json!({"phones": instance::summaries(1000)})));
    }
    if name == "sim_devices" {
        return Ok(Answer::Json(json!({"devices": instance::list().into_iter().map(|i| json!({"name": i.name, "id": i.id, "label": format!("{} · {}", i.model, i.name), "path": i.dir, "ios": i.ios, "installed": i.runtime_present})).collect::<Vec<_>>(),
                         "models": sim::models().into_iter().map(|m| m.0).collect::<Vec<_>>(),
                         "runtimes": sim::runtimes().into_iter().map(|r| r.0).collect::<Vec<_>>()})));
    }
    if name == "sim_add_device" {
        let want = a["ios"].as_str().unwrap_or("27.2");
        // Models 2: a runtime limited to some models (iOS 27.2 24B91016s: iPhone Duo only) is taken for those models
        // first and never for others
        let model = a["model"].as_str().unwrap_or("");
        let model_id = sim::models().into_iter().find(|m| m.0 == model || m.1 == model).map(|m| m.1).unwrap_or_default();
        // Platforms 1: only runtimes of the model's platform (an Apple TV takes tvOS, an Apple Watch watchOS)
        let pname = sim::platform_name(sim::model_platform(&model_id));
        let cands: Vec<_> = sim::runtimes()
            .into_iter()
            .filter(|r| r.0.starts_with(&format!("{pname} ")))
            .filter(|r| r.0.starts_with(&format!("{pname} {want} ")) || r.0.contains(want))
            .filter(|r| sim::runtime_allows(&r.1, &model_id))
            .collect();
        let rt = cands
            .iter()
            .find(|r| !sim::runtime_restricted_to(&r.1).is_empty())
            .or(cands.first())
            .map(|r| r.1.clone())
            .ok_or_else(|| anyhow::anyhow!("no {pname} {want} installed for {model} (see sim_devices runtimes)"))?;
        return Ok(Answer::Json(json!({"device": sim::create_device(a["model"].as_str().unwrap_or(""), a["name"].as_str().unwrap_or(""), &rt)?})));
    }
    let inst = phone_of(a, &cfg)?;
    let device = inst.dir.clone();
    Ok(Answer::Json(match name {
        "features_list" => json!({"phone": inst.name, "preset": features::preset_of(&features::load(&device)), "features": features::list(&device)}),
        "feature_get" => {
            let id = a["id"].as_str().unwrap_or("");
            json!({"id": id, "enabled": features::load(&device).enabled(id)})
        }
        "feature_set" => {
            let id = a["id"].as_str().unwrap_or("");
            let on = a["enabled"].as_bool().ok_or_else(|| anyhow::anyhow!("enabled: boolean required"))?;
            features::set(&device, id, on)?;
            json!({"phone": inst.name, "id": id, "enabled": on, "note": "applies at the next boot (sim_restart)"})
        }
        "features_preset" => {
            let n = a["name"].as_str().unwrap_or("");
            features::apply_preset(&device, n)?;
            json!({"phone": inst.name, "preset": n, "features": features::list(&device), "note": "applies at the next boot (sim_restart)"})
        }
        "app_install" => json!({"phone": inst.name, "installed": apps::install(&inst, std::path::Path::new(a["path"].as_str().unwrap_or("")))?}),
        "app_uninstall" => json!({"phone": inst.name, "answer": apps::uninstall(&inst, a["bundle_id"].as_str().unwrap_or(""))?}),
        "app_launch" => {
            let args: Vec<String> = a["args"].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            return Ok(ax_answer(ax::launch(&inst, a["bundle_id"].as_str().unwrap_or(""), &args, after)?));
        }
        "app_list" => json!({"phone": inst.name, "answer": apps::list(&inst)?}),
        "sim_restart" => json!({"phone": inst.name, "restarting": true, "pid": sim::restart(&device)?}),
        "sim_start" => json!({"phone": inst.name, "starting": true, "pid": sim::start(&device)?}),
        "sim_fold" => {
            anyhow::ensure!(inst.inner.is_some(), "{} has one screen", inst.name);
            let pose = match a["pose"].as_str() {
                Some(k) => Some(x86sim_core::hid::HINGE_POSES.iter().find(|p| p.key == k).ok_or_else(|| anyhow::anyhow!("unknown pose {k}: closed, half-portrait, half-landscape, open"))?),
                None => None,
            };
            let deg = pose.map(|p| p.angle).or(a["angle"].as_f64()).unwrap_or(if a["open"].as_bool().unwrap_or(true) { 180.0 } else { 0.0 }).clamp(0.0, 180.0);
            let ms = a["ms"].as_u64().unwrap_or(600) as u32;
            let mut hid = x86sim_core::hid::Hid::new(&inst.sock);
            if let Some(o) = pose.and_then(|p| p.orientation) {
                hid.orientation(o);
            }
            hid.hinge(deg, ms);
            std::thread::sleep(std::time::Duration::from_millis(ms as u64 + 300));
            json!({"phone": inst.name, "hinge": inst.hinge(), "state": inst.hinge().map(x86sim_core::hid::hinge_state), "orientation": inst.orientation(), "screen": if inst.inner_active() { "inner" } else { "outer" }})
        }
        // Inputs 2: the inputs Apple's simulator offers besides touches and keys (host/simhid)
        "sim_button" => {
            let n = a["name"].as_str().unwrap_or("");
            anyhow::ensure!(!n.is_empty(), "name required");
            x86sim_core::hid::Hid::new(&inst.sock).press_for(n, a["ms"].as_u64().unwrap_or(100));
            json!({"phone": inst.name, "pressed": n})
        }
        "sim_crown" => {
            let d = a["delta"].as_f64().unwrap_or(0.0) as f32;
            let mut h = x86sim_core::hid::Hid::new(&inst.sock);
            for _ in 0..a["steps"].as_u64().unwrap_or(8) {
                h.crown(d);
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            json!({"phone": inst.name, "crown": d})
        }
        "sim_shake" => {
            x86sim_core::hid::Hid::new(&inst.sock).shake();
            json!({"phone": inst.name, "shaken": true})
        }
        "sim_orientation" => {
            let before = std::fs::metadata(inst.dir.join("data/orientation")).and_then(|m| m.modified()).ok();
            let mut h = x86sim_core::hid::Hid::new(&inst.sock);
            match (a["set"].as_str(), a["rotate"].as_str()) {
                (Some(o), _) => h.orientation(o),
                (None, Some(r)) => h.rotate(r == "left"),
                _ => h.orientation(""),
            }
            // simhid writes the orientation it ends up in
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if std::fs::metadata(inst.dir.join("data/orientation")).and_then(|m| m.modified()).ok() != before {
                    break;
                }
            }
            json!({"phone": inst.name, "orientation": inst.orientation()})
        }
        "sim_biometric" => {
            let act = a["action"].as_str().unwrap_or("");
            anyhow::ensure!(matches!(act, "enroll" | "unenroll" | "match" | "nomatch"), "action: enroll, unenroll, match or nomatch");
            x86sim_core::hid::Hid::new(&inst.sock).biometric(act, a["kind"].as_str().unwrap_or(""));
            json!({"phone": inst.name, "biometric": act})
        }
        "sim_gamepad" => {
            let mut h = x86sim_core::hid::Hid::new(&inst.sock);
            if let Some(st) = a["stick"].as_str() {
                h.gamepad_stick(st == "right", a["x"].as_f64().unwrap_or(0.0) as f32, a["y"].as_f64().unwrap_or(0.0) as f32);
            } else {
                let b = a["button"].as_str().ok_or_else(|| anyhow::anyhow!("button or stick required"))?;
                h.gamepad(b, a["action"].as_str().unwrap_or("press"));
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            json!({"phone": inst.name, "gamepad": a})
        }
        "sim_pointer" => {
            let (dx, dy) = (a["dx"].as_f64().unwrap_or(0.0), a["dy"].as_f64().unwrap_or(0.0));
            let line = match a["action"].as_str().unwrap_or("") {
                "move" => format!("pointer move {dx:.2} {dy:.2}"),
                "at" => format!("pointer at {:.5} {:.5}", a["x"].as_f64().unwrap_or(0.5), a["y"].as_f64().unwrap_or(0.5)),
                "scroll" => format!("pointer scroll {dx:.2} {dy:.2}"),
                "click" => "pointer click".into(),
                "down" => "pointer button down".into(),
                "up" => "pointer button up".into(),
                "remove" => "pointer remove".into(),
                o => anyhow::bail!("action {o:?}: move, at, click, down, up, scroll or remove"),
            };
            x86sim_core::hid::Hid::new(&inst.sock).send(&line);
            std::thread::sleep(std::time::Duration::from_millis(100));
            json!({"phone": inst.name, "pointer": line})
        }
        "sim_system" => {
            let line = match a["action"].as_str().unwrap_or("") {
                "app_switcher" => "appswitcher",
                "standby_show" => "standby show",
                "standby_hide" => "standby hide",
                "double_tap" => "gesture doubletap",
                "wrist_flick" => "gesture wristflick",
                "trackpad_on" => "trackpad on",
                "trackpad_off" => "trackpad off",
                o => anyhow::bail!("action {o:?}: app_switcher, standby_show, standby_hide, double_tap, wrist_flick, trackpad_on or trackpad_off"),
            };
            x86sim_core::hid::Hid::new(&inst.sock).send(line);
            std::thread::sleep(std::time::Duration::from_millis(300));
            // StandBy's backdrop comes from PosterBoard, which runs only with the "wallpaper" feature
            if line == "standby show" && !features::load(&device).enabled("wallpaper") {
                json!({"phone": inst.name, "done": line, "note": "the backdrop stays black: turn on feature wallpaper (PosterBoard) and sim_restart for the blurred widgets"})
            } else {
                json!({"phone": inst.name, "done": line})
            }
        }
        "sim_memory_warning" => {
            inst.memory_warning()?;
            json!({"phone": inst.name, "memory_warning": true})
        }
        "sim_stop" => {
            sim::stop(&device)?;
            json!({"phone": inst.name, "stopped": true})
        }
        "snapshot" => {
            let prev = ax::last(&inst);
            let snap = ax::snapshot(&inst, ax::SnapOpts { all: a["all"].as_bool().unwrap_or(false), ..Default::default() })?;
            let text = match (a["diff"].as_bool().unwrap_or(false), prev) {
                (true, Some(p)) => snap.diff(&p),
                _ => snap.text(),
            };
            if a["json"].as_bool().unwrap_or(false) {
                return Ok(Answer::Text(text, Some(serde_json::to_value(&snap)?)));
            }
            return Ok(Answer::Text(text, None));
        }
        "tap" | "long_press" => {
            let hold = if name == "long_press" { a["ms"].as_u64().unwrap_or(800) } else { a["hold_ms"].as_u64().unwrap_or(80) };
            let target = match (a["target"].as_str(), a["x"].as_f64(), a["y"].as_f64()) {
                (Some(t), _, _) => t.to_string(),
                (None, Some(x), Some(y)) => format!("{x} {y}"),
                _ => anyhow::bail!("target (a ref, id:..., or text) or x and y required"),
            };
            return Ok(ax_answer(ax::tap(&inst, &target, hold, after)?));
        }
        "type" => {
            if let Some(t) = a["target"].as_str() {
                ax::tap(&inst, t, 80, ax::After::None)?;
                ax::settle(&inst, std::time::Duration::from_millis(250), std::time::Duration::from_millis(2000));
            }
            let o = ax::type_text(&inst, a["text"].as_str().unwrap_or(""), a["clear"].as_bool().unwrap_or(false), a["submit"].as_bool().unwrap_or(false), a["keys"].as_bool().unwrap_or(false), after)?;
            return Ok(ax_answer(o));
        }
        "key" => return Ok(ax_answer(ax::key(&inst, a["key"].as_str().unwrap_or(""), after)?)),
        "scroll" => {
            let o = ax::scroll(&inst, a["direction"].as_str().unwrap_or("down"), a["amount"].as_f64().unwrap_or(0.5), a["target"].as_str(), after)?;
            return Ok(ax_answer(o));
        }
        "scroll_to" => {
            let o = ax::scroll_to(&inst, a["target"].as_str().unwrap_or(""), a["direction"].as_str().unwrap_or("down"), a["max_steps"].as_u64().unwrap_or(10) as u32, after)?;
            return Ok(ax_answer(o));
        }
        "swipe" => {
            let o = match (a["x1"].as_f64(), a["y1"].as_f64(), a["x2"].as_f64(), a["y2"].as_f64()) {
                (Some(x1), Some(y1), Some(x2), Some(y2)) => ax::swipe(&inst, (x1, y1), (x2, y2), a["ms"].as_u64().unwrap_or(250), after)?,
                _ => ax::swipe_dir(&inst, a["direction"].as_str().unwrap_or("up"), after)?,
            };
            return Ok(ax_answer(o));
        }
        "wait_for" => {
            let t = std::time::Duration::from_secs_f64(a["timeout"].as_f64().unwrap_or(20.0).clamp(0.1, 600.0));
            return Ok(ax_answer(ax::wait_for(&inst, a["target"].as_str().unwrap_or(""), a["gone"].as_bool().unwrap_or(false), t)?));
        }
        "sim_keyboard" => return Ok(ax_answer(ax::keyboard(&inst, a["keyboard"].as_str().unwrap_or("status"), a["action"].as_str().unwrap_or("toggle"), after)?)),
        "press" => return Ok(ax_answer(ax::press(&inst, a["button"].as_str().unwrap_or("home"), a["ms"].as_u64().unwrap_or(100), after)?)),
        "open_url" => return Ok(ax_answer(ax::open_url(&inst, a["url"].as_str().unwrap_or(""), after)?)),
        "app_terminate" => return Ok(ax_answer(ax::terminate(&inst, a["bundle_id"].as_str().unwrap_or(""), after)?)),
        "screenshot" => {
            let div = a["scale"].as_u64().map(|s| s as u32).unwrap_or(inst.scale.round().max(1.0) as u32);
            // a stable frame: what an action or setting just changed, not the frame before it
            ax::settle(&inst, std::time::Duration::from_millis(150), std::time::Duration::from_millis(1500));
            let (png, w, h) = ax::screenshot(&inst, div)?;
            return Ok(Answer::Image(ax::base64(&png), format!("{} screen, {w}x{h} px", inst.name)));
        }
        "aot_status" => {
            // the phone's iOS version decides the image and its cache dir
            let rt = sim::device_runtime(&device);
            let p = aot::Paths::new(&sim::jc_for(&rt));
            let mut cfg = p.load_config();
            cfg.runtime_root = rt;
            cfg.device = Some(device.clone());
            let st = aot::plan(&p, &cfg)?.status(&p);
            let attention: Vec<_> = st.chunks.iter().filter(|c| c.state != "up-to-date").take(50).collect();
            json!({"phone": inst.name, "ios": inst.ios, "image": st.image, "image_current": st.image_current, "total": st.total, "up_to_date": st.up_to_date,
                   "stale": st.stale, "missing": st.missing, "codegen": format!("{:#x}", st.codegen), "codegen_source": st.codegen_source, "need_rebuild": attention})
        }
        _ => anyhow::bail!("unknown tool {name}"),
    }))
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let id = req.get("id").cloned();
        let method = req["method"].as_str().unwrap_or("");
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "x86sim", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                // ---- Develop (omarchy-apple-dev): dev_* tools, x86sim_core::dev ----
                let mut t = tools().as_array().cloned().unwrap_or_default();
                t.extend(x86sim_core::dev::mcp_tools());
                // ---- end Develop ----
                Ok(json!({"tools": pool_mcp::extend(Value::Array(t))}))
            }
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let args = req["params"].get("arguments").cloned().unwrap_or(json!({}));
                // a panic in a tool (a bug, a malformed input that slipped through) is that call's error: the server
                // goes on, with the pool leases its process holds
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(name, &args))).unwrap_or_else(|p| {
                    let m = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                    Err(anyhow::anyhow!("{name}: internal error (panic: {m})"))
                });
                Ok(match r {
                    Ok(Answer::Json(v)) => json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "structuredContent": v}),
                    Ok(Answer::Text(t, None)) => json!({"content": [{"type": "text", "text": t}]}),
                    Ok(Answer::Text(t, Some(v))) => json!({"content": [{"type": "text", "text": t}], "structuredContent": v}),
                    Ok(Answer::Image(b64, caption)) => json!({"content": [{"type": "image", "data": b64, "mimeType": "image/png"}, {"type": "text", "text": caption}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
                })
            }
            m if m.starts_with("notifications/") => continue,
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        let Some(id) = id else { continue };
        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((c, m)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": c, "message": m}}),
        };
        let _ = writeln!(out, "{resp}");
        let _ = out.flush();
    }
}

// ==== Phone pool + device builds (feat/ap-ipa-pool): pool_status / phone_take / phone_release, app_install of an .ipa
// (x86sim_core::pool, x86sim_core::devapp). Kept apart from the tools above so it merges as one block.
mod pool_mcp {
    use serde_json::{json, Value};
    use x86sim_core::pool;

    /// The pool tools added to the list; app_install's description names the device builds it now takes
    pub fn extend(mut tools: Value) -> Value {
        if let Some(arr) = tools.as_array_mut() {
            for t in arr.iter_mut() {
                if t["name"] == "app_install" {
                    t["description"] = json!("Install an app on a running device, as simctl install: installd installs it and the home screen shows its icon. Takes a simulator .app, or a device build: an .ipa, an .xcarchive or a device .app (converted for the simulator first: every Mach-O retagged to the simulator platform, Info.plist platform keys, ad-hoc re-signed keeping entitlements; arm64 only; FairPlay-encrypted App Store downloads are refused). Returns the bundle id and, for a device build, what was converted.");
                    t["inputSchema"]["properties"]["path"]["description"] = json!("the .app directory, .ipa file or .xcarchive on this machine");
                }
            }
            let pool = json!({"type": "string", "description": "the pool (default \"pool\"; x86sim pool up N --pool NAME makes one)"});
            let reset = json!({"type": "string", "enum": pool::RESETS, "description": "what happens when it is given back: none, apps (default: uninstall the apps installed during the lease, with their data, then Home), restart (fresh boot), erase (re-cloned from the pool's template, then booted)"});
            arr.push(json!({"name": "pool_status", "description": "The device pool (devices kept booted for agents; made with `x86sim pool up N`): each device's state (free / leased / resetting), whether it is at its home screen, who holds it and for how long, memory per device, how many more fit in memory, KSM sharing and reflink cloning.",
                "inputSchema": {"type": "object", "properties": {"pool": pool}}}));
            arr.push(json!({"name": "phone_take", "description": "Take a booted device from the pool (atomic: no other caller gets it). Returns its name: pass it as `device` to the other tools. The lease ends when this MCP session ends (the server's process is the holder) or after ttl_s (default 1800; renew by taking again is not needed: call phone_take with renew=<device>); then the device is reset and free again. Give it back with phone_release.",
                "inputSchema": {"type": "object", "properties": {"pool": pool, "ttl_s": {"type": "integer"}, "reset": reset, "wait_s": {"type": "integer", "description": "wait up to this long for a device to be free (default 0)"}, "renew": {"type": "string", "description": "extend the lease of this device you hold instead of taking another"}}}}));
            arr.push(json!({"name": "phone_release", "description": "Give a pool device back. It is reset in the background (reset, default: the mode given at phone_take) and free again when done.",
                "inputSchema": {"type": "object", "properties": {"pool": pool, "phone": {"type": "string"}, "reset": reset}, "required": ["phone"]}}));
        }
        tools
    }

    pub fn call(name: &str, a: &Value) -> Option<anyhow::Result<Value>> {
        let p = a["pool"].as_str().filter(|s| !s.is_empty()).map(String::from).or_else(|| std::env::var("X86SIM_POOL").ok()).unwrap_or_else(|| "pool".into());
        Some(match name {
            "pool_status" => pool::status(&p).and_then(|s| Ok(serde_json::to_value(s)?)),
            "phone_take" => {
                if let Some(ph) = a["renew"].as_str() {
                    return Some(pool::renew(&p, ph, a["ttl_s"].as_u64().unwrap_or(0)).map(|e| json!({"phone": ph, "expires": e})));
                }
                let o = pool::TakeOpts {
                    holder: format!("mcp:{}", std::process::id()),
                    holder_pid: Some(std::process::id()),
                    ttl_s: a["ttl_s"].as_u64().unwrap_or(0),
                    reset: a["reset"].as_str().unwrap_or("").into(),
                    wait_s: a["wait_s"].as_u64().unwrap_or(0),
                };
                pool::take(&p, &o).and_then(|t| Ok(serde_json::to_value(t)?))
            }
            "phone_release" => match a["phone"].as_str() {
                Some(ph) => pool::release(&p, ph, a["reset"].as_str()).map(|m| json!({"phone": ph, "released": true, "reset": m})),
                None => Err(anyhow::anyhow!("device required")),
            },
            _ => return None,
        })
    }
}
