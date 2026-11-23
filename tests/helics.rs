//! `ferrofire run` as a HELICS federate (docs/helics.md): a broker and a second federate in this process, `ferrofire run` as a
//! child process, values exchanged both ways. Needs libhelics: skipped (loudly) unless $FERROFIRE_HELICS_LIB points at it,
//! which the ff-rust image sets. Built only with the `helics` feature (the default).
#![cfg(feature = "helics")]

use ferrofire::bennu::{Shared, STOP};
use ferrofire::helics::ffi::{Fed, Lib};
use ferrofire::helics::{run_engine, Cli, Federate};
use ferrofire::model::{Kind, Model, Value};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");

/// A tank filled at (valve.open - 0.25) per second; `pump.on` mirrors the binary input.
const MODEL: &str = "version: 1
dt: 0.1
inputs:
  pump.run:   {type: binary, default: false}
  valve.open: {type: analog, default: 0.5, min: 0, max: 1}
states:
  tank.level: {ode: 'valve.open - 0.25', init: 1.0, min: 0, max: 10}
outputs:
  tank.level: {type: analog}
  pump.on:    {type: binary, expr: pump.run}
helics:
  name: tank
  broker: tcp://127.0.0.1:PORT
  subscribe:
    - {key: ctl/valve, tag: valve.open}
    - {key: ctl/run,   tag: pump.run}
  publish:
    - {tag: tank.level, key: tank/level}
    - {tag: pump.on,    key: tank/pump}
";

/// Two federations in one test process cross-talk in libhelics (a "duplicate federate name" from the other test's broker), so
/// the tests take turns.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Dropped last in a test (declare it before the broker): joins libhelics' threads, so that none is running when the test
/// process exits. libhelics' static destructors race them and crash the process at exit about one run in forty (ww-20t).
/// It finalizes any federate still left too: a test that abandons one whose peer never comes hangs here instead of crashing.
struct Closing(Arc<Lib>);

impl Drop for Closing {
    fn drop(&mut self) {
        // SAFETY: declared before the broker, so it drops after it; the federates were finalized and freed inside the test
        // (controller, run_engine), and this binary opens libhelics nowhere else, so no Broker or Fed outlives the close.
        unsafe { self.0.close() }
    }
}

