//! `ferrofire watch`: fake firmware shell scripts, observed through a marker file and the watcher's stderr.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const RTU: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/conformance/rtu-1.xml");

struct W {
    dir: PathBuf,
    child: Child,
}

impl W {
    fn path(&self, n: &str) -> PathBuf {
        self.dir.join(n)
    }
    fn err(&self) -> String {
        fs::read_to_string(self.path("err")).unwrap_or_default()
    }
    fn marker(&self) -> String {
        fs::read_to_string(self.path("mark")).unwrap_or_default()
    }
    fn wait(&self, what: &str, secs: u64, f: impl Fn(&W) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timeout: {what}\nstderr:\n{}\nmarker:\n{}",
            self.err(),
            self.marker()
        );
    }
    /// The pid of the first start (callers ask before any restart). The watcher logs it after the spawn returns, so the
    /// firmware's marker can come first.
    fn pid(&self) -> i32 {
        self.wait("started logged", 3, |w| {
            w.err().contains("watch: started pid=")
        });
        let e = self.err();
        let l = e
            .lines()
            .rev()
            .find(|l| l.starts_with("watch: started pid="))
            .unwrap();
        l.rsplit('=').next().unwrap().parse().unwrap()
    }
}

impl Drop for W {
    fn drop(&mut self) {
        // SIGTERM so the watcher stops its firmware too; a SIGKILL would orphan it
        let _ = Command::new("kill")
            .arg(self.child.id().to_string())
            .status();
        let end = Instant::now() + Duration::from_secs(7);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn script(body: &str) -> String {
    format!("#!/bin/sh\n{body}")
}
fn v(n: u8) -> String {
    script(&format!("echo \"v{n} $*\" >> \"$MARK\"\nexec sleep 1000\n"))
}
fn put(p: &Path, s: impl AsRef<[u8]>, mode: u32) {
    fs::write(p, s).unwrap();
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}
fn alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}
/// Gone or a zombie: an orphaned grandchild is reparented to a pid 1 (cargo, in the container) that never reaps it.
fn gone(pid: i32) -> bool {
    let o = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    s.trim().is_empty() || s.trim_start().starts_with('Z')
}
/// v1 firmware without `exec`: the shell stays the watcher's child and a grandchild writes its pid to `$MARK.gpid`.
fn v1_no_exec(tail: &str) -> String {
    script(&format!(
        "echo \"v1 $*\" >> \"$MARK\"\nsleep 1000 &\necho $! > \"$MARK.gpid\"\n{tail}\n"
    ))
}
fn gpid(w: &W) -> i32 {
    w.wait("grandchild pid", 3, |w| {
        fs::read_to_string(w.path("mark.gpid")).is_ok_and(|s| s.ends_with('\n'))
    });
    fs::read_to_string(w.path("mark.gpid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Start a watcher on `fw` (written with `mode`) and the rtu config copied to `cfg`.
fn spawn(tag: &str, fw: Option<(&[u8], u32)>) -> W {
    spawn_with(tag, fw, "50")
}

fn spawn_with(tag: &str, fw: Option<(&[u8], u32)>, poll_ms: &str) -> W {
    let dir = std::env::temp_dir().join(format!("ff-watch-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    if let Some((s, m)) = fw {
        put(&dir.join("fw"), s, m);
    }
    fs::copy(RTU, dir.join("cfg.xml")).unwrap();
    let child = Command::new(BIN)
        .args(["watch", "--poll-ms", poll_ms, "--firmware"])
        .arg(dir.join("fw"))
        .arg("--file")
        .arg(dir.join("cfg.xml"))
        .arg("--run-dir")
        .arg(dir.join("run"))
        .env("MARK", dir.join("mark"))
        .current_dir(&dir) // the real binary logs into its cwd
        .stderr(fs::File::create(dir.join("err")).unwrap())
        .spawn()
        .unwrap();
    W { dir, child }
}

#[test]
fn starts_firmware_with_config() {
    let w = spawn("start", Some((v(1).as_bytes(), 0o755)));
    let want = format!("v1 field-device --file {}", w.path("cfg.xml").display());
    w.wait("marker", 3, |w| w.marker().contains(&want));
    w.pid(); // waits for the started line
    let e = w.err();
    assert!(e.contains("watch: firmware sha256="), "{e}");
}

#[test]
fn restarts_on_firmware_change() {
    let w = spawn("fwchg", Some((v(1).as_bytes(), 0o755)));
    w.wait("v1", 3, |w| w.marker().contains("v1 "));
    let old = w.pid();
    put(&w.path("fw"), v(2), 0o755);
    w.wait("v2", 2, |w| w.marker().contains("v2 "));
    assert!(w.err().contains("watch: restart: firmware changed"));
    w.wait("v1 gone", 2, |_| !alive(old));
}

#[test]
fn runs_firmware_without_exec_bit() {
    let w = spawn("noexec", Some((v(1).as_bytes(), 0o644)));
    w.wait("marker", 3, |w| w.marker().contains("v1 "));
}

#[test]
fn waits_for_stable_firmware() {
    // a poll much longer than the 30 ms gap between the two writes, so a loaded runner cannot stretch the gap
    // past the two equal polls that make a hash stable
    let w = spawn_with("stable", Some((v(1).as_bytes(), 0o755)), "400");
    w.wait("v1", 3, |w| w.marker().contains("v1 "));
    put(&w.path("fw"), script("echo half >> \"$MARK\"\n"), 0o755);
    std::thread::sleep(Duration::from_millis(30));
    put(&w.path("fw"), v(2), 0o755);
    w.wait("v2", 4, |w| w.marker().contains("v2 "));
    assert!(!w.marker().contains("half"), "{}", w.marker());
}

#[test]
fn survives_garbage_firmware_then_recovers() {
    let junk: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8 | 0x80)
        .collect();
    let mut w = spawn("junk", Some((&junk, 0o755)));
    w.wait("failure logged", 3, |w| {
        let e = w.err();
        e.contains("watch: start failed") || e.contains("watch: exited")
    });
    std::thread::sleep(Duration::from_secs(1));
    assert!(w.child.try_wait().unwrap().is_none(), "watcher died");
    put(&w.path("fw"), v(2), 0o755);
    w.wait("v2", 2, |w| w.marker().contains("v2 "));
}

#[test]
fn config_change_restarts_and_bad_config_recovers() {
    let w = spawn("cfg", Some((v(1).as_bytes(), 0o755)));
    w.wait("v1", 3, |w| w.marker().contains("v1 "));
    let mut x = fs::read_to_string(RTU).unwrap();
    x.push_str("\n<!-- changed -->\n");
    fs::write(w.path("cfg.xml"), x).unwrap();
    w.wait("config restart", 2, |w| {
        w.err().contains("watch: restart: config changed")
    });
    // the real binary as firmware: invalid XML exits, a valid config starts it again
    fs::copy(BIN, w.path("real")).unwrap();
    put(&w.path("real"), fs::read(BIN).unwrap(), 0o755);
    fs::write(w.path("fw"), fs::read(w.path("real")).unwrap()).unwrap();
    fs::write(w.path("cfg.xml"), "<not xml").unwrap();
    w.wait("exited", 5, |w| w.err().contains("watch: exited"));
    // only a restore can cause a config restart now; the backoff retry would just start the bad config again
    let restarts = w.err().matches("watch: restart: config changed").count();
    fs::copy(RTU, w.path("cfg.xml")).unwrap();
    w.wait("restart after restore", 5, |w| {
        w.err().matches("watch: restart: config changed").count() > restarts
    });
    let started = w.err().matches("watch: started").count();
    w.wait("started again", 5, |w| {
        w.err().matches("watch: started").count() > started
    });
    let exits = w.err().matches("watch: exited").count();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        w.err().matches("watch: exited").count(),
        exits,
        "restored config still crashes"
    );
}

#[test]
fn never_runs_a_partial_push_while_retrying() {
    // crash-looping firmware; a push is still being written when the backoff retry comes round. It changes every few ms,
    // as a slow copy does, so no two polls hash it the same, however late this thread is to each sleep.
    let poll = Duration::from_millis(200);
    let w = spawn_with(
        "partial",
        Some((script("exit 1\n").as_bytes(), 0o755)),
        "200",
    );
    w.wait("exited", 3, |w| w.err().contains("watch: exited"));
    // the retry is due 1 s (the first backoff) after the exit; every tick after a refused retry retries again
    let end = Instant::now() + Duration::from_millis(1600);
    let (mut last, mut held) = (Instant::now(), Duration::ZERO);
    for i in 0.. {
        let done = Instant::now() >= end;
        let fw = if done {
            v(2)
        } else {
            script(&format!("echo HALF >> \"$MARK\"\n# {i}\n"))
        };
        // stamped before the put: [start of put i, start of put i+1] holds all the time version i was on disk
        let t = Instant::now();
        put(&w.path("fw"), fw, 0o755);
        held = held.max(t - last);
        last = t;
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    w.wait("v2", 4, |w| w.marker().contains("v2 "));
    // bytes that held still for a whole poll were stable on two polls: running them is right, so nothing to check
    if held < poll {
        assert!(!w.marker().contains("HALF"), "{}", w.marker());
    } else {
        eprintln!(
            "a write gap of {held:?} let the partial push hold still for a poll; HALF not checked"
        );
    }
}

#[test]
fn sigterm_stops_child() {
    let mut w = spawn("term", Some((v(1).as_bytes(), 0o755)));
    w.wait("v1", 3, |w| w.marker().contains("v1 "));
    let pid = w.pid();
    Command::new("kill")
        .arg(w.child.id().to_string())
        .status()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(7);
    while w.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "watcher still running");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid), "child survived");
}

#[test]
fn reports_signal_exit() {
    let w = spawn("sig", Some((script("kill -9 $$\n").as_bytes(), 0o755)));
    w.wait("signal exit", 3, |w| {
        w.err().contains("watch: exited status=signal 9")
    });
}

#[test]
fn restart_stops_firmware_grandchildren() {
    let w = spawn("grand", Some((v1_no_exec("wait").as_bytes(), 0o755)));
    let g = gpid(&w);
    assert!(!gone(g), "grandchild never ran");
    put(&w.path("fw"), v(2), 0o755);
    w.wait("v2", 3, |w| w.marker().contains("v2 "));
    w.wait("v1 grandchild gone", 7, |_| gone(g));
}

#[test]
fn sigterm_stops_firmware_grandchildren() {
    let mut w = spawn("grandterm", Some((v1_no_exec("wait").as_bytes(), 0o755)));
    let g = gpid(&w);
    Command::new("kill")
        .arg(w.child.id().to_string())
        .status()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(7);
    while w.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "watcher still running");
        std::thread::sleep(Duration::from_millis(20));
    }
    w.wait("grandchild gone", 2, |_| gone(g));
}

#[test]
fn crash_stops_firmware_grandchildren() {
    // the shell exits but leaves its background child holding the device's ports
    let w = spawn("grandcrash", Some((v1_no_exec("exit 1").as_bytes(), 0o755)));
    let g = gpid(&w);
    w.wait("exited", 3, |w| w.err().contains("watch: exited"));
    w.wait("grandchild gone", 2, |_| gone(g));
}
