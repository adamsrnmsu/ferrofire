//! `ffbench dg`: the datagram v1 benchmark client (ww-otj.18), plus `inject` (garbage into the multicast group) and `req`.
//!
//! `dg` runs a set of streams side by side against one provider, each with a `ferrofire::datagram::Receiver` (the real
//! receiver rules): `--whole` = one multicast stream of every tag, `--slices R` = R per-RTU multicast groups of `--k` tags,
//! `--pull R` = R real `ferrofire pull-bridge` processes (K tags) whose republished datagrams are received on loopback.
//! Per stream it reports delivered cycles, staleness (gap between complete cycles, gap between arrivals of 8 probe tags), `_ff.seq` gaps
//! (datagram loss), reject counters and, with `--restart-at`, the time to the first complete cycle after a leave/rejoin.
use crate::pull::rtu_tags;
use crate::{arg, now_ns, stats, Cycle, Req};
use ferrofire::bennu::{frame, group_for, unframe};
use ferrofire::datagram::{Receiver, Reject};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Stream {
    pub name: String,
    pub kind: &'static str,
    pub endpoint: String,
    pub port: u16,
    pub mcast: bool,
    pub tags: Vec<String>,
}

/// `_ff.seq` bookkeeping that does not care about order: lost = span - distinct. A newer `_ff.t` with a lower seq is a
/// provider restart and starts a new segment; a datagram far older than the newest is a replay and is ignored.
#[derive(Default)]
pub struct SeqTrack {
    seen: HashSet<u64>,
    span: Option<(u64, u64, u64)>, // min seq, max seq, max t
    got: u64,
    lost: u64,
    pub restarts: u64,
    pub epoch: u64,
}

const REPLAY_NS: u64 = 10_000_000_000;

impl SeqTrack {
    /// Record a datagram; true when it is new (not a repeat of a seq, not an ancient replay).
    pub fn push(&mut self, seq: u64, t: u64) -> bool {
        if let Some((_, hi, ht)) = self.span {
            if t + REPLAY_NS < ht {
                return false;
            }
            if seq < hi && t > ht {
                self.restarts += 1;
                self.close();
            }
        }
        let new = self.seen.insert(seq);
        let (lo, hi, ht) = self.span.unwrap_or((seq, seq, t));
        self.span = Some((lo.min(seq), hi.max(seq), ht.max(t)));
        new
    }

    /// End the current segment without counting what was missed after it (a subscriber that left, a provider that restarted).
    pub fn close(&mut self) {
        let (g, l) = self.totals();
        (self.got, self.lost) = (g, l);
        self.seen.clear();
        self.span = None;
        self.epoch += 1;
    }

    /// (datagrams seen, datagrams missing inside the seen ranges)
    pub fn totals(&self) -> (u64, u64) {
        let seen = self.seen.len() as u64;
        let missing = self.span.map_or(0, |(lo, hi, _)| hi - lo + 1 - seen);
        (self.got + seen, self.lost + missing)
    }
}

/// (`_ff.seq`, `_ff.cyc`, `_ff.t`) of a well-formed datagram of `group`; the trailer is the last three points.
fn raw_trailer(group: &str, d: &[u8]) -> Option<(u64, u64, u64)> {
    let (g, body) = unframe(d)?;
    let s = std::str::from_utf8(body.strip_suffix(&[0])?).ok()?;
    let (mut seq, mut cyc, mut t) = (None, None, None);
    if g != group {
        return None;
    }
    for p in s.rsplit(',').take(4) {
        match p.split_once(':') {
            Some(("_ff.seq", v)) => seq = v.parse().ok(),
            Some(("_ff.cyc", v)) => cyc = v.parse().ok(),
            Some(("_ff.t", v)) => t = v.parse().ok(),
            _ => {}
        }
    }
    Some((seq?, cyc?, t?))
}

fn reject_idx(r: &Reject) -> usize {
    match r {
        Reject::Garbage => 0,
        Reject::WrongGroup => 1,
        Reject::Truncated => 2,
        Reject::Stale => 3,
    }
}

