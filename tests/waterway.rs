//! Waterway regression (spec 6.3/6.4): the model file must behave like the original hand-written physics.
use ferrofire::model::{Model, Value};
use std::collections::HashMap;

const INPUTS: [&str; 4] = [
    "lowergate.open",
    "lowervalve.open",
    "uppergate.open",
    "uppervalve.open",
];

fn g(snap: &HashMap<String, f64>, t: &str) -> f64 {
    snap[t]
}

fn snap(m: &Model) -> HashMap<String, f64> {
    m.snapshot()
        .into_iter()
        .map(|(t, v)| {
            (
                t.to_string(),
                match v {
                    Value::Analog(x) => x,
                    Value::Binary(b) => b as u8 as f64,
                },
            )
        })
        .collect()
}

fn set(m: &mut Model, i: [bool; 4]) {
    let w: Vec<_> = INPUTS
        .iter()
        .zip(i)
        .map(|(t, b)| (t.to_string(), Value::Analog(b as u8 as f64)))
        .collect();
    assert!(m.apply_writes(&w).is_empty());
}

/// RTU control logic from waterway-scenario.yaml, evaluated from published sensors (the real RTUs do this over Modbus).
fn rtu_logic(s: &HashMap<String, f64>) -> [bool; 4] {
    let (lo, up, lk) = (
        g(s, "lowerwater.height"),
        g(s, "upperwater.height"),
        g(s, "lock.height"),
    );
    let (ld, ldir, ud) = (
        g(s, "lowersensor.direction"),
        g(s, "locksensor.direction"),
        g(s, "uppersensor.direction"),
    );
    let (la, lka, ua) = (
        g(s, "lowersensor.active") == 1.0,
        g(s, "locksensor.active") == 1.0,
        g(s, "uppersensor.active") == 1.0,
    );
    let near = |a: f64| lk >= a - 0.1 && lk <= a + 0.1;
    [
        (ld == 1.0 && la && near(lo)) || (ldir == -1.0 && lka && near(lo)),
        (ld == 1.0 && la && lk > lo) || (ldir == -1.0 && lka && lk > lo),
        (ldir == 1.0 && lka && near(up)) || (ud == -1.0 && ua && near(up)),
        (ldir == 1.0 && lka && lk < up) || (ud == -1.0 && ua && lk < up),
    ]
}

fn waterway() -> Model {
    Model::load(ferrofire::WATERWAY_YAML).unwrap()
}

#[test]
fn full_transit_with_rtu_logic() {
    let mut m = waterway();
    let dt = 0.05;
    let (mut reached_upper, mut back_down, mut max_lock) = (false, false, 0.0f64);
    for _ in 0..(600.0 / dt) as usize {
        let s = snap(&m);
        set(&mut m, rtu_logic(&s));
        m.step(dt);
        let s = snap(&m);
        max_lock = max_lock.max(s["lock.height"]);
        if s["boat.location"] >= 300.0 {
            reached_upper = true;
        }
        if reached_upper && s["boat.location"] <= 0.0 {
            back_down = true;
            break;
        }
    }
    assert!(reached_upper, "boat never reached the upper end");
    assert!(back_down, "boat never returned to the lower end");
    assert!(
        (max_lock - 15.0).abs() < 1e-9,
        "lock never filled to upper pool: {max_lock}"
    );
    assert!(
        (snap(&m)["lock.height"] - 5.0).abs() < 0.2,
        "lock not drained"
    );
}

#[test]
fn closed_gates_block_boat_and_valve_moves_level() {
    let mut m = waterway();
    for _ in 0..1000 {
        m.step(0.1);
    }
    assert_eq!(m.get("boat.location"), Some(Value::Analog(100.0))); // held at closed lower gate, still lower zone
    assert_eq!(m.get("lowersensor.active"), Some(Value::Analog(1.0)));
    set(&mut m, [false, false, false, true]);
    m.step(1.0);
    let Some(Value::Analog(h)) = m.get("lock.height") else {
        panic!()
    };
    assert!((h - 5.5).abs() < 1e-9);
}

