//! Characterization of `Model::apply_writes` + `Model::step` (ww-1df.39): a seeded run of random writes and steps
//! must reproduce, bit for bit, the trace recorded on the code that evaluated every expression every time.
use ferrofire::model::{Model, Value};

const OSC: &str = "version: 1\ndt: 0.05\nsubsteps: 3\ninputs:\n  u: {type: analog}\nstates:\n  x: {ode: \"v\", init: 1}\n  v: {ode: \"-x - 0.1*v + d\", init: 0}\n  w: {toward: {target: \"u * 2\", rate: 1}, init: 0}\noutputs:\n  x: {type: analog}\n  v: {type: analog}\n  w: {type: analog}\n  d: {type: analog, expr: \"u + w * 0.5\"}\n  u: {type: analog}\n";

const MESH: &str = "version: 1\ndt: 0.1\ninputs:\n  k: {type: analog, default: 1, min: 0, max: 10}\nstates:\n  s: {toward: {target: \"k\", rate: 1}, init: 0}\n  lvl: {ode: \"(k - out) / 4\", init: 1}\noutputs:\n  s: {type: analog}\n  lvl: {type: analog}\n  out: {type: analog, expr: \"lvl * 0.5 + `bus-3`.voltage\"}\npower:\n  tags: pypower\n  drive:\n    \"load-1_bus-2.mw\": \"s * 4 + out\"\n  inline:\n    buses: [{id: 1, type: slack}, {id: 2, type: pq, pd: 10}, {id: 3, type: pq, pd: 5}]\n    branches: [{from: 1, to: 2, r: 0.01, x: 0.05}, {from: 2, to: 3, r: 0.01, x: 0.05}, {from: 1, to: 3, r: 0.02, x: 0.08}]\n    gens: [{bus: 1, qmax: 50, qmin: -50, pmax: 50}]\n";

/// FNV-1a over the bit patterns of every published value, in publish order.
fn digest(m: &Model, h: &mut u64) {
    for (_, v) in m.snapshot() {
        let x = match v {
            Value::Analog(x) => x.to_bits(),
            Value::Binary(b) => b as u64,
        };
        for byte in x.to_le_bytes() {
            *h = (*h ^ byte as u64).wrapping_mul(0x100000001b3);
        }
    }
}

fn run(yaml: &str, steps: u32) -> u64 {
    let mut m = Model::load(yaml).unwrap();
    let writable: Vec<String> = m
        .tags()
        .map(|(t, _)| t.to_string())
        .filter(|t| m.writable(t))
        .collect();
    let mut h = 0xcbf29ce484222325u64;
    digest(&m, &mut h);
    let mut seed = 12345u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed >> 20
    };
    for i in 0..steps {
        let mut ws: Vec<(String, Value)> = vec![];
        if i % 7 == 0 && !writable.is_empty() {
            for _ in 0..1 + next() % 3 {
                let t = &writable[next() as usize % writable.len()];
                ws.push((t.clone(), Value::Analog((next() % 5) as f64 * 0.5)));
            }
        }
        if i % 11 == 0 {
            ws.push(("no.such.tag".into(), Value::Analog(1.0))); // rejected: must leave the state alone
        }
        m.apply_writes(&ws);
        digest(&m, &mut h);
        m.step(m.dt);
        digest(&m, &mut h);
    }
    h
}

#[test]
fn traces_are_bit_identical_to_the_evaluate_everything_code() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    let got = [
        run(OSC, 2000),
        run(&read("waterway.yaml"), 2000),
        run(&read("pump-station.yaml"), 2000),
        run(MESH, 2000),
    ];
    // osc, waterway: recorded on 16db214 (before ww-1df.36-39), no power flow. pump-station, mesh: re-recorded for ww-1df.49,
    // whose sparse LU rounds the Newton step differently from the dense one (the dense code gave e063122f1af5d474 and
    // 87cd445d380649b8, and those runs matched the evaluate-everything code bit for bit).
    let want = [
        0xb0dc5f075c500166,
        0x9e6fcaedd1cf13c9,
        0x2d1fe051c65a6c18,
        0x96fb51b70327aab1,
    ];
    assert_eq!(got, want, "got {got:016x?}");
}