#[derive(Default)]
pub struct Log {
    cycles: Vec<Cycle>,
    idx: HashMap<(u64, u64), usize>,
    complete: Vec<u64>,
    /// last arrival of each probe tag (tags spread over the chunks; a chunk's own boundaries move with the value digits)
    probe_last: HashMap<String, Option<u64>>,
    probe_gaps: Vec<(u64, u64)>,
    transit: Vec<(u64, u64)>,
    /// The order-tolerant view of the same datagrams: dedupe by `_ff.seq` only, so a chunk that overtakes its neighbour still counts
    /// (the shipped Receiver drops it as stale). `known` = the stream's tags; `tol` = tags seen per (epoch, cyc).
    known: HashSet<String>,
    tol: HashMap<(u64, u64), usize>,
    complete_tol: Vec<u64>,
    rejects: [u64; 4],
    accepted: u64,
    seq: SeqTrack,
    /// (left, rejoined) epoch ns of a subscriber restart
    restart: Option<(u64, u64)>,
}

/// Eight tags evenly spread over `tags`, whose arrival gaps measure how fresh a value an RTU reading that tag sees.
pub fn probes(tags: &[String]) -> Vec<String> {
    let mut v: Vec<String> = (0..8).map(|i| tags[i * (tags.len() - 1) / 7].clone()).collect();
    v.dedup();
    v
}

impl Log {
    pub fn new(tags: &[String]) -> Log {
        Log { probe_last: probes(tags).into_iter().map(|t| (t, None)).collect(), known: tags.iter().cloned().collect(), ..Default::default() }
    }

    /// One datagram received at `t` (epoch ns) by a stream expecting `n` tags per cycle.
    pub fn on_datagram(&mut self, rx: &mut Receiver, group: &str, n: usize, d: &[u8], t: u64) {
        let trailer = raw_trailer(group, d);
        if let Some((seq, cyc, ft)) = trailer {
            if self.seq.push(seq, ft) {
                self.take_tolerant(d, (self.seq.epoch, cyc), t, n);
            }
        }
        match rx.accept(d) {
            Err(r) => self.rejects[reject_idx(&r)] += 1,
            Ok(pairs) if pairs.is_empty() => self.accepted += 1,
            Ok(pairs) => self.take(&pairs, trailer, d.len(), t, n),
        }
    }

