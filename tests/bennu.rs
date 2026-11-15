//! Protocol pieces (spec 6.1): group hash, framing, chunking, request handling, and a real REP round trip.
use ferrofire::bennu::*;
use ferrofire::model::{Model, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn group_hash_matches_libstdcxx() {
    assert_eq!(libstdcxx_hash(b"udp://239.0.0.1:40000"), 0x54386972605942ba);
    assert_eq!(group_for("udp://239.0.0.1:40000"), "54386972605942b");
    assert_eq!(group_for("udp://127.0.0.1:5556"), "5622dcba8d3e1de");
    // interface prefix is stripped before hashing, on both sides
    assert_eq!(group_for("udp://*;239.0.0.1:40000"), "54386972605942b");
    assert_eq!(
        group_for("udp://10.1.1.2;239.0.0.1:40000"),
        "54386972605942b"
    );
}

#[test]
fn framing_and_chunking() {
    let d = frame("54386972605942b", b"a:1,\0");
    assert_eq!(d[0], 15);
    assert_eq!(unframe(&d), Some(("54386972605942b", &b"a:1,\0"[..])));
    assert_eq!(unframe(&[9, b'x']), None); // group_len exceeds the datagram
    assert_eq!(chunk_message("a:1,,b:2,", 100), vec!["a:1,b:2,"]); // empty part skipped
    assert_eq!(
        chunk_message("aaaa,bbbb,cccc,", 11),
        vec!["aaaa,bbbb,", "cccc,"]
    );
    assert_eq!(
        chunks_nul("a:1,b:2,", 7, 3, 99),
        vec![b"a:1,b:2,_ff.seq:7,_ff.cyc:3,_ff.t:99,\0".to_vec()]
    );
    assert_eq!(chunk_message("longlonglong,", 4), vec!["longlonglong,"]); // over-long single part alone
    let snap: Vec<_> = (0..400)
        .map(|i| (format!("bus-{i}.voltage"), Value::Analog(1.0123456789)))
        .collect();
    let ds = datagrams_msg("54386972605942b", &snapshot_msg(&snap), 0, 0, 0);
    assert!(
        ds.len() > 1
            && ds
                .iter()
                .all(|d| d.len() <= 1500 && *d.last().unwrap() == 0)
    );
    let all: String = ds
        .iter()
        .map(|d| {
            String::from_utf8(unframe(d).unwrap().1.to_vec())
                .unwrap()
                .trim_end_matches('\0')
                .to_string()
        })
        .collect();
    // no part split across datagrams: 400 points + 3 trailer pseudo-tags per datagram
    assert_eq!(all.matches(',').count(), 400 + 3 * ds.len());
}

fn setup() -> (Arc<Meta>, Arc<Mutex<Shared>>) {
    let m = Model::load(include_str!("../models/waterway.yaml")).unwrap();
    let sh = Shared {
        snap: m
            .snapshot()
            .into_iter()
            .map(|(t, v)| (t.to_string(), v))
            .collect(),
        queue: vec![],
    };
    (Arc::new(Meta::of(&m)), Arc::new(Mutex::new(sh)))
}

#[test]
fn requests() {
    let (meta, sh) = setup();
    let h = |s: &str| String::from_utf8(handle(s.as_bytes(), &meta, &sh)).unwrap();
    let q = h("QUERY=\0");
    assert!(
        q.starts_with("ACK=") && q.ends_with(",\0") && q.matches(',').count() == 15,
        "{q}"
    );
    assert_eq!(h("read=lock.height\0"), "ACK=5\0");
    assert!(h("READ=nope\0").starts_with("ERR="));
    // batched: one ACK in the update format, request order; an unknown tag fails the whole batch
    assert_eq!(
        h("READ=lock.height,lowervalve.open\0"),
        "ACK=lock.height:5,lowervalve.open:0,\0"
    );
    assert_eq!(h("READ=lock.height,\0"), "ACK=lock.height:5,\0");
    assert_eq!(h("READ=lock.height,nope\0"), "ERR=Tag 'nope' not found\0");
    assert!(h("READ\0").starts_with("ERR="));
    assert!(h("BOGUS=x\0").starts_with("ERR=Unknown command type"));
    assert_eq!(
        h("WRITE=lowervalve.open:1.000000,uppergate.open:true\0"),
        "ACK=\0"
    );
    assert_eq!(sh.lock().unwrap().queue.len(), 2);
    // one bad tag fails the whole batch, nothing queued
    assert!(h("WRITE=uppervalve.open:1.000000,lock.height:3.0\0").starts_with("ERR="));
    assert!(h("WRITE=uppervalve.open:abc\0").starts_with("ERR="));
    assert_eq!(sh.lock().unwrap().queue.len(), 2);
}

/// The QUERY reply is cached in `Meta`; it must stay what the per-request format produced, follow the tag set of
/// the model the `Meta` was built from, and not move when values or write limits do.
#[test]
fn query_reply_is_the_tag_list_of_its_model() {
    let old = |m: &Model| -> String {
        let body: String = m.tags().map(|(t, _)| format!("{t},")).collect();
        format!("ACK={body}\0")
    };
    let q = |meta: &Meta| {
        let sh = Mutex::new(Shared::default());
        String::from_utf8(handle(b"QUERY=\0", meta, &sh)).unwrap()
    };
    let wat = Model::load(include_str!("../models/waterway.yaml")).unwrap();
    let pump = Model::load(include_str!("../models/pump-station.yaml")).unwrap();
    let (mw, mp) = (Meta::of(&wat), Meta::of(&pump));
    assert_eq!(q(&mw), old(&wat));
    assert_eq!(q(&mp), old(&pump));
    assert_ne!(q(&mw), q(&mp));
    assert_eq!(q(&mw), q(&mw), "repeatable");
    let two = |lim: &str| {
        Model::load(&format!("version: 1\ndt: 0.1\ninputs:\n  sp: {{type: analog, min: 0, max: {lim}}}\noutputs:\n  sp: {{type: analog}}\n")).unwrap()
    };
    let (a, b) = (two("10"), two("20"));
    assert_eq!(q(&Meta::of(&a)), "ACK=sp,\0");
    assert_eq!(q(&Meta::of(&b)), "ACK=sp,\0", "limits are not in the reply");
    let grown = Model::load("version: 1\ndt: 0.1\ninputs:\n  sp: {type: analog}\n  en: {type: binary}\noutputs:\n  sp: {type: analog}\n  en: {type: binary}\n").unwrap();
    assert_eq!(q(&Meta::of(&grown)), old(&grown));
    assert_ne!(q(&Meta::of(&grown)), q(&Meta::of(&a)), "a new tag shows up");
    // a write that changes a value does not change the reply
    let sh = Mutex::new(Shared::default());
    let meta = Meta::of(&grown);
    let before = handle(b"QUERY=\0", &meta, &sh);
    assert_eq!(handle(b"WRITE=sp:3\0", &meta, &sh), b"ACK=\0");
    assert_eq!(handle(b"QUERY=\0", &meta, &sh), before);
}

#[test]
fn rep_round_trip() {
    let (meta, sh) = setup();
    let ctx = zmq::Context::new();
    let rep = ctx.socket(zmq::REP).unwrap();
    rep.bind("tcp://127.0.0.1:*").unwrap();
    let ep = rep.get_last_endpoint().unwrap().unwrap();
    let t = std::thread::spawn(move || {
        for _ in 0..3 {
            let r = rep.recv_bytes(0).unwrap();
            rep.send(handle(&r, &meta, &sh), 0).unwrap();
        }
    });
    let req = ctx.socket(zmq::REQ).unwrap();
    req.connect(&ep).unwrap();
    for (q, want) in [
        ("READ=lock.height\0", "ACK=5\0"),
        ("WRITE=lowervalve.open:1.000000\0", "ACK=\0"),
        ("WRITE=nope:1\0", "ERR=Tag 'nope' is unknown or read-only\0"),
    ] {
        req.send(q, 0).unwrap();
        assert_eq!(req.recv_bytes(0).unwrap(), want.as_bytes());
    }
    t.join().unwrap();
}

#[test]
fn our_requester_talks_to_a_libzmq_rep_and_recovers_after_a_timeout() {
    let rep = zmq::Context::new().socket(zmq::REP).unwrap();
    rep.bind("tcp://127.0.0.1:*").unwrap();
    let ep = rep.get_last_endpoint().unwrap().unwrap();
    let t = std::thread::spawn(move || {
        rep.send(
            rep.recv_bytes(0)
                .map(|r| [r, b"!".to_vec()].concat())
                .unwrap(),
            0,
        )
        .unwrap();
        rep.recv_bytes(0).unwrap(); // the client gave up on this one (lazy pirate)
        std::thread::sleep(std::time::Duration::from_millis(600));
        rep.send("late", 0).unwrap();
        rep.send(rep.recv_bytes(0).unwrap(), 0).unwrap();
    });
    let mut req = ferrofire::transport::requester(&ep, 300).unwrap();
    assert_eq!(req.call(b"READ=a\0").unwrap(), b"READ=a\0!");
    assert!(req.call(b"slow").is_err());
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(req.call(b"again").unwrap(), b"again");
    t.join().unwrap();
}

#[test]
fn run_arguments() {
    use clap::Parser;
    let a = |s: &[&str]| RunArgs::try_parse_from(std::iter::once("run").chain(s.iter().copied()));
    let ok = a(&[
        "m.yaml",
        "--rate",
        "0.5",
        "--server",
        "tcp://*:1",
        "--publish",
        "udp://*;239.0.0.1:2",
    ])
    .unwrap();
    assert_eq!(ok.path(), Some("m.yaml"));
    assert_eq!(
        (ok.server.as_str(), ok.publish.as_deref()),
        ("tcp://*:1", Some("udp://*;239.0.0.1:2"))
    );
    assert_eq!(ok.rate, Some(0.5));
    let d = a(&["--model", "x.yaml"]).unwrap();
    assert_eq!(d.path(), Some("x.yaml"));
    assert_eq!((d.server.as_str(), d.rate), ("tcp://0.0.0.0:5555", None));
    assert_eq!((d.publish, d.slices.len()), (None, 0)); // none given: the default endpoint carries everything
    let t = a(&[
        "x.yaml",
        "--publish-tags",
        "udp://h:1=a,b",
        "--publish-tags",
        "udp://h:2=@c.xml",
    ])
    .unwrap();
    assert_eq!(t.slices, vec!["udp://h:1=a,b", "udp://h:2=@c.xml"]);
    assert!(a(&["--transport", "zmq"]).is_err()); // gone with NNG
                                                  // a second model (bare word and --model together), a flag with no value, a non-number rate: clap refuses
    assert!(a(&["--model", "x.yaml", "y.yaml"]).is_err());
    assert!(a(&["--rate"]).is_err());
    assert!(a(&["--rate", "fast"]).is_err());
}

#[test]
fn publish_period_prefers_rate_then_model_then_a_tenth() {
    let m = Model::load("version: 1\ndt: 0.1\npublish_rate: 0.25\n").unwrap();
    let plain = Model::load("version: 1\ndt: 0.1\n").unwrap();
    assert_eq!(publish_period(Some(2.0), &m), Ok(2.0));
    assert_eq!(publish_period(None, &m), Ok(0.25));
    assert_eq!(publish_period(None, &plain), Ok(0.1));
    assert!(publish_period(Some(0.0), &m).is_err());
    assert!(publish_period(Some(-1.0), &m).is_err());
}

#[test]
fn udp_endpoints_and_senders() {
    let (iface, dest) = parse_udp("udp://10.1.1.2;239.0.0.1:40000").unwrap();
    assert_eq!(
        (iface.as_deref(), dest.to_string()),
        (Some("10.1.1.2"), "239.0.0.1:40000".into())
    );
    assert_eq!(parse_udp("udp://*;239.0.0.1:1").unwrap().0, None);
    assert!(parse_udp("tcp://x:1")
        .unwrap_err()
        .contains("must be udp://"));
    assert!(parse_udp("udp://no-port").is_err());
    // unicast needs no multicast setup; multicast takes an IPv4 interface or none
    let uni = parse_udp("udp://127.0.0.1:9").unwrap().1;
    assert!(udp_sender(Some("not-an-ip"), uni).is_ok());
    let mc = parse_udp("udp://239.0.0.1:9").unwrap().1;
    assert!(udp_sender(None, mc).is_ok());
    assert!(udp_sender(Some("127.0.0.1"), mc).is_ok());
    assert!(udp_sender(Some("not-an-ip"), mc)
        .unwrap_err()
        .contains("only IPv4"));
    assert!(udp_sender(Some("192.0.2.77"), mc)
        .unwrap_err()
        .contains("IP_MULTICAST_IF"));
}

// ---- ferrofire datagram v1: sequence trailer ----------------------------------------------------------------

fn big_snap(n: usize) -> Vec<(String, Value)> {
    let mut snap = vec![("sim.t".to_string(), Value::Analog(0.5))];
    snap.extend((1..n).map(|i| {
        (
            format!("bus-{i}.voltage"),
            Value::Analog(1.0123456789012345),
        )
    }));
    snap
}

/// A datagram as the receiver sees it: (group, `tag:value` pairs).
fn pairs(d: &[u8]) -> Vec<(String, f64)> {
    let (_, body) = unframe(d).unwrap();
    let text = std::str::from_utf8(&body[..body.len() - 1]).unwrap();
    text.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (t, v) = p.split_once(':').unwrap(); // what stock bennu's InputModule does
            (t.to_string(), v.parse().unwrap())
        })
        .collect()
}

