//! `ferrofire doctor`: the diagnosis on hand-made observations, and the real binary against a running `ferrofire run`.
use ferrofire::bennu::{datagrams_msg, group_for};
use ferrofire::doctor::*;
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");
const LISTEN: &str = "udp://239.0.0.1:40000";

fn tags() -> Vec<String> {
    vec!["a".into(), "b".into()]
}

fn dg(group: &str, msg: &str, seq: u64, cyc: u64, t: u64) -> Vec<u8> {
    datagrams_msg(group, msg, seq, cyc, t).remove(0)
}

fn diagnose_with(
    heard: &Heard,
    tags: Result<usize, String>,
    joined: Result<(), String>,
) -> Vec<Check> {
    diagnose(&Obs {
        tags,
        listen: LISTEN,
        joined,
        seconds: 3.0,
        heard,
    })
}

fn find<'a>(cs: &'a [Check], name: &str) -> Option<&'a Check> {
    cs.iter().find(|c| c.name == name)
}

fn status(cs: &[Check], name: &str) -> Option<Status> {
    find(cs, name).map(|c| c.status)
}

#[test]
fn healthy_range_is_all_ok() {
    let g = group_for(LISTEN);
    let mut h = Heard::new(&g, tags());
    h.feed(&dg(&g, "a:1,b:2,", 1, 1, 1_000), 1_000 + 2_000_000);
    h.feed(&dg(&g, "a:1,b:2,", 2, 2, 2_000), 2_000 + 2_000_000);
    let cs = diagnose_with(&h, Ok(2), Ok(()));
    assert!(cs.iter().all(|c| c.status == Status::Ok), "{cs:?}");
    assert!(find(&cs, "group").unwrap().detail.contains(&g));
    assert!(find(&cs, "datagrams")
        .unwrap()
        .detail
        .starts_with("2 in 3s"));
    assert_eq!(find(&cs, "clock skew").unwrap().detail, "2.0 ms");
    assert_eq!(cs[0].text(), "ok    provider: REP answers QUERY: 2 tags");
}

#[test]
fn provider_down_and_no_datagrams_each_say_what_to_fix() {
    let h = Heard::new(&group_for(LISTEN), tags());
    let cs = diagnose_with(&h, Err("timed out".into()), Ok(()));
    assert_eq!(status(&cs, "provider"), Some(Status::Fail));
    assert_eq!(status(&cs, "datagrams"), Some(Status::Fail));
    assert!(find(&cs, "provider").unwrap().fix.contains("--server"));
    assert!(find(&cs, "datagrams")
        .unwrap()
        .fix
        .contains("same endpoint string"));
    assert!(find(&cs, "provider")
        .unwrap()
        .text()
        .contains("\n      fix: "));
    assert_eq!(find(&cs, "seq gaps"), None); // nothing arrived: nothing more to judge
}

#[test]
fn a_failed_join_stops_at_the_join() {
    let h = Heard::new(&group_for(LISTEN), tags());
    let cs = diagnose_with(&h, Ok(2), Err("no such device".into()));
    assert_eq!(status(&cs, "multicast join"), Some(Status::Fail));
    assert_eq!(find(&cs, "datagrams"), None);
}

#[test]
fn gaps_bystanders_skew_and_stale_are_warnings() {
    let g = group_for(LISTEN);
    let mut h = Heard::new(&g, tags());
    let t = 5_000_000_000u64;
    h.feed(&dg(&g, "a:1,b:2,", 1, 1, t), t as i64 + 5_000_000_000); // 5 s behind
    h.feed(&dg(&g, "a:1,", 4, 2, t), t as i64 + 5_000_000_000); // seq 2, 3 missing
    h.feed(&dg(&g, "a:1,", 5, 3, t), t as i64 + 5_000_000_000);
    h.feed(&dg(&g, "a:1,", 6, 4, t), t as i64 + 5_000_000_000); // b last seen 3 cycles ago
    h.feed(&dg("other", "a:1,", 1, 1, t), 0);
    h.feed(b"\x02", 0);
    let cs = diagnose_with(&h, Ok(2), Ok(()));
    for name in ["seq gaps", "bystanders", "clock skew", "stale tags"] {
        let c = find(&cs, name).unwrap();
        assert_eq!((name, c.status), (name, Status::Warn), "{c:?}");
        assert!(!c.fix.is_empty());
    }
    assert!(find(&cs, "seq gaps").unwrap().detail.starts_with("2 "));
    assert!(find(&cs, "bystanders").unwrap().detail.starts_with("1 "));
    assert!(find(&cs, "clock skew")
        .unwrap()
        .detail
        .starts_with("5000 ms"));
    assert_eq!(find(&cs, "stale tags").unwrap().detail, "1: b");
    assert_eq!(h.unusable, 1);
}

#[test]
fn other_groups_only_is_a_fail_naming_them() {
    let mut h = Heard::new(&group_for(LISTEN), tags());
    h.feed(&dg("other", "a:1,", 1, 1, 1), 0);
    let cs = diagnose_with(&h, Ok(2), Ok(()));
    assert!(find(&cs, "datagrams")
        .unwrap()
        .detail
        .contains("1 for other groups"));
}

#[test]
fn json_lines() {
    let c = Check {
        name: "x",
        status: Status::Warn,
        detail: "a\"b".into(),
        fix: "do it",
    };
    assert_eq!(
        c.json(),
        r#"{"check":"x","status":"warn","detail":"a\"b","fix":"do it"}"#
    );
    let ok = Check {
        name: "x",
        status: Status::Ok,
        detail: "fine".into(),
        fix: "",
    };
    assert_eq!(
        ok.json(),
        r#"{"check":"x","status":"ok","detail":"fine","fix":null}"#
    );
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn doctor(server: &str, listen: &str, extra: &[&str]) -> (bool, String) {
    let o = Command::new(BIN)
        .args([
            "doctor",
            "--server",
            server,
            "--listen",
            listen,
            "--seconds",
            "1.5",
            "--timeout-ms",
            "500",
        ])
        .args(extra)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
    )
}

#[test]
fn doctor_against_a_running_provider_and_against_nothing() {
    let listen = format!("udp://239.0.0.8:{}", free_port());
    let ep = format!("tcp://127.0.0.1:{}", free_port());
    let _srv = Server(
        Command::new(BIN)
            .args([
                "run",
                MODEL,
                "--server",
                &ep,
                "--publish",
                &listen,
                "--rate",
                "0.05",
            ])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (ok, out) = doctor(&ep, &listen, &[]);
    assert!(
        ok && out.contains("ok    provider: REP answers QUERY: 15 tags"),
        "{out}"
    );
    assert!(out.contains("ok    datagrams: "), "{out}");
    let (_, json) = doctor(&ep, &listen, &["--json"]);
    assert!(
        json.starts_with("{\"check\":\"provider\",\"status\":\"ok\""),
        "{json}"
    );

    // nothing listening on either endpoint: both fail, exit 1, a fix each
    let dead = format!("tcp://127.0.0.1:{}", free_port());
    let quiet = format!("udp://239.0.0.9:{}", free_port());
    let (ok, out) = doctor(&dead, &quiet, &[]);
    assert!(
        !ok && out.contains("FAIL  provider") && out.contains("FAIL  datagrams"),
        "{out}"
    );
    assert_eq!(out.matches("fix: ").count(), 2, "{out}");
    let (ok, out) = doctor(&dead, "tcp://nope", &[]);
    assert!(!ok && out.contains("FAIL  multicast join"), "{out}");
}
