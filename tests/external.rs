//! `<external-module>` over stdio (ffmod/1): a whole device with `sh` scripts as fake modules. The lifecycle cases of the plan's
//! Review Focus are here: a module that crashes or is missing (backoff), one that ignores SIGTERM (SIGKILL at 5 s), one that never
//! reads its stdin (stop still bounded), and `error` under `--strict` (the device exits non-zero).
#![cfg(feature = "external")]

mod common;

use common::clients::device;
use ferrofire::model::Value;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");

/// A throwaway directory for one test's scripts and files.
struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("ff-ext-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    /// Write `body` to `name` and return the command that runs it with `sh`.
    fn script(&self, name: &str, body: &str) -> String {
        let p = self.0.join(name);
        std::fs::write(&p, body).unwrap();
        format!("sh {}", p.display())
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn free_tcp() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A provider REP that ACKs everything and reports each request, until `stop`.
fn provider(port: u16, stop: CancellationToken) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let rep = zmq::Context::new().socket(zmq::REP).unwrap();
    rep.set_rcvtimeo(100).unwrap();
    rep.bind(&format!("tcp://127.0.0.1:{port}")).unwrap();
    std::thread::spawn(move || {
        while !stop.is_cancelled() {
            if let Ok(m) = rep.recv_bytes(0) {
                let _ = tx.send(String::from_utf8_lossy(&m).into_owned());
                rep.send("ACK=\0", 0).unwrap();
            }
        }
    });
    rx
}

/// Whether the provider sees `want` in a request within `secs`.
fn provider_sees(reqs: &mpsc::Receiver<String>, want: &str, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if reqs
            .recv_timeout(Duration::from_millis(50))
            .is_ok_and(|r| r.contains(want))
        {
            return true;
        }
    }
    false
}

/// A device with output tag `out` (provider point `out.p` on `port`), internal tag `tick`, and `modules` as its `<comms>`.
fn xml(port: u16, cycle_ms: u64, tick: &str, modules: &str) -> String {
    format!(
        "<SCEPTRE><field-device><name>d</name><cycle-time>{cycle_ms}</cycle-time>\
        <tags><external-tag><name>out</name><io>O0</io><type>analog</type></external-tag>\
        <internal-tag><name>{tick}</name><value>0</value></internal-tag></tags>\
        <output><endpoint>tcp://127.0.0.1:{port}</endpoint><analog><id>O0</id><name>out.p</name></analog></output>\
        <comms>{modules}</comms></field-device></SCEPTRE>"
    )
}

fn module(name: &str, command: &str) -> String {
    format!("<external-module><name>{name}</name><command>{command}</command><config><x/></config></external-module>")
}

/// Reads `hello`, says `ready`, writes 5.0 to `out` (after `$2` seconds), then reads to the end of its stdin.
const WRITER: &str = r#"echo $$ > "$1"
read hello
echo '{"t":"ready","role":"server","uses":[{"tag":"out","kind":"analog","dir":"in"}]}'
sleep "${2:-0}"
echo '{"t":"write","id":1,"tag":"out","value":5.0}'
cat > /dev/null
"#;

/// Wait for `file` (a module's `$$`) and return the pid in it.
fn pid_in(file: &str) -> i32 {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let pid = std::fs::read_to_string(file).ok();
        if let Some(pid) = pid.and_then(|s| s.trim().parse().ok()) {
            return pid;
        }
        assert!(Instant::now() < end, "module never started ({file})");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every live (not zombie) process in process group `pgid`, from /proc.
fn alive_in_group(pgid: i32) -> Vec<String> {
    let stats = std::fs::read_dir("/proc").unwrap().flatten();
    let stats = stats.filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok());
    stats
        .filter(|s| {
            // `pid (comm) state ppid pgrp ...`; comm may hold spaces, so split after its last `)`
            let f: Vec<&str> = s[s.rfind(')').unwrap() + 1..].split_whitespace().collect();
            f[0] != "Z" && f[2] == pgid.to_string()
        })
        .collect()
}

/// Stop `dev` and return how long its stop took.
fn stop_time(mut dev: common::clients::Device) -> Duration {
    let t = Instant::now();
    dev.stop();
    t.elapsed()
}

#[test]
fn stdio_module_round_trip() {
    let (dir, port, stop) = (Dir::new("round"), free_tcp(), CancellationToken::new());
    let reqs = provider(port, stop.clone());
    let cmd = format!("{} {}", dir.script("m.sh", WRITER), dir.path("pid"));
    let dev = device(&xml(port, 20, "tick", &module("m", &cmd)));
    assert!(
        provider_sees(&reqs, "out.p:5.000000", 15),
        "the module's write never reached the provider"
    );
    let pid = pid_in(&dir.path("pid"));
    // the module exits once its stdin closes: no signal needed
    assert!(stop_time(dev) < Duration::from_secs(2));
    assert_eq!(alive_in_group(pid), Vec::<String>::new());
    stop.cancel();
}