    fn take_tolerant(&mut self, d: &[u8], key: (u64, u64), t: u64, n: usize) {
        let body = unframe(d).map(|(_, b)| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
        let got = body.split(',').filter_map(|p| p.split_once(':')).filter(|(k, _)| self.known.contains(*k)).count();
        let tags = self.tol.entry(key).or_insert(0);
        let before = *tags;
        *tags += got;
        if before < n && *tags >= n {
            self.complete_tol.push(t);
        }
    }

    fn take(&mut self, pairs: &[(String, f64)], trailer: Option<(u64, u64, u64)>, len: usize, t: u64, n: usize) {
        self.accepted += 1;
        if let Some((_, _, ft)) = trailer {
            self.transit.push((t, t.saturating_sub(ft)));
        }
        for (name, _) in pairs {
            if let Some(slot) = self.probe_last.get_mut(name) {
                if let Some(prev) = slot.replace(t) {
                    self.probe_gaps.push((t, t - prev));
                }
            }
        }
        let key = (self.seq.epoch, trailer.map_or(0, |x| x.1));
        let i = *self.idx.entry(key).or_insert_with(|| {
            self.cycles.push(Cycle { first_ns: t, simt: f64::NAN, ..Default::default() });
            self.cycles.len() - 1
        });
        let c = &mut self.cycles[i];
        let before = c.tags as usize;
        c.tags += pairs.len() as u32;
        (c.last_ns, c.chunks, c.bytes) = (t, c.chunks + 1, c.bytes + len as u32);
        if let Some((_, v)) = pairs.iter().find(|(k, _)| k == "sim.t") {
            c.simt = *v;
        }
        if before < n && c.tags as usize >= n {
            self.complete.push(t);
        }
    }
}

fn open(st: &Stream) -> UdpSocket {
    let s = UdpSocket::bind(("0.0.0.0", st.port)).unwrap();
    if st.mcast {
        s.join_multicast_v4(&Ipv4Addr::new(239, 0, 0, 1), &Ipv4Addr::UNSPECIFIED).unwrap();
    }
    s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    s
}

fn new_rx(st: &Stream) -> Receiver {
    Receiver::new(&group_for(&st.endpoint), st.tags.iter().map(String::as_str))
}

/// Receive until `stop`. A multicast stream with `restart = Some((at, gap))` leaves its group `at` after start and
/// rejoins `gap` later with a fresh receiver, like a restarted field device.
fn recv_stream(st: Arc<Stream>, stop: Arc<AtomicBool>, restart: Option<(Duration, Duration)>) -> Log {
    let (group, n, t0) = (group_for(&st.endpoint), st.tags.len(), Instant::now());
    let (mut log, mut rx, mut buf) = (Log::new(&st.tags), new_rx(&st), vec![0u8; 65536]);
    let (mut sock, mut pending) = (open(&st), restart.filter(|_| st.mcast));
    while !stop.load(Ordering::Relaxed) {
        if let Some((at, gap)) = pending.filter(|(at, _)| t0.elapsed() >= *at) {
            let left = now_ns();
            drop(sock);
            std::thread::sleep(gap);
            sock = open(&st);
            (rx, pending) = (new_rx(&st), None);
            log.seq.close();
            log.restart = Some((left, now_ns()));
            let _ = at;
        }
        if let Ok(len) = sock.recv(&mut buf) {
            log.on_datagram(&mut rx, &group, n, &buf[..len], now_ns());
        }
    }
    log
}

fn spawn_bridge(st: &Stream, host: &str, ff: &str, rate_ms: u64, i: usize) -> Child {
    let err = std::fs::OpenOptions::new().create(true).append(true).open(format!("/tmp/bridge{i}.err")).unwrap();
    Command::new(ff)
        .args(["pull-bridge", "--server", &format!("tcp://{host}:5555"), "--publish", &st.endpoint])
        .args(["--tags", &st.tags.join(","), "--rate", &rate_ms.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::from(err))
        .spawn()
        .unwrap()
}

fn kill(mut b: Child) {
    unsafe { libc::kill(b.id() as i32, libc::SIGTERM) };
    let _ = b.wait();
}

fn in_win(v: &[(u64, u64)], w: (u64, u64)) -> Vec<u64> {
    v.iter().filter(|(t, _)| *t >= w.0 && *t < w.1).map(|(_, x)| *x).collect()
}

/// Gaps between consecutive complete-cycle arrivals in `w`, with the lead-in from `w.0` and the tail to `w.1`: the
/// staleness an RTU reading "the latest complete state" would see.
pub fn staleness_gaps(complete: &[u64], w: (u64, u64)) -> Vec<u64> {
    let arr: Vec<u64> = complete.iter().copied().filter(|t| *t >= w.0 && *t < w.1).collect();
    let edges = [w.0].into_iter().chain(arr.iter().copied()).chain([w.1]);
    let v: Vec<u64> = edges.clone().zip(edges.skip(1)).map(|(a, b)| b - a).collect();
    v
}

fn summary(l: &Log, st: &Stream, w: (u64, u64), period: f64, events: bool) -> Value {
    let mut v = stats::pub_stats(&l.cycles, w, st.tags.len(), period);
    let (got, lost) = l.seq.totals();
    let secs = (w.1 - w.0) as f64 / 1e9;
    let (mut gaps, mut cg, mut tr) = (staleness_gaps(&l.complete, w), in_win(&l.probe_gaps, w), in_win(&l.transit, w));
    v["delivered_per_s"] = json!(v["complete_cycles"].as_f64().unwrap_or(0.0) / secs);
    let mut tg = staleness_gaps(&l.complete_tol, w);
    let tol_n = l.complete_tol.iter().filter(|t| **t >= w.0 && **t < w.1).count();
    v["tolerant"] = json!({"delivered_per_s": tol_n as f64 / secs, "staleness": stats::summary_us(&mut tg)});
    v["staleness"] = stats::summary_us(&mut gaps);
    v["probe_gap"] = stats::summary_us(&mut cg);
    v["transit"] = stats::summary_us(&mut tr);
    v["datagrams"] = json!({"seen": got, "lost": lost, "loss_pct": 100.0 * lost as f64 / (got + lost).max(1) as f64,
                            "accepted": l.accepted, "provider_restarts": l.seq.restarts});
    v["rejects"] = json!({"garbage": l.rejects[0], "wrong_group": l.rejects[1], "truncated": l.rejects[2], "stale": l.rejects[3]});
    v["name"] = json!(st.name);
    v["kind"] = json!(st.kind);
    v["tags_per_cycle"] = json!(st.tags.len());
    if let Some((left, rejoin)) = l.restart {
        let first = l.complete.iter().find(|&&t| t >= rejoin);
        v["restart"] = json!({"left_ns": left, "rejoin_ns": rejoin, "first_complete_ms_after_rejoin": first.map(|t| (t - rejoin) as f64 / 1e6)});
    }
    if events {
        v["complete_ms"] = json!(l.complete.iter().map(|t| t / 1_000_000).collect::<Vec<_>>());
        v["complete_tol_ms"] = json!(l.complete_tol.iter().map(|t| t / 1_000_000).collect::<Vec<_>>());
    }
    v
}

fn count_lines(path: &str, needle: &str) -> usize {
    std::fs::read_to_string(path).map(|s| s.lines().filter(|l| l.contains(needle)).count()).unwrap_or(0)
}

fn num<T: std::str::FromStr>(args: &[String], flag: &str, d: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    arg(args, flag, d).parse().unwrap()
}

pub fn run(args: &[String]) {
    let host = arg(args, "--host", "ffb-prov");
    let (period, secs, warm): (f64, f64, f64) = (num(args, "--period", "0.1"), num(args, "--secs", "30"), num(args, "--warmup", "3"));
    let (slices, pulls, k): (usize, usize, usize) = (num(args, "--slices", "0"), num(args, "--pull", "0"), num(args, "--k", "50"));
    let (restart_at, restart_gap): (f64, f64) = (num(args, "--restart-at", "0"), num(args, "--restart-gap", "2"));
    let ff = arg(args, "--ff", "/src/target-linux/release/ferrofire");
    let events = args.iter().any(|a| a == "--events");
    let req = Req::connect("zmq", &host);
    let all: Vec<String> = req.call("QUERY=").trim_start_matches("ACK=").split(',').filter(|t| !t.is_empty()).map(String::from).collect();
    let st = |name: String, kind, endpoint: String, port, mcast, tags| Arc::new(Stream { name, kind, endpoint, port, mcast, tags });
    let mut streams = vec![];
    if args.iter().any(|a| a == "--whole") {
        streams.push(st("whole".into(), "whole", "udp://239.0.0.1:40000".into(), 40000, true, all.clone()));
    }
    streams.extend((0..slices).map(|i| st(format!("slice{i}"), "slice", format!("udp://239.0.0.1:{}", 41000 + i), 41000 + i as u16, true, rtu_tags(&all, i, k))));
    let first_pull = streams.len();
    streams.extend((0..pulls).map(|i| st(format!("pull{i}"), "pull", format!("udp://127.0.0.1:{}", 42000 + i), 42000 + i as u16, false, rtu_tags(&all, i, k))));
    // the gap gets a pseudo-random extra fraction of a period: a gap that is a whole number of periods would rejoin exactly at the next cycle
    let restart_gap = restart_gap + period * (now_ns() % 1000) as f64 / 1000.0;
    let restart = (restart_at > 0.0).then(|| (Duration::from_secs_f64(warm + restart_at), Duration::from_secs_f64(restart_gap)));
    let stop = Arc::new(AtomicBool::new(false));
    let handles: Vec<_> = streams.iter().map(|s| (s.clone(), stop.clone())).map(|(s, p)| std::thread::spawn(move || recv_stream(s, p, restart))).collect();
    let _ = std::fs::remove_file("/tmp/bridge0.err");
    let rate_ms = (period * 1000.0).round() as u64;
    let mut bridges: Vec<Child> = streams[first_pull..].iter().enumerate().map(|(i, s)| spawn_bridge(s, &host, &ff, rate_ms, i)).collect();
    std::thread::sleep(Duration::from_secs_f64(warm));
    let start = now_ns();
    let mut bridge_restart = None;
    if restart_at > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(restart_at));
        let left = now_ns();
        bridges.drain(..).for_each(kill);
        std::thread::sleep(Duration::from_secs_f64(restart_gap));
        bridges = streams[first_pull..].iter().enumerate().map(|(i, s)| spawn_bridge(s, &host, &ff, rate_ms, i)).collect();
        bridge_restart = Some((left, now_ns()));
        std::thread::sleep(Duration::from_secs_f64((secs - restart_at - restart_gap).max(0.0)));
    } else {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }
    let win = (start, now_ns());
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    bridges.drain(..).for_each(kill);
    let mut out = vec![];
    for (i, (s, h)) in streams.iter().zip(handles).enumerate() {
        let mut log = h.join().unwrap();
        if s.kind == "pull" {
            log.restart = bridge_restart;
        }
        let mut v = summary(&log, s, win, period, events);
        if s.kind == "pull" {
            let f = format!("/tmp/bridge{}.err", i - first_pull);
            v["bridge_log"] = json!({"warn_lines": count_lines(&f, "Warn:"), "recovered_lines": count_lines(&f, "recovered")});
        }
        out.push(v);
    }
    let j = json!({"host": host, "period_s": period, "secs": secs, "k": k, "provider_tags": all.len(), "restart_gap_s": restart_gap,
                   "phases": {"publish": {"start_ns": win.0, "end_ns": win.1}}, "streams": out});
    println!("{}", serde_json::to_string_pretty(&j).unwrap());
}

