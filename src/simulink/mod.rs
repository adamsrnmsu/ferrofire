//! simulinksolver frontend: speaks the modelHooks.c shm/semaphore contract (see CONTRACT.md) and drives a `Model`.
//! Args (-tf inf, -v, ...) are accepted and ignored, except `--model <file>` and `--gt-addr HOST:PORT` (the ground-truth page that
//! replaces `simulinkgt`, docs in the README).

pub mod contract;
pub mod ipc;

use crate::bennu::{install_signals, STOP};
use crate::model::{Model, Value};
use contract::*;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn die(msg: &str, e: impl std::fmt::Display) -> ! {
    println!("Fatal: {msg}: {e}");
    std::process::exit(1);
}

/// `--model <file>`, else ./model.yaml, else the embedded waterway model: (yaml, where it came from).
fn model_source(args: &[String]) -> (String, String) {
    if let Some(p) = flag_value(args, "--model") {
        let src = std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("model {p}"), e));
        return (src, p.clone());
    }
    match std::fs::read_to_string("model.yaml") {
        Ok(s) => (s, "./model.yaml".to_string()),
        Err(_) => (
            crate::WATERWAY_YAML.to_string(),
            "built-in waterway model".to_string(),
        ),
    }
}

fn load_model(args: &[String]) -> Model {
    let (src, from) = model_source(args);
    println!("Info: model: {from}");
    Model::load_in(&src, std::path::Path::new(&from).parent())
        .unwrap_or_else(|e| die("load model", format!("{from}: {e}")))
}

/// publishPoints.txt, else the model's own publish order.
fn read_points(model: &Model) -> Vec<String> {
    let text = std::fs::read_to_string("publishPoints.txt").unwrap_or_else(|_| {
        println!("Info: publishPoints.txt not found in cwd, using the model's publish order");
        model.tags().map(|(t, _)| format!("{t}\n")).collect()
    });
    let points = parse_publish_points(&text);
    println!("Info: Read {} PublishPoints", points.len());
    points
}

/// The shared-memory / semaphore / FIFO endpoints of the modelHooks.c contract.
struct Io {
    points: Vec<String>,
    publish_sem: ipc::Sem,
    updates_sem: ipc::Sem,
    fifo: ipc::Fifo,
    count_shm: ipc::Shm,
    points_shm: ipc::Shm,
}

impl Io {
    /// Init order exactly as modelHooks.c init(): semaphores (locked) -> FIFO -> count shm -> points shm.
    fn open(points: Vec<String>) -> Io {
        let publish_sem =
            ipc::Sem::open_locked(PUBLISH_SEM).unwrap_or_else(|e| die("publish semaphore", e));
        let updates_sem =
            ipc::Sem::open_locked(UPDATES_SEM).unwrap_or_else(|e| die("updates semaphore", e));
        let fifo = ipc::Fifo::create().unwrap_or_else(|e| die("updates FIFO", e));
        let count_shm =
            ipc::Shm::create(NUM_PUBLISH_POINTS_SHM_KEY, 4).unwrap_or_else(|e| die("count shm", e));
        count_shm.zero();
        count_shm.write(0, &encode_count(points.len()));
        let points_shm = ipc::Shm::create(PUBLISH_POINTS_SHM_KEY, points.len() * MAX_MSG_LEN)
            .unwrap_or_else(|e| die("points shm", e));
        points_shm.zero();
        Io {
            points,
            publish_sem,
            updates_sem,
            fifo,
            count_shm,
            points_shm,
        }
    }

    /// Write every publish point into its slot; a point the model does not publish reads 0 (as the old solver did).
    fn publish(&self, m: &Model) {
        let snap = m.snapshot();
        for (n, tag) in self.points.iter().enumerate() {
            let v = snap
                .iter()
                .find(|(t, _)| t == tag)
                .map_or(Value::Analog(0.0), |(_, v)| *v);
            self.points_shm.write(n * MAX_MSG_LEN, &encode_dto(tag, v));
        }
    }

    /// Drain the updates FIFO under the updates semaphore and latch the writes into the model.
    fn apply_updates(&self, model: &mut Model) {
        self.updates_sem.wait();
        let mut writes = vec![];
        for rec in self.fifo.drain() {
            match decode_update(&rec) {
                Some((tag, v)) => writes.push((tag, Value::Analog(v))),
                None => println!("Warn: malformed update record ignored"),
            }
        }
        self.updates_sem.post();
        for tag in model.apply_writes(&writes) {
            println!("update for unknown/readonly tag {tag} ignored");
        }
    }

    /// step() hook: apply updates, then publish under the publish semaphore (engine order, spec section 3.1).
    fn cycle(&self, model: &mut Model) {
        self.apply_updates(model);
        self.publish_sem.wait();
        self.publish(model);
        self.publish_sem.post();
    }

    /// term(): take both locks so the provider is not mid-read, then tear everything down.
    fn term(&self) {
        println!("Info: Model termination");
        self.publish_sem.wait();
        self.updates_sem.wait();
        self.count_shm.destroy();
        self.points_shm.destroy();
        self.publish_sem.destroy();
        self.updates_sem.destroy();
        self.fifo.destroy();
    }
}

/// The value after `flag`, if the flag is there.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
}

pub fn main(args: &[String]) {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("simulinksolver [-tf inf ...] [--model model.yaml] [--gt-addr HOST:PORT]  process simulator (modelHooks shm/semaphore contract)");
        return;
    }
    install_signals();
    crate::log::start("simulink");
    let mut model = load_model(args);
    let addr = flag_value(args, "--gt-addr").map(String::as_str);
    let gt = crate::gt::start(addr, crate::gt::STALE_AFTER)
        .unwrap_or_else(|e| die("ground truth page", e));
    let io = Io::open(read_points(&model));
    io.publish(&model);
    io.publish_sem.post();
    io.updates_sem.post();
    let dt = model.dt;
    println!("Info: solver running (dt={dt}s)");

    let t0 = Instant::now();
    let mut k: u64 = 0;
    while !STOP.load(Ordering::SeqCst) {
        io.cycle(&mut model);
        if gt {
            // ponytail: owned names every cycle, only when the page is on; a borrowed publish in top if that ever shows in a profile
            let snap: Vec<_> = model
                .snapshot()
                .into_iter()
                .map(|(t, v)| (t.to_string(), v))
                .collect();
            crate::top::publish(&snap, Instant::now());
        }
        model.step(dt);
        k += 1;
        let due = t0 + Duration::from_secs_f64(k as f64 * dt);
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
    io.term();
    crate::log::stop();
}
