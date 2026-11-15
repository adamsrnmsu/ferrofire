//! ffmaxrate: the observer of the maximum-publish-rate bench (ww-1df.43). It sits next to a REAL ferrofire provider and R REAL field devices (nothing is scripted)
//! and only watches: it joins every RTU's multicast group (one socket per group, so it sees the stream the devices see), polls RTU 1's counter register over Modbus,
//! and reads cpu, RSS and UDP counters of the provider and of every device from /proc (run it with `--pid host`: the processes are other containers').
//!
//! `ffmaxrate --groups 239.0.1.1:40000,239.0.1.2:40000 --fd-host rtu-1 --ir 30000 --period-ms 5 --step 1 --warm 3 --secs 15 --out /out/prefix`
//! Writes `<out>.metrics.csv` (metric,value) and `<out>.s2r.csv.gz`-to-be (`<out>.s2r.csv`: one stream-to-register latency in us per observed counter change).
//! The provider's `bench.cnt` is a tick counter mod 65536 (a register that is the integer sent); `_ff.seq/_ff.cyc/_ff.t` come from datagram v1's trailer.
//! Exit 2: the stream or the register never showed up.
#[path = "../../control/src/modbus.rs"]
mod modbus;
use ferrofire::bennu::unframe;
use modbus::Mb;
use socket2::{Domain, Socket, Type};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn arg(a: &[String], k: &str, d: &str) -> String {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned().unwrap_or(d.into())
}

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
}

/// Nearest-rank quantile of a sorted slice.
fn q(v: &[u64], p: f64) -> f64 {
    match v.len() {
        0 => f64::NAN,
        n => v[((n as f64 * p).ceil() as usize).saturating_sub(1).min(n - 1)] as f64,
    }
}

// ---- the stream ---------------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Dg {
    arr: u64,
    seq: u64,
    cyc: u64,
    t: u64,
    cnt: Option<u64>,
}

/// The value after `key` in a `tag:value,` body (a number up to the next comma).
fn field(body: &[u8], key: &[u8]) -> Option<u64> {
    let at = body.windows(key.len()).rposition(|w| w == key)? + key.len();
    let end = body[at..].iter().position(|&b| b == b',').unwrap_or(body.len() - at);
    std::str::from_utf8(&body[at..at + end]).ok()?.parse::<f64>().ok().map(|x| x as u64)
}

