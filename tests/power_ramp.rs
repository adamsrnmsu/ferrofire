//! Generator ramp (ww-1df.23.2): spin-up is status on then PG rises 0 -> setpoint, spin-down is PG falls to 0 then
//! status off, at a configurable MW/s; a command mid-ramp retargets from the current PG. Off by default.
use ferrofire::power::case::{load_file, Case, PG};
use ferrofire::power::{Islands, Power};
use std::path::PathBuf;

fn case30() -> Case {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/golden/case30.py");
    let mut c = load_file(&p).unwrap_or_else(|e| panic!("{e} (run `make golden` first)"));
    c.gen[1][PG] = 40.0; // bus 2: round numbers for the arithmetic below
    c
}

const G: &str = "generator-1_bus-2";

fn ramped(rate: f64) -> Power {
    let mut p = Power::new(case30(), Islands::Fail, false, &[], &[]).unwrap();
    p.enable_ramp(Some(rate), &[]).unwrap();
    p
}

fn tag(p: &Power, f: &str) -> f64 {
    p.get(p.tag_index(&format!("{G}.{f}")).unwrap())
}

fn put(p: &mut Power, f: &str, x: f64) {
    let i = p.tag_index(&format!("{G}.{f}")).unwrap();
    p.set(i, x);
}

/// advance by `dt`, re-solving like the model does when anything moved
fn run(p: &mut Power, dt: f64) {
    if p.advance(dt) {
        p.solve();
    }
}

#[test]
fn ramp_tags_exist_only_when_the_ramp_is_enabled() {
    let off = Power::new(case30(), Islands::Fail, false, &[], &[]).unwrap();
    let on = ramped(10.0);
    assert!(off.tag_index(&format!("{G}.ramp_state")).is_none());
    assert_eq!(on.n_tags(), off.n_tags() + 2 * on.case.gen.len());
    assert!(on.tag_index(&format!("{G}.ramp_target")).is_some());
    let i = on.tag_index(&format!("{G}.ramp_state")).unwrap();
    assert!(!on.writable(i));
    let mut off = off;
    assert!(!off.advance(1.0), "no ramp, nothing moves");
}

#[test]
fn spin_down_ramps_pg_to_zero_then_drops_status() {
    let mut p = ramped(10.0);
    let sp = tag(&p, "mw");
    assert_eq!(sp, 40.0);
    put(&mut p, "active", 0.0);
    assert_eq!(
        (
            tag(&p, "active"),
            tag(&p, "ramp_state"),
            tag(&p, "ramp_target")
        ),
        (1.0, 2.0, 0.0)
    );
    run(&mut p, 1.0);
    assert_eq!((tag(&p, "mw"), tag(&p, "active")), (30.0, 1.0));
    run(&mut p, 2.5);
    assert_eq!((tag(&p, "mw"), tag(&p, "active")), (5.0, 1.0));
    run(&mut p, 1.0); // overshoot is clamped at 0, not -5
    assert_eq!(
        (tag(&p, "mw"), tag(&p, "active"), tag(&p, "ramp_state")),
        (0.0, 0.0, 0.0)
    );
    assert!(!p.advance(1.0), "idle again");
}

#[test]
fn spin_up_turns_status_on_then_raises_pg_to_the_setpoint() {
    let mut p = ramped(10.0);
    put(&mut p, "active", 0.0);
    run(&mut p, 10.0);
    put(&mut p, "active", 1.0);
    assert_eq!(
        (
            tag(&p, "active"),
            tag(&p, "mw"),
            tag(&p, "ramp_state"),
            tag(&p, "ramp_target")
        ),
        (1.0, 0.0, 1.0, 40.0)
    );
    run(&mut p, 3.0);
    assert_eq!(tag(&p, "mw"), 30.0);
    run(&mut p, 3.0);
    assert_eq!((tag(&p, "mw"), tag(&p, "ramp_state")), (40.0, 0.0));
}

#[test]
fn a_command_mid_ramp_retargets_from_the_current_pg() {
    let mut p = ramped(10.0);
    put(&mut p, "active", 0.0);
    run(&mut p, 1.0); // 30
    put(&mut p, "active", 1.0); // changed our mind: back up from 30, not from 0
    assert_eq!(
        (tag(&p, "mw"), tag(&p, "ramp_state"), tag(&p, "active")),
        (30.0, 1.0, 1.0)
    );
    run(&mut p, 0.5);
    assert_eq!(tag(&p, "mw"), 35.0);
    put(&mut p, "active", 1.0); // same direction again: no restart
    run(&mut p, 0.25);
    assert_eq!(tag(&p, "mw"), 37.5);
    put(&mut p, "active", 0.0);
    run(&mut p, 0.5);
    assert_eq!(tag(&p, "mw"), 32.5);
    put(&mut p, "mw", 20.0); // a new setpoint while ramping down: keep the setpoint for the next spin-up
    run(&mut p, 1.0);
    put(&mut p, "active", 1.0);
    assert_eq!(tag(&p, "ramp_target"), 20.0);
    run(&mut p, 1.0);
    assert_eq!(tag(&p, "mw"), 20.0);
}

