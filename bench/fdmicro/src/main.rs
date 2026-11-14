//! fdmicro: the harness of the field-device microbench (ww-1df.41). One process, one clock (CLOCK_MONOTONIC), standing in for everything around ONE field device
//! (ferrofire or stock bennu-field-deviced, same config): a scripted provider (the datagram stream at a fixed rate, the probe point flipped on a known schedule, a
//! counter in every datagram) and a scripted REP that answers every WRITE with ACK and stamps its arrival, plus the Modbus and DNP3 clients that measure.
//! It runs in a container that shares the field device's PID namespace (`--pid container:...`), so it reads the device's cpu, RSS and UDP counters from /proc.
//!
//! `fdmicro --layout S.json --proto mb|dnp3 --stack ferrofire|stock --fd-host rtu --out /out/prefix [--rates 1,10,100] [--flips 100] [--secs 5] [--drop-secs 30]`
//! Writes `<out>.metrics.csv` (phase,variant,metric,value) and the per-sample files `<out>.s2r.csv`, `<out>.wf.csv`. Exit 2: the device never showed the stream.
#[path = "../../control/src/modbus.rs"]
mod modbus;
use ferrofire::bennu::{chunk_message, cycle_bytes, frame, grow_sndbuf, group_for, parse_udp, send_all, sleep_until, trailer, udp_sender, with_nul, CHUNK, STOP, TRAILER_MAX};
use ferrofire::fd::proto::dnp3::link;
use modbus::{bit, Mb};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STREAM: &str = "udp://*;239.0.0.1:40000";
/// Room kept in each chunk for the counter piece (`cntNNN:NNNNNNNN,`) prepended to it.
const CNT_ROOM: usize = 24;
const TICKS: f64 = 100.0;

fn arg(a: &[String], k: &str, d: &str) -> String {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned().unwrap_or(d.into())
}

fn us(d: Duration) -> u64 {
    d.as_nanos() as u64
}

/// Nearest-rank quantile of a sorted slice.
fn q(v: &[u64], p: f64) -> f64 {
    match v.len() {
        0 => f64::NAN,
        n => v[((n as f64 * p).ceil() as usize).saturating_sub(1).min(n - 1)] as f64,
    }
}

// ---- layout -------------------------------------------------------------------------------------------------

struct Tag {
    name: String,
    binary: bool,
}

struct Layout {
    tags: Vec<Tag>,
    kmax: usize,
    hz: f64,
    n_ir: usize,
    n_di: usize,
}

fn load_layout(p: &str) -> Layout {
    let j: Value = serde_json::from_str(&std::fs::read_to_string(p).expect("layout")).expect("layout json");
    let tags: Vec<Tag> = j["tags"].as_array().unwrap().iter().map(|t| Tag { name: t["name"].as_str().unwrap().into(), binary: t["kind"] == "binary" }).collect();
    let n_di = tags.iter().filter(|t| t.binary).count();
    Layout { kmax: j["kmax"].as_u64().unwrap() as usize, hz: j["hz"].as_f64().unwrap(), n_ir: tags.len() - n_di, n_di, tags }
}

/// What the provider sends for tag `i` (a non-probe, non-counter tag) in cycle `c`; the final-state check recomputes it.
fn analog_at(i: usize, c: u64) -> f64 {
    ((i as u64 * 37 + c * 13) % 100_000) as f64 / 100.0
}
fn binary_at(i: usize, c: u64) -> bool {
    (i as u64 + c) % 2 == 0
}

// ---- scripted provider ----------------------------------------------------------------------------------------

#[derive(Default)]
struct Prov {
    on: AtomicBool,
    quit: AtomicBool,
    hz: AtomicU64,
    flip_every: AtomicU64,
    cyc: AtomicU64,
    /// Datagrams (= counters in use) of the latest cycle.
    chunks: AtomicU64,
    sent: AtomicU64,
    dropped: AtomicU64,
    probe: AtomicBool,
    /// (flip id, send instant, new probe state) of every flip since the phase began.
    flips: Mutex<Vec<(usize, Instant, bool)>>,
    /// Every WRITE the REP saw: (arrival, body).
    writes: Mutex<Vec<(Instant, String)>>,
}

fn unix_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)
}

