//! pull-bridge: config tag extraction, argument parsing, one tick with fakes, and the real thing end to end (provider, bridge, subscriber).
use ferrofire::pull::*;
use ferrofire::transport::{Publisher, Requester};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
                                                             // a tag in both sections and non-adjacent repeats: first occurrence wins, order kept
    let xml = "<input><analog><name>x</name></analog><analog><name>y</name></analog><analog><name>x</name></analog><binary><name>y</name></binary><binary><name>z</name></binary></input>";
    assert_eq!(tags_from_config(xml), Ok(s(&["x", "y", "z"])));
    let a = parse(&s(&["--publish", "p", "--tags", "b,a,b,c,a"])).unwrap();
    assert_eq!(a.tags, s(&["b", "a", "c"]));
    assert!(tags_from_config("<x/>").unwrap_err().contains("no <input>"));
    assert!(
        tags_from_config("<input><analog><id>I0</id></analog></input>")
            .unwrap_err()
            .contains("no <analog>/<binary>")
    );
}

fn parse(args: &[String]) -> Result<PullArgs, String> {
    use clap::Parser;
    PullCli::try_parse_from(std::iter::once("pull-bridge".to_string()).chain(args.iter().cloned()))
        .map_err(|e| e.to_string())?
        .resolve()
}

#[test]
fn arguments() {
    let a = parse(&s(&["--publish", "udp://127.0.0.1:40000", "--tags", "x,y"])).unwrap();
    assert_eq!(
        a,
        PullArgs {
            server: "tcp://127.0.0.1:5555".into(),
            rate_ms: 100,
            tags: s(&["x", "y"]),
            publish: "udp://127.0.0.1:40000".into()
        }
    );
    let rtu = format!("{}/tests/conformance/rtu-1.xml", env!("CARGO_MANIFEST_DIR"));
    let b = parse(&s(&[
        "--publish",
        "p",
        "--tags-from",
        &rtu,
        "--server",
        "tcp://h:1",
        "--rate",
        "10",
    ]))
    .unwrap();
    assert_eq!(
        (b.tags, b.server, b.rate_ms),
        (
            s(&["lock.height", "lowergate.open"]),
            "tcp://h:1".into(),
            10
        )
    );
    for (args, err) in [
        (vec!["--tags", "x"], "--publish"),
        (vec!["--publish", "p"], "--tags"),
        (
            vec!["--publish", "p", "--tags", "x", "--rate", "0"],
            "--rate",
        ),
        (
            vec!["--publish", "p", "--tags", "x", "--rate", "z"],
            "--rate",
        ),
        (
            vec!["--publish", "p", "--tags", "x", "--bogus", "1"],
            "--bogus",
        ),
        (vec!["--publish"], "--publish"),
        (
            vec!["--publish", "p", "--tags-from", "/nonexistent/c.xml"],
            "/nonexistent/c.xml:",
        ),
    ] {
        assert!(parse(&s(&args)).unwrap_err().contains(err), "{args:?}");
    }
}

#[test]
fn request_and_reply_framing() {
    assert_eq!(requests(&s(&["a", "b.c"])), [b"READ=a,b.c,".to_vec()]);
    assert_eq!(requests(&s(&["a"])), [b"READ=a,".to_vec()]); // one tag stays a batch: `READ=a` would be answered `ACK=value`
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
    assert_eq!(tick(&mut r, &[b"READ=a".to_vec()], &out), Ok(()));
    assert_eq!(r.1, 1);
    let mut r = Fake(vec![timeout.clone(), ok.clone()], 0);
    assert_eq!(tick(&mut r, &[b"READ=a".to_vec()], &out), Ok(())); // the retry answered
    assert_eq!(r.1, 2);
    assert_eq!(*out.0.lock().unwrap(), ["a:1,", "a:1,"]);
    let mut r = Fake(vec![timeout.clone(), timeout.clone()], 0);
    assert_eq!(
        tick(&mut r, &[b"READ=a".to_vec()], &out),
        Err("timeout".into())
    ); // two tries, then give up
    assert_eq!(r.1, 2);
    let mut r = Fake(vec![Ok(b"ERR=x\0".to_vec())], 0);
    assert!(tick(&mut r, &[b"READ=a".to_vec()], &out).is_err());
    assert_eq!(r.1, 1); // an ERR is an answer: no retry
    assert_eq!(out.0.lock().unwrap().len(), 2); // nothing published by the failures
}

