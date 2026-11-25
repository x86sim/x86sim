//! x86sim-pool: the phone pool (x86sim-core's pool module) from the command line, as `x86sim pool ...`.
use x86sim_core::pool;

const USAGE: &str = "usage: x86sim pool <command> [--pool NAME] [--json]
  up N [--model M] [--os 27.2] [--preset lean|minimal|full] [--reserve-mb MB] [--force]
                    make the pool N phones (clones of <pool>-template) and boot them one at a time while memory allows
  take [--ttl S] [--reset none|apps|restart|erase] [--pid PID | --hold-parent] [--holder TEXT] [--wait S]
                    a booted free phone (prints its name). The lease ends at --ttl (default 1800 s) or when the --pid
                    process (--hold-parent: the calling shell) exits; then the phone is reset and free again
  release PHONE [--reset MODE]   give it back (reset in the background; default: the mode given at take, else apps)
  renew PHONE [--ttl S]          extend a lease
  status            members, leases, memory per phone, room for more, KSM, reflink cloning
  down [--delete] [--force]      stop the pool's phones (--delete: remove them and the template)
  ksm [on|off]      KSM page sharing: status, or turn it on/off (sudo, shown and asked for on a terminal)
The pool defaults to \"pool\" (X86SIM_POOL). Reset modes: none, apps (uninstall apps installed during the lease),
restart (fresh boot), erase (re-clone from the template).";

fn main() {
    let mut a: Vec<String> = std::env::args().skip(1).collect();
    if a.is_empty() || matches!(a[0].as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return;
    }
    if let Err(e) = run(&mut a) {
        eprintln!("x86sim pool: {e:#}");
        std::process::exit(1);
    }
}

fn opt(a: &mut Vec<String>, k: &str) -> Option<String> {
    let i = a.iter().position(|x| x == k)?;
    a.remove(i);
    (i < a.len()).then(|| a.remove(i))
}
fn flag(a: &mut Vec<String>, k: &str) -> bool {
    a.iter().position(|x| x == k).map(|i| a.remove(i)).is_some()
}
fn num<T: std::str::FromStr>(v: Option<String>, what: &str) -> anyhow::Result<Option<T>> {
    v.map(|s| s.parse::<T>().map_err(|_| anyhow::anyhow!("{what}: not a number: {s}"))).transpose()
}

