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
fn run_syntax_errors_are_clap_errors_with_exit_2() {
    for (args, want) in [
        (vec!["run", MODEL, "--rate", "x"], "--rate"),
        (vec!["run", MODEL, "--bogus"], "--bogus"),
        (vec!["run", MODEL, "--server"], "--server"),
    ] {
        let o = ff(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        let e = text(&o.stderr);
        assert!(e.contains(want) && e.contains("--help"), "{args:?}: {e}");
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
        assert!(text(&o.stderr).contains("--help"));
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

#[test]
fn a_write_due_exactly_at_the_tolerance_applies_once_in_the_first_step() {
    // the epsilon is 1e-9 s: a write stamped @0.000000001 is due at t = 0, and is not re-applied afterwards
    let o = ff(&[
        "check",
        MODEL,
        "--steps",
        "1",
        "--write",
        "lowervalve.open=1@0.000000001",
    ]);
    assert!(
        text(&o.stdout).contains("lowervalve.open=1.000000"),
        "{}",
        text(&o.stdout)
    );
    let o = ff(&[
        "check",
        MODEL,
        "--steps",
        "3",
        "--write",
        "zz=1@0.000000001",
    ]);
    assert_eq!(text(&o.stderr).matches("tag zz ignored").count(), 1);
}

/// Datagram body of the next packet on `rx` as `tag:value` strings, trailer included.
fn next_pairs(rx: &std::net::UdpSocket) -> Vec<String> {
    let mut buf = [0u8; 2000];
    let n = rx.recv(&mut buf).unwrap();
    let body = unframe(&buf[..n]).unwrap().1;
    text(&body[..body.len() - 1])
        .split(',')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

#[test]
fn run_publishes_per_rtu_slices_to_their_own_endpoints_with_a_counter_each() {
    let rx = |_| {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s
    };
    let (a, b) = (rx(0), rx(1));
    let ep =
        |s: &std::net::UdpSocket| format!("udp://127.0.0.1:{}", s.local_addr().unwrap().port());
    let xml = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/conformance/rtu-1.xml");
    let child = Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--server",
            &format!("tcp://127.0.0.1:{}", free_port()),
        ])
        .args([
            "--publish-tags",
            &format!("{}=lock.height,lowervalve.open", ep(&a)),
        ])
        .args(["--publish-tags", &format!("{}=@{xml}", ep(&b))])
        .args(["--rate", "0.05"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let tag = |p: &str| p.split(':').next().unwrap().to_string();
    let (a1, a2, b1) = (next_pairs(&a), next_pairs(&a), next_pairs(&b));
    assert_eq!(
        a1.iter().map(|p| tag(p)).collect::<Vec<_>>(),
        [
            "lock.height",
            "lowervalve.open",
            "_ff.seq",
            "_ff.cyc",
            "_ff.t"
        ]
    );
    assert_eq!(
        b1.iter().map(|p| tag(p)).collect::<Vec<_>>(),
        [
            "lock.height",
            "lowergate.open",
            "_ff.seq",
            "_ff.cyc",
            "_ff.t"
        ]
    ); // overlaps a on lock.height
    let seq = |v: &[String]| {
        v[2].strip_prefix("_ff.seq:")
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    assert_eq!(seq(&a2), seq(&a1) + 1); // one datagram per cycle here: per-endpoint counter
    assert!(terminate(child).0);
    // an unknown tag in a slice stops startup
    let o = ff(&["run", MODEL, "--publish-tags", "udp://127.0.0.1:41990=nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("unknown tags: nope"));
}

#[test]
fn every_subcommand_has_help_and_typos_get_a_suggestion() {
    for sub in ["run", "pull-bridge", "pypower", "check", "scpi"] {
        let o = ff(&[sub, "--help"]);
        assert!(o.status.success(), "{sub}");
        assert!(
            text(&o.stdout).contains(&format!("Usage: ferrofire {sub}")),
            "{sub}"
        );
    }
    let o = ff(&["--help"]);
    assert!(text(&o.stdout).contains("completions"));
    let o = ff(&["pul-bridge"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).contains("pull-bridge"));
    let o = ff(&["run", MODEL, "--sever", "x"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).contains("--server"));
}

#[test]
fn completions_are_printed_for_each_shell() {
    for shell in ["bash", "zsh", "fish"] {
        let o = ff(&["completions", shell]);
        assert!(o.status.success(), "{shell}");
        let out = text(&o.stdout);
        assert!(
            out.contains("pull-bridge") && out.contains("completions"),
            "{shell}"
        );
    }
    assert_eq!(ff(&["completions", "nosh"]).status.code(), Some(2));
}
