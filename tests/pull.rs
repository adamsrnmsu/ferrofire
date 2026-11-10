//! pull-bridge: config tag extraction, argument parsing, one tick with fakes, and the real thing end to end (provider, bridge, subscriber).
use ferrofire::pull::*;
use ferrofire::transport::{Publisher, Requester};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");
const RTU1: &str = include_str!("conformance/rtu-1.xml");

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn tags_come_from_the_input_points_only() {
    // the <output> point (uppervalve.open) and the tag aliases are not subscribed to
    assert_eq!(
        tags_from_config(RTU1),
        Ok(s(&["lock.height", "lowergate.open"]))
    );
    let xml = "<input><binary><id>B0</id><name> a.b </name></binary><analog><name>c</name></analog><analog><name>c</name></analog></input>";
    assert_eq!(tags_from_config(xml), Ok(s(&["c", "a.b"]))); // analog first, then binary; no repeats
    assert!(tags_from_config("<x/>").unwrap_err().contains("no <input>"));
    assert!(
        tags_from_config("<input><analog><id>I0</id></analog></input>")
            .unwrap_err()
            .contains("no <analog>/<binary>")
    );
}

#[test]
fn arguments() {
    let a = PullArgs::parse(&s(&["--publish", "udp://127.0.0.1:40000", "--tags", "x,y"])).unwrap();
    assert_eq!(
        a,
        PullArgs {
            server: "tcp://127.0.0.1:5555".into(),
            transport: None,
            rate_ms: 100,
            tags: s(&["x", "y"]),
            publish: "udp://127.0.0.1:40000".into()
        }
    );
    let rtu = format!("{}/tests/conformance/rtu-1.xml", env!("CARGO_MANIFEST_DIR"));
    let b = PullArgs::parse(&s(&[
        "--publish",
        "p",
        "--tags-from",
        &rtu,
        "--server",
        "tcp://h:1",
        "--transport",
        "nng",
        "--rate",
        "10",
    ]))
    .unwrap();
    assert_eq!(
        (b.tags, b.server, b.transport, b.rate_ms),
        (
            s(&["lock.height", "lowergate.open"]),
            "tcp://h:1".into(),
            Some("nng".into()),
            10
        )
    );
    for (args, err) in [
        (vec!["--tags", "x"], "--publish is required"),
        (vec!["--publish", "p"], "--tags or --tags-from is required"),
        (
            vec!["--publish", "p", "--tags", "x", "--rate", "0"],
            "--rate must be > 0",
        ),
        (
            vec!["--publish", "p", "--tags", "x", "--rate", "z"],
            "--rate:",
        ),
        (
            vec!["--publish", "p", "--tags", "x", "--bogus", "1"],
            "unknown argument `--bogus`",
        ),
        (vec!["--publish"], "--publish needs a value"),
        (
            vec!["--tags-from", "/nonexistent/c.xml"],
            "/nonexistent/c.xml:",
        ),
    ] {
        assert!(
            PullArgs::parse(&s(&args)).unwrap_err().contains(err),
            "{args:?}"
        );
    }
}

#[test]
fn request_and_reply_framing() {
    assert_eq!(request(&s(&["a", "b.c"])), b"READ=a,b.c");
    assert_eq!(update_msg(b"ACK=a:1,b:2,\0"), Ok("a:1,b:2,".into()));
    assert_eq!(update_msg(b"ACK="), Ok(String::new()));
    assert_eq!(
        update_msg(b"ERR=Tag 'x' not found\0"),
        Err("provider said `ERR=Tag 'x' not found`".into())
    );
}

struct Fake(Vec<Result<Vec<u8>, String>>, usize);
impl Requester for Fake {
    fn call(&mut self, _: &[u8]) -> Result<Vec<u8>, String> {
        self.1 += 1;
        self.0.remove(0)
    }
}
#[derive(Default)]
struct Sink(Mutex<Vec<String>>);
impl Publisher for Sink {
    fn publish(&self, msg: &str) {
        self.0.lock().unwrap().push(msg.into());
    }
    fn describe(&self) -> String {
        String::new()
    }
}