fn lib() -> Option<(Arc<Lib>, Closing)> {
    if std::env::var_os("FERROFIRE_HELICS_LIB").is_none() {
        eprintln!("SKIPPED: FERROFIRE_HELICS_LIB is not set (the ff-rust image sets it)");
        return None;
    }
    let lib = Lib::open(None).unwrap();
    Some((lib.clone(), Closing(lib)))
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// A port whose successor is free too (the zmq broker binds `port` and `port + 1`). Scanned from a start that differs per test
/// process and never the same twice (libhelics reuses an in-process core for an address it has seen), below the ephemeral
/// range: after a busy test run that range is full, so asking the kernel for a port is slow and a scan up there finds nothing.
fn free_port_pair() -> u16 {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let free = |p: u16| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok();
    let start = 12000 + (std::process::id() % 8000) as u16 + NEXT.fetch_add(10, Ordering::SeqCst);
    (start..32000)
        .find(|&p| free(p) && free(p + 1))
        .expect("no free port pair")
}

/// A broker for `n` federates on a free port, and a model file that points at it.
fn federation(lib: &Arc<Lib>, n: u32) -> (ferrofire::helics::ffi::Broker, u16, tempdir::Dir) {
    let port = free_port_pair();
    let init = format!("-f {n} --port={port} --local_interface=127.0.0.1");
    let broker = lib
        .broker("zmq", &format!("ff-test-{port}"), &init)
        .unwrap();
    let dir = tempdir::Dir::new(&MODEL.replace("PORT", &port.to_string()));
    (broker, port, dir)
}

/// A throwaway model file (std only: no tempfile crate in the tree).
mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Dir {
        pub fn new(yaml: &str) -> Dir {
            let d = std::env::temp_dir().join(format!(
                "ff-helics-{}-{}",
                std::process::id(),
                yaml.len()
            ));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("tank.yaml"), yaml).unwrap();
            Dir(d)
        }
        pub fn model(&self) -> String {
            self.0.join("tank.yaml").display().to_string()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn spawn(model: &str) -> Child {
    let server = format!("tcp://127.0.0.1:{}", free_port());
    Command::new(BIN)
        .args([
            "run",
            model,
            "--server",
            &server,
            "--publish",
            "udp://*;127.0.0.1:45998",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn terminate(c: &Child) {
    // SAFETY: plain kill(2) of our own child.
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
}

fn wait_for(c: &mut Child, secs: u64) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if c.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn output(c: &mut Child) -> String {
    let mut s = String::new();
    c.stdout.take().unwrap().read_to_string(&mut s).unwrap();
    c.stderr.take().unwrap().read_to_string(&mut s).unwrap();
    s
}

/// The controller: opens the valve and starts the pump at t = 0, then records what the tank published at each grant.
fn controller(lib: &Arc<Lib>, port: u16, grants: usize) -> (Vec<f64>, Vec<bool>) {
    let init = format!("--broker_address=tcp://127.0.0.1:{port}");
    let mut f = Fed::new(lib, "ctl", "zmq", &init, 0.1, || false).unwrap();
    f.add_pub("ctl/valve", Kind::Analog).unwrap();
    f.add_pub("ctl/run", Kind::Binary).unwrap();
    f.add_sub("tank/level", Kind::Analog).unwrap();
    f.add_sub("tank/pump", Kind::Binary).unwrap();
    f.enter_exec().unwrap();
    f.publish(0, Value::Analog(1.0)).unwrap();
    f.publish(1, Value::Binary(true)).unwrap();
    let (mut level, mut pump) = (vec![], vec![]);
    for g in 1..=grants {
        let t = f.request_time(g as f64 * 0.1).unwrap().unwrap();
        assert!((t - g as f64 * 0.1).abs() < 1e-9, "granted {t}");
        if let Some(Value::Analog(x)) = f.read(0).unwrap() {
            level.push(x);
        }
        if let Some(Value::Binary(b)) = f.read(1).unwrap() {
            pump.push(b);
        }
    }
    f.finalize();
    (level, pump)
}

#[test]
fn values_flow_both_ways_one_grant_late() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some((lib, _closing)) = lib() else { return };
    let (_broker, port, dir) = federation(&lib, 2);
    let mut ff = spawn(&dir.model());
    let (level, pump) = controller(&lib, port, 8);
    // HELICS does not tell a lone federate its peers are gone: it free-runs until it is stopped
    terminate(&ff);
    assert!(wait_for(&mut ff, 20), "ferrofire did not stop on SIGTERM");
    let out = output(&mut ff);
    assert!(ff.wait().unwrap().success(), "{out}");
    assert!(
        out.contains("Info: helics ") && out.contains("Info: stopped"),
        "{out}"
    );

    assert_exchange(&level, &pump, &out);
}

/// What the controller must have seen. The tank publishes its state at t (grant 0 = the initial 1.0), so the controller sees
/// it one grant late; the valve it set at t = 0 reaches the tank at 0.1 and acts on the step after: +0.025 per step at valve
/// 0.5, +0.075 at 1.0. The pump command comes back as the binary output, one grant for each leg.
fn assert_exchange(level: &[f64], pump: &[bool], out: &str) {
    let want = [1.0, 1.025, 1.1, 1.175, 1.25, 1.325, 1.4, 1.475];
    assert_eq!(level.len(), want.len(), "{level:?}\n{out}");
    for (got, want) in level.iter().zip(want) {
        assert!((got - want).abs() < 1e-9, "{level:?}");
    }
    assert_eq!(pump.first(), Some(&false));
    assert_eq!(pump.last(), Some(&true), "{pump:?}");
}

/// The same exchange with the engine in this process (so its code is covered): the controller thread stops it when done.
#[test]
fn the_engine_runs_in_process_against_a_controller() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some((lib, _closing)) = lib() else { return };
    let (_broker, port, dir) = federation(&lib, 2);
    let mut model = Model::load(&std::fs::read_to_string(dir.model()).unwrap()).unwrap();
    let sh = std::sync::Mutex::new(Shared::default());
    let ctl = std::thread::spawn(move || {
        let seen = controller(&lib, port, 8);
        STOP.store(true, Ordering::SeqCst);
        seen
    });
    let ran = run_engine(&Cli::default(), Some(&dir.model()), &mut model, &sh, &[], 1);
    let (level, pump) = ctl.join().unwrap();
    assert_eq!(ran, Ok(()));
    assert_exchange(&level, &pump, "");
}

#[test]
fn sigterm_ends_a_run_still_waiting_for_its_peers() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some((lib, _closing)) = lib() else { return };
    let (_broker, _, dir) = federation(&lib, 2); // the second federate never comes
    let mut ff = spawn(&dir.model());
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        ff.try_wait().unwrap().is_none(),
        "left before its peer joined"
    );
    terminate(&ff);
    if !wait_for(&mut ff, 10) {
        let _ = ff.kill();
        let _ = ff.wait();
        panic!(
            "SIGTERM did not end a run blocked in HELICS:\n{}",
            output(&mut ff)
        );
    }
    assert!(ff.wait().unwrap().success(), "{}", output(&mut ff));
}

#[test]
fn helics_flags_need_a_helics_section() {
    let server = format!("tcp://127.0.0.1:{}", free_port());
    let model = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");
    let out = Command::new(BIN)
        .args([
            "run",
            model,
            "--server",
            &server,
            "--publish",
            "udp://*;127.0.0.1:45997",
        ])
        .args(["--helics-name", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("need a helics: section"));
}
