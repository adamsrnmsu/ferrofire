//! ferrofire CLI. Also the `simulinksolver` entry point: selected when argv[0] ends in `simulinksolver`.
//! Subcommands: `simulink [args]`, `check model.yaml [--steps N] [--write tag=value@t]`.

use ferrofire::model::{Model, Value};

fn usage() -> ! {
    eprintln!("usage: ferrofire run model.yaml [--server tcp://0.0.0.0:5555] [--publish udp://*;239.0.0.1:40000] [--rate 0.1]\n       ferrofire pypower {{start|stop|restart|status}} [-c /etc/sceptre/config.ini] [-e pybennu] [-d] [-v]\n       ferrofire simulink [-tf inf] [--model model.yaml]\n       ferrofire check model.yaml [--steps N] [--write tag=value@t]...");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args[0].ends_with("simulinksolver") {
        return ferrofire::simulink::main(&args[1..]);
    }
    if args[0].ends_with("pybennu-power-solver") {
        return ferrofire::pypower::main(&args[1..]);
    }
    match args.get(1).map(String::as_str) {
        Some("simulink") => ferrofire::simulink::main(&args[2..]),
        Some("run") => ferrofire::bennu::main(&args[2..]),
        Some("pypower") => ferrofire::pypower::main(&args[2..]),
        Some("check") => check(&args[2..]),
        _ => usage(),
    }
}

type Write = (f64, String, f64);

/// `tag=value[@t]` (value `true`/`false` allowed) -> (t, tag, value).
fn parse_write(s: &str) -> Option<Write> {
    let (tv, t) = s.split_once('@').unwrap_or((s, "0"));
    let (tag, v) = tv.split_once('=')?;
    let v = match v {
        "true" => 1.0,
        "false" => 0.0,
        _ => v.parse().ok()?,
    };
    Some((t.parse().ok()?, tag.to_string(), v))
}

/// `model.yaml [--steps N] [--write tag=value@t]...`
fn parse_check(args: &[String]) -> (&String, usize, Vec<Write>) {
    let path = args.first().unwrap_or_else(|| usage());
    let (mut steps, mut writes) = (100usize, vec![]);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let val = it.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--steps" => steps = val.parse().unwrap_or_else(|_| usage()),
            "--write" => writes.push(parse_write(val).unwrap_or_else(|| usage())),
            _ => usage(),
        }
    }
    (path, steps, writes)
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

/// Apply (and drop) the writes due at or before the model's current time.
fn apply_due(m: &mut Model, writes: &mut Vec<Write>) {
    let due: Vec<_> = writes
        .iter()
        .filter(|(t, ..)| *t <= m.time + 1e-9)
        .map(|(_, tag, v)| (tag.clone(), Value::Analog(*v)))
        .collect();
    writes.retain(|(t, ..)| *t > m.time + 1e-9);
    for tag in m.apply_writes(&due) {
        eprintln!("write to unknown/readonly tag {tag} ignored");
    }
}

/// Offline run printing the snapshot at t=0 and after N steps; `--write tag=value@t` applies at model time t.
fn check(args: &[String]) {
    let (path, steps, mut writes) = parse_check(args);
    let yaml = std::fs::read_to_string(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let mut m =
        Model::load_in(&yaml, std::path::Path::new(path).parent()).unwrap_or_else(|e| fail(e));
    for _ in 0..steps {
        apply_due(&mut m, &mut writes);
        m.step(m.dt);
    }
    println!("t={:.3}", m.time);
    for (tag, v) in m.snapshot() {
        match v {
            Value::Analog(x) => println!("{tag}={x:.6}"),
            Value::Binary(b) => println!("{tag}={b}"),
        }
    }
}