#[test]
fn tick_retries_once_and_publishes_nothing_on_failure() {
    let (ok, timeout) = (Ok(b"ACK=a:1,\0".to_vec()), Err("timeout".to_string()));
    let out = Sink::default();
    let mut r = Fake(vec![ok.clone()], 0);
    assert_eq!(tick(&mut r, b"READ=a", &out), Ok(()));
    assert_eq!(r.1, 1);
    let mut r = Fake(vec![timeout.clone(), ok.clone()], 0);
    assert_eq!(tick(&mut r, b"READ=a", &out), Ok(())); // the retry answered
    assert_eq!(r.1, 2);
    assert_eq!(*out.0.lock().unwrap(), ["a:1,", "a:1,"]);
    let mut r = Fake(vec![timeout.clone(), timeout.clone()], 0);
    assert_eq!(tick(&mut r, b"READ=a", &out), Err("timeout".into())); // two tries, then give up
    assert_eq!(r.1, 2);
    let mut r = Fake(vec![Ok(b"ERR=x\0".to_vec())], 0);
    assert!(tick(&mut r, b"READ=a", &out).is_err());
    assert_eq!(r.1, 1); // an ERR is an answer: no retry
    assert_eq!(out.0.lock().unwrap().len(), 2); // nothing published by the failures
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn spawn(args: &[&str]) -> Child {
    Command::new(BIN)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
fn terminate(mut c: Child) -> (bool, String) {
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    let ok = c.wait().unwrap().success();
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    (ok, out)
}

/// Provider on `transport`, bridge pulling `lock.height,lowergate.open` from it, `listen` receives what the bridge publishes.
fn bridge_round_trip(transport: &str, publish: &str, listen: impl FnOnce() -> String) {
    let server = format!("tcp://127.0.0.1:{}", free_port());
    let prov_pub = format!("tcp://127.0.0.1:{}", free_port());
    let prov = spawn(&[
        "run",
        MODEL,
        "--transport",
        transport,
        "--server",
        &server,
        "--publish",
        if transport == "nng" {
            &prov_pub
        } else {
            "udp://127.0.0.1:41999"
        },
    ]);
    let bridge = spawn(&[
        "pull-bridge",
        "--transport",
        transport,
        "--server",
        &server,
        "--publish",
        publish,
        "--tags",
        "lock.height,lowervalve.open",
        "--rate",
        "20",
    ]);
    let got = listen();
    assert!(terminate(bridge).0);
    assert!(terminate(prov).0);
    assert!(
        got.starts_with("lock.height:") && got.contains(",lowervalve.open:") && got.ends_with(','),
        "{got}"
    );
    assert_eq!(got.matches(',').count(), 2, "{got}");
}

#[cfg(feature = "zmq")]
#[test]
fn bridge_republishes_as_radio_datagrams() {
    use ferrofire::bennu::{group_for, unframe};
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let ep = format!("udp://127.0.0.1:{}", sock.local_addr().unwrap().port());
    bridge_round_trip("zmq", &ep, || {
        let mut buf = [0u8; 2048];
        let n = sock.recv(&mut buf).unwrap();
        let (group, body) = unframe(&buf[..n]).unwrap();
        assert_eq!(group, group_for(&ep));
        assert_eq!(*body.last().unwrap(), 0);
        String::from_utf8(body[..body.len() - 1].to_vec()).unwrap()
    });
}

#[cfg(feature = "nng")]
#[test]
fn bridge_republishes_over_nng() {
    use nng::options::{protocol::pubsub::Subscribe, Options, RecvTimeout};
    let ep = format!("tcp://127.0.0.1:{}", free_port());
    let sub = nng::Socket::new(nng::Protocol::Sub0).unwrap();
    sub.set_opt::<Subscribe>(vec![]).unwrap();
    sub.set_opt::<RecvTimeout>(Some(Duration::from_secs(10)))
        .unwrap();
    sub.dial_async(&ep).unwrap();
    bridge_round_trip("nng", &ep, || {
        let m = sub.recv().unwrap();
        String::from_utf8(m[..m.len() - 1].to_vec()).unwrap()
    });
}

#[test]
fn bridge_with_a_bad_command_line_exits_1_with_usage() {
    let o = Command::new(BIN)
        .args(["pull-bridge", "--tags", "x"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(
        e.contains("--publish is required") && e.contains("usage: ferrofire pull-bridge"),
        "{e}"
    );
}