#[test]
fn the_trailer_never_busts_the_chunk_budget_even_with_maximal_counters() {
    let msg = snapshot_msg(&big_snap(800));
    for (seq0, cyc, t) in [(0, 0, 0), (u64::MAX - 100, u64::MAX, u64::MAX)] {
        let ds = datagrams_msg("54386972605942b", &msg, seq0, cyc, t);
        assert!(ds.len() > 2);
        for d in &ds {
            let (_, body) = unframe(d).unwrap();
            assert!(body.len() <= CHUNK, "body {} > {CHUNK}", body.len());
            assert!(d.len() <= CHUNK + 16);
        }
    }
    assert_eq!(trailer(u64::MAX, u64::MAX, u64::MAX).len(), 85); // TRAILER_MAX is the worst case
    assert!(trailer(u64::MAX, u64::MAX, u64::MAX).len() <= TRAILER_MAX);
}

#[test]
fn seq_counts_datagrams_cyc_is_constant_in_a_cycle_and_the_first_starts_with_sim_t() {
    let ds = datagrams_msg("g", &snapshot_msg(&big_snap(800)), 40, 7, 123);
    let tails: Vec<_> = ds
        .iter()
        .map(|d| pairs(d)[pairs(d).len() - 3..].to_vec())
        .collect();
    for (i, t) in tails.iter().enumerate() {
        let want = [
            ("_ff.seq", 40.0 + i as f64),
            ("_ff.cyc", 7.0),
            ("_ff.t", 123.0),
        ];
        let got: Vec<_> = t.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        assert_eq!(got, want);
    }
    assert_eq!(pairs(&ds[0])[0].0, "sim.t"); // the bench client keys a cycle on it
                                             // every real tag appears exactly once across the cycle, and stock parsing (above) accepted every pair
    let real: usize = ds
        .iter()
        .map(|d| {
            pairs(d)
                .iter()
                .filter(|(n, _)| !n.starts_with("_ff."))
                .count()
        })
        .sum();
    assert_eq!(real, 800);
}

