//! pybennu-power-solver drop-in (spec 5(b), 6.5): request/reply forms, noise rules, and the daemon verbs end to end.
//! Uses target/golden/case9.py (from `make golden`); the daemon test writes /var/run + /var/log, so run it in the container.
use rustbelt::bennu::{group_for, unframe};
use rustbelt::model::Kind;
use rustbelt::power::{case, Islands, Power};
use rustbelt::pypower::*;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

fn case9() -> std::path::PathBuf { std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/golden/case9.py") }
fn power() -> Mutex<Power> { Mutex::new(Power::new(case::load_file(&case9()).unwrap(), Islands::Fail, false, &[], &[]).unwrap()) }
fn ask(p: &Mutex<Power>, r: &str) -> String { let b = handle(r.as_bytes(), p, false); assert_eq!(*b.last().unwrap(), 0); String::from_utf8(b[..b.len() - 1].to_vec()).unwrap() }
fn val(p: &Mutex<Power>, tag: &str) -> f64 { ask(p, &format!("READ={tag}")).rsplit(':').next().unwrap().parse().unwrap() }

#[test]
fn ini_parser() {
    let i = parse_ini("# c\n[power-solver-service]\nsolver-type = PyPower\nPublish-Endpoint: udp://*;239.0.0.1:40000\n; x\ndebug=false\n[other]\nk=v\n");
    let s = &i["power-solver-service"];
    assert_eq!((s["solver-type"].as_str(), s["publish-endpoint"].as_str(), s["debug"].as_str()), ("PyPower", "udp://*;239.0.0.1:40000", "false"));
    assert_eq!(i["other"]["k"], "v");
}

#[test]
fn value_forms() {
    assert_eq!(fmt_py("bus-1.base_kv", Kind::Analog, 345.0), "345.0");
    assert_eq!(fmt_py("branch-1_1-4.source", Kind::Analog, 1.0), "1");
    assert_eq!(fmt_py("bus-1.voltage", Kind::Analog, 1.04), "1.04");
    assert_eq!(fmt_py("bus-1.active", Kind::Binary, 1.0), "true");
    for (t, k, n) in [("bus-1.base_kv", Kind::Analog, false), ("bus-1.voltage", Kind::Analog, true), ("bus-1.active", Kind::Binary, false), ("load-1_bus-5.bus", Kind::Analog, false), ("load-1_bus-5.mw", Kind::Analog, true),
        ("generator-1_bus-1.mw", Kind::Analog, true), ("generator-1_bus-1.mw_max", Kind::Analog, false), ("generator-1_bus-1.voltage", Kind::Analog, true), ("branch-1_1-4.resistance", Kind::Analog, false)] {
        assert_eq!(noisy(t, k), n, "{t}");
    }
}

#[test]
fn query_read_write_match_pybennu_forms() {
    let p = power();
    let q = ask(&p, "QUERY=");
    let tags: Vec<_> = q.strip_prefix("ACK=").unwrap().split(',').filter(|t| !t.is_empty()).collect();
    assert_eq!(tags.len(), 208);
    assert_eq!(tags[0], "bus-alert.active");
    assert_eq!(ask(&p, "READ=bus-alert.active"), "ACK=bus-alert.active:false");
    assert_eq!(ask(&p, "READ=branch-1_1-4.source"), "ACK=branch-1_1-4.source:1");
    assert!(ask(&p, "READ=bus-5.voltage").starts_with("ACK=bus-5.voltage:0.9"));
    assert!(ask(&p, "READ=nope.x").starts_with("ERR="));
    let before = val(&p, "bus-5.voltage");
    // a bad point anywhere in the batch applies nothing
    assert!(ask(&p, "WRITE=branch-1_4-5.active:false,bogus-1.x:1").starts_with("ERR="));
    assert_eq!(ask(&p, "READ=branch-1_4-5.active"), "ACK=branch-1_4-5.active:true");
    assert_eq!(ask(&p, "WRITE=bus-5.voltage:true"), "ERR=Invalid value for tag: bus-5.voltage");
    assert_eq!(ask(&p, "WRITE=bus-5.voltage:abc,bus-5.angle:0").split(':').next().unwrap(), "ERR=Provider failed to process write message with exception");
    assert_eq!(ask(&p, "WRITE=branch-1_4-5.active:false,bus-5.load_mw:80"), "ACK=Success processing PyPower write command");
    assert_eq!(ask(&p, "READ=branch-1_4-5.active"), "ACK=branch-1_4-5.active:false");
    assert!(val(&p, "bus-5.voltage") != before, "re-solved after the write");
    assert_eq!(ask(&p, "FOO=x"), "ERR=Unknown command type 'FOO'");
    assert_eq!(ask(&p, ""), "ERR=Message empty");
}

#[test]
fn published_message_has_pybennu_noise() {
    let p = power();
    let p = p.lock().unwrap();
    let mut rng = Rng::seeded();
    let mut moved = 0;
    for _ in 0..20 {
        for part in pack(&p, &mut rng).split(',').filter(|s| !s.is_empty()) {
            let (t, v) = part.split_once(':').unwrap();
            let i = p.tag_index(t).unwrap();
            let (clean, k) = (p.get(i), p.tag_kind(i));
            if k == Kind::Binary { assert_eq!(v, if clean != 0.0 { "true" } else { "false" }); continue; }
            let x: f64 = v.parse().unwrap();
            if !noisy(t, k) { assert_eq!(x, clean, "{t} must be exact"); }
            else if clean == 0.0 { assert!(x.abs() <= 0.1, "{t}"); }
            else { assert!((x / clean - 1.0).abs() <= 0.0101, "{t}: {x} vs {clean}"); if x != clean { moved += 1; } }
        }
    }
    assert!(moved > 500);
}

fn run(bin: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new(bin).args(args).output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn req(ep: &str, m: &str) -> String {
    let s = zmq::Context::new().socket(zmq::REQ).unwrap();
    s.set_rcvtimeo(5000).unwrap();
    s.connect(ep).unwrap();
    s.send(m, 0).unwrap();
    String::from_utf8(s.recv_bytes(0).unwrap()).unwrap().trim_end_matches('\0').to_string()
}

#[test]
fn daemon_verbs_pidfile_logs_and_feed() {
    let dir = std::env::temp_dir().join(format!("rb-pyp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("pybennu-power-solver"); // the install name selects this mode
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rustbelt"), &bin).unwrap();
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let (pub_ep, rep_ep) = (format!("udp://*;127.0.0.1:{}", rx.local_addr().unwrap().port()), "tcp://127.0.0.1:35655");
    let conf = dir.join("config.ini");
    std::fs::write(&conf, format!("[power-solver-service]\nsolver-type = PyPower\ndebug = false\nserver-endpoint = {rep_ep}\npublish-endpoint = {pub_ep}\ncase-file = {}\n", case9().display())).unwrap();
    let (c, env) = (conf.to_str().unwrap(), format!("rbtest{}", std::process::id()));
    let pidfile = format!("/var/run/bennu-{env}.pid");

    assert!(run(&bin, &["status", "-e", &env]).contains("Daemon is not running"));
    assert!(run(&bin, &["restart", "-c", c, "-e", &env, "-d"]).contains("cannot restart"));
    let bad = dir.join("bad.ini");
    std::fs::write(&bad, "[power-solver-service]\nsolver-type = PowerWorld\n").unwrap();
    assert!(run(&bin, &["start", "-c", bad.to_str().unwrap(), "-e", &env, "-d"]).contains("not supported"));
    assert!(!std::path::Path::new(&pidfile).exists());

    run(&bin, &["start", "-c", c, "-e", &env, "-d"]);
    std::thread::sleep(Duration::from_millis(800));
    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    assert!(run(&bin, &["status", "-e", &env]).contains(&format!("running as PID {pid}")));
    assert!(std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap().starts_with("pybennu-power-"), "ps cax name must match grep pybennu");
    assert!(std::fs::read_to_string(format!("/var/log/bennu-{env}.out")).unwrap().contains("Info: pybennu-power-solver"));
    assert!(run(&bin, &["start", "-c", c, "-e", &env, "-d"]).contains("already"));

    assert_eq!(req(rep_ep, "QUERY=").matches(',').count(), 208);
    assert!(req(rep_ep, "READ=bus-1.slack") == "ACK=bus-1.slack:true");
    let mut buf = [0u8; 2000];
    let n = rx.recv(&mut buf).unwrap();
    let (g, body) = unframe(&buf[..n]).unwrap();
    assert_eq!(g, group_for(&pub_ep.replace("*;", "")));
    assert_eq!(*body.last().unwrap(), 0);
    assert!(std::str::from_utf8(body).unwrap().starts_with("bus-alert.active:false,bus-1.voltage:"));

    // restart replaces the process and keeps serving; the old pid is gone
    assert!(run(&bin, &["restart", "-c", c, "-e", &env]).contains("Restarting daemon"));
    std::thread::sleep(Duration::from_millis(800));
    let pid2: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    assert_ne!(pid, pid2);
    assert_eq!(req(rep_ep, "READ=bus-1.slack"), "ACK=bus-1.slack:true");
    run(&bin, &["stop", "-e", &env]);
    assert!(!std::path::Path::new(&pidfile).exists());
    assert!(run(&bin, &["status", "-e", &env]).contains("Daemon is not running"));
    let _ = std::fs::remove_file(format!("/var/log/bennu-{env}.out"));
    let _ = std::fs::remove_file(format!("/var/log/bennu-{env}.err"));
}