/// Review Focus 3: a command that exits at once and one that does not exist are restarted on the backoff (1 s, 2 s, ...), not in
/// a loop, and the device's other module keeps working.
#[test]
fn a_crashing_module_backs_off() {
    let (dir, port, stop) = (Dir::new("crash"), free_tcp(), CancellationToken::new());
    let reqs = provider(port, stop.clone());
    // its write comes once the others have crashed and been restarted
    let good = format!("{} {} 2.5", dir.script("m.sh", WRITER), dir.path("pid"));
    let mods = [
        module("quits", "false"),
        module("missing", "/nonexistent/module --flag"),
        module("good", &good),
    ];
    let cfg = dir.path("fd.xml");
    std::fs::write(&cfg, xml(port, 20, "tick", &mods.concat())).unwrap();
    let mut dev = binary(&cfg, false);
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        provider_sees(&reqs, "out.p:5.000000", 5),
        "the good module stopped working"
    );
    let (ok, log) = sigterm(&mut dev);
    assert!(ok, "{log}");
    for name in ["quits", "missing"] {
        let spawns = log.matches(&format!("module {name}: starting")).count();
        assert!((2..=3).contains(&spawns), "{name}: {spawns} spawns\n{log}");
    }
    assert!(log.contains("module missing: cannot start"), "{log}");
    stop.cancel();
}

/// Review Focus 4: a module that ignores SIGTERM (its whole process group) is killed at 5 s; stop waits no longer.
#[test]
fn stop_escalates_to_sigkill() {
    let dir = Dir::new("kill");
    let body = "echo $$ > \"$1\"\ntrap \"\" TERM\nread x\nsleep 60\n";
    let cmd = format!("{} {}", dir.script("m.sh", body), dir.path("pid"));
    let dev = device(&xml(free_tcp(), 20, "tick", &module("stubborn", &cmd)));
    let pid = pid_in(&dir.path("pid"));
    std::thread::sleep(Duration::from_millis(300)); // past `trap`
    let took = stop_time(dev);
    assert!(took < Duration::from_secs(6), "stop took {took:?}");
    assert!(
        took >= Duration::from_secs(4),
        "SIGTERM should have been ignored: {took:?}"
    );
    assert_eq!(alive_in_group(pid), Vec::<String>::new());
}

/// Review Focus 2: a module that never reads its stdin, while a long tag changes every cycle (well past the pipe's buffer), does
/// not hold the store up or make stop take longer than 5 s.
#[test]
fn a_module_that_never_reads_does_not_block_stop() {
    let dir = Dir::new("deaf");
    let body =
        "echo $$ > \"$1\"\necho '{\"t\":\"ready\",\"role\":\"server\",\"uses\":[]}'\nsleep 60\n";
    let cmd = format!("{} {}", dir.script("m.sh", body), dir.path("pid"));
    let tick = "t".repeat(2000);
    let dev = device(&xml(free_tcp(), 5, &tick, &module("deaf", &cmd)));
    let pid = pid_in(&dir.path("pid"));
    let t = Instant::now();
    let mut n = 0.0;
    while t.elapsed() < Duration::from_secs(3) {
        n += 1.0;
        dev.store.write(&tick, Value::Analog(n)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    // the store keeps up: the last write lands
    let end = Instant::now() + Duration::from_secs(3);
    while dev.store.get(&tick) != Some(Value::Analog(n)) {
        assert!(Instant::now() < end, "the last write never landed");
        std::thread::sleep(Duration::from_millis(10));
    }
    let took = stop_time(dev);
    assert!(took < Duration::from_secs(6), "stop took {took:?}");
    assert_eq!(alive_in_group(pid), Vec::<String>::new());
}

/// `ferrofire field-device --file cfg [--strict]`, its stderr piped.
fn binary(cfg: &str, strict: bool) -> Child {
    let mut c = Command::new(BIN);
    c.args(["field-device", "--file", cfg]);
    if strict {
        c.arg("--strict");
    }
    c.stderr(Stdio::piped()).spawn().unwrap()
}

/// SIGTERM the device (never SIGKILL: no coverage data, and its modules would be left over); whether it exited 0, and its stderr.
fn sigterm(c: &mut Child) -> (bool, String) {
    ferrofire::sys::kill(c.id() as i32, libc::SIGTERM);
    exited(c, 10)
}

/// Wait up to `secs` for the device to exit: whether it exited 0, and its stderr.
fn exited(c: &mut Child, secs: u64) -> (bool, String) {
    let end = Instant::now() + Duration::from_secs(secs);
    let status = loop {
        if let Some(s) = c.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < end, "device did not exit within {secs} s");
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut err = String::new();
    c.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    (status.success(), err)
}

#[test]
fn strict_error_exits_nonzero() {
    let dir = Dir::new("strict");
    let body = "read x\necho '{\"t\":\"error\",\"msg\":\"no rpoll-rate\"}'\ncat > /dev/null\n";
    let cfg = dir.path("fd.xml");
    let mods = module("picky", &dir.script("m.sh", body));
    std::fs::write(&cfg, xml(free_tcp(), 20, "tick", &mods)).unwrap();

    let (ok, log) = exited(&mut binary(&cfg, true), 15);
    assert!(!ok, "{log}");
    assert!(log.contains("no rpoll-rate"), "{log}");

    let mut dev = binary(&cfg, false);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        dev.try_wait().unwrap().is_none(),
        "device exited without --strict"
    );
    let (ok, log) = sigterm(&mut dev);
    assert!(ok, "{log}");
    assert_eq!(log.matches("module picky: starting").count(), 1, "{log}");
}