fn listen(group: &str, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<Vec<Dg>> {
    let (ip, port) = group.rsplit_once(':').expect("group ip:port");
    let (ip, port): (Ipv4Addr, u16) = (ip.parse().expect("group ip"), port.parse().expect("port"));
    let s = Socket::new(Domain::IPV4, Type::DGRAM, None).unwrap();
    s.set_reuse_address(true).unwrap();
    s.set_reuse_port(true).unwrap();
    s.bind(&SocketAddrV4::new(ip, port).into()).unwrap();   // bound to the group: only this group's datagrams
    s.join_multicast_v4(&ip, &Ipv4Addr::UNSPECIFIED).unwrap();
    let _ = s.set_recv_buffer_size(8 << 20);
    let sock: UdpSocket = s.into();
    sock.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    std::thread::spawn(move || {
        let (mut buf, mut out) = (vec![0u8; 65536], Vec::with_capacity(1 << 20));
        while !stop.load(SeqCst) {
            let Ok(n) = sock.recv(&mut buf) else { continue };
            let arr = now_ns();
            let Some((_, body)) = unframe(&buf[..n]) else { continue };
            let body = body.strip_suffix(&[0]).unwrap_or(body);
            let tail = &body[body.len().saturating_sub(96)..];   // the trailer is the last three points
            let (Some(seq), Some(cyc), Some(t)) = (field(tail, b"_ff.seq:"), field(tail, b"_ff.cyc:"), field(tail, b"_ff.t:")) else { continue };
            out.push(Dg { arr, seq, cyc, t, cnt: field(body, b"bench.cnt:") });
        }
        out
    })
}

// ---- the register poller --------------------------------------------------------------------------------------------

struct Polled {
    changes: Vec<(u64, u16)>,   // (response time ns, new value)
    rtt: Vec<u64>,              // ns, every poll
    errors: u64,
}

fn poll(host: String, ir: u16, stop: Arc<AtomicBool>, seen: Arc<AtomicBool>, pause: Arc<AtomicBool>) -> std::thread::JoinHandle<Polled> {
    std::thread::spawn(move || {
        let mut p = Polled { changes: vec![], rtt: Vec::with_capacity(1 << 20), errors: 0 };
        let mut mb = None;
        let (mut prev, mut moved) = (None, 0u32);
        while !stop.load(SeqCst) {
            if pause.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
                prev = None;
                continue;
            }
            if mb.is_none() {
                mb = Mb::connect(&host, 502).ok();
            }
            let Some(c) = mb.as_mut() else {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            let t0 = Instant::now();
            match c.read_regs(ir, 1) {
                Ok(r) => {
                    let t = now_ns();
                    p.rtt.push(t0.elapsed().as_nanos() as u64);
                    if prev.is_some_and(|x| x != r[0]) {
                        p.changes.push((t, r[0]));
                        moved += 1;
                        if moved >= 3 {
                            seen.store(true, SeqCst);
                        }
                    }
                    prev = Some(r[0]);
                }
                Err(_) => {
                    p.errors += 1;
                    mb = None;
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        p
    })
}

// ---- /proc ------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Snap {
    t: u64,
    cpu_ticks: u64,
    rss_kb: u64,
    hwm_kb: u64,
    in_dg: u64,
    out_dg: u64,
    rcvbuf_err: u64,
    sndbuf_err: u64,
    sock_drops: u64,
}

fn proc_snap(pid: u32) -> Snap {
    let mut s = Snap { t: now_ns(), ..Default::default() };
    if let Ok(st) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let f: Vec<&str> = st.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();   // f[0] = state (field 3)
        let n = |i: usize| f.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        s.cpu_ticks = n(11) + n(12);
    }
    for l in std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default().lines() {
        let kb = || l.split_whitespace().nth(1).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        if l.starts_with("VmRSS") {
            s.rss_kb = kb()
        } else if l.starts_with("VmHWM") {
            s.hwm_kb = kb()
        }
    }
    let snmp = std::fs::read_to_string(format!("/proc/{pid}/net/snmp")).unwrap_or_default();
    let udp: Vec<&str> = snmp.lines().filter(|l| l.starts_with("Udp:")).collect();
    if let [h, v] = udp[..] {
        let col = |n: &str| h.split_whitespace().position(|x| x == n).and_then(|i| v.split_whitespace().nth(i)?.parse::<u64>().ok()).unwrap_or(0);
        (s.in_dg, s.out_dg, s.rcvbuf_err, s.sndbuf_err) = (col("InDatagrams"), col("OutDatagrams"), col("RcvbufErrors"), col("SndbufErrors"));
    }
    // per-socket drops (the last column of /proc/net/udp) of every socket in that process's network namespace
    for l in std::fs::read_to_string(format!("/proc/{pid}/net/udp")).unwrap_or_default().lines().skip(1) {
        s.sock_drops += l.split_whitespace().last().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    }
    s
}

fn cmdline(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline")).map(|b| b.split(|&c| c == 0).map(|w| String::from_utf8_lossy(w).into_owned()).collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

/// (provider pid, device pids ordered by rtu number) from the processes of the whole VM.
fn find_pids(r: usize) -> (u32, Vec<u32>) {
    let (mut prov, mut fds) = (0, vec![0; r]);
    let me = std::process::id();
    for p in std::fs::read_dir("/proc").unwrap().filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok()).filter(|&p| p != me) {
        let c = cmdline(p);
        let mut w = c.split_whitespace();
        if !w.next().is_some_and(|x| x.ends_with("ferrofire")) {
            continue;
        }
        match w.next() {
            Some("run") => prov = p,
            Some("field-device") => {
                let n = c.rsplit("rtu-").next().and_then(|x| x.split('.').next()?.parse::<usize>().ok());
                if let Some(n) = n.filter(|n| (1..=r).contains(n)) {
                    fds[n - 1] = p
                }
            }
            _ => {}
        }
    }
    (prov, fds)
}

// ---- main ---------------------------------------------------------------------------------------------------------------

struct Out(Vec<String>);

impl Out {
    fn add(&mut self, k: &str, v: f64) {
        println!("{k} {v:.4}");
        self.0.push(format!("{k},{v:.4}"));
    }
}

fn proc_metrics(o: &mut Out, name: &str, a: Snap, b: Snap) {
    let wall = (b.t - a.t) as f64 / 1e9;
    o.add(&format!("{name}.cpu_pct"), (b.cpu_ticks - a.cpu_ticks) as f64 / 100.0 / wall * 100.0);
    o.add(&format!("{name}.rss_mb"), b.rss_kb as f64 / 1024.0);
    o.add(&format!("{name}.hwm_mb"), b.hwm_kb as f64 / 1024.0);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let groups: Vec<String> = arg(&a, "--groups", "239.0.1.1:40000").split(',').map(String::from).collect();
    let r = groups.len();
    let (host, ir) = (arg(&a, "--fd-host", "rtu-1"), arg(&a, "--ir", "30000").parse::<u16>().unwrap());
    let period_ms: f64 = arg(&a, "--period-ms", "5").parse().unwrap();
    let step: u64 = arg(&a, "--step", "1").parse().unwrap();   // counter step per publish cycle (publish period / dt)
    let (warm, secs): (u64, u64) = (arg(&a, "--warm", "3").parse().unwrap(), arg(&a, "--secs", "15").parse().unwrap());
    let out_prefix = arg(&a, "--out", "/tmp/maxrate");

    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicBool::new(false));
    let ls: Vec<_> = groups.iter().map(|g| listen(g, stop.clone())).collect();
    let pause = Arc::new(AtomicBool::new(false));
    let pl = poll(host, ir, stop.clone(), seen.clone(), pause.clone());
    let wait = Instant::now();
    while !seen.load(SeqCst) && wait.elapsed() < Duration::from_secs(90) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if !seen.load(SeqCst) {
        eprintln!("the counter register never moved");
        stop.store(true, SeqCst);
        std::process::exit(2);
    }
    std::thread::sleep(Duration::from_secs(warm));
    let (prov, fds) = find_pids(r);
    assert!(prov != 0 && fds.iter().all(|&p| p != 0), "processes not found: provider {prov} devices {fds:?}");
    // window A: nobody polls the device, so its cpu is the stream's alone. Window B: the tight Modbus poll is back (the stream-to-register probe).
    pause.store(true, SeqCst);
    std::thread::sleep(Duration::from_millis(500));
    let (p0, f0): (Snap, Vec<Snap>) = (proc_snap(prov), fds.iter().map(|&p| proc_snap(p)).collect());
    let me0 = proc_snap(std::process::id());
    std::thread::sleep(Duration::from_secs(secs));
    let (p1, f1): (Snap, Vec<Snap>) = (proc_snap(prov), fds.iter().map(|&p| proc_snap(p)).collect());
    let me1 = proc_snap(std::process::id());
    pause.store(false, SeqCst);
    std::thread::sleep(Duration::from_secs(1));
    let b0 = proc_snap(fds[0]);
    std::thread::sleep(Duration::from_secs(secs.min(10)));
    let b1 = proc_snap(fds[0]);
    stop.store(true, SeqCst);
    let recs: Vec<Vec<Dg>> = ls.into_iter().map(|h| h.join().unwrap()).collect();
    let polled = pl.join().unwrap();
    let (t0, t1) = (p0.t, p1.t);
    let (tb0, tb1) = (b0.t, b1.t);

    let mut o = Out(vec![]);
    o.add("window_s", (t1 - t0) as f64 / 1e9);
    o.add("rtus", r as f64);
    o.add("period_ms_target", period_ms);
    proc_metrics(&mut o, "prov", p0, p1);
    o.add("prov.udp_out", (p1.out_dg - p0.out_dg) as f64);
    o.add("prov.sndbuf_err", (p1.sndbuf_err - p0.sndbuf_err) as f64);
    o.add("obs.cpu_pct", (me1.cpu_ticks - me0.cpu_ticks) as f64 / 100.0 / ((me1.t - me0.t) as f64 / 1e9) * 100.0);
    let (mut sent_total, mut in_total, mut err_total) = (0u64, 0u64, 0u64);
    for (i, (g, (a, b))) in recs.iter().zip(f0.iter().zip(&f1)).enumerate() {
        let w: Vec<&Dg> = g.iter().filter(|d| d.arr >= t0 && d.arr < t1).collect();
        let name = format!("rtu{}", i + 1);
        proc_metrics(&mut o, &name, *a, *b);
        let (seen_n, span) = match (w.iter().map(|d| d.seq).min(), w.iter().map(|d| d.seq).max()) {
            (Some(lo), Some(hi)) => (w.len() as u64, hi - lo + 1),
            _ => (0, 0),
        };
        let (fin, ferr) = (b.in_dg - a.in_dg, b.rcvbuf_err - a.rcvbuf_err);
        o.add(&format!("{name}.dg_sent"), span as f64);              // seq span seen by the observer's own socket in the window
        o.add(&format!("{name}.dg_seen_by_observer"), seen_n as f64);
        o.add(&format!("{name}.seq_gaps_observer"), (span - seen_n) as f64);   // loss between provider and the observer's socket
        o.add(&format!("{name}.udp_in"), fin as f64);
        o.add(&format!("{name}.rcvbuf_errors"), ferr as f64);
        o.add(&format!("{name}.sock_drops"), (b.sock_drops - a.sock_drops) as f64);
        o.add(&format!("{name}.dg_lost"), span as f64 - fin as f64);   // sent minus delivered to the device's socket (window edges differ by a few)
        sent_total += span;
        in_total += fin;
        err_total += ferr;
    }
    o.add("dg_sent_total", sent_total as f64);
    o.add("dg_lost_total", sent_total as f64 - in_total as f64);
    o.add("dg_lost_pct", (sent_total as f64 - in_total as f64) / sent_total.max(1) as f64 * 100.0);
    o.add("rcvbuf_errors_total", err_total as f64);

    // cycles of group 1: the first datagram of each publish cycle, by the provider's own send clock
    let g1: Vec<&Dg> = recs[0].iter().filter(|d| d.arr >= t0 && d.arr < t1).collect();
    let mut cyc: Vec<(u64, u64)> = vec![];   // (cyc, t) first datagram of each cycle, in order
    for d in &g1 {
        match cyc.last_mut() {
            Some(l) if l.0 == d.cyc => l.1 = l.1.min(d.t),
            _ => cyc.push((d.cyc, d.t)),
        }
    }
    let period = period_ms * 1e6;
    if cyc.len() > 2 {
        let (first, last) = (cyc[0], cyc[cyc.len() - 1]);
        let wall = (last.1 - first.1) as f64 / 1e9;
        o.add("pub_rate_hz", (last.0 - first.0) as f64 / wall);
        o.add("pub_rate_target_hz", 1000.0 / period_ms);
        o.add("cycles_seen", cyc.len() as f64);
        o.add("cycles_missing_at_observer", ((last.0 - first.0 + 1) as f64) - cyc.len() as f64);
        let mut iv: Vec<u64> = cyc.windows(2).map(|w| w[1].1.saturating_sub(w[0].1) / (w[1].0 - w[0].0).max(1)).collect();   // per-cycle interval, ns
        iv.sort_unstable();
        o.add("interval_p50_ms", q(&iv, 0.5) / 1e6);
        o.add("interval_p99_ms", q(&iv, 0.99) / 1e6);
        o.add("interval_max_ms", *iv.last().unwrap() as f64 / 1e6);
        o.add("late_1p5x", iv.iter().filter(|&&x| x as f64 > 1.5 * period).count() as f64);   // intervals more than 1.5 periods: a tick overran
        o.add("late_pct", iv.iter().filter(|&&x| x as f64 > 1.5 * period).count() as f64 / iv.len() as f64 * 100.0);
        // the tick counter, unwrapped: ticks per second actually run
        let cn: Vec<(u64, u64)> = g1.iter().filter_map(|d| Some((d.t, d.cnt?))).collect();
        if let (Some(f), Some(l)) = (cn.first(), cn.last()) {
            let (mut tot, mut prev) = (0u64, f.1);
            for &(_, c) in &cn {
                tot += (c + 65536 - prev) % 65536;
                prev = c;
            }
            o.add("tick_rate_hz", tot as f64 / ((l.0 - f.0) as f64 / 1e9));
        }
    }

    // stream-to-register: each change of the register against the send time (the provider's _ff.t) of the datagram that carried that counter value
    let mut by_cnt: HashMap<u64, u64> = HashMap::new();
    let mut next = 0;
    let all: Vec<&Dg> = recs[0].iter().filter(|d| d.cnt.is_some()).collect();
    let (mut lat, mut neg, mut unmatched, mut skipped, mut prev) = (vec![], 0u64, 0u64, 0u64, None::<u16>);
    let mut file = String::new();
    for &(t, v) in polled.changes.iter().filter(|c| c.0 >= tb0 && c.0 < tb1) {
        while next < all.len() && all[next].arr <= t + 5_000_000 {
            by_cnt.insert(all[next].cnt.unwrap() % 65536, all[next].t);
            next += 1;
        }
        if let Some(p) = prev {
            let d = (v as u64 + 65536 - p as u64) % 65536;
            skipped += (d / step.max(1)).saturating_sub(1);
        }
        prev = Some(v);
        match by_cnt.get(&(v as u64)) {
            Some(&ts) if t >= ts => {
                lat.push(t - ts);
                file += &format!("{}\n", (t - ts) / 1000);
            }
            Some(_) => neg += 1,
            None => unmatched += 1,
        }
    }
    lat.sort_unstable();
    o.add("rtu1.cpu_pct_polled", (b1.cpu_ticks - b0.cpu_ticks) as f64 / 100.0 / ((b1.t - b0.t) as f64 / 1e9) * 100.0);   // device 1 while the probe polls it
    o.add("s2r_n", lat.len() as f64);
    o.add("s2r_p50_ms", q(&lat, 0.5) / 1e6);
    o.add("s2r_p95_ms", q(&lat, 0.95) / 1e6);
    o.add("s2r_p99_ms", q(&lat, 0.99) / 1e6);
    o.add("s2r_max_ms", lat.last().map_or(f64::NAN, |&x| x as f64 / 1e6));
    o.add("s2r_neg", neg as f64);
    o.add("s2r_unmatched", unmatched as f64);
    o.add("counter_steps_skipped", skipped as f64);   // register jumps of more than one publish cycle: a poll missed it or a datagram was lost
    let mut rtt = polled.rtt.clone();
    rtt.sort_unstable();
    o.add("poll_rtt_p50_us", q(&rtt, 0.5) / 1e3);
    o.add("poll_rtt_p99_us", q(&rtt, 0.99) / 1e3);
    o.add("polls", rtt.len() as f64);
    o.add("poll_errors", polled.errors as f64);
    std::fs::write(format!("{out_prefix}.metrics.csv"), "metric,value\n".to_string() + &o.0.join("\n") + "\n").unwrap();
    std::fs::write(format!("{out_prefix}.s2r.csv"), file).unwrap();
}