fn provider(p: Arc<Prov>, lay: Arc<Layout>) {
    let (iface, dest) = parse_udp(STREAM).expect("stream");
    let sock = udp_sender(iface.as_deref(), dest).expect("udp sender");
    let group = group_for(STREAM);
    let mut seq = 0u64;
    let mut deadline = Instant::now();
    let (mut cur_fe, mut next_flip, mut rng) = (0u64, 0u64, 99u64);
    while !p.quit.load(SeqCst) {
        if !p.on.load(SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
            deadline = Instant::now();
            continue;
        }
        let period = Duration::from_secs_f64(1.0 / f64::from_bits(p.hz.load(SeqCst)));
        deadline += period;
        if deadline < Instant::now() {
            deadline = Instant::now(); // fell behind: do not burst to catch up
        }
        sleep_until(deadline, Duration::from_micros(200));
        let c = p.cyc.fetch_add(1, SeqCst) + 1;
        // flips come `fe` cycles apart plus a random 0..=fe, so they do not line up with any period inside the device (its 50 ms cycle, a 1 s refresh)
        let fe = p.flip_every.load(SeqCst);
        if fe != cur_fe {
            (cur_fe, next_flip) = (fe, c + fe);
        }
        let flip = fe > 0 && c >= next_flip;
        if flip {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            next_flip = c + fe + (rng >> 33) % (fe + 1);
        }
        if flip {
            p.probe.fetch_xor(true, SeqCst);
        }
        let mut msg = format!("probe:{},", p.probe.load(SeqCst));
        for (i, t) in lay.tags.iter().enumerate().skip(1 + lay.kmax) {
            match t.binary {
                true => msg += &format!("{}:{},", t.name, binary_at(i, c)),
                false => msg += &format!("{}:{:.2},", t.name, analog_at(i, c)),
            }
        }
        let t_ns = unix_ns();
        let ds: Vec<Vec<u8>> = chunk_message(&msg, CHUNK - TRAILER_MAX - CNT_ROOM)
            .into_iter()
            .enumerate()
            .map(|(k, ch)| {
                assert!(k < lay.kmax, "cycle needs {} chunks, layout has {} counters", k + 1, lay.kmax);
                let mut b = format!("cnt{k}:{c},{ch}{}", trailer(seq + k as u64, c, t_ns)).into_bytes();
                b.push(0);
                frame(&group, &b)
            })
            .collect();
        seq += ds.len() as u64;
        p.chunks.store(ds.len() as u64, SeqCst);
        grow_sndbuf(&sock, cycle_bytes(&ds));
        if flip {
            let mut f = p.flips.lock().unwrap();
            let id = f.len();
            f.push((id, Instant::now(), p.probe.load(SeqCst)));
        }
        let (dropped, _) = send_all(&ds, |d| sock.send_to(d, dest), |_| { std::thread::sleep(Duration::from_micros(200)); true }, Duration::from_millis(10));
        p.dropped.fetch_add(dropped, SeqCst);
        p.sent.fetch_add(ds.len() as u64 - dropped, SeqCst);
    }
}

fn start_rep(p: Arc<Prov>) {
    let handler = Box::new(move |req: &[u8]| {
        let t = Instant::now();
        p.writes.lock().unwrap().push((t, String::from_utf8_lossy(req).trim_end_matches('\0').to_string()));
        with_nul("ACK=".into())
    });
    ferrofire::transport::serve("tcp://*:5555", handler).expect("REP bind");
}

// ---- the device under test, seen from /proc ----------------------------------------------------------------------

struct Dev {
    pids: Vec<u32>,
}

#[derive(Clone, Copy, Default)]
struct Snap {
    cpu_ticks: u64,
    rss_kb: u64,
    hwm_kb: u64,
    in_dgrams: u64,
    rcvbuf_err: u64,
    t: Option<Instant>,
}

impl Dev {
    fn find(comm: &str) -> Dev {
        let pids = std::fs::read_dir("/proc").unwrap().filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).is_ok_and(|c| c.trim().starts_with(comm))).collect();
        Dev { pids }
    }
    fn snap(&self) -> Snap {
        let mut s = Snap { t: Some(Instant::now()), ..Default::default() };
        for p in &self.pids {
            if let Ok(st) = std::fs::read_to_string(format!("/proc/{p}/stat")) {
                let f: Vec<&str> = st.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect(); // f[0] = state (field 3)
                s.cpu_ticks += f.get(11).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0) + f.get(12).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
            }
            for l in std::fs::read_to_string(format!("/proc/{p}/status")).unwrap_or_default().lines() {
                let kb = |l: &str| l.split_whitespace().nth(1).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
                if l.starts_with("VmRSS") { s.rss_kb += kb(l) } else if l.starts_with("VmHWM") { s.hwm_kb += kb(l) }
            }
        }
        if let Some(p) = self.pids.first() {
            let snmp = std::fs::read_to_string(format!("/proc/{p}/net/snmp")).unwrap_or_default();
            let udp: Vec<&str> = snmp.lines().filter(|l| l.starts_with("Udp:")).collect();
            if let [h, v] = udp[..] {
                let col = |n: &str| h.split_whitespace().position(|x| x == n).and_then(|i| v.split_whitespace().nth(i)?.parse::<u64>().ok()).unwrap_or(0);
                s.in_dgrams = col("InDatagrams");
                s.rcvbuf_err = col("RcvbufErrors");
            }
        }
        s
    }
}