// ---- injection ---------------------------------------------------------------------------------------------------

const KINDS: usize = 7;

/// Datagram number `i` of the injector: seven kinds the receiver must each refuse without harm.
pub fn bad_datagram(i: usize, group: &str, rng: &mut u64) -> Vec<u8> {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    match i % KINDS {
        0 => (0..1 + *rng % 200).map(|j| (*rng >> (j % 56)) as u8 ^ j as u8).collect(), // noise
        1 => frame(group, b"sim.t:1,bench.in0:2"),                                      // cut before the NUL
        2 => frame("deadbeef", b"sim.t:1,\0"),                                          // another endpoint's group
        3 => frame(group, &[0xff, 0xfe, 0]),                                            // not UTF-8
        4 => frame(group, b"sim.t:1,_ff.seq:1,_ff.cyc:0,_ff.t:1,\0"),                   // an old replay
        5 => vec![],                                                                    // empty datagram
        _ => frame(group, b"sim.t:1,ben\0"),                                            // cut mid point, NUL kept
    }
}

pub fn inject(args: &[String]) {
    let ep = arg(args, "--endpoint", "udp://239.0.0.1:40000");
    let dest = ep.trim_start_matches("udp://").rsplit(';').next().unwrap().to_string();
    let (rate, secs): (f64, f64) = (num(args, "--rate", "100"), num(args, "--secs", "30"));
    let group = group_for(&ep);
    let s = UdpSocket::bind("0.0.0.0:0").unwrap();
    s.set_multicast_ttl_v4(1).unwrap();
    let (t0, mut rng, mut sent) = (Instant::now(), 0x9e3779b97f4a7c15u64, 0usize);
    while t0.elapsed().as_secs_f64() < secs {
        s.send_to(&bad_datagram(sent, &group, &mut rng), &dest).unwrap();
        sent += 1;
        let due = Duration::from_secs_f64(sent as f64 / rate);
        if let Some(d) = due.checked_sub(t0.elapsed()) {
            std::thread::sleep(d);
        }
    }
    println!("{}", json!({"sent": sent, "rate": rate, "kinds": KINDS}));
}

