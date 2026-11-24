//! x86sim-ax: the agent commands of the CLI (cli/x86sim snapshot / tap / type / ...): read a phone's screen as text
//! and act on its elements. Same core as the MCP server's tools (x86sim_core::ax).
use anyhow::{anyhow, bail, Result};
use std::time::Duration;
use x86sim_core::ax::{self, After, SnapOpts};
use x86sim_core::instance;

const USAGE: &str = "usage: x86sim-ax [--phone NAME] <command> [args]
  snapshot [--json] [--all] [--diff]   the screen as text: one element per line with a ref (@e3), role, label, value,
                                       id and its center point; --diff: only what changed since the last snapshot;
                                       --all: also elements scrolled out of view; --json: everything (frames, states)
  find TARGET                          the elements a target matches, best first
  tap TARGET                           TARGET: @e3 | id:IDENTIFIER | \"label text\" | X Y (points)
  long-press TARGET [--ms N]           (default 800 ms)
  type TEXT [--clear] [--submit] [--keys]
                                       into the focused field (as the software keyboard: any text); --keys: key presses
  key COMBO                            return | tab | escape | delete | up ... | cmd+a | shift+tab
  scroll up|down|left|right [AMOUNT] [--at TARGET]
                                       precise scroll (no fling), AMOUNT in screens (default 0.5)
  swipe up|down|left|right|home|notifications|controlcenter|back | swipe X1 Y1 X2 Y2 [MS]
  scroll-to TARGET [--dir down] [--max 10]
  wait-for TARGET [--gone] [--timeout S]   (default 20 s)
  press BUTTON [MS]                    home lock side volup voldown siri action ...
  keyboard software on|off|toggle | hardware connect|disconnect|toggle | status
                                       Apple's I/O > Keyboard: Toggle Software Keyboard (Cmd-K), Connect Hardware
                                       Keyboard (Shift-Cmd-K; kept across boots)
  open URL | launch BUNDLEID [ARGS] | terminate BUNDLEID
  screenshot [OUT.png] [--scale N]     N: divisor (default: the device scale, a picture in points)
  inspect [TARGET | X Y] [--json]      Accessibility Inspector: every property of an element (label, value, hint,
                                       traits, actions, custom actions, rotors, language, class, view, controller,
                                       hierarchy, font / color); no target: the screen + views VoiceOver cannot reach
  audit [--dynamic-type] [--all] [--rules a,b] [--json]
                                       accessibility audit of the app in front (labels, 44 pt targets, contrast,
                                       Dynamic Type, duplicates, hidden controls, traits); --dynamic-type: re-check at
                                       the largest text size (the phone's setting is put back)
  action NAME [TARGET] [--dir up|down|left|right|next|previous] [--name CUSTOM]
                                       an accessibility action: activate increment decrement scroll escape magic_tap
                                       custom
Actions print what they did and the screen after it (--after diff: only changes, --after none: nothing; --json).
The phone: --phone NAME (name, folder or path), else $X86SIM_DEVICE, else the only running phone.";

fn pick(phone: Option<&str>) -> Result<instance::Instance> {
    if let Some(k) = phone {
        return instance::find(k).ok_or_else(|| anyhow!("no device {k:?} (devices: {})", instance::list().iter().map(|i| i.name.clone()).collect::<Vec<_>>().join(", ")));
    }
    if let Ok(d) = std::env::var("X86SIM_DEVICE") {
        if !d.is_empty() && std::path::Path::new(&d).join("sim.env").exists() {
            return Ok(instance::load(std::path::Path::new(&d)));
        }
    }
    let running: Vec<_> = instance::summaries(0).into_iter().filter(|s| s.state != "off").collect();
    if running.len() == 1 {
        return instance::find(&running[0].id).ok_or_else(|| anyhow!("device vanished"));
    }
    bail!("say which device (--phone NAME)")
}

fn main() {
    if let Err(e) = run() {
        eprintln!("x86sim: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut flag = |name: &str| -> bool {
        if let Some(i) = args.iter().position(|a| a == name) {
            args.remove(i);
            true
        } else {
            false
        }
    };
    let json = flag("--json");
    let all = flag("--all");
    let diff = flag("--diff");
    let gone = flag("--gone");
    let clear = flag("--clear");
    let submit = flag("--submit");
    let keys = flag("--keys");
    let dynamic_type = flag("--dynamic-type");
    let mut opt = |name: &str| -> Option<String> {
        let i = args.iter().position(|a| a == name)?;
        if i + 1 >= args.len() {
            return None;
        }
        let v = args.remove(i + 1);
        args.remove(i);
        Some(v)
    };
    let phone = opt("--phone").or_else(|| opt("-p"));
    let after_s = opt("--after");
    let ms = opt("--ms");
    let at = opt("--at");
    let dir = opt("--dir");
    let max = opt("--max");
    let timeout = opt("--timeout");
    let scale = opt("--scale");
    let rules = opt("--rules");
    let aname = opt("--name");
    if args.is_empty() || args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
        println!("{USAGE}");
        return Ok(());
    }
    let inst = pick(phone.as_deref())?;
    let after = After::parse(after_s.as_deref());
    let cmd = args.remove(0);
    let rest = args.join(" ");
    let need = |what: &str| -> Result<()> {
        if rest.is_empty() {
            bail!("{cmd}: {what} required\n{USAGE}");
        }
        Ok(())
    };
    let print = |o: ax::Outcome| {
        if json {
            println!("{}", serde_json::to_string_pretty(&o).unwrap_or_default());
        } else {
            print!("{}", o.text());
        }
    };
    match cmd.as_str() {
        "snapshot" | "snap" | "ss" => {
            let prev = ax::last(&inst);
            let s = ax::snapshot(&inst, SnapOpts { all, ..Default::default() })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else if diff {
                match prev {
                    Some(p) => print!("{}", s.diff(&p)),
                    None => print!("{}", s.text()),
                }
            } else {
                print!("{}", s.text());
            }
            if !json {
                eprintln!("({} elements, {} ms)", s.elements.len(), s.ms);
            }
        }
        "inspect" => {
            let s = x86sim_core::axinspect::inspect(&inst, all)?;
            let n: Vec<f64> = args.iter().filter_map(|a| a.parse().ok()).collect();
            let el = if n.len() == 2 && args.len() == 2 {
                Some(x86sim_core::axinspect::element_at(&s, n[0], n[1]).ok_or_else(|| anyhow!("no element at {} {}", n[0], n[1]))?)
            } else if !rest.is_empty() {
                Some(ax::find(&s, &rest)?)
            } else {
                None
            };
            match (el, json) {
                (Some(e), true) => println!("{}", serde_json::to_string_pretty(&x86sim_core::axinspect::element_json(e))?),
                (Some(e), false) => print!("{}", x86sim_core::axinspect::describe(e)),
                (None, true) => println!("{}", serde_json::to_string_pretty(&s)?),
                (None, false) => {
                    print!("{}", s.text());
                    for h in &s.hidden {
                        println!("not reachable by VoiceOver: {} {} {} {}", h["app"].as_str().unwrap_or(""), h["class"].as_str().unwrap_or(""), h["frame"], h["why"].as_str().unwrap_or(""));
                    }
                }
            }
        }
        "audit" => {
            let o = x86sim_core::axaudit::AuditOpts { dynamic_type, all, rules: rules.map(|r| r.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default() };
            let r = x86sim_core::axaudit::audit(&inst, &o)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", r.text());
            }
        }
        "action" => {
            let name = args.first().cloned().ok_or_else(|| anyhow!("action: {}", x86sim_core::axinspect::ACTIONS.join(" | ")))?;
            let target = args[1..].join(" ");
            let arg = dir.as_deref().or(aname.as_deref());
            print(x86sim_core::axinspect::action(&inst, (!target.is_empty()).then_some(target.as_str()), &name, arg, after)?)
        }
        "find" => {
            need("a target")?;
            let s = ax::snapshot(&inst, SnapOpts { all, ..Default::default() })?;
            let hits = ax::find_all(&s, &ax::parse_target(&rest));
            if hits.is_empty() {
                bail!("nothing matches {rest:?}");
            }
            for e in hits {
                if json {
                    println!("{}", serde_json::to_string(e)?);
                } else {
                    println!("{}  [{} {}]", e.line(s.screen), e.layer, e.app);
                }
            }
        }
        "tap" => {
            need("a target")?;
            print(ax::tap(&inst, &rest, 80, after)?)
        }
        "long-press" | "longpress" => {
            need("a target")?;
            print(ax::tap(&inst, &rest, ms.and_then(|m| m.parse().ok()).unwrap_or(800), after)?)
        }
        "type" => print(ax::type_text(&inst, &rest, clear, submit, keys, after)?),
        "key" => {
            need("a key")?;
            print(ax::key(&inst, &rest, after)?)
        }
        "scroll" => {
            let d = args.first().cloned().unwrap_or_else(|| "down".into());
            let amount = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(0.5);
            print(ax::scroll(&inst, &d, amount, at.as_deref(), after)?)
        }
        "swipe" => {
            let n: Vec<f64> = args.iter().filter_map(|a| a.parse().ok()).collect();
            if n.len() >= 4 {
                print(ax::swipe(&inst, (n[0], n[1]), (n[2], n[3]), n.get(4).map(|m| *m as u64).unwrap_or(250), after)?)
            } else {
                print(ax::swipe_dir(&inst, args.first().map(|s| s.as_str()).unwrap_or("up"), after)?)
            }
        }
        "scroll-to" | "scrollto" => {
            need("a target")?;
            print(ax::scroll_to(&inst, &rest, dir.as_deref().unwrap_or("down"), max.and_then(|m| m.parse().ok()).unwrap_or(10), after)?)
        }
        "wait-for" | "waitfor" => {
            need("a target")?;
            let t = Duration::from_secs_f64(timeout.and_then(|t| t.parse().ok()).unwrap_or(20.0));
            print(ax::wait_for(&inst, &rest, gone, t)?)
        }
        "press" => {
            let b = args.first().cloned().ok_or_else(|| anyhow!("press: a button (home lock side ...)"))?;
            print(ax::press(&inst, &b, args.get(1).and_then(|m| m.parse().ok()).unwrap_or(100), after)?)
        }
        "keyboard" | "kb" => {
            let w = args.first().cloned().unwrap_or_else(|| "status".into());
            let h = args.get(1).cloned().unwrap_or_else(|| "toggle".into());
            print(ax::keyboard(&inst, &w, &h, after)?)
        }
        "open" => {
            need("a URL")?;
            print(ax::open_url(&inst, &rest, after)?)
        }
        "launch" => {
            let b = args.first().cloned().ok_or_else(|| anyhow!("launch: a bundle id"))?;
            print(ax::launch(&inst, &b, &args[1..], after)?)
        }
        "terminate" => {
            need("a bundle id")?;
            print(ax::terminate(&inst, &rest, after)?)
        }
        "screenshot" | "shot" => {
            let div = scale.and_then(|s| s.parse().ok()).unwrap_or(inst.scale.round().max(1.0) as u32);
            let (png, w, h) = ax::screenshot(&inst, div)?;
            let out = args.first().cloned().unwrap_or_else(|| format!("x86sim-{}.png", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)));
            std::fs::write(&out, png)?;
            println!("{out} ({w}x{h})");
        }
        o => bail!("unknown command {o}\n{USAGE}"),
    }
    Ok(())
}