struct Sink(Arc<Mutex<Vec<String>>>);
impl ferrofire::transport::Publisher for Sink {
    fn publish(&self, msg: &str) {
        self.0.lock().unwrap().push(msg.to_string());
    }
    fn describe(&self) -> String {
        "sink".into()
    }
}

/// A target over `Sink`; a slice is built for the snapshot tag order `order`.
fn sink(tags: Option<&[&str]>, order: &[String]) -> (Target, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(vec![]));
    let out = Box::new(Sink(log.clone()));
    let target = match tags {
        None => Target::all(out),
        Some(t) => Target::slice(out, t.iter().map(|s| s.to_string()).collect(), order),
    };
    (target, log)
}

fn names(snap: &[(String, Value)]) -> Vec<String> {
    snap.iter().map(|(n, _)| n.clone()).collect()
}

#[test]
fn publish_happens_once_per_tick_and_every_nth_tick() {
    let mut m = Model::load(include_str!("../models/waterway.yaml")).unwrap();
    let (meta, sh) = setup();
    let _ = meta;
    let (all, log) = sink(None, &[]);
    for k in 0..6 {
        tick_and_publish(&mut m, &sh, std::slice::from_ref(&all), &mut vec![], k, 1);
    }
    assert_eq!(log.lock().unwrap().len(), 6);
    log.lock().unwrap().clear();
    for k in 0..6 {
        tick_and_publish(&mut m, &sh, std::slice::from_ref(&all), &mut vec![], k, 3);
    }
    assert_eq!(log.lock().unwrap().len(), 2); // ticks 0 and 3
    assert!((m.time - 12.0 * m.dt).abs() < 1e-9); // every call also stepped the engine
}

