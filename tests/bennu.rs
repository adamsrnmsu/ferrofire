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
    assert_eq!(chunks_nul("a:1,b:2,"), vec![b"a:1,b:2,\0".to_vec()]); // the bytes both transports send
    assert_eq!(chunk_message("longlonglong,", 4), vec!["longlonglong,"]); // over-long single part alone
    let snap: Vec<_> = (0..400)
        .map(|i| (format!("bus-{i}.voltage"), Value::Analog(1.0123456789)))
        .collect();
    let ds = datagrams("54386972605942b", &snap);
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
    assert_eq!(all.matches(',').count(), 400); // no part split across datagrams
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

#[cfg(feature = "zmq")]
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
    assert_eq!((d.publish, d.transport), (None, None)); // the transport picks the publish default
    let t = a(&["x.yaml", "--transport", "nng"]).unwrap();
    assert_eq!(t.transport.as_deref(), Some("nng"));
    assert_eq!(d.nng_sendbuf, None);
    assert_eq!(a(&["--nng-sendbuf", "64"]).unwrap().nng_sendbuf, Some(64));
    assert!(a(&["--nng-sendbuf", "-1"])
        .unwrap_err()
        .starts_with("--nng-sendbuf:"));
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
