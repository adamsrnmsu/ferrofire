//! ffbench: measuring client (`client`) and in-netns monitor (`mon`) for the zmq vs nng transport benchmark.
//! `client`: REQ latency phases + a subscriber (zmq: UDP RADIO/DISH multicast; nng: SUB0), JSON on stdout.
//! `mon`: AF_PACKET byte/packet counters by traffic class + provider (PID 1) CPU ticks and RSS, CSV rows.
mod mon;
mod pull;
mod stats;

use serde_json::{json, Value};
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

pub fn arg(args: &[String], flag: &str, default: &str) -> String {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
        .unwrap_or_else(|| default.to_string())
}

// ---- requests ---------------------------------------------------------------------------------------------

enum Req {
    Zmq(zmq::Socket),
    Nng(nng::Socket),
}

impl Req {
    fn connect(transport: &str, host: &str) -> Req {
        let url = format!("tcp://{host}:5555");
        match transport {
            "zmq" => {
                let ctx = zmq::Context::new();
                let s = ctx.socket(zmq::REQ).unwrap();
                s.set_rcvtimeo(3000).unwrap();
                s.connect(&url).unwrap();
                std::mem::forget(ctx);
                Req::Zmq(s)
            }
            _ => {
                use nng::options::{Options, RecvTimeout};
                let s = nng::Socket::new(nng::Protocol::Req0).unwrap();
                s.set_opt::<RecvTimeout>(Some(Duration::from_secs(3))).unwrap();
                s.dial(&url).unwrap();
                Req::Nng(s)
            }
        }
    }
    /// One request, one reply (NUL stripped). Panics on timeout: a wedged provider invalidates the run.
    fn call(&self, req: &str) -> String {
        let raw = match self {
            Req::Zmq(s) => {
                s.send(req.as_bytes(), 0).unwrap();
                s.recv_bytes(0).expect("zmq reply timeout")
            }
            Req::Nng(s) => {
                s.send(req.as_bytes()).map_err(|e| e.1).unwrap();
                s.recv().expect("nng reply timeout").to_vec()
            }
        };
        String::from_utf8_lossy(&raw).trim_end_matches('\0').to_string()
    }
}

// ---- subscriber -------------------------------------------------------------------------------------------

/// One publish cycle as received: from the chunk starting with the first tag (`sim.t`) to the last chunk before the next.
#[derive(Clone, Default)]
pub struct Cycle {
    pub first_ns: u64,
    pub last_ns: u64,
    pub chunks: u32,
    pub tags: u32,
    pub bytes: u32,
    pub simt: f64,
}

#[derive(Default)]
struct SubLog {
    cycles: Vec<Cycle>,
    chunks: u64,
    malformed: u64,
}

/// Fold one NUL-terminated `tag:value,...` chunk (received at `t`) into the log.
fn ingest(log: &mut SubLog, body: &[u8], t: u64) {
    let body = body.strip_suffix(&[0]).unwrap_or(body);
    log.chunks += 1;
    if body.starts_with(b"sim.t:") {
        let end = body.iter().position(|&b| b == b',').unwrap_or(body.len());
        let simt = std::str::from_utf8(&body[6..end]).ok().and_then(|s| s.parse().ok());
        log.cycles.push(Cycle { first_ns: t, simt: simt.unwrap_or(f64::NAN), ..Default::default() });
    }
    let Some(c) = log.cycles.last_mut() else {
        log.malformed += 1;
        return;
    };
    c.last_ns = t;
    c.chunks += 1;
    c.tags += body.iter().filter(|&&b| b == b',').count() as u32;
    c.bytes += body.len() as u32 + 1;
}

/// UDP RADIO/DISH-equivalent subscriber on `port`; `multicast` joins 239.0.0.1 (the provider's default), else plain unicast.
fn sub_zmq(port: u16, multicast: bool, stop: Arc<AtomicBool>) -> SubLog {
    let s = UdpSocket::bind(("0.0.0.0", port)).unwrap();
    if multicast {
        s.join_multicast_v4(&Ipv4Addr::new(239, 0, 0, 1), &Ipv4Addr::UNSPECIFIED).unwrap();
    }
    s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let (mut log, mut buf) = (SubLog::default(), vec![0u8; 65536]);
    while !stop.load(Ordering::Relaxed) {
        let Ok(n) = s.recv(&mut buf) else { continue };
        let t = now_ns();
        match stats::unframe(&buf[..n]) {
            Some(body) => ingest(&mut log, body, t),
            None => log.malformed += 1,
        }
    }
    log
}

fn sub_nng(endpoint: &str, stop: Arc<AtomicBool>) -> SubLog {
    use nng::options::protocol::pubsub::Subscribe;
    use nng::options::{Options, RecvTimeout};
    let s = nng::Socket::new(nng::Protocol::Sub0).unwrap();
    s.set_opt::<Subscribe>(vec![]).unwrap();
    s.set_opt::<RecvTimeout>(Some(Duration::from_millis(200))).unwrap();
    s.dial_async(endpoint).unwrap(); // the publisher may start after us
    let mut log = SubLog::default();
    while !stop.load(Ordering::Relaxed) {
        if let Ok(m) = s.recv() {
            ingest(&mut log, &m, now_ns());
        }
    }
    log
}

// ---- phases -----------------------------------------------------------------------------------------------

fn lat_json(ns: &mut Vec<u64>, secs: f64) -> Value {
    let mut v = stats::summary_us(ns);
    v["rate_per_s"] = json!(ns.len() as f64 / secs);
    v
}

