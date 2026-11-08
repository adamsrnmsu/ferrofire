//! Protocol pieces (spec 6.1): group hash, framing, chunking, request handling, and a real REP round trip.
use rustbelt::bennu::*;
use rustbelt::model::{Model, Value};
use std::sync::{Arc, Mutex};

#[test]
fn group_hash_matches_libstdcxx() {
    assert_eq!(libstdcxx_hash(b"udp://239.0.0.1:40000"), 0x54386972605942ba);
    assert_eq!(group_for("udp://239.0.0.1:40000"), "54386972605942b");
    assert_eq!(group_for("udp://127.0.0.1:5556"), "5622dcba8d3e1de");
    // interface prefix is stripped before hashing, on both sides
    assert_eq!(group_for("udp://*;239.0.0.1:40000"), "54386972605942b");
    assert_eq!(group_for("udp://10.1.1.2;239.0.0.1:40000"), "54386972605942b");
}

#[test]
fn framing_and_chunking() {
    let d = frame("54386972605942b", b"a:1,\0");
    assert_eq!(d[0], 15);
    assert_eq!(unframe(&d), Some(("54386972605942b", &b"a:1,\0"[..])));
    assert_eq!(unframe(&[9, b'x']), None); // group_len exceeds the datagram
    assert_eq!(chunk_message("a:1,,b:2,", 100), vec!["a:1,b:2,"]); // empty part skipped
    assert_eq!(chunk_message("aaaa,bbbb,cccc,", 11), vec!["aaaa,bbbb,", "cccc,"]);
    assert_eq!(chunk_message("longlonglong,", 4), vec!["longlonglong,"]); // over-long single part alone
    let snap: Vec<_> = (0..400).map(|i| (format!("bus-{i}.voltage"), Value::Analog(1.0123456789))).collect();
    let ds = datagrams("54386972605942b", &snap);
    assert!(ds.len() > 1 && ds.iter().all(|d| d.len() <= 1500 && *d.last().unwrap() == 0));
    let all: String = ds.iter().map(|d| String::from_utf8(unframe(d).unwrap().1.to_vec()).unwrap().trim_end_matches('\0').to_string()).collect();
    assert_eq!(all.matches(',').count(), 400); // no part split across datagrams
}

fn setup() -> (Arc<Meta>, Arc<Mutex<Shared>>) {
    let m = Model::load(include_str!("../models/waterway.yaml")).unwrap();
    let sh = Shared { snap: m.snapshot().into_iter().map(|(t, v)| (t.to_string(), v)).collect(), queue: vec![] };
    (Arc::new(Meta::of(&m)), Arc::new(Mutex::new(sh)))
}

#[test]
fn requests() {
    let (meta, sh) = setup();
    let h = |s: &str| String::from_utf8(handle(s.as_bytes(), &meta, &sh)).unwrap();
    let q = h("QUERY=\0");
    assert!(q.starts_with("ACK=") && q.ends_with(",\0") && q.matches(',').count() == 15, "{q}");
    assert_eq!(h("read=lock.height\0"), "ACK=5\0");
    assert!(h("READ=nope\0").starts_with("ERR="));
    assert!(h("READ\0").starts_with("ERR="));
    assert!(h("BOGUS=x\0").starts_with("ERR=Unknown command type"));
    assert_eq!(h("WRITE=lowervalve.open:1.000000,uppergate.open:true\0"), "ACK=\0");
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
    let t = std::thread::spawn(move || for _ in 0..3 { let r = rep.recv_bytes(0).unwrap(); rep.send(handle(&r, &meta, &sh), 0).unwrap(); });
    let req = ctx.socket(zmq::REQ).unwrap();
    req.connect(&ep).unwrap();
    for (q, want) in [("READ=lock.height\0", "ACK=5\0"), ("WRITE=lowervalve.open:1.000000\0", "ACK=\0"), ("WRITE=nope:1\0", "ERR=Tag 'nope' is unknown or read-only\0")] {
        req.send(q, 0).unwrap();
        assert_eq!(req.recv_bytes(0).unwrap(), want.as_bytes());
    }
    t.join().unwrap();
}
