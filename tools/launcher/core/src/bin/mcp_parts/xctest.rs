//! x86sim-mcp: xctest_run, XCTest / XCUITest bundles on a phone (cli/x86sim test, cli/libexec/xctest.py), as
//! `xcodebuild test-without-building` runs them in Apple's simulator. Kept apart from mcp.rs like mcp_parts/simctl.rs.
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use x86sim_core::{aot::repo_root, instance};

pub fn tools(phone: &Value) -> Vec<Value> {
    let list = json!({"type": "array", "items": {"type": "string"}});
    vec![json!({"name": "xctest_run", "description": "Run XCTest unit / logic tests and XCUITest UI tests on a running device, as xcodebuild test-without-building: give an .xctestrun (what `xcodebuild build-for-testing` writes next to its products), or app (+ test_bundle) for unit tests hosted in the app, test_bundle alone for logic tests, ui_test_runner + app for UI tests, or spm (a Swift package dir: its tests are built on Linux for the device's simulator). Answers per test: result (passed / failed / skipped), duration, failures with file, line and message; counts; the result; and out, a dir with each target's log, results.json and junit.xml. UI tests need the device's xctest feature (feature_set id=xctest, then sim_restart). Can take minutes.",
        "inputSchema": {"type": "object", "properties": {"phone": phone,
            "xctestrun": {"type": "string", "description": "path of the .xctestrun"},
            "products": {"type": "string", "description": "the products dir when it is not next to the .xctestrun"},
            "app": {"type": "string", "description": "the host app (unit tests) or the app under test (UI tests)"},
            "test_bundle": {"type": "string", "description": "an .xctest bundle"},
            "ui_test_runner": {"type": "string", "description": "the <Name>UITests-Runner.app"},
            "spm": {"type": "string", "description": "a Swift package dir"},
            "only_testing": list.clone(), "skip_testing": list,
            "timeout_s": {"type": "number", "description": "per test target (default 1800)"}}}})]
}

/// Some(result) for xctest_run, None for other tools.
pub fn call(name: &str, a: &Value, phone: &dyn Fn() -> anyhow::Result<instance::Instance>) -> Option<anyhow::Result<Value>> {
    if name != "xctest_run" {
        return None;
    }
    Some((|| {
        let inst = phone()?;
        let mut c = Command::new(repo_root().join("cli/x86sim"));
        c.arg("--phone").arg(&inst.dir).arg("test").arg("--json").arg("-");
        for (k, flag) in [("xctestrun", "--xctestrun"), ("products", "--products"), ("app", "--app"), ("test_bundle", "--test-bundle"), ("ui_test_runner", "--ui-test-runner"), ("spm", "--spm")] {
            if let Some(v) = a[k].as_str().filter(|v| !v.is_empty()) {
                c.arg(flag).arg(v);
            }
        }
        for (k, flag) in [("only_testing", "--only-testing"), ("skip_testing", "--skip-testing")] {
            for v in a[k].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                c.arg(flag).arg(v);
            }
        }
        if let Some(t) = a["timeout_s"].as_f64() {
            c.arg("--timeout").arg(t.to_string());
        }
        let out = c.current_dir(repo_root()).stdin(Stdio::null()).output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let err = String::from_utf8_lossy(&out.stderr);
        match serde_json::from_str::<Value>(text.trim()) {
            Ok(v) => Ok(v),
            Err(_) => anyhow::bail!("{}", err.trim().lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")),
        }
    })())
}
