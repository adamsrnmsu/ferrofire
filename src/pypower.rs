//! `pybennu-power-solver` drop-in (spec 5(b)): same CLI, INI config, daemon files, tag vocabulary, noise and reply forms
//! as pybennu's PyPower provider, backed by the native power flow. Behaviour matched from pybennu's documented
//! interface only; no pybennu code is used.

use crate::bennu::{datagrams_msg, group_for, on_signal, parse_udp, udp_sender, STOP};
use crate::model::Kind;
use crate::power::{case, Islands, Power};
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
    pub fn seeded() -> Rng {
        Rng(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            | 1)
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
    let mut r = reply(req, p, debug).into_bytes();
    r.push(0);
    r
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
        "query" => format!(
            "ACK={}",
            (0..p.n_tags())
                .map(|i| format!("{},", p.tag_name(i)))
                .collect::<String>()
        ),
        "read" => match p.tag_index(payload) {
            Some(i) => format!(
                "ACK={}:{}",
                p.tag_name(i),
                fmt_py(p.tag_name(i), p.tag_kind(i), p.get(i))
            ),
            None => format!("ERR='{payload}'"),
        },
        "write" => {
            // validate the whole batch first, then apply, then one solve (pybennu: unknown tag -> nothing applied)
            let mut batch = vec![];
            for pt in payload.split(',').filter(|s| !s.is_empty()) {
                let Some((tag, val)) = pt.split_once(':') else {
                    return format!("ERR=Malformed WRITE point '{pt}'");
                };
                let Some(i) = p.tag_index(tag) else {
                    return format!(
                        "ERR=Provider failed to process write message with exception: '{tag}'"
                    );
                };
                let x = match val {
                    "true" | "false" if p.tag_kind(i) == Kind::Binary => (val == "true") as u8 as f64,
                    "true" | "false" => return format!("ERR=Invalid value for tag: {tag}"),
                    _ => match val.parse::<f64>() { Ok(x) => x, Err(_) => return format!("ERR=Provider failed to process write message with exception: could not convert string to float: '{val}'") },
                };
                batch.push((i, x));
            }
            for (i, x) in batch {
                if p.writable(i) {
                    p.set(i, x);
                }
            } // read-only fields: accepted, then overwritten by the solve (as pybennu)
            p.solve();
            "ACK=Success processing PyPower write command".into()
        }
        _ => format!("ERR=Unknown command type '{op}'"),
    }
}

// ---- config + daemon -----------------------------------------------------------------------------------------

struct Cfg {
    server: String,
    publish: String,
    case: String,
    debug: bool,
}

fn load_cfg(path: &str, debug: bool) -> Result<Cfg, String> {
    let ini = parse_ini(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?);
    let s = ini
        .get("power-solver-service")
        .ok_or("no [power-solver-service] section in the configuration file")?;
    let get = |k: &str| {
        s.get(k).cloned().ok_or_else(|| format!("The following must be defined in the configuration file: 'server-endpoint', 'publish-endpoint', 'case-file' (missing '{k}')"))
    };
    let st = s
        .get("solver-type")
        .ok_or("'solver-type' must be defined in the configuration file")?;
    if st != "PyPower" && st != "Prosim" {
        return Err(format!(
            "solver-type '{st}' is not supported by this power solver (PyPower only)"
        ));
    }
    let debug = match s.get("debug") {
        Some(d) => d.to_lowercase() == "true",
        None => debug,
    };
    let abs = |f: String| {
        std::fs::canonicalize(&f)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or(f)
    };
    Ok(Cfg {
        server: get("server-endpoint")?,
        publish: get("publish-endpoint")?.replace("*;", ""),
        case: abs(get("case-file")?),
        debug,
    })
}

fn pid_of(pidfile: &str) -> Option<i32> {
    std::fs::read_to_string(pidfile).ok()?.trim().parse().ok()
}
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn daemonize(pidfile: &str, out: &str, err: &str) -> Result<(), String> {
    unsafe {
        if libc::fork() > 0 {
            libc::_exit(0);
        }
        libc::chdir(c"/".as_ptr());
        libc::setsid();
        libc::umask(0);
        if libc::fork() > 0 {
            libc::_exit(0);
        }
    }
    let pid = std::process::id();
    std::fs::write(pidfile, format!("{pid}\n")).map_err(|e| format!("{pidfile}: {e}"))?;
    eprintln!("\nProcess started with PID {pid}");
    use std::os::fd::AsRawFd;
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
        unsafe {
            libc::dup2(f.as_raw_fd(), fd);
        }
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
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if alive(pid) {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    let _ = std::fs::remove_file(pidfile);
}

pub fn main(args: &[String]) {
    std::process::exit(match cli(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ERROR: {e}");
            1
        }
    });
}

