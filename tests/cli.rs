//! The `ferrofire` binary as an operator runs it: `run` (REP + UDP RADIO until SIGTERM) and `check` (offline trace).
use ferrofire::bennu::{group_for, unframe};
use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");

fn ff(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}
fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn req(ep: &str, m: &str) -> String {
    let s = zmq::Context::new().socket(zmq::REQ).unwrap();
    s.set_rcvtimeo(5000).unwrap();
    s.connect(ep).unwrap();
    s.send(m, 0).unwrap();
    text(&s.recv_bytes(0).unwrap())
        .trim_end_matches('\0')
        .into()
}
fn terminate(mut c: Child) -> (bool, String) {
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    let status = c.wait().unwrap();
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    (status.success(), out)
}

#[test]
fn run_serves_requests_publishes_and_stops_on_sigterm() {
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let publish = format!("udp://*;127.0.0.1:{}", rx.local_addr().unwrap().port());
    let server = format!("tcp://127.0.0.1:{}", free_port());
    let child = Command::new(BIN)
        .args(["run", MODEL, "--server", &server, "--publish", &publish])
        .args(["--rate", "0.05"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    assert_eq!(req(&server, "QUERY=").matches(',').count(), 15);
    assert_eq!(req(&server, "WRITE=lowervalve.open:1"), "ACK=");
    let mut buf = [0u8; 2000];
    let n = rx.recv(&mut buf).unwrap();
    let (group, body) = unframe(&buf[..n]).unwrap();
    assert_eq!(group, group_for(&publish));
    assert!(text(body).starts_with("lowerwater.height:"));
    let mut open = String::new();
    for _ in 0..50 {
        open = req(&server, "READ=lowervalve.open");
        if open == "ACK=1" {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(open, "ACK=1");
    assert!(req(&server, "WRITE=nope:1").starts_with("ERR="));

    let (ok, out) = terminate(child);
    assert!(ok);
    assert!(out.contains("Info: ferrofire run:") && out.contains("Info: stopped"));
}

#[test]
fn run_rejects_bad_invocations_with_usage() {
    for (args, want) in [
        (vec!["run"], "no model given"),
        (vec!["run", "/nonexistent.yaml"], "/nonexistent.yaml"),
        (
            vec!["run", MODEL, "--rate", "0"],
            "publish rate must be > 0",
        ),
        (vec!["run", MODEL, "--rate", "x"], "--rate:"),
        (vec!["run", MODEL, "--bogus"], "unknown argument `--bogus`"),
        (vec!["run", MODEL, "--server"], "--server needs a value"),
        (
            vec!["run", MODEL, "--publish", "tcp://x:1"],
            "must be udp://",
        ),
    ] {
        let o = ff(&args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let e = text(&o.stderr);
        assert!(
            e.contains(want) && e.contains("usage: ferrofire run"),
            "{args:?}: {e}"
        );
    }
}

#[test]
fn check_prints_the_trace_and_applies_timed_writes() {
    let o = ff(&[
        "check",
        MODEL,
        "--steps",
        "20",
        "--write",
        "lowervalve.open=true@0",
    ]);
    assert!(o.status.success());
    let out = text(&o.stdout);
    assert!(out.starts_with("t=1.000\n"), "{out}");
    assert!(out.contains("lowervalve.open=1.000000") && out.contains("lock.height="));
    // a write at t beyond the run never applies; an unknown tag is reported on stderr
    let o = ff(&[
        "check",
        MODEL,
        "--steps",
        "2",
        "--write",
        "lowervalve.open=1@99",
        "--write",
        "zz=1",
    ]);
    assert!(text(&o.stdout).contains("lowervalve.open=0.000000"));
    assert!(text(&o.stderr).contains("write to unknown/readonly tag zz ignored"));
}

#[test]
fn check_rejects_bad_invocations() {
    for args in [
        vec!["check"],
        vec!["check", MODEL, "--steps", "x"],
        vec!["check", MODEL, "--write", "lock.height"],
        vec!["check", MODEL, "--write", "a=b"],
        vec!["check", MODEL, "--write", "a=1@x"],
        vec!["check", MODEL, "--bogus"],
        vec!["nonsense"],
    ] {
        let o = ff(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(text(&o.stderr).contains("usage: ferrofire run"));
    }
    let o = ff(&["check", "/nonexistent.yaml"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("/nonexistent.yaml"));
    let bad = std::env::temp_dir().join(format!("ff-bad-{}.yaml", std::process::id()));
    std::fs::write(&bad, "version: 2\ndt: 0.1\n").unwrap();
    let o = ff(&["check", bad.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("unsupported version"));
}

#[test]
fn the_install_name_selects_the_simulink_solver() {
    let dir = std::env::temp_dir().join(format!("ff-name-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("simulinksolver");
    let _ = std::fs::remove_file(&bin);
    std::os::unix::fs::symlink(BIN, &bin).unwrap();
    let o = Command::new(&bin).arg("--help").output().unwrap();
    assert!(text(&o.stdout).starts_with("simulinksolver [-tf inf"));
}
