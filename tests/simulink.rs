//! simulinksolver contract (SysV shm, POSIX semaphores, updates FIFO) against the real kernel objects.
//! The objects have fixed system-wide names, so every test here takes `LOCK`.
use ferrofire::simulink::contract::*;
use ferrofire::simulink::ipc::{Fifo, Sem, Shm};
use std::ffi::CString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

static LOCK: Mutex<()> = Mutex::new(());

fn slot_text(shm: &Shm, n: usize) -> String {
    let s = unsafe { std::slice::from_raw_parts(shm.addr.add(n * MAX_MSG_LEN), MAX_MSG_LEN) };
    String::from_utf8_lossy(&s[..s.iter().position(|&b| b == 0).unwrap()]).into_owned()
}

/// The provider's view of a segment the solver created.
fn attach(key: i32) -> Shm {
    Shm::create(key, 0).unwrap()
}

#[test]
fn shm_is_recreated_when_a_stale_segment_has_another_size() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = Shm::create(20111, 64).unwrap();
    a.write(0, b"stale");
    let b = Shm::create(20111, 4096).unwrap();
    b.zero();
    b.write(4090, b"end");
    assert_eq!(unsafe { *b.addr.add(4090) }, b'e');
    assert_eq!(unsafe { *b.addr }, 0);
    b.destroy();
    a.destroy();
}

#[test]
#[should_panic]
fn shm_write_past_the_segment_is_refused() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = Shm::create(20112, 8).unwrap();
    a.write(6, b"abc");
}

#[test]
fn semaphore_starts_locked_and_counts_posts() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let s = Sem::open_locked("ff_test_sem").unwrap();
    s.post();
    s.post();
    s.wait();
    s.wait(); // would block forever if the count were stale or the posts lost
    s.post();
    let again = Sem::open_locked("ff_test_sem").unwrap(); // reopen drains the stale count
    s.destroy();
    again.destroy();
}

#[test]
fn fifo_drains_whole_records() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let f = Fifo::create().unwrap();
    assert!(f.drain().is_empty());
    let path = CString::new(UPDATES_FIFO).unwrap();
    let w = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    assert!(w >= 0);
    for tag in *b"ab" {
        let mut rec = [0u8; MAX_MSG_LEN];
        rec[0] = tag;
        let n = unsafe { libc::write(w, rec.as_ptr().cast(), MAX_MSG_LEN) };
        assert_eq!(n as usize, MAX_MSG_LEN);
    }
    let recs = f.drain();
    assert_eq!(recs.len(), 2);
    assert_eq!((recs[0][0], recs[1][0]), (b'a', b'b'));
    unsafe { libc::close(w) };
    f.destroy();
    assert!(!std::path::Path::new(UPDATES_FIFO).exists());
}

/// Kills the solver if a test fails before it is stopped, so one failure does not poison the next test.
struct Solver(std::process::Child);
impl Drop for Solver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_solver(dir: &std::path::Path) -> Solver {
    Solver(spawn_child(dir))
}

fn spawn_child(dir: &std::path::Path) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_ferrofire"))
        .args(["simulink", "--model"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml"))
        .current_dir(dir)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn points_segment_exists() -> bool {
    unsafe { libc::shmget(PUBLISH_POINTS_SHM_KEY as libc::key_t, 0, 0o666) >= 0 }
}

/// SIGTERM, require a clean exit, return what the solver printed.
fn terminate(mut s: Solver) -> String {
    unsafe { libc::kill(s.0.id() as i32, libc::SIGTERM) };
    let mut out = String::new();
    s.0.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert!(s.0.wait().unwrap().success());
    out
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn solver_publishes_points_applies_updates_and_cleans_up() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("ff-sim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // CRLF and blank lines are tolerated; a point the model does not publish reads 0
    std::fs::write(
        dir.join("publishPoints.txt"),
        "lowervalve.open\r\n\r\nlock.height\nno.such.point\n",
    )
    .unwrap();
    let child = spawn_solver(&dir);
    wait_for("the FIFO", || std::path::Path::new(UPDATES_FIFO).exists());
    wait_for("the points segment", points_segment_exists);
    let (count, points) = (
        attach(NUM_PUBLISH_POINTS_SHM_KEY),
        attach(PUBLISH_POINTS_SHM_KEY),
    );
    wait_for("the first publish", || {
        slot_text(&points, 1).contains("DOUBLE")
    });
    assert_eq!(slot_text(&count, 0), "3");
    assert_eq!(slot_text(&points, 0), "lowervalve.open:DOUBLE:0.000000");
    assert_eq!(slot_text(&points, 2), "no.such.point:DOUBLE:0.000000");

    // updates arrive as 256-byte FIFO records; junk and unknown tags are survived
    let path = CString::new(UPDATES_FIFO).unwrap();
    let w = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    for msg in ["junk", "nope:DOUBLE:1", "lowervalve.open:BOOLEAN:1"] {
        let mut rec = [0u8; MAX_MSG_LEN];
        rec[..msg.len()].copy_from_slice(msg.as_bytes());
        unsafe { libc::write(w, rec.as_ptr().cast(), MAX_MSG_LEN) };
    }
    wait_for("the update to show", || {
        slot_text(&points, 0) == "lowervalve.open:DOUBLE:1.000000"
    });
    unsafe { libc::close(w) };

    let out = terminate(child);
    assert!(out.contains("Info: Read 3 PublishPoints") && out.contains("Info: solver running"));
    assert!(out.contains("Warn: malformed update record ignored"));
    assert!(out.contains("update for unknown/readonly tag nope ignored"));
    assert!(out.contains("Info: Model termination"));
    assert!(!std::path::Path::new(UPDATES_FIFO).exists());
    assert!(!points_segment_exists());
}

#[test]
fn solver_without_points_file_uses_the_model_order_and_help_exits() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("ff-sim2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let help = Command::new(env!("CARGO_BIN_EXE_ferrofire"))
        .args(["simulink", "--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("simulinksolver"));

    let child = spawn_solver(&dir);
    wait_for("the points segment", points_segment_exists);
    let points = attach(PUBLISH_POINTS_SHM_KEY);
    wait_for("the first publish", || {
        slot_text(&points, 14).contains("DOUBLE")
    });
    assert!(slot_text(&points, 0).starts_with("lowerwater.height:DOUBLE:"));
    let out = terminate(child);
    assert!(out.contains("publishPoints.txt not found"));
}

#[test]
fn solver_fails_loudly_on_a_missing_or_broken_model() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bin = env!("CARGO_BIN_EXE_ferrofire");
    let o = Command::new(bin)
        .args(["simulink", "--model", "/nonexistent.yaml"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Fatal: model /nonexistent.yaml"));
    let dir = std::env::temp_dir().join(format!("ff-sim3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.yaml"), "version: 3\ndt: 1\n").unwrap();
    let o = Command::new(bin)
        .arg("simulink")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Info: model: ./model.yaml") && out.contains("Fatal: load model"));
}
