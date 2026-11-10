//! `ferrofire log` against a real `ferrofire run`: toggle, idempotency, the printed destination and discovery.
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");

struct Run {
    child: Child,
    server: String,
    dir: std::path::PathBuf,
}

fn spawn(tag: &str) -> Run {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = format!("tcp://127.0.0.1:{port}");
    let dir = std::env::temp_dir().join(format!("ff-logit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let child = Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--server",
            &server,
            "--publish",
            "udp://*;127.0.0.1:45999",
        ])
        .env("FERROFIRE_RUN_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let r = Run { child, server, dir };
    for _ in 0..100 {
        if std::fs::read_dir(&r.dir).is_ok_and(|d| d.count() > 0) {
            return r;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no control socket appeared in {:?}", r.dir);
}

impl Run {
    fn log(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("log")
            .args(args)
            .env("FERROFIRE_RUN_DIR", &self.dir)
            .output()
            .unwrap()
    }
    fn out(&self, args: &[&str]) -> String {
        let o = self.log(args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }
    /// A rejected WRITE makes the process log one `Warn:` line.
    fn warn(&self, tag: &str) {
        let s = zmq::Context::new().socket(zmq::REQ).unwrap();
        s.set_rcvtimeo(5000).unwrap();
        s.connect(&self.server).unwrap();
        s.send(&format!("WRITE={tag}:1"), 0).unwrap();
        assert!(s.recv_bytes(0).unwrap().starts_with(b"ERR="));
    }
    fn finish(mut self) -> String {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        self.child.wait().unwrap();
        let mut err = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        err
    }
}

#[test]
fn on_off_status_are_idempotent_and_say_where() {
    let r = spawn("idem");
    let f = r.dir.join("x.log");
    let file = f.to_str().unwrap();
    let pid = r.child.id();
    assert!(r.out(&["status"]).contains(&format!(
        "ferrofire run (pid {pid}): logging to stderr, level info"
    )));
    for i in 0..3 {
        let o = r.out(&["on", "--file", file]);
        let first = o.starts_with(&format!("logging to {file}\n"));
        assert_eq!(first, i == 0, "{o}");
        assert!(
            o.contains(&format!("(pid {pid}): logging to {file}, level info")),
            "{o}"
        );
    }
    r.warn("one.nope");
    assert_eq!(
        std::fs::read_to_string(&f)
            .unwrap()
            .matches("rejected WRITE")
            .count(),
        1
    );
    assert!(r
        .out(&["off"])
        .starts_with(&format!("stopped logging to {file}")));
    assert!(r.out(&["off"]).starts_with("already off"));
    assert!(r
        .out(&["level", "debug", "--for", "10m"])
        .contains("level debug, reverts to info in"));
    let sock = r.dir.join(format!("run-{pid}.sock"));
    assert_eq!(
        std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let err = r.finish();
    assert!(err.contains("log control socket"), "{err}");
}

#[test]
fn a_relative_file_resolves_against_the_clients_directory_and_no_process_is_an_error() {
    let a = spawn("rel");
    let o = Command::new(BIN)
        .args([
            "log",
            "--pid",
            &a.child.id().to_string(),
            "on",
            "--file",
            "rel.log",
        ])
        .env("FERROFIRE_RUN_DIR", &a.dir)
        .current_dir(&a.dir)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        out.contains(&format!("logging to {}/rel.log", a.dir.display())),
        "{out}"
    );
    // no socket anywhere: a clear error
    let bad = Command::new(BIN)
        .args(["log", "status"])
        .env("FERROFIRE_RUN_DIR", "/nonexistent")
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("no running ferrofire process"));
    a.finish();
}

#[test]
fn tail_streams_live_lines_with_no_file() {
    let r = spawn("tail");
    let mut t = Command::new(BIN)
        .args(["log", "tail"])
        .env("FERROFIRE_RUN_DIR", &r.dir)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(t.stdout.take().unwrap()).lines();
    assert!(lines
        .next()
        .unwrap()
        .unwrap()
        .starts_with("tailing at level info"));
    assert!(lines.next().unwrap().unwrap().contains("logging to stderr"));
    r.warn("tail.nope");
    assert!(lines
        .next()
        .unwrap()
        .unwrap()
        .contains("Warn: rejected WRITE"));
    t.kill().unwrap();
    t.wait().unwrap();
    r.finish();
}
