//! The NNG transport end to end: the real binary, a REQ0 client for QUERY/READ/WRITE and a SUB0 client for updates.
#![cfg(feature = "nng")]
use nng::options::{protocol::pubsub::Subscribe, Options, RecvTimeout};
use nng::{Protocol, Socket};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn client(p: Protocol, ep: &str) -> Socket {
    let s = Socket::new(p).unwrap();
    s.set_opt::<RecvTimeout>(Some(Duration::from_secs(5)))
        .unwrap();
    s.dial_async(ep).unwrap(); // the provider may still be starting
    s
}
fn req(ep: &str, m: &str) -> String {
    let s = client(Protocol::Req0, ep);
    s.send(m.as_bytes()).unwrap();
    let r = s.recv().unwrap();
    String::from_utf8(r.to_vec())
        .unwrap()
        .trim_end_matches('\0')
        .into()
}
fn sub(ep: &str) -> Socket {
    let s = client(Protocol::Sub0, ep);
    s.set_opt::<Subscribe>(vec![]).unwrap();
    s
}
fn update(s: &Socket) -> String {
    String::from_utf8(s.recv().unwrap().to_vec()).unwrap()
}
fn terminate(mut c: Child) -> (bool, String) {
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    let status = c.wait().unwrap();
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    (status.success(), out)
}
fn spawn(args: &[&str]) -> Child {
    Command::new(BIN)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}
fn eps() -> (String, String) {
    (
        format!("tcp://127.0.0.1:{}", free_port()),
        format!("tcp://127.0.0.1:{}", free_port()),
    )
}

#[test]
fn run_over_nng_serves_queries_writes_and_publishes() {
    let (server, publish) = eps();
    let child = spawn(&[
        "run",
        MODEL,
        "--transport",
        "nng",
        "--server",
        &server,
        "--publish",
        &publish,
        "--rate",
        "0.05",
    ]);
    assert_eq!(req(&server, "QUERY=").matches(',').count(), 15);
    assert_eq!(req(&server, "READ=lowervalve.open"), "ACK=0");
    assert_eq!(req(&server, "WRITE=lowervalve.open:1"), "ACK=");
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
    assert!(req(&server, "FROB=1").starts_with("ERR=Unknown command"));

    let feed = sub(&publish);
    let msg = update(&feed);
    assert!(
        msg.starts_with("lowerwater.height:") && msg.ends_with(",\0"),
        "{msg}"
    );
    assert!(update(&feed).contains("lowervalve.open:"));

    let (ok, out) = terminate(child);
    assert!(ok);
    assert!(
        out.contains("transport nng") && out.contains("Info: stopped"),
        "{out}"
    );
}

#[test]
fn pybennu_power_solver_takes_the_transport_from_config_ini() {
    let dir = std::env::temp_dir().join(format!("ff-nngp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("pybennu-power-solver"); // the install name selects this mode
    std::os::unix::fs::symlink(BIN, &bin).unwrap();
    let case = concat!(env!("CARGO_MANIFEST_DIR"), "/target/golden/case9.py");
    let (server, publish) = eps();
    let conf = dir.join("config.ini");
    std::fs::write(&conf, format!("[power-solver-service]\nsolver-type = PyPower\ntransport = nng\nserver-endpoint = {server}\npublish-endpoint = {publish}\ncase-file = {case}\n")).unwrap();
    let child = Command::new(&bin)
        .args(["start", "-c", conf.to_str().unwrap(), "-e", "nngtest"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(req(&server, "QUERY=").matches(',').count(), 208);
    assert_eq!(req(&server, "READ=bus-1.slack"), "ACK=bus-1.slack:true");
    // 208 noisy tags do not fit one message: every message is whole `tag:value,` points, NUL-terminated
    let feed = sub(&publish);
    let (first, second) = (update(&feed), update(&feed));
    assert!(
        first.starts_with("bus-alert.active:false,bus-1.voltage:"),
        "{first}"
    );
    for m in [&first, &second] {
        assert!(m.ends_with(",\0"));
        assert!(m
            .trim_end_matches('\0')
            .split_terminator(',')
            .all(|p| p.contains(':')));
    }
    assert!(first.len() <= 1401);
    let (ok, out) = terminate(child);
    assert!(ok && out.contains("transport nng"), "{out}");
}

#[test]
fn the_model_key_selects_nng_and_the_flag_overrides_it() {
    let dir = std::env::temp_dir().join(format!("ff-nng-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let model = dir.join("m.yaml");
    let yaml = std::fs::read_to_string(MODEL).unwrap();
    std::fs::write(&model, format!("transport: nng\n{yaml}")).unwrap();
    let m = model.to_str().unwrap();

    let (server, publish) = eps();
    let child = spawn(&["run", m, "--server", &server, "--publish", &publish]);
    assert_eq!(req(&server, "QUERY=").matches(',').count(), 15);
    assert!(update(&sub(&publish)).starts_with("lowerwater.height:"));
    let (_, out) = terminate(child);
    assert!(out.contains("transport nng"), "{out}");
}

#[cfg(feature = "zmq")]
#[test]
fn the_flag_overrides_the_model_key() {
    let dir = std::env::temp_dir().join(format!("ff-nngf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let model = dir.join("m.yaml");
    let yaml = std::fs::read_to_string(MODEL).unwrap();
    std::fs::write(&model, format!("transport: nng\n{yaml}")).unwrap();
    let m = model.to_str().unwrap();
    let o = Command::new(BIN)
        .args([
            "run",
            m,
            "--transport",
            "zmq",
            "--publish",
            "udp://*;127.0.0.1:9",
        ])
        .args(["--server", &eps().0])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let (_, out) = terminate(o);
    assert!(out.contains("transport zmq"), "{out}");
}
