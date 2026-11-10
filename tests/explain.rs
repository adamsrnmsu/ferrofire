//! `ferrofire explain <tag>`: offline description of a tag from the model YAML.
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const PUMP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/pump-station.yaml");

fn explain(model: &str, tag: &str) -> (bool, String) {
    let o = Command::new(BIN)
        .args(["explain", model, tag])
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (o.status.success(), text(&o.stdout) + &text(&o.stderr))
}

#[test]
fn input_shows_type_limits_default_units_and_what_uses_it() {
    let (ok, out) = explain(PUMP, "valve.open");
    assert!(ok, "{out}");
    for want in [
        "valve.open",
        "input",
        "type: analog",
        "default: 0.5",
        "min: 0",
        "max: 1",
        "units: fraction",
        "used by: valve.flow",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

#[test]
fn state_and_output_show_expression_and_inputs() {
    let (ok, out) = explain(PUMP, "tank.level");
    assert!(ok, "{out}");
    for want in [
        "state",
        "ode: d/dt = (pump.flow - valve.flow) / tank.area",
        "init: 2.0",
        "units: m",
        "depends on: pump.flow, tank.area, valve.flow",
        "output",
        "republishes",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
    let (_, out) = explain(PUMP, "valve.flow");
    assert!(out.contains("depends on: tank.level, valve.cv, valve.open"));
    assert!(out.contains("units: m3/s"));
}

#[test]
fn param_shows_its_value() {
    let (ok, out) = explain(PUMP, "valve.cv");
    assert!(ok, "{out}");
    assert!(
        out.contains("param") && out.contains("default: 0.3"),
        "{out}"
    );
}

#[test]
fn power_tags_show_drive_expression_or_the_solver() {
    let (ok, out) = explain(PUMP, "load-1_bus-3.mw");
    assert!(ok, "{out}");
    for want in [
        "power tag (writable)",
        "drive: 1000 * 9.81 * pump.flow",
        "depends on: pump.flow, pump.head",
        "units: MW",
        "used by: load-1_bus-3.mvar",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
    let (_, out) = explain(PUMP, "bus-3.voltage");
    assert!(out.contains("AC power flow"), "{out}");
    assert!(out.contains("units: pu") && out.contains("used by: pump.flow"));
    let (_, out) = explain(PUMP, "branch-1_2-3.active");
    assert!(out.contains("type: binary"), "{out}");
}

#[test]
fn body_fields_show_speed_and_barriers() {
    let (ok, out) = explain(
        concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml"),
        "boat.x",
    );
    assert!(ok, "{out}");
    for want in [
        "body",
        "speed: 5.0",
        "barrier: at 100: lowergate.open",
        "init: 0",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

#[test]
fn unknown_tag_suggests_the_closest_and_fails() {
    let (ok, out) = explain(PUMP, "valve.opn");
    assert!(!ok);
    assert!(
        out.contains("unknown tag `valve.opn`") && out.contains("did you mean `valve.open`"),
        "{out}"
    );
}
