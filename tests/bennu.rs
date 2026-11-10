//! Protocol pieces (spec 6.1): group hash, framing, chunking, request handling, and a real REP round trip.
use ferrofire::bennu::*;
use ferrofire::model::{Model, Value};
use std::sync::{Arc, Mutex};

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
fn run_arguments() {
    let a = |s: &[&str]| RunArgs::parse(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
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
    assert_eq!(ok.path.as_deref(), Some("m.yaml"));
    assert_eq!(
        (ok.server.as_str(), ok.publish.as_deref()),
        ("tcp://*:1", Some("udp://*;239.0.0.1:2"))
    );
    assert_eq!(ok.rate, Some(0.5));
    let d = a(&["--model", "x.yaml"]).unwrap();
    assert_eq!(d.path.as_deref(), Some("x.yaml"));
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
                                                  // a second bare word is not a model, and a flag with no value is refused
    assert!(a(&["--model", "x.yaml", "y.yaml"])
        .unwrap_err()
        .contains("unknown argument `y.yaml`"));
    assert!(a(&["--rate"]).unwrap_err().contains("--rate needs a value"));
    assert!(a(&["--rate", "fast"]).unwrap_err().starts_with("--rate:"));
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

fn sink(tags: Option<&[&str]>) -> (Target, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(vec![]));
    let tags = tags.map(|t| t.iter().map(|s| s.to_string()).collect());
    (
        Target {
            out: Box::new(Sink(log.clone())),
            tags,
        },
        log,
    )
}

#[test]
fn publish_happens_once_per_tick_and_every_nth_tick() {
    let mut m = Model::load(include_str!("../models/waterway.yaml")).unwrap();
    let (meta, sh) = setup();
    let _ = meta;
    let (all, log) = sink(None);
    for k in 0..6 {
        tick_and_publish(&mut m, &sh, std::slice::from_ref(&all), k, 1);
    }
    assert_eq!(log.lock().unwrap().len(), 6);
    log.lock().unwrap().clear();
    for k in 0..6 {
        tick_and_publish(&mut m, &sh, std::slice::from_ref(&all), k, 3);
    }
    assert_eq!(log.lock().unwrap().len(), 2); // ticks 0 and 3
    assert!((m.time - 12.0 * m.dt).abs() < 1e-9); // every call also stepped the engine
}

#[test]
fn slices_get_their_own_tags_and_may_overlap() {
    let snap = big_snap(10);
    let (a, la) = sink(Some(&["bus-3.voltage", "sim.t"]));
    let (b, lb) = sink(Some(&["bus-3.voltage", "bus-9.voltage"]));
    let (all, lall) = sink(None);
    publish_all(&[a, b, all], &snap);
    // snapshot order, not slice order; the overlapping tag goes to both
    assert!(la.lock().unwrap()[0].starts_with("sim.t:0.5,bus-3.voltage:"));
    assert_eq!(la.lock().unwrap()[0].matches(',').count(), 2);
    assert_eq!(lb.lock().unwrap()[0].matches(',').count(), 2);
    assert!(lb.lock().unwrap()[0].contains("bus-9.voltage:"));
    assert_eq!(lall.lock().unwrap()[0].matches(',').count(), 10);
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