/// `ffbench req --host H REQUEST...`: each request on its own line as `request => reply`.
pub fn req(args: &[String]) {
    let (mut host, mut reqs, mut it) = ("ffb-prov".to_string(), vec![], args.iter());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--host" => host = it.next().cloned().unwrap_or(host),
            _ => reqs.push(a),
        }
    }
    let r = Req::connect("zmq", &host);
    reqs.iter().for_each(|q| println!("{q} => {}", r.call(q)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_gaps_are_counted() {
        let mut s = SeqTrack::default();
        [10, 11, 13, 14].iter().for_each(|&q| {
            s.push(q, 1000 + q);
        });
        assert_eq!(s.totals(), (4, 1));
    }

    #[test]
    fn reordering_is_not_loss() {
        let mut s = SeqTrack::default();
        [10, 12, 11].iter().for_each(|&q| {
            s.push(q, 1000 + q);
        });
        assert_eq!(s.totals(), (3, 0));
    }

    #[test]
    fn provider_restart_starts_a_new_segment() {
        let mut s = SeqTrack::default();
        [10, 11, 13].iter().for_each(|&q| {
            s.push(q, 1000 + q);
        });
        [0, 1, 2].iter().for_each(|&q| {
            s.push(q, 9000 + q);
        });
        assert_eq!((s.restarts, s.epoch, s.totals()), (1, 1, (6, 1)));
    }

    #[test]
    fn ancient_replays_are_ignored() {
        let mut s = SeqTrack::default();
        s.push(5000, 100 * REPLAY_NS);
        s.push(1, 1);
        s.push(5001, 100 * REPLAY_NS + 1);
        assert_eq!(s.totals(), (2, 0));
    }

    #[test]
    fn trailer_is_read_from_the_last_points() {
        let d = frame("g", b"sim.t:1,a:2,_ff.seq:7,_ff.cyc:3,_ff.t:99,\0");
        assert_eq!(raw_trailer("g", &d), Some((7, 3, 99)));
        assert_eq!(raw_trailer("other", &d), None);
        assert_eq!(raw_trailer("g", &frame("g", b"sim.t:1,")), None);
    }

    #[test]
    fn staleness_includes_edges() {
        assert_eq!(staleness_gaps(&[10, 20, 100], (0, 40)), [10, 10, 20]);
        assert_eq!(staleness_gaps(&[], (0, 40)), [40]);
    }

    #[test]
    fn injector_kinds_are_all_refused() {
        let group = group_for("udp://239.0.0.1:40000");
        let mut rx = Receiver::new(&group, ["sim.t"]);
        let (mut rng, mut seen) = (7u64, [0u64; 4]);
        rx.accept(&frame(&group, b"sim.t:1,_ff.seq:9,_ff.cyc:0,_ff.t:9,\0")).unwrap(); // newer than the replay
        for i in 0..KINDS * 3 {
            let d = bad_datagram(i, &group, &mut rng);
            // noise may by chance parse as a datagram of another group; it must never be accepted
            seen[reject_idx(&rx.accept(&d).unwrap_err())] += 1;
        }
        assert!(seen.iter().all(|&c| c > 0), "{seen:?}");
    }

    #[test]
    fn a_chunk_that_overtakes_its_neighbour_still_completes_the_cycle() {
        let group = group_for("udp://239.0.0.1:40000");
        let mut rx = Receiver::new(&group, ["sim.t", "a"]);
        let mut log = Log::new(&["sim.t".to_string(), "a".to_string()]);
        let d = |seq, body: &str| frame(&group, format!("{body}_ff.seq:{seq},_ff.cyc:0,_ff.t:{},\0", 5 + seq).as_bytes());
        log.on_datagram(&mut rx, &group, 2, &d(1, "a:2,"), 100); // seq 1 first ...
        log.on_datagram(&mut rx, &group, 2, &d(0, "sim.t:1,"), 101); // ... then seq 0: reordered, not stale (ww-d4a)
        assert_eq!((log.complete.len(), log.rejects[3]), (1, 0));
        assert_eq!((log.complete.clone(), log.complete_tol.clone()), (vec![101], vec![101]));
        assert_eq!(log.seq.totals(), (2, 0));
    }

    #[test]
    fn complete_cycle_needs_every_tag() {
        let group = group_for("udp://239.0.0.1:40000");
        let mut rx = Receiver::new(&group, ["sim.t", "a", "b"]);
        let mut log = Log::new(&["sim.t".to_string(), "a".to_string(), "b".to_string()]);
        let d = |seq, body: &str| frame(&group, format!("{body}_ff.seq:{seq},_ff.cyc:{},_ff.t:5,\0", seq / 2).as_bytes());
        for (i, body) in [(0, "sim.t:1,a:2,"), (1, "b:3,"), (2, "sim.t:2,a:2,"), (4, "sim.t:3,a:2,"), (5, "b:3,")] {
            log.on_datagram(&mut rx, &group, 3, &d(i, body), 100 + i * 10);
        }
        assert_eq!(log.complete, [110, 150]); // cycle 1 lost its second chunk (seq 3)
        assert_eq!(log.seq.totals(), (5, 1));
        // probes are sim.t, a, b: sim.t arrived at 100, 120, 140 -> gaps 20, 20; b at 110, 150 -> 40
        let mut g: Vec<u64> = log.probe_gaps.iter().map(|x| x.1).collect();
        g.sort();
        assert_eq!(g, [20, 20, 20, 20, 40]);
    }
}