#[test]
fn big_tag_lists_split_into_several_reads() {
    let tags: Vec<String> = (0..MAX_TAGS_PER_READ * 2 + 1)
        .map(|i| format!("t{i}"))
        .collect();
    let reqs = requests(&tags);
    assert_eq!(reqs.len(), 3);
    assert!(
        reqs[0].starts_with(b"READ=t0,t1,")
            && reqs[2] == format!("READ=t{},", MAX_TAGS_PER_READ * 2).as_bytes()
    );
    let all: String = reqs
        .iter()
        .map(|r| String::from_utf8_lossy(&r[5..]).to_string())
        .collect();
    assert_eq!(
        all,
        tags.iter().map(|t| format!("{t},")).collect::<String>()
    );
    // one publish per tick, only when every part answered
    let (out, ok) = (Sink::default(), Ok(b"ACK=a:1,\0".to_vec()));
    let mut r = Fake(vec![ok.clone(), ok.clone()], 0);
    let two = [b"READ=a".to_vec(), b"READ=b".to_vec()];
    assert_eq!(tick(&mut r, &two, &out), Ok(()));
    assert_eq!(*out.0.lock().unwrap(), ["a:1,a:1,"]);
    let mut r = Fake(vec![ok, Ok(b"ERR=x\0".to_vec())], 0);
    assert!(tick(&mut r, &two, &out).is_err());
    assert_eq!(out.0.lock().unwrap().len(), 1); // the half answer was not published
}

#[test]
fn startup_validation_names_unknown_tags() {
    let known = || Ok(b"ACK=a,b,c,\0".to_vec());
    assert_eq!(
        validate(&mut Fake(vec![known()], 0), &s(&["c", "a"])),
        Ok(())
    );
    let e = validate(&mut Fake(vec![known()], 0), &s(&["a", "nope", "zip"])).unwrap_err();
    assert!(
        e.contains("nope") && e.contains("zip") && !e.contains("a,"),
        "{e}"
    );
    let e = validate(&mut Fake(vec![Ok(b"ERR=bad\0".to_vec())], 0), &s(&["a"])).unwrap_err();
    assert!(e.contains("ERR=bad"), "{e}");
    // an unreachable provider is retried a few times, then reported
    let mut r = Fake(vec![Err("timeout".into()); STARTUP_TRIES], 0);
    assert!(validate(&mut r, &s(&["a"]))
        .unwrap_err()
        .contains("timeout"));
    assert_eq!(r.1, STARTUP_TRIES);
    let mut r = Fake(vec![Err("timeout".into()), known()], 0);
    assert_eq!(validate(&mut r, &s(&["a"])), Ok(()));
}

#[test]
fn errors_are_logged_on_a_cadence_not_only_on_transition() {
    let (mut o, t) = (Outage::default(), Instant::now());
    let e = || Err::<(), String>("provider said `ERR=Tag 'x' not found`".into());
    let first = o.log_line(e(), t).unwrap().1;
    assert!(first.contains("Tag 'x' not found"), "{first}");
    assert_eq!(o.log_line(e(), t + Duration::from_secs(1)), None);
    assert!(o
        .log_line(e(), t + LOG_EVERY)
        .unwrap()
        .1
        .contains("still failing"));
    assert_eq!(
        o.log_line(e(), t + LOG_EVERY + Duration::from_secs(1)),
        None
    );
    assert!(o
        .log_line(Ok(()), t + LOG_EVERY * 2)
        .unwrap()
        .1
        .contains("recovered"));
    assert_eq!(o.log_line(Ok(()), t + LOG_EVERY * 3), None);
}