#[test]
fn slices_get_their_own_tags_and_may_overlap() {
    let snap = big_snap(10);
    let order = names(&snap);
    let (a, la) = sink(Some(&["bus-3.voltage", "sim.t"]), &order);
    let (b, lb) = sink(Some(&["bus-3.voltage", "bus-9.voltage"]), &order);
    let (all, lall) = sink(None, &order);
    publish_all(&[a, b, all], &snap);
    // snapshot order, not slice order; the overlapping tag goes to both
    assert!(la.lock().unwrap()[0].starts_with("sim.t:0.5,bus-3.voltage:"));
    assert_eq!(la.lock().unwrap()[0].matches(',').count(), 2);
    assert_eq!(lb.lock().unwrap()[0].matches(',').count(), 2);
    assert!(lb.lock().unwrap()[0].contains("bus-9.voltage:"));
    assert_eq!(lall.lock().unwrap()[0].matches(',').count(), 10);
}

/// The publish path before ww-1df.38: a `HashSet` filter that clones points, text from `format!`.
fn old_msg(snap: &[(String, Value)]) -> String {
    snap.iter()
        .map(|(t, v)| format!("{t}:{},", fmt_value(*v)))
        .collect()
}

fn old_part(snap: &[(String, Value)], tags: &[&str]) -> String {
    let set: std::collections::HashSet<&str> = tags.iter().copied().collect();
    let part: Vec<_> = snap
        .iter()
        .filter(|(n, _)| set.contains(n.as_str()))
        .cloned()
        .collect();
    old_msg(&part)
}

