//! `pybennu-power-solver` drop-in (spec 5(b)): same CLI, INI config, daemon files, tag vocabulary, noise and reply forms
//! as pybennu's PyPower provider, backed by the native power flow. Behaviour matched from pybennu's documented
//! interface only; no pybennu code is used.

use crate::bennu::{install_signals, sleep_until, with_nul, STOP};
use crate::model::Kind;
use crate::power::{case, Islands, Power};
use crate::transport::{self, Publisher};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---- INI -----------------------------------------------------------------------------------------------------

/// Minimal INI: `[section]`, `key = value` or `key: value`, whole-line `#`/`;` comments, keys lower-cased.
pub fn parse_ini(src: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut cur = String::new();
    for line in src.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with(';') {
            continue;
        }
        if let Some(s) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            cur = s.trim().to_string();
            out.entry(cur.clone()).or_default();
            continue;
        }
        if let Some(i) = l.find(['=', ':']) {
            out.entry(cur.clone())
                .or_default()
                .insert(l[..i].trim().to_lowercase(), l[i + 1..].trim().to_string());
        }
    }
    out
}

// ---- values, noise -------------------------------------------------------------------------------------------

/// Python `str(x).lower()` look: shortest round-trip decimal, `.0` on integral floats; id-like fields print as ints.
pub fn fmt_py(tag: &str, kind: Kind, x: f64) -> String {
    if kind == Kind::Binary {
        return (x != 0.0).to_string();
    }
    let f = tag.rsplit('.').next().unwrap_or("");
    if matches!(f, "bus" | "source" | "target") && x.fract() == 0.0 {
        return format!("{}", x as i64);
    }
    let s = x.to_string();
    if x.is_finite() && !s.contains('.') {
        format!("{s}.0")
    } else {
        s.to_lowercase()
    }
}

/// pybennu adds noise to every analog field except these (and none at all on branches).
pub fn noisy(tag: &str, kind: Kind) -> bool {
    if kind == Kind::Binary {
        return false;
    }
    let (dev, f) = tag.split_once('.').unwrap_or((tag, ""));
    match dev.split('-').next().unwrap_or("") {
        "bus" => f != "base_kv",
        "load" | "shunt" => f != "bus",
        "generator" => !matches!(f, "mw_max" | "mw_min" | "mvar_max" | "mvar_min"),
        _ => false,
    }
}

pub struct Rng(u64);
impl Rng {
    /// xorshift state must never be 0, so the low bit is forced on.
    pub fn from_seed(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    pub fn seeded() -> Rng {
        Rng::from_seed(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or_default(),
        )
    }
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17; // xorshift64
        lo + (hi - lo) * ((self.0 >> 11) as f64 / (1u64 << 53) as f64)
    }
    /// +-1 % of the value, +-0.1 absolute around zero.
    pub fn noise(&mut self, x: f64) -> f64 {
        if x == 0.0 {
            self.uniform(-0.1, 0.1)
        } else {
            x * self.uniform(0.99, 1.01)
        }
    }
}

/// The `tag:value,` message for one publish tick.
pub fn pack(p: &Power, rng: &mut Rng) -> String {
    (0..p.n_tags())
        .map(|i| {
            let (t, k, x) = (p.tag_name(i), p.tag_kind(i), p.get(i));
            let x = if noisy(t, k) { rng.noise(x) } else { x };
            format!("{t}:{},", fmt_py(t, k, x))
        })
        .collect()
}

// ---- requests ------------------------------------------------------------------------------------------------

pub fn handle(req: &[u8], p: &Mutex<Power>, debug: bool) -> Vec<u8> {
    with_nul(reply(req, p, debug))
}

fn reply(req: &[u8], p: &Mutex<Power>, debug: bool) -> String {
    let Ok(req) = std::str::from_utf8(req) else {
        return "ERR=Invalid message request".into();
    };
    let req = req.trim_end_matches('\0');
    if req.is_empty() {
        return "ERR=Message empty".into();
    }
    let Some((op, payload)) = req.split_once('=') else {
        return "ERR=Invalid message request".into();
    };
    if debug {
        println!("Debug: {op} {payload}");
    }
    let mut p = p.lock().unwrap();
    match op.to_ascii_lowercase().as_str() {
        "query" => reply_query(&p),
        "read" => reply_read(&p, payload),
        "write" => reply_write(&mut p, payload),
        _ => format!("ERR=Unknown command type '{op}'"),
    }
}

fn reply_query(p: &Power) -> String {
    format!(
        "ACK={}",
        (0..p.n_tags())
            .map(|i| format!("{},", p.tag_name(i)))
            .collect::<String>()
    )
}

