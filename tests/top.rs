//! `ferrofire top --once` against the real binary: a local `ferrofire run` over its control socket, the datagram
//! stream, and a replayed `probe record` CSV.
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");

struct Run {
    child: Child,
    server: String,
    dir: std::path::PathBuf,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn(tag: &str, publish: &str) -> Run {
    let server = format!("tcp://127.0.0.1:{}", free_port());
    let dir = std::env::temp_dir().join(format!("ff-top-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let child = Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--server",
            &server,
            "--publish",
            publish,
            "--rate",
            "0.05",
        ])
        .env("FERROFIRE_RUN_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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

fn top(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("top")
        .args(args)
        .env("FERROFIRE_RUN_DIR", dir)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_running_process_streams_its_tags_over_the_control_socket() {
    let r = spawn("local", "udp://*;127.0.0.1:45998");
    let text = stdout(&top(&r.dir, &["--once"]));
    let line = text
        .lines()
        .find(|l| l.starts_with("lowerwater.height="))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.ends_with("s ok"), "{line}");
    assert!(
        text.lines().count() > 5,
        "every published tag is in the table: {text}"
    );
    let json = stdout(&top(
        &r.dir,
        &["--once", "--json", "--filter", "lowerwater.*"],
    ));
    assert!(
        json.lines()
            .all(|l| l.starts_with("{\"tag\":\"lowerwater.")),
        "{json}"
    );
    assert!(json.contains("\"stale\":false"), "{json}");
    let none = top(&r.dir, &["--once", "--pid", "1"]);
    assert!(
        !none.status.success()
            && String::from_utf8_lossy(&none.stderr).contains("no running ferrofire process")
    );
}

#[test]
fn a_datagram_stream_fills_the_table_from_any_host() {
    let listen = format!("udp://239.0.0.8:{}", free_port());
    let r = spawn("stream", &listen);
    let out = top(
        &r.dir,
        &[
            "--stream",
            &listen,
            "--server",
            &r.server,
            "--tags",
            "lowerwater.height",
            "--once",
            "--json",
        ],
    );
    let text = stdout(&out);
    assert!(
        text.starts_with("{\"tag\":\"lowerwater.height\",\"value\":"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 1, "only the tag asked for");
}

#[test]
fn a_recorded_csv_replays_to_its_last_state() {
    let f = std::env::temp_dir().join(format!("ff-top-replay-{}.csv", std::process::id()));
    std::fs::write(
        &f,
        "cycle,t,a,a_stale,b,b_stale\n1,0.000,1,0,5,0\n2,5.000,2,0,5,1\n",
    )
    .unwrap();
    let path = f.to_str().unwrap();
    let text = stdout(&top(
        std::path::Path::new("/nonexistent"),
        &["--replay", path, "--once"],
    ));
    assert_eq!(text, "a=2 age=0.0s ok\nb=5 age=5.0s STALE\n");
    let bad = top(
        std::path::Path::new("/nonexistent"),
        &["--replay", "/no/such.csv", "--once"],
    );
    assert!(String::from_utf8_lossy(&bad.stderr).contains("/no/such.csv"));
}

#[test]
fn bad_option_combinations_are_refused() {
    let d = std::path::Path::new("/nonexistent");
    for args in [
        &["--json"][..],
        &["--stream", "udp://239.0.0.1:1", "--pid", "3"],
        &["--speed", "0", "--once"],
    ] {
        assert!(!top(d, args).status.success(), "{args:?}");
    }
}

/// The real screen on a pseudo-terminal: it draws the replayed tags, and `q` ends it cleanly.
#[test]
fn the_screen_draws_on_a_terminal_and_q_quits() {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, OwnedFd};
    let (mut master, slave) = unsafe {
        let (mut m, mut s) = (0, 0);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size
            ),
            0
        );
        (std::fs::File::from_raw_fd(m), OwnedFd::from_raw_fd(s))
    };
    let f = std::env::temp_dir().join(format!("ff-top-pty-{}.csv", std::process::id()));
    std::fs::write(&f, "cycle,t,pty.tag,pty.tag_stale\n1,0.000,42,0\n").unwrap();
    let io = |fd: &OwnedFd| Stdio::from(fd.try_clone().unwrap());
    let mut child = Command::new(BIN)
        .args(["top", "--replay", f.to_str().unwrap()])
        .stdin(io(&slave))
        .stdout(io(&slave))
        .stderr(io(&slave))
        .spawn()
        .unwrap();
    drop(slave);
    let (mut seen, mut buf) = (String::new(), [0u8; 4096]);
    while !seen.contains("finished]") {
        // the terminal gets cursor moves between words
        let mut p = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&master),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut p, 1, 10_000) },
            1,
            "screen stalled: {seen}"
        );
        let n = master.read(&mut buf).unwrap();
        seen += &String::from_utf8_lossy(&buf[..n]);
    }
    assert!(
        seen.contains("pty.tag") && seen.contains("42") && seen.contains("[replay"),
        "{seen}"
    );
    master.write_all(b"q").unwrap();
    assert!(child.wait().unwrap().success());
}