#[test]
fn publish_slices_and_all_are_byte_for_byte_what_they_were() {
    use Value::{Analog as A, Binary as B};
    let mut snap = big_snap(40);
    snap.extend([
        ("x.nan".into(), A(f64::NAN)),
        ("x.inf".into(), A(f64::INFINITY)),
        ("x.ninf".into(), A(f64::NEG_INFINITY)),
        ("x.nz".into(), A(-0.0)),
        ("x.big".into(), A(1e300)),
        ("x.tiny".into(), A(5e-324)),
        ("x.third".into(), A(1.0 / 3.0)),
        ("x.on".into(), B(true)),
        ("x.off".into(), B(false)),
    ]);
    let order = names(&snap);
    let every: Vec<&str> = order.iter().map(String::as_str).collect();
    let slices: [&[&str]; 6] = [
        &["bus-3.voltage", "sim.t"],
        &["x.off", "x.nan", "bus-9.voltage", "x.on"], // not in snapshot order
        &["bus-3.voltage", "no.such.tag", "x.big"],   // one tag the snapshot lacks
        &["no.such.tag"],                             // nothing of it present
        &["x.inf", "x.ninf", "x.nz", "x.tiny", "x.third"],
        &every,
    ];
    let mut targets = vec![];
    let mut logs = vec![];
    for tags in slices.iter().map(|s| Some(*s)).chain([None]) {
        let (t, l) = sink(tags, &order);
        targets.push(t);
        logs.push(l);
    }
    for round in 0..3 {
        for (i, (_, v)) in snap.iter_mut().enumerate() {
            if let A(x) = v {
                if x.is_finite() && round > 0 {
                    *x = *x * 1.0000001 + i as f64 * 0.001;
                }
            }
        }
        for l in &logs {
            l.lock().unwrap().clear();
        }
        publish_all(&targets, &snap);
        for (i, s) in slices.iter().enumerate() {
            assert_eq!(
                logs[i].lock().unwrap().as_slice(),
                [old_part(&snap, s)],
                "slice {i} round {round}"
            );
        }
        assert_eq!(
            logs[6].lock().unwrap().as_slice(),
            [old_msg(&snap)],
            "all, round {round}"
        );
        assert_eq!(snapshot_msg(&snap), old_msg(&snap));
    }
    // a snapshot of another length than the slice was built for is still filtered by name
    let short = &snap[..20];
    for l in &logs {
        l.lock().unwrap().clear();
    }
    publish_all(&targets, short);
    for (i, s) in slices.iter().enumerate() {
        assert_eq!(
            logs[i].lock().unwrap().as_slice(),
            [old_part(short, s)],
            "short snapshot, slice {i}"
        );
    }
}

/// The engine tick before ww-1df.51: apply the writes, snapshot into fresh Strings, step.
fn old_tick(m: &mut Model, writes: &[(String, Value)]) -> Vec<(String, Value)> {
    m.apply_writes(writes);
    let snap = names_values(m);
    m.step(m.dt);
    snap
}

fn names_values(m: &Model) -> Vec<(String, Value)> {
    m.snapshot()
        .into_iter()
        .map(|(t, v)| (t.to_string(), v))
        .collect()
}

/// Writes the model accepts: each writable tag, nudged.
fn nudges(m: &Model, round: u64) -> Vec<(String, Value)> {
    let meta = Meta::of(m);
    names_values(m)
        .into_iter()
        .filter(|(t, _)| meta.writable.contains(t))
        .map(|(t, v)| match v {
            Value::Binary(b) => (t, Value::Binary(!b)),
            Value::Analog(x) => (t, Value::Analog(x + 0.25 * round as f64)),
        })
        .collect()
}

/// The reused snapshot buffers must hold exactly what a fresh snapshot per tick held: published bytes, the REP side's
/// copy and the model state, over ticks with writes, from an empty, a stale and a wrong-length start.
#[test]
fn engine_ticks_publish_and_store_what_a_fresh_snapshot_per_tick_did() {
    for yaml in [
        include_str!("../models/waterway.yaml"),
        include_str!("../models/pump-station.yaml"),
    ] {
        let stale = vec![("not.a.tag".to_string(), Value::Analog(7.0)); 3];
        for start in [vec![], stale, big_snap(2)] {
            let mut new = Model::load(yaml).unwrap();
            let mut old = Model::load(yaml).unwrap();
            let n = new.snapshot().len();
            let (all, log) = sink(None, &[]);
            let sh = Mutex::new(Shared {
                snap: start,
                queue: vec![],
            });
            let mut writes_seen = 0;
            let mut snap = vec![];
            for k in 0..8u64 {
                let w = if k % 3 == 1 { nudges(&new, k) } else { vec![] };
                writes_seen += w.len();
                sh.lock().unwrap().queue = w.clone();
                tick(&mut new, &sh, &all, &mut snap, k, 2);
                let want = old_tick(&mut old, &w);
                assert_eq!(sh.lock().unwrap().snap.len(), n);
                assert_eq!(
                    snapshot_msg(&sh.lock().unwrap().snap),
                    old_msg(&want),
                    "stored, tick {k}"
                );
                let published = log.lock().unwrap().clone();
                assert_eq!(published.len() as u64, k / 2 + 1, "tick {k}");
                if k % 2 == 0 {
                    assert_eq!(published.last().unwrap(), &old_msg(&want), "tick {k}");
                }
            }
            assert!(writes_seen > 0);
            assert_eq!(new.time, old.time);
            assert_eq!(old_msg(&names_values(&new)), old_msg(&names_values(&old)));
        }
    }
}