fn cli(args: &[String]) -> Result<(), String> {
    let (mut op, mut conf, mut env, mut dmn, mut debug) = (
        None,
        "/etc/sceptre/config.ini".to_string(),
        "pybennu".to_string(),
        false,
        false,
    );
    let mut it = args.iter();
    let usage = || -> ! {
        eprintln!("usage: pybennu-power-solver [-c CONFIG_FILE] [-d] [-e ENV] [-v] {{start,stop,restart,status}}");
        std::process::exit(2)
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" | "--config-file" => conf = it.next().cloned().unwrap_or_else(|| usage()),
            "-e" | "--env" => env = it.next().cloned().unwrap_or_else(|| usage()),
            "-d" | "--daemonize" => dmn = true,
            "-v" | "--debug" => debug = true,
            o @ ("start" | "stop" | "restart" | "status") if op.is_none() => {
                op = Some(o.to_string())
            }
            _ => usage(),
        }
    }
    let op = op.unwrap_or_else(|| usage());
    let (pidfile, out, err) = (
        format!("/var/run/bennu-{env}.pid"),
        format!("/var/log/bennu-{env}.out"),
        format!("/var/log/bennu-{env}.err"),
    );
    let running = pid_of(&pidfile).filter(|p| alive(*p));
    match op.as_str() {
        "stop" => stop(&pidfile, false),
        "status" => {
            println!("Viewing daemon status");
            match running {
                Some(p) => println!("Daemon is running as PID {p}"),
                None => println!("Daemon is not running"),
            }
        }
        "restart" if running.is_none() => {
            println!("Daemon is not already running - cannot restart")
        }
        _ => {
            if op == "restart" {
                println!("Restarting daemon");
                stop(&pidfile, true);
            } else if let Some(p) = running {
                return Err(format!(
                    "pidfile {pidfile} already exist. Daemon already running (PID {p})?"
                ));
            }
            if !std::path::Path::new(&conf).exists() {
                return Err(format!(
                    "The file {conf} does not exist - please provide a valid config file."
                ));
            }
            let conf = std::fs::canonicalize(&conf)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(conf);
            let cfg = load_cfg(&conf, debug)?;
            // load and solve before forking so errors reach the terminal; threads and sockets come after
            let c = case::load_file(std::path::Path::new(&cfg.case))?;
            let power = Power::new(c, Islands::Fail, false, &[], &[])?;
            let daemon = op == "restart" || dmn;
            if daemon {
                daemonize(&pidfile, &out, &err)?;
            }
            let r = serve(cfg, power);
            if daemon {
                let _ = std::fs::remove_file(&pidfile);
            }
            return r;
        }
    }
    Ok(())
}

fn serve(cfg: Cfg, power: Power) -> Result<(), String> {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
    let p = Arc::new(Mutex::new(power));
    let ctx = zmq::Context::new();
    let rep = ctx.socket(zmq::REP).map_err(|e| e.to_string())?;
    rep.bind(&cfg.server)
        .map_err(|e| format!("bind {}: {e}", cfg.server))?;
    let (p2, debug) = (p.clone(), cfg.debug);
    let rep_t = std::thread::spawn(move || {
        while !STOP.load(Ordering::SeqCst) {
            if !matches!(rep.poll(zmq::POLLIN, 100), Ok(n) if n > 0) {
                continue;
            }
            if let Ok(req) = rep.recv_bytes(0) {
                let _ = rep.send(handle(&req, &p2, debug), 0);
            }
        }
    });
    let (iface, dest) = parse_udp(&cfg.publish)?;
    let sock = udp_sender(iface.as_deref(), dest)?;
    let group = group_for(&cfg.publish);
    println!(
        "Info: pybennu-power-solver (ferrofire): case {}; REP {}; RADIO {} group {group} every 1s",
        cfg.case, cfg.server, cfg.publish
    );
    let (mut rng, t0, mut k) = (Rng::seeded(), Instant::now(), 0u64);
    while !STOP.load(Ordering::SeqCst) {
        let msg = pack(&p.lock().unwrap(), &mut rng);
        for d in datagrams_msg(&group, &msg) {
            let _ = sock.send_to(&d, dest);
        }
        k += 1;
        while !STOP.load(Ordering::SeqCst) && Instant::now() < t0 + Duration::from_secs(k) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = rep_t.join();
    println!("Info: stopped");
    Ok(())
}