/// Closed loop for `secs`: `f` does one operation and records its own latencies. Returns elapsed seconds.
fn timed(secs: f64, mut f: impl FnMut()) -> (u64, u64) {
    let start = now_ns();
    let t0 = Instant::now();
    while t0.elapsed().as_secs_f64() < secs {
        f();
    }
    (start, now_ns())
}

fn phase_read(r: &Req, tags: &[String], secs: f64) -> (Value, (u64, u64)) {
    let (mut lat, mut err, mut x) = (vec![], 0u64, 12345u64);
    let win = timed(secs, || {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let tag = &tags[(x >> 33) as usize % tags.len()];
        let t = Instant::now();
        let rep = r.call(&format!("READ={tag}"));
        lat.push(t.elapsed().as_nanos() as u64);
        err += !rep.starts_with("ACK=") as u64;
    });
    let mut j = lat_json(&mut lat, (win.1 - win.0) as f64 / 1e9);
    j["errors"] = json!(err);
    (j, win)
}

fn phase_query(r: &Req, n: usize, secs: f64) -> (Value, (u64, u64)) {
    let (mut lat, mut err, mut bytes) = (vec![], 0u64, 0usize);
    let win = timed(secs, || {
        let t = Instant::now();
        let rep = r.call("QUERY=");
        lat.push(t.elapsed().as_nanos() as u64);
        bytes = rep.len() + 1;
        err += (!rep.starts_with("ACK=") || rep.matches(',').count() != n) as u64;
    });
    let mut j = lat_json(&mut lat, (win.1 - win.0) as f64 / 1e9);
    j["errors"] = json!(err);
    j["reply_bytes"] = json!(bytes);
    (j, win)
}

/// WRITE a fresh value, then READ it back until it shows (the snapshot only refreshes on the next engine tick).
fn phase_write(r: &Req, secs: f64) -> (Value, (u64, u64)) {
    let (mut wr, mut rb, mut vis, mut err, mut seq) = (vec![], vec![], vec![], 0u64, 0u64);
    let win = timed(secs, || {
        seq += 1;
        let t = Instant::now();
        let rep = r.call(&format!("WRITE=bench.in0:{seq}"));
        wr.push(t.elapsed().as_nanos() as u64);
        err += !rep.starts_with("ACK=") as u64;
        loop {
            let t1 = Instant::now();
            let rep = r.call("READ=bench.in0");
            rb.push(t1.elapsed().as_nanos() as u64);
            if rep.strip_prefix("ACK=").and_then(|v| v.parse::<f64>().ok()) == Some(seq as f64) {
                vis.push(t.elapsed().as_nanos() as u64);
                break;
            }
            if t.elapsed() > Duration::from_secs(2) {
                err += 1;
                break;
            }
        }
    });
    let s = (win.1 - win.0) as f64 / 1e9;
    let j = json!({"write_rtt": lat_json(&mut wr, s), "readback_rtt": lat_json(&mut rb, s),
                   "write_to_visible": lat_json(&mut vis, s), "errors": err});
    (j, win)
}

fn client(args: &[String]) {
    let transport = arg(args, "--transport", "zmq");
    let host = arg(args, "--host", "ffb-prov");
    let n: usize = arg(args, "--n", "10").parse().unwrap();
    let period: f64 = arg(args, "--period", "0.1").parse().unwrap();
    let (warm, pub_s, req_s): (f64, f64, f64) = (
        arg(args, "--warmup", "3").parse().unwrap(),
        arg(args, "--pub-secs", "30").parse().unwrap(),
        arg(args, "--req-secs", "10").parse().unwrap(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (st, h) = (stop.clone(), host.clone());
    let t_arg = transport.clone();
    let sub = std::thread::spawn(move || match t_arg.as_str() {
        "zmq" => sub_zmq(40000, true, st),
        _ => sub_nng(&format!("tcp://{h}:5556"), st),
    });
    let req = Req::connect(&transport, &host);
    let tags: Vec<String> = req.call("QUERY=").trim_start_matches("ACK=").split(',').filter(|t| !t.is_empty()).map(String::from).collect();
    assert_eq!(tags.len(), n, "provider publishes {} tags, expected {n}", tags.len());
    std::thread::sleep(Duration::from_secs_f64(warm));
    let pub_start = now_ns();
    std::thread::sleep(Duration::from_secs_f64(pub_s));
    let pub_win = (pub_start, now_ns());
    let (read, read_w) = phase_read(&req, &tags, req_s);
    let (query, query_w) = phase_query(&req, n, req_s);
    let (write, write_w) = phase_write(&req, req_s);
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    let log = sub.join().unwrap();
    let publ = |w: (u64, u64)| stats::pub_stats(&log.cycles, w, n, period);
    let win = |w: (u64, u64)| json!({"start_ns": w.0, "end_ns": w.1});
    let out = json!({
        "transport": transport, "n": n, "period_s": period, "host": host,
        "phases": {"publish": win(pub_win), "read": win(read_w), "query": win(query_w), "write": win(write_w)},
        "read": read, "query": query, "write": write,
        "publish": {"idle": publ(pub_win), "during_read": publ(read_w), "during_query": publ(query_w), "during_write": publ(write_w),
                    "total_chunks": log.chunks, "malformed": log.malformed},
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("client") => client(&args[1..]),
        Some("mon") => mon::run(&args[1..]),
        Some("pull") => pull::run(&args[1..]),
        _ => {
            eprintln!("usage: ffbench client --transport zmq|nng --host H --n N --period S | mon --out FILE | pull --mode pull|push --transport zmq|nng --host H --rtus R --k K --period S");
            std::process::exit(2)
        }
    }
}