fn reply_read(p: &Power, tag: &str) -> String {
    match p.tag_index(tag) {
        Some(i) => format!(
            "ACK={}:{}",
            p.tag_name(i),
            fmt_py(p.tag_name(i), p.tag_kind(i), p.get(i))
        ),
        None => format!("ERR='{tag}'"),
    }
}

/// One `tag:value` write point -> (tag index, value); the error is the whole reply.
fn parse_point(p: &Power, pt: &str) -> Result<(usize, f64), String> {
    let (tag, val) = pt
        .split_once(':')
        .ok_or_else(|| format!("ERR=Malformed WRITE point '{pt}'"))?;
    let i = p.tag_index(tag).ok_or_else(|| {
        format!("ERR=Provider failed to process write message with exception: '{tag}'")
    })?;
    let x = match val {
        "true" | "false" if p.tag_kind(i) == Kind::Binary => (val == "true") as u8 as f64,
        "true" | "false" => return Err(format!("ERR=Invalid value for tag: {tag}")),
        _ => val.parse::<f64>().map_err(|_| format!("ERR=Provider failed to process write message with exception: could not convert string to float: '{val}'"))?,
    };
    Ok((i, x))
}

/// Validate the whole batch first, then apply, then one solve (pybennu: unknown tag -> nothing applied).
fn reply_write(p: &mut Power, payload: &str) -> String {
    let batch: Result<Vec<_>, String> = payload
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|pt| parse_point(p, pt))
        .collect();
    let batch = match batch {
        Ok(b) => b,
        Err(reply) => return reply,
    };
    for (i, x) in batch {
        if p.writable(i) {
            p.set(i, x);
        }
    } // read-only fields: accepted, then overwritten by the solve (as pybennu)
    p.solve();
    "ACK=Success processing PyPower write command".into()
}

// ---- config + daemon -----------------------------------------------------------------------------------------

struct Cfg {
    server: String,
    publish: String,
    case: String,
    debug: bool,
}

/// Absolute form of `f` when it exists, else `f` as given.
fn canon(f: &str) -> String {
    std::fs::canonicalize(f)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| f.to_string())
}

fn required(s: &HashMap<String, String>, k: &str) -> Result<String, String> {
    s.get(k).cloned().ok_or_else(|| format!("The following must be defined in the configuration file: 'server-endpoint', 'publish-endpoint', 'case-file' (missing '{k}')"))
}

fn check_solver(s: &HashMap<String, String>) -> Result<(), String> {
    let st = s
        .get("solver-type")
        .ok_or("'solver-type' must be defined in the configuration file")?;
    if st != "PyPower" && st != "Prosim" {
        return Err(format!(
            "solver-type '{st}' is not supported by this power solver (PyPower only)"
        ));
    }
    Ok(())
}

fn load_cfg(path: &str, debug: bool) -> Result<Cfg, String> {
    let ini = parse_ini(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?);
    let s = ini
        .get("power-solver-service")
        .ok_or("no [power-solver-service] section in the configuration file")?;
    check_solver(s)?;
    let debug = s.get("debug").map_or(debug, |d| d.to_lowercase() == "true");
    Ok(Cfg {
        server: required(s, "server-endpoint")?,
        publish: required(s, "publish-endpoint")?.replace("*;", ""),
        case: canon(&required(s, "case-file")?),
        debug,
    })
}

fn pid_of(pidfile: &str) -> Option<i32> {
    std::fs::read_to_string(pidfile).ok()?.trim().parse().ok()
}
fn alive(pid: i32) -> bool {
    crate::sys::kill(pid, 0)
}

fn daemonize(pidfile: &str, out: &str, err: &str) -> Result<(), String> {
    crate::sys::double_fork_detach();
    let pid = std::process::id();
    std::fs::write(pidfile, format!("{pid}\n")).map_err(|e| format!("{pidfile}: {e}"))?;
    eprintln!("\nProcess started with PID {pid}");
    let open = |p: &str, w: bool| {
        if w {
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(p)
        } else {
            std::fs::File::open(p)
        }
    };
    for (path, fd, w) in [("/dev/null", 0, false), (out, 1, true), (err, 2, true)] {
        let f = open(path, w).map_err(|e| format!("{path}: {e}"))?;
        crate::sys::dup2(&f, fd);
    }
    Ok(())
}

