//! simulinksolver frontend: speaks the modelHooks.c shm/semaphore contract (see CONTRACT.md) and drives a `Model`.
//! Args (-tf inf, -v, ...) are accepted and ignored, except `--model <file>`.

pub mod contract;
pub mod ipc;

use crate::model::{Model, Value};
use contract::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn die(msg: &str, e: impl std::fmt::Display) -> ! {
    println!("Fatal: {msg}: {e}");
    std::process::exit(1);
}

/// `--model <file>`, else ./model.yaml, else the embedded waterway model.
fn load_model(args: &[String]) -> Model {
    let explicit = args.iter().position(|a| a == "--model").and_then(|i| args.get(i + 1));
    let (src, from) = match explicit {
        Some(p) => (std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("model {p}"), e)), p.clone()),
        None => match std::fs::read_to_string("model.yaml") {
            Ok(s) => (s, "./model.yaml".to_string()),
            Err(_) => (crate::WATERWAY_YAML.to_string(), "built-in waterway model".to_string()),
        },
    };
    println!("Info: model: {from}");
    Model::load_in(&src, std::path::Path::new(&from).parent()).unwrap_or_else(|e| die("load model", e))
}

pub fn main(args: &[String]) {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("simulinksolver [-tf inf ...] [--model model.yaml]  process simulator (modelHooks shm/semaphore contract)");
        return;
    }
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }

    let mut model = load_model(args);
    let text = std::fs::read_to_string("publishPoints.txt").unwrap_or_else(|_| {
        println!("Info: publishPoints.txt not found in cwd, using the model's publish order");
        model.tags().map(|(t, _)| format!("{t}\n")).collect()
    });
    let points = parse_publish_points(&text);
    println!("Info: Read {} PublishPoints", points.len());

    // init order exactly as modelHooks.c init(): semaphores (locked) -> FIFO -> count shm -> points shm -> publish -> unlock
    let publish_sem = ipc::Sem::open_locked(PUBLISH_SEM).unwrap_or_else(|e| die("publish semaphore", e));
    let updates_sem = ipc::Sem::open_locked(UPDATES_SEM).unwrap_or_else(|e| die("updates semaphore", e));
    let fifo = ipc::Fifo::create().unwrap_or_else(|e| die("updates FIFO", e));
    let count_shm = ipc::Shm::create(NUM_PUBLISH_POINTS_SHM_KEY, 4).unwrap_or_else(|e| die("count shm", e));
    count_shm.zero();
    count_shm.write(0, &encode_count(points.len()));
    let points_shm = ipc::Shm::create(PUBLISH_POINTS_SHM_KEY, points.len() * MAX_MSG_LEN).unwrap_or_else(|e| die("points shm", e));
    points_shm.zero();

    let publish = |m: &Model| {
        let snap = m.snapshot();
        for (n, tag) in points.iter().enumerate() {
            // a point the model does not publish reads 0 (as the old solver did)
            let v = snap.iter().find(|(t, _)| t == tag).map(|(_, v)| *v).unwrap_or(Value::Analog(0.0));
            points_shm.write(n * MAX_MSG_LEN, &encode_dto(tag, v));
        }
    };
    publish(&model);
    publish_sem.post();
    updates_sem.post();
    let dt = model.dt;
    println!("Info: solver running (dt={dt}s)");

    let t0 = Instant::now();
    let mut k: u64 = 0;
    while !STOP.load(Ordering::SeqCst) {
        // step() hook: apply updates, publish, then advance (engine order, spec section 3.1)
        updates_sem.wait();
        let mut writes = vec![];
        for rec in fifo.drain() {
            match decode_update(&rec) {
                Some((tag, v)) => writes.push((tag, Value::Analog(v))),
                None => println!("Warn: malformed update record ignored"),
            }
        }
        updates_sem.post();
        for tag in model.apply_writes(&writes) {
            println!("update for unknown/readonly tag {tag} ignored");
        }

        publish_sem.wait();
        publish(&model);
        publish_sem.post();

        model.step(dt);
        k += 1;
        let due = t0 + Duration::from_secs_f64(k as f64 * dt);
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }

    // term(): take both locks so the provider is not mid-read, then tear everything down
    println!("Info: Model termination");
    publish_sem.wait();
    updates_sem.wait();
    count_shm.destroy();
    points_shm.destroy();
    publish_sem.destroy();
    updates_sem.destroy();
    fifo.destroy();
}