#[test]
fn a_new_setpoint_while_on_ramps_to_it_in_either_direction() {
    let mut p = ramped(10.0);
    put(&mut p, "mw", 60.0);
    assert_eq!(
        (tag(&p, "mw"), tag(&p, "ramp_state"), tag(&p, "ramp_target")),
        (40.0, 1.0, 60.0)
    );
    run(&mut p, 1.0);
    assert_eq!(tag(&p, "mw"), 50.0);
    put(&mut p, "mw", 45.0);
    run(&mut p, 1.0);
    assert_eq!((tag(&p, "mw"), tag(&p, "ramp_state")), (45.0, 0.0));
    put(&mut p, "mw", 45.0); // at the setpoint already: nothing to do
    assert_eq!(tag(&p, "ramp_state"), 0.0);
    put(&mut p, "active", 0.0);
    put(&mut p, "active", 0.0);
    run(&mut p, 1.0);
    assert_eq!(tag(&p, "mw"), 35.0);
}

#[test]
fn off_and_idle_commands_are_noops_or_instant() {
    let mut p = ramped(10.0);
    put(&mut p, "active", 1.0); // already on at its setpoint
    assert_eq!(tag(&p, "ramp_state"), 0.0);
    put(&mut p, "active", 0.0);
    run(&mut p, 10.0);
    put(&mut p, "active", 0.0); // already off
    assert_eq!((tag(&p, "ramp_state"), tag(&p, "active")), (0.0, 0.0));
    put(&mut p, "mw", 25.0); // setpoint while off: stored, not applied
    assert_eq!(
        (tag(&p, "mw"), tag(&p, "ramp_state"), tag(&p, "active")),
        (0.0, 0.0, 0.0)
    );
    put(&mut p, "active", 1.0);
    assert_eq!(tag(&p, "ramp_target"), 25.0);
}

#[test]
fn per_gen_rate_overrides_the_global_one_and_rate_zero_is_instant() {
    let mut p = Power::new(case30(), Islands::Fail, false, &[], &[]).unwrap();
    p.enable_ramp(
        Some(10.0),
        &[(G.into(), 20.0), ("generator-1_bus-22".into(), 0.0)],
    )
    .unwrap();
    put(&mut p, "active", 0.0);
    run(&mut p, 1.0);
    assert_eq!(tag(&p, "mw"), 20.0);
    let j = p.tag_index("generator-1_bus-22.active").unwrap();
    let mw = p.tag_index("generator-1_bus-22.mw").unwrap();
    let before = p.get(mw);
    p.set(j, 0.0);
    assert_eq!((p.get(j), p.get(mw)), (0.0, before), "rate 0 flips at once");
    p.set(j, 1.0);
    assert_eq!((p.get(j), p.get(mw)), (1.0, before));
}

#[test]
fn bad_ramp_config_is_rejected() {
    let mut p = Power::new(case30(), Islands::Fail, false, &[], &[]).unwrap();
    assert!(p.enable_ramp(Some(-1.0), &[]).unwrap_err().contains("rate"));
    assert!(p
        .enable_ramp(None, &[("generator-9_bus-9".into(), 1.0)])
        .unwrap_err()
        .contains("generator-9_bus-9"));
    assert!(p.enable_ramp(Some(f64::NAN), &[]).is_err());
}

#[test]
fn the_solver_keeps_the_ramped_pg_and_the_slack_picks_up_the_difference() {
    let mut p = ramped(10.0);
    let slack0 = p.case.gen[0][PG];
    put(&mut p, "active", 0.0);
    run(&mut p, 2.0);
    assert_eq!(tag(&p, "mw"), 20.0);
    assert!(
        p.case.gen[0][PG] > slack0 + 15.0,
        "slack covers the 20 MW the ramp took away"
    );
    run(&mut p, 2.0);
    let p0 = p.case.gen[0][PG];
    assert!(p0 > slack0 + 35.0 && !p.alert);
}

#[test]
fn the_model_steps_the_ramp_with_dt_and_rejects_a_bad_ramp_block() {
    use ferrofire::model::{Model, Value};
    let y = |ramp: &str| {
        format!("version: 1\ndt: 0.5\npower:\n{ramp}  inline:\n    buses: [{{id: 1, type: slack}}, {{id: 2, type: pv}}, {{id: 3, type: pq, pd: 30}}]\n    branches: [{{from: 1, to: 2, r: 0.01, x: 0.05}}, {{from: 2, to: 3, r: 0.01, x: 0.05}}]\n    gens: [{{bus: 1, qmax: 50, qmin: -50, pmax: 80}}, {{bus: 2, pg: 20, vg: 1.0, qmax: 50, qmin: -50, pmax: 80}}]\n")
    };
    assert!(
        Model::load(&y("  ramp: {rate: 5, gens: {generator-9_bus-9: 1}}\n"))
            .err()
            .unwrap()
            .contains("generator-9_bus-9")
    );
    let mut m = Model::load(&y("  ramp: {rate: 4}\n")).unwrap();
    let get = |m: &Model, t: &str| x_of(m.get(t).unwrap());
    fn x_of(v: Value) -> f64 {
        match v {
            Value::Analog(x) => x,
            Value::Binary(b) => b as u8 as f64,
        }
    }
    assert_eq!(get(&m, "generator-1_bus-2.mw"), 20.0);
    m.apply_writes(&[("generator-1_bus-2.active".into(), Value::Binary(false))]);
    m.step(0.5);
    m.step(0.5);
    assert_eq!(get(&m, "generator-1_bus-2.mw"), 16.0);
    assert_eq!(get(&m, "generator-1_bus-2.ramp_state"), 2.0);
    for _ in 0..10 {
        m.step(0.5);
    }
    assert_eq!(
        (
            get(&m, "generator-1_bus-2.mw"),
            get(&m, "generator-1_bus-2.active")
        ),
        (0.0, 0.0)
    );
}