fn tick(
    m: &mut Model,
    sh: &Mutex<Shared>,
    all: &Target,
    snap: &mut Vec<(String, Value)>,
    k: u64,
    every: u64,
) {
    tick_and_publish(m, sh, std::slice::from_ref(all), snap, k, every);
}

/// A slice is resolved against `Meta::tags` once, so the engine's snapshot must keep that order.
#[test]
fn the_engines_snapshot_is_in_meta_tag_order() {
    for yaml in [
        include_str!("../models/waterway.yaml"),
        include_str!("../models/pump-station.yaml"),
    ] {
        let mut m = Model::load(yaml).unwrap();
        let meta = Meta::of(&m);
        let (all, log) = sink(None, &[]);
        let sh = Mutex::new(Shared::default());
        for k in 0..3 {
            tick_and_publish(&mut m, &sh, std::slice::from_ref(&all), &mut vec![], k, 1);
        }
        let msg = log.lock().unwrap()[2].clone();
        let got: Vec<&str> = msg
            .split(',')
            .filter(|p| !p.is_empty())
            .map(|p| p.split(':').next().unwrap())
            .collect();
        assert_eq!(got, meta.tags);
    }
}

#[test]
fn plan_rules() {
    let known: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
    let sl = |ep: &str, t: &[&str]| Slice {
        endpoint: ep.into(),
        tags: t.iter().map(|s| s.to_string()).collect(),
    };
    // default: one endpoint, all tags
    assert_eq!(
        plan(None, vec![], &known).unwrap(),
        vec![("udp://*;239.0.0.1:40000".into(), None)]
    );
    // slices alone replace the default; --publish adds the all-tags endpoint; overlap is fine
    let p = plan(
        None,
        vec![sl("udp://h:1", &["a", "b"]), sl("udp://h:2", &["b", "c"])],
        &known,
    )
    .unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(
        p[1],
        (
            "udp://h:2".to_string(),
            Some(vec!["b".to_string(), "c".to_string()])
        )
    );
    let p = plan(Some("udp://h:9"), vec![sl("udp://h:1", &["a"])], &known).unwrap();
    assert_eq!((p[0].1.clone(), p.len()), (None, 2));
    // refusals: unknown tag, empty slice, the same endpoint twice (also via an iface prefix)
    assert!(plan(None, vec![sl("udp://h:1", &["a", "zz"])], &known)
        .unwrap_err()
        .contains("unknown tags: zz"));
    assert!(plan(None, vec![sl("udp://h:1", &[])], &known)
        .unwrap_err()
        .contains("no tags"));
    assert!(plan(
        None,
        vec![sl("udp://h:1", &["a"]), sl("udp://x;h:1", &["b"])],
        &known
    )
    .unwrap_err()
    .contains("twice"));
    assert!(plan(Some("udp://h:1"), vec![sl("udp://h:1", &["a"])], &known).is_err());
}

#[test]
fn slice_specs_parse_from_the_command_line_and_resolve_against_the_model_dir() {
    let s = parse_slice("udp://127.0.0.1:5001=a,b").unwrap();
    assert_eq!(
        (s.endpoint.as_str(), s.tags.clone(), s.tags_from.clone()),
        (
            "udp://127.0.0.1:5001",
            vec!["a".to_string(), "b".to_string()],
            None
        )
    );
    assert!(parse_slice("no-equals").is_err());
    let f = parse_slice("udp://h:1=@tests/conformance/rtu-1.xml").unwrap();
    assert_eq!(f.tags_from.as_deref(), Some("tests/conformance/rtu-1.xml"));
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let r = resolve_slice(&f, Some(base)).unwrap();
    assert_eq!(r.tags, vec!["lock.height", "lowergate.open"]);
    // the model YAML carries the same thing
    let m = Model::load("version: 1\ndt: 0.1\npublish_slices:\n  - endpoint: udp://h:1\n    tags: [a]\n    tags_from: x.xml\n").unwrap();
    assert_eq!(m.publish_slices[0].tags, vec!["a"]);
}