fn stop(pidfile: &str, silent: bool) {
    let Some(pid) = pid_of(pidfile) else {
        if !silent {
            eprintln!("pidfile {pidfile} does not exist. Daemon not running?");
        }
        return;
    };
    for _ in 0..100 {
        // ~10 s of SIGTERM, then SIGKILL
        if !crate::sys::kill(pid, libc::SIGTERM) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if alive(pid) {
        crate::sys::kill(pid, libc::SIGKILL);
    }
    let _ = std::fs::remove_file(pidfile);
}

/// The `pybennu-power-solver` install name: the same options with no subcommand word in front.
pub fn main(args: &[String]) {
    use clap::Parser;
    run_cli(Cli::parse_from(
        std::iter::once("pybennu-power-solver").chain(args.iter().map(String::as_str)),
    ))
}

pub fn run_cli(c: Cli) {
    std::process::exit(match cli(c) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ERROR: {e}");
            1
        }
    });
}

/// pybennu-power-solver compatible daemon control (also the `pybennu-power-solver` install name).
#[derive(clap::Parser)]
#[command(name = "ferrofire pypower")]
pub struct Cli {
    #[arg(value_parser = ["start", "stop", "restart", "status"])]
    op: String,
    /// Solver config file
    #[arg(
        short = 'c',
        long = "config-file",
        default_value = "/etc/sceptre/config.ini",
        value_name = "CONFIG_FILE"
    )]
    conf: String,
    /// Environment name (pid and log file suffix)
    #[arg(short = 'e', long, default_value = "pybennu")]
    env: String,
    /// Run in the background
    #[arg(short = 'd', long = "daemonize")]
    daemon: bool,
    /// Debug logging
    #[arg(short = 'v', long = "debug")]
    debug: bool,
}

struct Paths {
    pid: String,
    out: String,
    err: String,
}

fn cli(c: Cli) -> Result<(), String> {
    let paths = Paths {
        pid: format!("/var/run/bennu-{}.pid", c.env),
        out: format!("/var/log/bennu-{}.out", c.env),
        err: format!("/var/log/bennu-{}.err", c.env),
    };
    let running = pid_of(&paths.pid).filter(|p| alive(*p));
    match c.op.as_str() {
        "stop" => stop(&paths.pid, false),
        "status" => print_status(running),
        "restart" if running.is_none() => {
            println!("Daemon is not already running - cannot restart")
        }
        _ => return start(&c, &paths, running),
    }
    Ok(())
}

fn print_status(running: Option<i32>) {
    println!("Viewing daemon status");
    match running {
        Some(p) => println!("Daemon is running as PID {p}"),
        None => println!("Daemon is not running"),
    }
}

/// `start`, or `restart` of a running daemon: load and solve before forking so errors reach the terminal.
fn start(c: &Cli, paths: &Paths, running: Option<i32>) -> Result<(), String> {
    if c.op == "restart" {
        println!("Restarting daemon");
        stop(&paths.pid, true);
    } else if let Some(p) = running {
        return Err(format!(
            "pidfile {} already exist. Daemon already running (PID {p})?",
            paths.pid
        ));
    }
    let (cfg, power) = load_service(c)?;
    let daemon = c.op == "restart" || c.daemon;
    if daemon {
        daemonize(&paths.pid, &paths.out, &paths.err)?;
    }
    let r = serve(cfg, power);
    if daemon {
        let _ = std::fs::remove_file(&paths.pid);
    }
    r
}

fn load_service(c: &Cli) -> Result<(Cfg, Power), String> {
    if !std::path::Path::new(&c.conf).exists() {
        return Err(format!(
            "The file {} does not exist - please provide a valid config file.",
            c.conf
        ));
    }
    let cfg = load_cfg(&canon(&c.conf), c.debug)?;
    let case = case::load_file(std::path::Path::new(&cfg.case))?;
    let power = Power::new(case, Islands::Fail, false, &[], &[])?;
    Ok((cfg, power))
}

/// Publish a noisy snapshot every second until `STOP`.
fn publish_loop(p: &Mutex<Power>, out: &dyn Publisher) {
    let (mut rng, t0, mut k) = (Rng::seeded(), Instant::now(), 0u64);
    while !STOP.load(Ordering::SeqCst) {
        out.publish(&pack(&p.lock().unwrap(), &mut rng));
        k += 1;
        sleep_until(t0 + Duration::from_secs(k), Duration::from_millis(20));
    }
}

fn serve(cfg: Cfg, power: Power) -> Result<(), String> {
    install_signals();
    crate::log::start("pypower");
    let p = Arc::new(Mutex::new(power));
    let (p2, debug) = (p.clone(), cfg.debug);
    let rep_t = transport::serve(&cfg.server, Box::new(move |req| handle(req, &p2, debug)))?;
    let out = transport::publisher(&cfg.publish)?;
    println!(
        "Info: pybennu-power-solver (ferrofire): case {}; REP {}; {} every 1s",
        cfg.case,
        cfg.server,
        out.describe()
    );
    publish_loop(&p, out.as_ref());
    let _ = rep_t.join();
    crate::log::stop();
    println!("Info: stopped");
    Ok(())
}