struct Rec {
    rows: Vec<String>,
}

impl Rec {
    fn add(&mut self, phase: &str, variant: &str, metric: &str, v: f64) {
        println!("{phase} {variant} {metric} {v:.3}");
        self.rows.push(format!("{phase},{variant},{metric},{v:.4}"));
    }
    /// cpu (percent of one core), RSS and UDP counters of the device over [a, b].
    fn window(&mut self, phase: &str, variant: &str, a: Snap, b: Snap, sent: u64) {
        let wall = b.t.unwrap().duration_since(a.t.unwrap()).as_secs_f64();
        self.add(phase, variant, "cpu_pct", (b.cpu_ticks - a.cpu_ticks) as f64 / TICKS / wall * 100.0);
        self.add(phase, variant, "rss_mb", b.rss_kb as f64 / 1024.0);
        self.add(phase, variant, "rss_hwm_mb", b.hwm_kb as f64 / 1024.0);
        self.add(phase, variant, "udp_in", (b.in_dgrams - a.in_dgrams) as f64);
        self.add(phase, variant, "udp_sent", sent as f64);
        self.add(phase, variant, "rcvbuf_errors", (b.rcvbuf_err - a.rcvbuf_err) as f64);
    }
    fn dist(&mut self, phase: &str, variant: &str, name: &str, mut v: Vec<u64>, scale: f64) {
        v.sort_unstable();
        for (m, p) in [("p50", 0.5), ("p95", 0.95), ("p99", 0.99)] {
            self.add(phase, variant, &format!("{name}_{m}"), q(&v, p) / scale);
        }
        self.add(phase, variant, &format!("{name}_max"), v.last().copied().unwrap_or(0) as f64 / scale);
        self.add(phase, variant, &format!("{name}_n"), v.len() as f64);
    }
}