/// The test vector in docs/datagram-v1.md is what the code produces, and the reference receiver reads it.
#[test]
fn the_spec_test_vector_matches_the_code() {
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/datagram-v1.md"))
        .unwrap();
    let ep = "udp://239.0.0.1:40000";
    let g = group_for(ep);
    let d = datagrams_msg(
        &g,
        "sim.t:0.1,lock.height:5,lowervalve.open:0,",
        7,
        3,
        1234567890,
    );
    assert_eq!(d.len(), 1);
    let hex: String = d[0].iter().map(|b| format!("{b:02x}")).collect();
    for want in [
        format!("{:#x}", libstdcxx_hash(ep.as_bytes())),
        g.clone(),
        hex,
    ] {
        assert!(doc.contains(&want), "docs/datagram-v1.md lacks {want}");
    }
    let mut r = ferrofire::datagram::Receiver::new(&g, ["lock.height"]);
    assert_eq!(
        r.accept(&d[0]).unwrap(),
        vec![("lock.height".to_string(), 5.0)]
    );
}

#[test]
fn writes_are_checked_against_type_and_limits() {
    let m = Model::load("version: 1\ndt: 0.1\ninputs:\n  sp: {type: analog, min: 0, max: 10}\n  en: {type: binary}\noutputs:\n  sp: {type: analog}\n  en: {type: binary}\n").unwrap();
    let meta = Meta::of(&m);
    let sh = Mutex::new(Shared::default());
    let h = |s: &str| String::from_utf8(handle(s.as_bytes(), &meta, &sh)).unwrap();
    assert_eq!(h("WRITE=sp:10,en:1\0"), "ACK=\0");
    assert_eq!(
        h("WRITE=sp:11\0"),
        "ERR=Tag 'sp' value 11 is above its max 10\0"
    );
    assert_eq!(
        h("WRITE=sp:-1\0"),
        "ERR=Tag 'sp' value -1 is below its min 0\0"
    );
    assert!(h("WRITE=sp:true\0").contains("is analog"));
    assert!(h("WRITE=en:5\0").contains("is binary"));
    assert_eq!(
        sh.lock().unwrap().queue.len(),
        2,
        "rejected writes queue nothing"
    );
}

#[test]
fn stamp_lines_carry_both_clocks_the_tag_and_the_value_and_skip_rejected_writes() {
    use ferrofire::bennu::stamp_lines;
    let w = [
        ("a.b".to_string(), Value::Binary(false)),
        ("c".to_string(), Value::Analog(2.5)),
        ("bad".to_string(), Value::Analog(1.0)),
    ];
    let rejected = ["bad".to_string()];
    assert_eq!(
        stamp_lines(7, 9, &w, &rejected),
        [
            "Applied: t_apply_ns=7 t_solved_ns=9 tag=a.b val=false",
            "Applied: t_apply_ns=7 t_solved_ns=9 tag=c val=2.5"
        ]
    );
    assert_eq!(stamp_lines(7, 9, &w, &[]).len(), 3);
    assert!(stamp_lines(7, 9, &[], &[]).is_empty());
}

// ---- RADIO sender: interface choice for '*' and backpressure (ww-1df.27) ------------------------------------

fn multicast_if_of(s: &std::net::UdpSocket) -> std::net::Ipv4Addr {
    use std::os::fd::AsRawFd;
    let mut a = libc::in_addr { s_addr: 0 };
    let mut len = std::mem::size_of::<libc::in_addr>() as libc::socklen_t;
    // SAFETY: `a` and `len` outlive the call and `len` is exactly the size of `a`; the fd is live (borrowed).
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            &mut a as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    assert_eq!(rc, 0);
    std::net::Ipv4Addr::from(u32::from_be(a.s_addr))
}

#[test]
fn pick_up_ipv4_skips_loopback_and_down_interfaces() {
    use ferrofire::sys::pick_up_ipv4;
    let up = libc::IFF_UP as u32;
    let lo = libc::IFF_LOOPBACK as u32;
    let a = |n: u8| std::net::Ipv4Addr::new(10, 0, 0, n);
    assert_eq!(
        pick_up_ipv4([(up | lo, a(1)), (up, a(2)), (up, a(3))]),
        Some(a(2))
    );
    assert_eq!(pick_up_ipv4([(0, a(1)), (up, a(2))]), Some(a(2)));
    assert_eq!(pick_up_ipv4([(up | lo, a(1)), (0, a(2))]), None);
    assert_eq!(pick_up_ipv4([]), None);
}

#[test]
fn wildcard_interface_sets_multicast_if_to_the_first_up_non_loopback_address() {
    let mc = parse_udp("udp://239.0.0.1:9").unwrap().1;
    let want = ferrofire::sys::first_up_ipv4().expect("the test container has a non-loopback IPv4");
    assert!(!want.is_loopback());
    assert_eq!(multicast_if_of(&udp_sender(None, mc).unwrap()), want);
    // an explicit interface stays authoritative
    let s = udp_sender(Some("127.0.0.1"), mc).unwrap();
    assert_eq!(multicast_if_of(&s), std::net::Ipv4Addr::LOCALHOST);
}

