//! Process logging: the default line shape, and level / destination changes while running.
use ferrofire::trace;
use tracing::level_filters::LevelFilter;

#[test]
fn level_and_output_change_at_run_time() {
    assert!(
        !trace::set_level(LevelFilter::DEBUG),
        "no subscriber before init"
    );
    trace::init();
    trace::init(); // idempotent
    let path = std::env::temp_dir().join(format!("ff-trace-{}.log", std::process::id()));
    let open = || {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .unwrap()
    };
    trace::set_output(Some(open()));
    tracing::warn!("a {}", 1);
    tracing::debug!("hidden at info");
    assert!(trace::set_level(LevelFilter::DEBUG));
    tracing::debug!(tag = "t1", "shown");
    assert!(trace::set_level(LevelFilter::ERROR));
    tracing::warn!("hidden at error");
    tracing::error!("e");
    trace::set_output(None);
    tracing::error!("to stderr, not the file");
    let got = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(got, "Warn: a 1\nDebug: shown tag=\"t1\"\nError: e\n");
}

/// Default output unchanged: a refused write is one `Warn: rejected WRITE: ...` line on stderr of `ferrofire run`.
#[test]
fn run_logs_a_refused_write_on_stderr() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let ep = format!("tcp://127.0.0.1:{port}");
    let model = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");
    let mut p = Command::new(env!("CARGO_BIN_EXE_ferrofire"))
        .args([
            "run",
            model,
            "--server",
            &ep,
            "--publish",
            "udp://*;127.0.0.1:9",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let s = zmq::Context::new().socket(zmq::REQ).unwrap();
    s.set_rcvtimeo(5000).unwrap();
    s.connect(&ep).unwrap();
    s.send("WRITE=nope:1\0", 0).unwrap();
    assert!(s.recv_bytes(0).unwrap().starts_with(b"ERR="));
    unsafe { libc::kill(p.id() as i32, libc::SIGTERM) };
    let mut err = String::new();
    p.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    p.wait().unwrap();
    assert!(
        err.lines()
            .any(|l| l == "Warn: rejected WRITE: Tag 'nope' is unknown or read-only"),
        "{err}"
    );
}