#[test]
fn boat_reverses_at_ends() {
    let mut m = waterway();
    set(&mut m, [true, false, true, false]);
    for _ in 0..70 {
        m.step(1.0);
    } // 350 m at 5 m/s: past the top end
    assert_eq!(m.get("boat.direction"), Some(Value::Analog(-1.0)));
    let Some(Value::Analog(x)) = m.get("boat.location") else {
        panic!()
    };
    assert!(x < 300.0);
}

#[test]
fn nonzero_write_latches_one_and_unknown_tags_rejected() {
    let mut m = waterway();
    let rej = m.apply_writes(&[
        ("lowervalve.open".into(), Value::Analog(0.3)),
        ("lock.height".into(), Value::Analog(9.0)),
        ("nope".into(), Value::Analog(1.0)),
    ]);
    assert_eq!(rej, vec!["lock.height", "nope"]); // states and outputs are not writable
    assert_eq!(m.get("lowervalve.open"), Some(Value::Analog(1.0)));
}

/// 600 s closed loop must reproduce the trace captured from the original physics.rs (every 5 s, 15 tags).
#[test]
fn golden_trace_from_original_physics() {
    let csv = include_str!("golden/waterway_trace.csv");
    let mut lines = csv.lines();
    let tags: Vec<&str> = lines.next().unwrap().split(',').skip(1).collect();
    let rows: Vec<Vec<f64>> = lines
        .map(|l| l.split(',').map(|x| x.parse().unwrap()).collect())
        .collect();
    let mut m = waterway();
    let dt = 0.05;
    for k in 0..(600.0 / dt) as usize {
        let s = snap(&m);
        set(&mut m, rtu_logic(&s));
        if k % 100 == 0 {
            let row = &rows[k / 100];
            let s = snap(&m);
            for (t, want) in tags.iter().zip(&row[1..]) {
                assert!(
                    (s[*t] - want).abs() < 1e-8,
                    "t={} {t}: {} vs {want}",
                    row[0],
                    s[*t]
                );
            }
        }
        m.step(dt);
    }
}

/// seeded LCG: random gate/valve schedule; boat never crosses a closed gate, exactly one sensor is active.
#[test]
fn barriers_and_sensors_property() {
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut rnd = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    for _ in 0..20 {
        let mut m = waterway();
        let mut gates = [false; 2];
        for k in 0..4000 {
            if k % 40 == 0 {
                let r = rnd();
                gates = [r & 1 != 0, r & 2 != 0];
                set(&mut m, [gates[0], r & 4 != 0, gates[1], r & 8 != 0]);
            }
            let before = snap(&m)["boat.location"];
            m.step(0.05);
            let s = snap(&m);
            let after = s["boat.location"];
            let active = s["lowersensor.active"] + s["locksensor.active"] + s["uppersensor.active"];
            assert_eq!(active, 1.0);
            for (gx, open) in [(100.0, gates[0]), (200.0, gates[1])] {
                if !open {
                    assert!(
                        !(before <= gx && after > gx),
                        "crossed closed gate {gx} upstream"
                    );
                }
            }
        }
    }
}

/// Tank with a linear valve has a closed form: RK4 must match it, and the bounds must hold.
#[test]
fn tank_matches_analytic_solution_and_clamps() {
    let y = "version: 1\ndt: 0.1\nparams: {area: 50, cv: 0.3, qin: 0.5}\ninputs:\n  pump.run: {type: binary, default: true}\nstates:\n  tank.level: {ode: '(qin * pump.run - cv * tank.level) / area', init: 2.0, min: 0, max: 10}\noutputs:\n  tank.level: {type: analog}\n";
    let mut m = Model::load(y).unwrap();
    let (k, eq) = (0.3 / 50.0, 0.5 / 0.3);
    for _ in 0..1000 {
        m.step(0.1);
    }
    let want = eq + (2.0 - eq) * (-k * 100.0f64).exp();
    let Some(Value::Analog(l)) = m.get("tank.level") else {
        panic!()
    };
    assert!((l - want).abs() < 1e-9, "{l} vs {want}");
    // overfill: inflow 10x, level must pin at max
    let y = y
        .replace("qin: 0.5", "qin: 50")
        .replace("max: 10", "max: 4");
    let mut m = Model::load(&y).unwrap();
    for _ in 0..5000 {
        m.step(0.1);
    }
    assert_eq!(m.get("tank.level"), Some(Value::Analog(4.0)));
}