#[test]
fn grow_sndbuf_raises_a_small_buffer_to_the_need_and_caps_it() {
    use socket2::SockRef;
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    SockRef::from(&s).set_send_buffer_size(4096).unwrap();
    let small = SockRef::from(&s).send_buffer_size().unwrap();
    let got = grow_sndbuf(&s, 100_000);
    assert!(got > small && got >= 100_000, "{small} -> {got}");
    assert_eq!(got, SockRef::from(&s).send_buffer_size().unwrap());
    // already big enough: untouched
    assert_eq!(grow_sndbuf(&s, 1000), got);
    // absurd need: request capped at SNDBUF_CAP (the kernel doubles it)
    assert!(grow_sndbuf(&s, usize::MAX / 4) <= 2 * SNDBUF_CAP);
}

fn would_block() -> std::io::Error {
    std::io::ErrorKind::WouldBlock.into()
}

#[test]
fn send_all_waits_for_writability_on_eagain_and_drops_nothing() {
    // a send buffer that holds 3 datagrams until the wait drains it
    let ds: Vec<Vec<u8>> = (0..10u8).map(|i| vec![i; 4]).collect();
    let (room, mut sent, mut waits) = (std::cell::Cell::new(3), Vec::new(), 0);
    let (dropped, err) = send_all(
        &ds,
        |d| {
            if room.get() == 0 {
                return Err(would_block());
            }
            room.set(room.get() - 1);
            sent.push(d.to_vec());
            Ok(d.len())
        },
        |_| {
            waits += 1;
            room.set(3);
            true
        },
        Duration::from_secs(5),
    );
    assert_eq!((dropped, err.is_none()), (0, true));
    assert_eq!(sent, ds, "in order, none lost");
    assert_eq!(waits, 3);
}

#[test]
fn send_all_drops_and_counts_what_it_cannot_send_within_the_budget() {
    let ds: Vec<Vec<u8>> = vec![vec![1], vec![2], vec![3]];
    // never writable: every datagram is dropped, none waits more than once
    let mut waits = 0;
    let (dropped, err) = send_all(
        &ds,
        |_| Err(would_block()),
        |_| {
            waits += 1;
            false
        },
        Duration::from_secs(5),
    );
    assert_eq!(dropped, 3);
    assert_eq!(err.unwrap().kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(waits, 3);
    // an exhausted budget does not even wait
    let (dropped, _) = send_all(
        &ds,
        |_| Err(would_block()),
        |_| panic!("no time left to wait"),
        Duration::ZERO,
    );
    assert_eq!(dropped, 3);
    // writable but still full: bounded by the budget, not by the wait's answer
    let t = std::time::Instant::now();
    let (dropped, _) = send_all(
        &ds[..1],
        |_| Err(would_block()),
        |_| true,
        Duration::from_millis(20),
    );
    assert_eq!(dropped, 1);
    assert!(t.elapsed() >= Duration::from_millis(20) && t.elapsed() < Duration::from_secs(2));
}

#[test]
fn send_all_keeps_going_after_a_hard_error_and_reports_it() {
    let ds: Vec<Vec<u8>> = vec![vec![1], vec![2], vec![3]];
    let mut n = 0;
    let (dropped, err) = send_all(
        &ds,
        |d| {
            n += 1;
            if n == 2 {
                Err(std::io::ErrorKind::NetworkUnreachable.into())
            } else {
                Ok(d.len())
            }
        },
        |_| panic!("a hard error is not waited on"),
        Duration::from_secs(1),
    );
    assert_eq!((dropped, n), (1, 3));
    assert_eq!(err.unwrap().kind(), std::io::ErrorKind::NetworkUnreachable);
}

#[test]
fn a_full_8112_tag_cycle_through_the_real_publisher_loses_no_datagram() {
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket2::SockRef::from(&rx)
        .set_recv_buffer_size(8 << 20)
        .unwrap();
    rx.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let ep = format!("udp://127.0.0.1:{}", rx.local_addr().unwrap().port());
    let p = ferrofire::transport::publisher(&ep).unwrap();
    let msg = snapshot_msg(&big_snap(8112));
    let want = datagrams_msg("g", &msg, 0, 0, 0).len();
    assert!(want > 100, "a cycle is many datagrams: {want}");
    p.publish(&msg);
    let mut buf = [0u8; 2048];
    let got = std::iter::from_fn(|| rx.recv(&mut buf).ok()).count();
    assert_eq!((p.dropped(), got), (0, want));
}