fn run(a: &mut Vec<String>) -> anyhow::Result<()> {
    let cmd = a.remove(0);
    let name = opt(a, "--pool").or_else(|| std::env::var("X86SIM_POOL").ok()).unwrap_or_else(|| "pool".into());
    let json = flag(a, "--json");
    let mut say = |s: &str| eprintln!("pool: {s}");
    match cmd.as_str() {
        "up" => {
            let o = pool::UpOpts {
                model: opt(a, "--model").unwrap_or_default(),
                os: opt(a, "--os").or_else(|| opt(a, "--ios")).unwrap_or_default(),
                preset: opt(a, "--preset"),
                reserve_mb: num(opt(a, "--reserve-mb"), "--reserve-mb")?.unwrap_or(0.0),
                force: flag(a, "--force"),
            };
            let n: usize = a.first().and_then(|s| s.parse().ok()).ok_or_else(|| anyhow::anyhow!("up N: how many devices"))?;
            let up = pool::up(&name, n, &o, &mut say)?;
            if json {
                println!("{}", serde_json::json!({"pool": name, "running": up}));
            } else {
                println!("{} of {n} devices of pool {name} at their home screen", up.len());
            }
        }
        "take" => {
            let pid = if flag(a, "--hold-parent") {
                // SAFETY: plain libc call
                Some(unsafe { libc::getppid() } as u32)
            } else {
                num(opt(a, "--pid"), "--pid")?
            };
            let o = pool::TakeOpts {
                holder: opt(a, "--holder").unwrap_or_else(|| format!("cli:{}", std::env::var("USER").unwrap_or_default())),
                holder_pid: pid,
                ttl_s: num(opt(a, "--ttl"), "--ttl")?.unwrap_or(0),
                reset: opt(a, "--reset").unwrap_or_default(),
                wait_s: num(opt(a, "--wait"), "--wait")?.unwrap_or(0),
            };
            let t = pool::take(&name, &o)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&t)?);
            } else {
                println!("{}", t.phone);
            }
        }
        "release" => {
            let p = a.first().ok_or_else(|| anyhow::anyhow!("release PHONE"))?.clone();
            let mode = pool::release(&name, &p, opt(a, "--reset").as_deref())?;
            eprintln!("pool: {p} released (reset: {mode})");
        }
        "renew" => {
            let ttl = num(opt(a, "--ttl"), "--ttl")?.unwrap_or(0);
            let p = a.first().ok_or_else(|| anyhow::anyhow!("renew PHONE"))?.clone();
            let exp = pool::renew(&name, &p, ttl)?;
            eprintln!("pool: {p} held until {exp} (unix time)");
        }
        "status" => {
            let s = pool::status(&name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }
            println!("pool {} ({}{}): {} members, {} free and ready", s.pool, if s.model.is_empty() { "no model yet" } else { &s.model }, s.template.as_ref().map(|t| format!(", template {t}")).unwrap_or_default(), s.members.len(), s.free);
            for m in &s.members {
                let lease = match (&m.holder, m.expires_in_s) {
                    (Some(h), Some(e)) => format!(" held by {h}{} for {e} s more, reset {}", m.holder_pid.map(|p| format!(" (pid {p})")).unwrap_or_default(), m.reset.clone().unwrap_or_default()),
                    _ => String::new(),
                };
                println!("  {:<14} {:<9} {:<8} {:>6.0} MB{}{}", m.phone, m.state, if m.ready { "home" } else { &m.phase }, m.memory_mb, lease, if m.note.is_empty() { String::new() } else { format!("  [{}]", m.note) });
            }
            println!(
                "memory: {:.0} MB available, ~{:.0} MB per device: room for {} more; KSM {}{}; clones: {}",
                s.mem_available_mb,
                s.cost_mb,
                s.room_for,
                if !s.ksm.available { "not in this kernel" } else if s.ksm.running { "on" } else { "off (x86sim pool ksm on)" },
                if s.ksm.running { format!(", {:.0} MB shared", s.ksm.saved_mb) } else { String::new() },
                if s.reflink { "reflink" } else { "full copy" }
            );
        }
        "down" => {
            let (del, force) = (flag(a, "--delete"), flag(a, "--force"));
            pool::down(&name, del, force, &mut say)?;
        }
        "ksm" => {
            let k = pool::ksm();
            match a.first().map(|s| s.as_str()) {
                None | Some("status") => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&k)?);
                    } else {
                        println!(
                            "KSM: {}{}; pool devices opt in (PR_SET_MEMORY_MERGE) when they start{}",
                            if !k.available { "not in this kernel" } else if k.running { "on" } else { "off" },
                            if k.running { format!(", {} pages per scan, {:.0} MB shared now", k.pages_to_scan, k.saved_mb) } else { String::new() },
                            if k.available && !k.running { format!("\nturn it on (root):\n  {}", k.enable.join("\n  ")) } else { String::new() }
                        );
                    }
                }
                Some(v @ ("on" | "off")) => {
                    anyhow::ensure!(k.available, "this kernel has no KSM (CONFIG_KSM)");
                    let cmds: Vec<String> = if v == "on" { k.enable.clone() } else { vec!["echo 0 | sudo tee /sys/kernel/mm/ksm/run".into()] };
                    // SAFETY: plain libc call
                    if unsafe { libc::isatty(0) } == 0 {
                        anyhow::bail!("needs root; run these yourself (not run without a terminal, to never sudo behind your back):\n  {}", cmds.join("\n  "));
                    }
                    eprintln!("pool: running with sudo (it may ask for your password):\n  {}", cmds.join("\n  "));
                    for c in &cmds {
                        let st = std::process::Command::new("sh").args(["-c", &format!("{c} > /dev/null")]).status()?;
                        anyhow::ensure!(st.success(), "{c} failed");
                    }
                    eprintln!("pool: KSM {v}");
                }
                Some(o) => anyhow::bail!("ksm {o}: on, off or status"),
            }
        }
        "_reset" => {
            // the background worker spawn_reset starts: _reset POOL ID MODE APPS
            let (p, id, mode) = (a.first().cloned().unwrap_or_default(), a.get(1).cloned().unwrap_or_default(), a.get(2).cloned().unwrap_or_default());
            let apps: Vec<String> = a.get(3).map(|s| s.split(',').filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default();
            pool::reset_worker(&p, &id, &mode, &apps)?;
        }
        "list" => {
            for p in pool::pools() {
                println!("{p}");
            }
        }
        o => anyhow::bail!("unknown command {o}\n{USAGE}"),
    }
    Ok(())
}
