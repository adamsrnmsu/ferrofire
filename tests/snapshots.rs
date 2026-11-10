//! insta snapshots of the human and `--json` output of probe, doctor, explain and top --once, so a format change is a reviewable diff.
//! (`ferrofire log` snapshots live in tests/log.rs next to the process helper.) Review with `cargo insta review`.
use ferrofire::diff::Finding;
use ferrofire::doctor::{diagnose, Heard, Obs};
use ferrofire::probe::{audit_line, Row};
use ferrofire::top::{once_text, Table};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const PUMP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/pump-station.yaml");

fn rows() -> Vec<Row> {
    let r = |tag: &str, value: &str, live| Row {
        tag: tag.into(),
        value: value.into(),
        live,
    };
    vec![
        r("tank.level", "2.5", None),
        r("pump.run", "true", Some((0, false, 0))),
        r("valve.open", "0.5", Some((4, true, 2))),
        r("note", "x\"y", None),
    ]
}

#[test]
fn probe_rows() {
    let rows = rows();
    let text: Vec<_> = rows.iter().map(Row::text).collect();
    let json: Vec<_> = rows.iter().map(Row::json).collect();
    insta::assert_snapshot!("probe_text", text.join("\n"));
    insta::assert_snapshot!("probe_json", json.join("\n"));
}

#[test]
fn probe_audit_and_diff() {
    let pts = ["a=1".to_string(), "b=2".to_string()];
    insta::assert_snapshot!(audit_line(42, "me@pid7", "tcp://h:1", &pts, "ACK"));
    let f = |tag: &str, what| Finding {
        rtu: "rtu1".into(),
        tag: tag.into(),
        what,
    };
    let fs = [
        f("a", Ok((1.0, 0.5))),
        f("*", Err("connection refused".into())),
    ];
    let text: Vec<_> = fs.iter().map(|f| f.text(12.5)).collect();
    let json: Vec<_> = fs.iter().map(|f| f.json(12.5)).collect();
    insta::assert_snapshot!("diff_text", text.join("\n"));
    insta::assert_snapshot!("diff_json", json.join("\n"));
}

#[test]
fn doctor_text_and_json() {
    let (listen, group) = ("udp://239.0.0.1:40000", "g");
    let heard = Heard::new(group, vec!["a".into(), "b".into()]);
    let obs = |tags, joined| Obs {
        tags,
        listen,
        joined,
        seconds: 3.0,
        heard: &heard,
    };
    for (name, o) in [
        ("silent", obs(Err("timed out".into()), Ok(()))),
        ("no_join", obs(Ok(2), Err("no such device".into()))),
    ] {
        let cs = diagnose(&o);
        let text: Vec<_> = cs.iter().map(|c| c.text()).collect();
        let json: Vec<_> = cs.iter().map(|c| c.json()).collect();
        insta::assert_snapshot!(format!("doctor_{name}_text"), text.join("\n"));
        insta::assert_snapshot!(format!("doctor_{name}_json"), json.join("\n"));
    }
}

#[test]
fn explain_tags() {
    for tag in ["valve.open", "tank.level", "valve.cv", "valve.flow"] {
        let o = Command::new(BIN)
            .args(["explain", PUMP, tag])
            .output()
            .unwrap();
        assert!(o.status.success(), "{tag}");
        insta::assert_snapshot!(
            format!("explain_{}", tag.replace('.', "_")),
            String::from_utf8_lossy(&o.stdout)
        );
    }
}

#[test]
fn top_once_text_and_json() {
    let mut t = Table::new(1.0);
    t.apply("a\"b", 1.5, 0.0);
    t.apply("c", 0.0, 4.0);
    let rows = t.rows(4.0);
    insta::assert_snapshot!("top_once_text", once_text(&rows, false));
    insta::assert_snapshot!("top_once_json", once_text(&rows, true));
}