fn connect_retry(mut f: impl FnMut() -> std::io::Result<()>, what: &str) {
    let end = Instant::now() + Duration::from_secs(60);
    while let Err(e) = f() {
        assert!(Instant::now() < end, "{what}: never came up: {e}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn set_stream(p: &Prov, on: bool, hz: f64, flip_every: u64) {
    p.hz.store(hz.to_bits(), SeqCst);
    p.flip_every.store(flip_every, SeqCst);
    p.flips.lock().unwrap().clear();
    p.on.store(on, SeqCst);
}

/// The stream is up when every counter register shows a cycle number; exit 2 if it never does.
fn wait_stream(p: &Prov, lay: &Layout, host: &str) {
    let mut mb = Mb::connect(host, 502).expect("modbus");
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
        if mb.read_regs(30000, 1).is_ok_and(|r| r[0] > 0) && p.cyc.load(SeqCst) > 0 {
            return;
        }
    }
    eprintln!("stream never reached the device (counter register stayed 0; {} cycles sent)", p.cyc.load(SeqCst));
    let _ = lay;
    std::process::exit(2);
}

// ---- phases: modbus ------------------------------------------------------------------------------------------------

fn idle_and_stream(r: &mut Rec, dev: &Dev, p: &Prov, lay: &Layout, host: &str, secs: u64) {
    let (a, t) = (dev.snap(), Instant::now());
    std::thread::sleep(Duration::from_secs(secs));
    r.window("idle", "-", a, dev.snap(), 0);
    let _ = t;
    set_stream(p, true, lay.hz, 0);
    wait_stream(p, lay, host);
    std::thread::sleep(Duration::from_secs(3));
    let (a, s0) = (dev.snap(), p.sent.load(SeqCst));
    std::thread::sleep(Duration::from_secs(secs));
    r.window("stream", &format!("{}hz", lay.hz), a, dev.snap(), p.sent.load(SeqCst) - s0);
}

/// Stream-to-register: the delay from the send of the datagram carrying a probe flip to the first read that shows it (an upper bound: the register
/// changed somewhere inside the last read's round trip). The poll is a tight loop on one connection.
fn s2r(r: &mut Rec, raw: &mut Vec<String>, dev: &Dev, p: &Prov, lay: &Layout, host: &str, hz: f64, flips: usize) {
    let every = ((hz * 0.3).round() as u64).max(1); // a flip 0.3-0.6 s apart (1-2 s at 1 Hz)
    let flips = if hz <= 1.0 { flips.min(30) } else { flips }; // a flip a second at 1 Hz
    let variant = format!("{hz}hz");
    let mut mb = Mb::connect(host, 502).expect("modbus");
    set_stream(p, true, hz, 0);
    std::thread::sleep(Duration::from_millis(500));
    let mut last = bit(&mb.read_bits(0, 1).unwrap(), 0);
    if p.probe.load(SeqCst) != last { /* the previous phase left the probe flipped: the first flip below corrects for it */ }
    p.probe.store(last, SeqCst);
    let (a, s0) = (dev.snap(), p.sent.load(SeqCst));
    p.flip_every.store(every, SeqCst);
    let (mut delays, mut seen, mut polls, mut unmatched) = (vec![], vec![false; flips], 0u64, 0u64);
    let mut gap = Duration::ZERO;
    let mut t_prev = Instant::now();
    let t_begin = t_prev;
    loop {
        let b = match mb.read_bits(0, 1) { Ok(b) => bit(&b, 0), Err(e) => { eprintln!("s2r read: {e}"); break } };
        let t = Instant::now();
        polls += 1;
        gap = gap.max(t - t_prev);
        t_prev = t;
        let f = p.flips.lock().unwrap();
        if f.len() >= flips { p.flip_every.store(0, SeqCst); } // enough flips: stop the schedule, then wait for the last one
        if b != last {
            last = b;
            match f.iter().rev().find(|x| x.2 == b && x.1 <= t) {
                Some(&(id, ts, _)) if id < flips && !seen[id] => { seen[id] = true; delays.push(us(t - ts)); raw.push(format!("{variant},{id},{}", us(t - ts))); }
                _ => unmatched += 1,
            }
        }
        let done = f.len() >= flips;
        let quiet = f.last().is_some_and(|x| t.duration_since(x.1) > Duration::from_secs(2));
        drop(f);
        if done && quiet || t - t_begin > Duration::from_secs_f64(flips as f64 * (2.0 * every as f64 / hz) + 10.0) {
            break;
        }
    }
    let b = dev.snap();
    let sent = p.flips.lock().unwrap().len().min(flips);
    p.flip_every.store(0, SeqCst);
    for (id, s) in seen.iter().enumerate().take(sent) {
        if !s { raw.push(format!("{variant},{id},-1")); }
    }
    r.add("s2r", &variant, "flips", sent as f64);
    r.add("s2r", &variant, "missed", (sent - delays.len()) as f64);
    r.add("s2r", &variant, "unmatched_changes", unmatched as f64);
    r.add("s2r", &variant, "polls_per_s", polls as f64 / (t_prev - t_begin).as_secs_f64());
    r.add("s2r", &variant, "max_poll_gap_ms", gap.as_secs_f64() * 1e3);
    let mean = delays.iter().sum::<u64>() as f64 / delays.len().max(1) as f64;
    r.add("s2r", &variant, "mean_ms", mean / 1e6);
    r.dist("s2r", &variant, "ms", delays, 1e6);
    r.window("s2r", &variant, a, b, p.sent.load(SeqCst) - s0);
    let _ = lay;
}

/// Read latency and throughput: `clients` connections each in a tight loop of FC4 reads of `n` registers for `secs`.
fn reads(r: &mut Rec, dev: &Dev, p: &Prov, host: &str, clients: usize, n: u16, secs: u64) {
    let variant = format!("c{clients}_n{n}");
    let barrier = Arc::new(std::sync::Barrier::new(clients + 1));
    let handles: Vec<_> = (0..clients).map(|_| {
        let (h, b) = (host.to_string(), barrier.clone());
        std::thread::spawn(move || {
            let mut lat = Vec::with_capacity(1 << 16);
            let mut errs = 0u64;
            let mut mb = Mb::connect(&h, 502).ok();
            b.wait();
            let end = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < end {
                let Some(m) = mb.as_mut() else { errs += 1; break };
                let t = Instant::now();
                match m.read_regs(30000, n) {
                    Ok(_) => lat.push(t.elapsed().as_nanos() as u32),
                    Err(_) => { errs += 1; mb = Mb::connect(&h, 502).ok(); }
                }
            }
            (lat, errs, mb.is_some())
        })
    }).collect();
    barrier.wait();
    let (a, s0) = (dev.snap(), p.sent.load(SeqCst));
    let res: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let b = dev.snap();
    let all: Vec<u64> = res.iter().flat_map(|x| x.0.iter().map(|&v| v as u64)).collect();
    r.add("read", &variant, "req_per_s", all.len() as f64 / secs as f64);
    r.add("read", &variant, "errors", res.iter().map(|x| x.1).sum::<u64>() as f64);
    r.add("read", &variant, "clients_served", res.iter().filter(|x| !x.0.is_empty()).count() as f64);
    r.dist("read", &variant, "us", all, 1e3);
    r.window("read", &variant, a, b, p.sent.load(SeqCst) - s0);
}

/// Write-forward: a Modbus FC5 write of coil 0 (alternating) to the arrival of the WRITE= on the scripted REP. One write at a time. `spaced` waits 1.2 s or more
/// between writes (every write lands at a random phase of an idle device); `rapid` writes every 60-150 ms, which shows whether a device forwards every write or
/// coalesces. A write whose WRITE= has not arrived after `wait` is counted lost.
fn write_forward(r: &mut Rec, raw: &mut Vec<String>, dev: &Dev, p: &Prov, host: &str, label: &str, gap_us: (u64, u64), wait: Duration, n: usize) {
    let mut mb = Mb::connect(host, 502).expect("modbus");
    let (a, s0) = (dev.snap(), p.sent.load(SeqCst));
    let (mut fwd, mut ack, mut lost, mut rng, mut on) = (vec![], vec![], 0u64, 12345u64, false);
    for i in 0..n {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        std::thread::sleep(Duration::from_micros(gap_us.0 + (rng >> 33) % gap_us.1));
        on = !on;
        let want = format!("cmd0:{on}");
        p.writes.lock().unwrap().clear();
        let t = Instant::now();
        if mb.write_coil(0, on).is_err() { lost += 1; continue; }
        ack.push(us(t.elapsed()));
        let end = t + wait;
        let got = loop {
            if let Some(x) = p.writes.lock().unwrap().iter().find(|w| w.1.contains(&want)) { break Some(x.0); }
            if Instant::now() > end { break None; }
            std::thread::sleep(Duration::from_micros(50));
        };
        match got {
            Some(ta) => { fwd.push(us(ta.duration_since(t))); raw.push(format!("{label},{i},{}", us(ta.duration_since(t)))); }
            None => { lost += 1; raw.push(format!("{label},{i},-1")); }
        }
    }
    r.add("write", label, "writes", n as f64);
    r.add("write", label, "lost", lost as f64);
    r.dist("write", label, "forward_ms", fwd, 1e6);
    r.dist("write", label, "ack_us", ack, 1e3);
    r.window("write", label, a, dev.snap(), p.sent.load(SeqCst) - s0);
}

/// Drops: stream for `secs` while one client reads every counter in a tight loop. Counter steps of more than 1 are cycles whose value was never visible
/// (lost, or superseded before the device published them); the kernel counters and the end-state check say which. After the stream stops every register
/// is read and compared with what the last cycle sent.
fn drops(r: &mut Rec, dev: &Dev, p: &Prov, lay: &Layout, host: &str, secs: u64) {
    let mut mb = Mb::connect(host, 502).expect("modbus");
    set_stream(p, true, lay.hz, 0);
    std::thread::sleep(Duration::from_millis(300));
    let nk = p.chunks.load(SeqCst) as usize; // counters in use: one per datagram of a cycle
    let (a, s0, c0, d0) = (dev.snap(), p.sent.load(SeqCst), p.cyc.load(SeqCst), p.dropped.load(SeqCst));
    let (mut last, mut steps, mut skipped, mut changes, mut gap, mut errs) = (vec![0u16; nk], 0u64, 0u64, 0u64, Duration::ZERO, 0u64);
    let mut by_idx = vec![0u64; nk];
    let (t0, mut tp) = (Instant::now(), Instant::now());
    while t0.elapsed() < Duration::from_secs(secs) {
        let v: std::io::Result<Vec<u16>> = (0..nk).step_by(120).try_fold(vec![], |mut acc, from| {
            acc.extend(mb.read_regs(30000 + from as u16, (nk - from).min(120) as u16)?);
            Ok(acc)
        });
        match v {
            Ok(v) => {
                let t = Instant::now();
                gap = gap.max(t - tp);
                tp = t;
                for (i, &x) in v.iter().enumerate() {
                    if x != last[i] {
                        if last[i] != 0 && x > last[i] { skipped += (x - last[i] - 1) as u64; by_idx[i] += (x - last[i] - 1) as u64; }
                        last[i] = x;
                        changes += 1;
                    }
                }
                steps += 1;
            }
            Err(_) => errs += 1,
        }
    }
    let b = dev.snap();
    let (cyc, dropped) = (p.cyc.load(SeqCst) - c0, p.dropped.load(SeqCst) - d0);
    p.on.store(false, SeqCst);
    std::thread::sleep(Duration::from_secs(1));
    let sent_total = p.sent.load(SeqCst);
    let c = p.cyc.load(SeqCst);
    // end state: every counter, binary and analog register against the last cycle
    let mut bad = 0u64;
    let mut ir_at = 30000u16;
    let mut regs = vec![];
    while (ir_at as usize) < 30000 + lay.n_ir {
        let n = (30000 + lay.n_ir - ir_at as usize).min(120) as u16;
        match mb.read_regs(ir_at, n) { Ok(v) => regs.extend(v), Err(_) => break }
        ir_at += n;
    }
    let (mut di, mut bits) = (0u16, vec![]);
    while (di as usize) < lay.n_di {
        let n = (lay.n_di - di as usize).min(1900) as u16;
        match mb.read_bits(di, n) { Ok(v) => bits.extend((0..n as usize).map(|i| bit(&v, i))), Err(_) => break }
        di += n;
    }
    let (mut ai, mut bi) = (0usize, 1usize); // analog register index, binary index (0 = probe)
    for (i, t) in lay.tags.iter().enumerate() {
        if i == 0 { continue; }
        if t.binary {
            bad += (bits.get(bi) != Some(&binary_at(i, c))) as u64;
            bi += 1;
        } else if i <= lay.kmax {
            bad += (i <= nk && regs.get(ai) != Some(&(c as u16))) as u64; // counters past the cycle's datagram count are never sent
            ai += 1;
        } else {
            // analog: scaled 0..65535 over 0..1000; both stacks truncate, allow 2 counts of rounding
            let want = (analog_at(i, c) * 65.535).floor() as i64;
            bad += regs.get(ai).is_none_or(|&g| (g as i64 - want).abs() > 2) as u64;
            ai += 1;
        }
    }
    let mut top: Vec<(usize, u64)> = by_idx.iter().copied().enumerate().filter(|x| x.1 > 0).collect();
    top.sort_by_key(|x| std::cmp::Reverse(x.1));
    eprintln!("counter skips by datagram index of the cycle (top 5 of {}): {:?}", top.len(), &top[..top.len().min(5)]);
    let variant = format!("{}hz", lay.hz);
    r.add("drops", &variant, "cycles_sent", cyc as f64);
    r.add("drops", &variant, "sender_dropped", dropped as f64);
    r.add("drops", &variant, "counter_steps_skipped", skipped as f64);
    r.add("drops", &variant, "counter_changes", changes as f64);
    r.add("drops", &variant, "counter_polls", steps as f64);
    r.add("drops", &variant, "poll_errors", errs as f64);
    r.add("drops", &variant, "max_poll_gap_ms", gap.as_secs_f64() * 1e3);
    r.add("drops", &variant, "end_state_bad_regs", bad as f64);
    r.add("drops", &variant, "end_state_checked", (lay.tags.len() - 1 - (lay.kmax - nk)) as f64);
    r.add("drops", &variant, "datagrams_per_cycle", nk as f64);
    r.window("drops", &variant, a, b, sent_total - s0);
}

// ---- phases: dnp3 -------------------------------------------------------------------------------------------------------

struct Master {
    s: TcpStream,
    seq: u8,
}

impl Master {
    fn connect(host: &str) -> std::io::Result<Master> {
        let s = TcpStream::connect((host, 20000))?;
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(Master { s, seq: 0 })
    }

    /// One transport-layer message (link frames up to the one with the transport FIN bit): its application data and the bytes it took on the wire.
    fn fragment(&mut self) -> std::io::Result<(Vec<u8>, usize)> {
        let (mut app, mut wire, mut buf) = (vec![], 0usize, vec![]);
        loop {
            while buf.len() >= 10 {
                let user = (buf[2] as usize).saturating_sub(5);
                let n = link::wire_len(user);
                if buf.len() < n { break; }
                let f: Vec<u8> = buf.drain(..n).collect();
                wire += n;
                let (mut data, mut j) = (vec![], 10);
                while data.len() < user {
                    let take = (user - data.len()).min(16);
                    data.extend_from_slice(&f[j..j + take]);
                    j += take + 2;
                }
                if data.is_empty() { continue; }
                app.extend_from_slice(&data[1..]);
                if data[0] & 0x80 != 0 { return Ok((app, wire)); }
            }
            let mut tmp = [0u8; 65536];
            let k = self.s.read(&mut tmp)?;
            if k == 0 { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
            buf.extend_from_slice(&tmp[..k]);
        }
    }

    /// One class-0 integrity poll (READ g60v1 all) to the last application fragment, confirming each fragment that asks for it:
    /// the first fragment's application data and the bytes of the whole response on the wire.
    fn poll(&mut self) -> std::io::Result<(Vec<u8>, usize)> {
        let (tp, ap) = (0xC0 | (self.seq & 0x3F), 0xC0 | (self.seq & 0x0F));
        self.seq = self.seq.wrapping_add(1);
        self.s.write_all(&link::encode(0xC4, 10, 1, &[tp, ap, 1, 60, 1, 6]))?;
        let (mut first, mut wire) = (None, 0usize);
        loop {
            let (app, w) = self.fragment()?;
            wire += w;
            if app.len() < 2 || app[1] != 0x81 { continue; } // not a solicited response (an unsolicited one is not confirmed: the master never enabled them)
            let ctl = app[0];
            if ctl & 0x20 != 0 { // CON: confirm this fragment's sequence number
                self.s.write_all(&link::encode(0xC4, 10, 1, &[0xC0 | (self.seq & 0x3F), 0xC0 | (ctl & 0x0F), 0]))?;
                self.seq = self.seq.wrapping_add(1);
            }
            first.get_or_insert(app);
            if ctl & 0x40 != 0 { return Ok((first.unwrap(), wire)); } // FIN
        }
    }
}

/// State of binary input 0 in a class-0 response (g1v1 packed or g1v2 with flags), or None if the response does not carry it.
fn probe_in(app: &[u8]) -> Option<bool> {
    let mut o = app.get(4..)?; // application control, function, IIN
    while o.len() >= 3 {
        let (g, v, qual) = (o[0], o[1], o[2]);
        let (start, stop, hdr) = match qual { 0 => (*o.get(3)? as usize, *o.get(4)? as usize, 5), 1 => (u16::from_le_bytes([*o.get(3)?, *o.get(4)?]) as usize, u16::from_le_bytes([*o.get(5)?, *o.get(6)?]) as usize, 7), _ => return None };
        let n = stop + 1 - start;
        let size = match (g, v) { (1, 1) => n.div_ceil(8), (1, 2) => n, (30, 1) | (30, 5) => 5 * n, (30, 2) => 3 * n, (30, 3) => 4 * n, (30, 4) => 2 * n, (30, 6) => 9 * n, _ => return None };
        if g == 1 && start == 0 {
            let b = *o.get(hdr)?;
            return Some(if v == 1 { b & 1 == 1 } else { b & 0x80 != 0 });
        }
        o = o.get(hdr + size..)?;
    }
    None
}

fn dnp3(r: &mut Rec, raw: &mut Vec<String>, dev: &Dev, p: &Prov, lay: &Layout, host: &str, secs: u64, flips: usize) {
    let mut m = None;
    connect_retry(|| { m = Some(Master::connect(host)?); Ok(()) }, "dnp3");
    let mut m = m.unwrap();
    let (a, t0) = (dev.snap(), Instant::now());
    std::thread::sleep(Duration::from_secs(secs));
    r.window("idle", "-", a, dev.snap(), 0);
    let _ = t0;
    set_stream(p, true, lay.hz, 0);
    // the first poll that shows a non-zero counter: the stream has arrived (every point is published once a second at most)
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end && m.poll().is_ok_and(|(a, _)| a.len() < 8) { }
    std::thread::sleep(Duration::from_secs(3));
    let (a, s0) = (dev.snap(), p.sent.load(SeqCst));
    std::thread::sleep(Duration::from_secs(secs));
    let variant = format!("{}hz", lay.hz);
    r.window("stream", &variant, a, dev.snap(), p.sent.load(SeqCst) - s0);
    // class-0 poll latency, one master, a poll every 20 ms
    let (mut lat, mut bytes, mut errs) = (vec![], 0usize, 0u64);
    for _ in 0..300 {
        let t = Instant::now();
        match m.poll() { Ok((_, w)) => { lat.push(us(t.elapsed())); bytes = w; } Err(_) => errs += 1 }
        std::thread::sleep(Duration::from_millis(20));
    }
    r.add("dnp3read", &variant, "errors", errs as f64);
    r.add("dnp3read", &variant, "response_bytes", bytes as f64);
    r.dist("dnp3read", &variant, "ms", lat, 1e6);
    // throughput: back-to-back polls
    let (a, t0, mut n) = (dev.snap(), Instant::now(), 0u64);
    let mut lat = vec![];
    while t0.elapsed() < Duration::from_secs(secs) {
        let t = Instant::now();
        if m.poll().is_ok() { n += 1; lat.push(us(t.elapsed())); }
    }
    r.add("dnp3tight", &variant, "polls_per_s", n as f64 / secs as f64);
    r.dist("dnp3tight", &variant, "ms", lat, 1e6);
    r.window("dnp3tight", &variant, a, dev.snap(), 0);
    // stream-to-visible through DNP3: probe flips about every 1.5 s, one master polling class 0 back to back
    let every = ((lay.hz * 1.5).round() as u64).max(1); // 1.5-3 s apart
    p.flips.lock().unwrap().clear();
    let mut last = probe_in(&m.poll().unwrap().0).unwrap_or(false);
    p.probe.store(last, SeqCst);
    p.flip_every.store(every, SeqCst);
    let (mut delays, mut seen, mut unmatched) = (vec![], vec![false; flips], 0u64);
    let t_begin = Instant::now();
    loop {
        let Ok((app, _)) = m.poll() else { break };
        let t = Instant::now();
        let Some(b) = probe_in(&app) else { continue };
        let f = p.flips.lock().unwrap();
        if f.len() >= flips { p.flip_every.store(0, SeqCst); } // enough flips: stop the schedule, then wait for the last one
        if b != last {
            last = b;
            match f.iter().rev().find(|x| x.2 == b && x.1 <= t) {
                Some(&(id, ts, _)) if id < flips && !seen[id] => { seen[id] = true; delays.push(us(t - ts)); raw.push(format!("dnp3-{variant},{id},{}", us(t - ts))); }
                _ => unmatched += 1,
            }
        }
        let quiet = f.last().is_some_and(|x| t.duration_since(x.1) > Duration::from_secs(3));
        if f.len() >= flips && quiet || t - t_begin > Duration::from_secs_f64(flips as f64 * 3.0 + 15.0) { break; }
    }
    p.flip_every.store(0, SeqCst);
    let sent = p.flips.lock().unwrap().len().min(flips);
    r.add("dnp3s2r", &variant, "flips", sent as f64);
    r.add("dnp3s2r", &variant, "missed", (sent - delays.len()) as f64);
    r.add("dnp3s2r", &variant, "unmatched_changes", unmatched as f64);
    r.dist("dnp3s2r", &variant, "ms", delays, 1e6);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let lay = Arc::new(load_layout(&arg(&a, "--layout", "")));
    let (proto, host, out, stack) = (arg(&a, "--proto", "mb"), arg(&a, "--fd-host", "rtu"), arg(&a, "--out", "/tmp/fdmicro"), arg(&a, "--stack", "ferrofire"));
    let secs: u64 = arg(&a, "--secs", "5").parse().unwrap();
    let flips: usize = arg(&a, "--flips", "100").parse().unwrap();
    let rates: Vec<f64> = arg(&a, "--rates", "").split(',').filter_map(|x| x.parse().ok()).collect();
    let comm = if stack == "ferrofire" { "ferrofire" } else { "bennu-field-d" };
    let dev = Dev::find(comm);
    assert!(!dev.pids.is_empty(), "no process named {comm} in the shared PID namespace");
    let p = Arc::new(Prov { hz: AtomicU64::new(lay.hz.to_bits()), ..Default::default() });
    start_rep(p.clone());
    let (p2, l2) = (p.clone(), lay.clone());
    std::thread::spawn(move || provider(p2, l2));
    let (mut r, mut raw_s2r, mut raw_wf) = (Rec { rows: vec![] }, vec![], vec![]);
    if proto == "dnp3" {
        dnp3(&mut r, &mut raw_s2r, &dev, &p, &lay, &host, secs.max(5), flips.min(30));
    } else {
        connect_retry(|| Mb::connect(&host, 502).map(|_| ()), "modbus");
        idle_and_stream(&mut r, &dev, &p, &lay, &host, 10);
        for hz in if rates.is_empty() { vec![lay.hz] } else { rates } {
            s2r(&mut r, &mut raw_s2r, &dev, &p, &lay, &host, hz, flips);
        }
        set_stream(&p, true, lay.hz, 0);
        let big = lay.n_ir.min(100) as u16;
        for clients in [1, 4, 16] {
            for n in [1, big] {
                reads(&mut r, &dev, &p, &host, clients, n, secs);
            }
        }
        write_forward(&mut r, &mut raw_wf, &dev, &p, &host, "spaced", (1_200_000, 100_000), Duration::from_secs(1), 60);
        write_forward(&mut r, &mut raw_wf, &dev, &p, &host, "rapid", (60_000, 90_000), Duration::from_millis(250), 100);
        drops(&mut r, &dev, &p, &lay, &host, 30);
    }
    r.add("meta", "-", "device_pids", dev.pids.len() as f64);
    std::fs::write(format!("{out}.metrics.csv"), "phase,variant,metric,value\n".to_string() + &r.rows.join("\n") + "\n").unwrap();
    let cat = |hdr: &str, rows: &[String]| hdr.to_string() + "\n" + &rows.join("\n") + "\n";
    std::fs::write(format!("{out}.s2r.csv"), cat("variant,flip,delay_ns", &raw_s2r)).unwrap();
    if proto != "dnp3" {
        std::fs::write(format!("{out}.wf.csv"), cat("variant,write,delay_ns", &raw_wf)).unwrap();
    }
    p.quit.store(true, SeqCst);
    STOP.store(true, SeqCst);
}