#[test]
fn missed_ticks_are_skipped_not_replayed() {
    let (t0, p) = (Instant::now(), Duration::from_millis(100));
    let at = |ms| t0 + Duration::from_millis(ms);
    assert_eq!(next_k(t0, p, at(10), 1), 2); // on time: the next slot
    assert_eq!(next_k(t0, p, at(250), 1), 3); // overran slots 2: jump to the next future one
    assert_eq!(next_k(t0, p, at(300), 1), 4); // exactly on a slot counts as past
    assert_eq!(next_k(t0, p, at(0), 5), 6); // never goes backwards
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

/// Provider, bridge pulling `lock.height,lowergate.open` from it, `listen` receives what the bridge publishes.
fn bridge_round_trip(publish: &str, listen: impl FnOnce() -> String) {
    let server = format!("tcp://127.0.0.1:{}", free_port());
    let prov = spawn(&[
        "run",
        MODEL,
        "--server",
        &server,
        "--publish",
        "udp://127.0.0.1:41999",
    ]);
    let bridge = spawn(&[
        "pull-bridge",
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
    let got = got.split(",_ff.seq:").next().unwrap().to_string() + ","; // the sequence trailer is checked in tests/bennu.rs
    assert!(terminate(bridge).0);
    assert!(terminate(prov).0);
    assert!(
        got.starts_with("lock.height:") && got.contains(",lowervalve.open:") && got.ends_with(','),
        "{got}"
    );
    assert_eq!(got.matches(',').count(), 2, "{got}");
}

#[test]
fn bridge_republishes_as_radio_datagrams() {
    use ferrofire::bennu::{group_for, unframe};
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let ep = format!("udp://127.0.0.1:{}", sock.local_addr().unwrap().port());
    bridge_round_trip(&ep, || {
        let mut buf = [0u8; 2048];
        let n = sock.recv(&mut buf).unwrap();
        let (group, body) = unframe(&buf[..n]).unwrap();
        assert_eq!(group, group_for(&ep));
        assert_eq!(*body.last().unwrap(), 0);
        String::from_utf8(body[..body.len() - 1].to_vec()).unwrap()
    });
}

/// Provider (`ferrofire run`) on `server`; its own publish goes nowhere useful.
fn provider(server: &str) -> Child {
    spawn(&[
        "run",
        MODEL,
        "--server",
        server,
        "--publish",
        "udp://127.0.0.1:41998",
    ])
}

/// Kill the provider under a running bridge, start a new one on the same endpoint: the bridge must go on publishing.
/// `recv(timeout)` reads one message the bridge published, if any arrives in time.
fn restart_recovers(publish: &str, mut recv: impl FnMut(Duration) -> Option<String>) {
    let server = format!("tcp://127.0.0.1:{}", free_port());
    let prov = provider(&server);
    let mut bridge = spawn(&[
        "pull-bridge",
        "--server",
        &server,
        "--publish",
        publish,
        "--tags",
        "lock.height",
        "--rate",
        "20",
    ]);
    // a message may arrive in several datagrams/frames: look for the one that carries the tag
    let seen = |recv: &mut dyn FnMut(Duration) -> Option<String>| {
        (0..40)
            .any(|_| recv(Duration::from_millis(500)).is_some_and(|m| m.contains("lock.height:")))
    };
    assert!(seen(&mut recv), "no first update");
    assert!(terminate(prov).0);
    while recv(Duration::from_millis(500)).is_some() {} // drain; the bridge publishes nothing while the provider is gone
    let prov = provider(&server);
    let got = seen(&mut recv);
    assert!(bridge.try_wait().unwrap().is_none(), "bridge died");
    assert!(terminate(bridge).0);
    assert!(terminate(prov).0);
    assert!(got, "no update after the provider came back");
}

#[test]
fn bridge_recovers_when_the_zmq_provider_restarts() {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let ep = format!("udp://127.0.0.1:{}", sock.local_addr().unwrap().port());
    restart_recovers(&ep, |d| {
        sock.set_read_timeout(Some(d)).unwrap();
        let mut buf = [0u8; 2048];
        let n = sock.recv(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    });
}

#[test]
fn bridge_exits_1_naming_unknown_tags() {
    let server = format!("tcp://127.0.0.1:{}", free_port());
    let prov = provider(&server);
    let publish = "udp://127.0.0.1:41997";
    let o = Command::new(BIN)
        .args([
            "pull-bridge",
            "--server",
            &server,
            "--publish",
            publish,
            "--tags",
            "lock.height,no.such,also.bad",
        ])
        .output()
        .unwrap();
    assert!(terminate(prov).0);
    assert_eq!(o.status.code(), Some(1));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(
        e.contains("no.such") && e.contains("also.bad") && !e.contains("lock.height,"),
        "{e}"
    );
}

#[test]
fn bridge_with_a_bad_command_line_exits_2_with_usage() {
    let o = Command::new(BIN)
        .args(["pull-bridge", "--tags", "x"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(
        e.contains("--publish") && e.contains("Usage: ferrofire pull-bridge"),
        "{e}"
    );
}
