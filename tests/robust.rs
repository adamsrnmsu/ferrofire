//! Nothing falls over: hostile requests, a panicking handler, lost replies, failing UDP sends, garbage datagrams.
use ferrofire::bennu::{frame, group_for, handle, Meta, Shared};
use ferrofire::datagram::{Receiver, Reject};
use ferrofire::model::Model;
use ferrofire::transport::{self, Publisher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn setup() -> (Meta, Mutex<Shared>) {
    let m = Model::load(include_str!("../models/waterway.yaml")).unwrap();
    let snap = m
        .snapshot()
        .into_iter()
        .map(|(t, v)| (t.to_string(), v))
        .collect();
    let sh = Shared {
        snap,
        queue: vec![],
    };
    (Meta::of(&m), Mutex::new(sh))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// xorshift64*: a seeded generator, so a failure repeats.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn is_reply(r: &[u8]) -> bool {
    r.last() == Some(&0) && (r.starts_with(b"ACK=") || r.starts_with(b"ERR="))
}

#[test]
fn every_kind_of_bad_request_gets_one_err_naming_the_problem() {
    let (meta, sh) = setup();
    let h = |b: &[u8]| String::from_utf8(handle(b, &meta, &sh)).unwrap();
    let big = format!("WRITE={}", "a:1,".repeat(500_000));
    for (req, names) in [
        (&b""[..], "Malformed"),
        (b"\0", "Malformed"),
        (b"=", "Unknown command"),
        (b"READ=", "not found"),
        (b"WRITE=lowervalve.open:abc", "lowervalve.open"),
        (b"WRITE=lowervalve.open:", "lowervalve.open"),
        (b"WRITE=nope:1", "nope"),
        (b"WRITE=:1", "Tag ''"),
        (b"WRITE=lowervalve.open", "Malformed point"),
        (b"WRITE=lowervalve.open:nan", "lowervalve.open"),
        (b"WRITE=lowervalve.open:inf", "lowervalve.open"),
        (b"\xff\xfe\x00\x80", "UTF-8"),
        (big.as_bytes(), "too large"),
    ] {
        let r = h(req);
        assert!(r.starts_with("ERR="), "{req:?} -> {r}");
        assert!(r.contains(names), "{req:?} -> {r}");
        assert!(r.len() < 300 && r.ends_with('\0'), "{r}");
    }
    assert!(
        sh.lock().unwrap().queue.is_empty(),
        "no bad write was queued"
    );
}

#[test]
fn a_misspelt_tag_is_an_err_that_suggests_the_right_one() {
    let (meta, sh) = setup();
    let h = |s: &str| String::from_utf8(handle(s.as_bytes(), &meta, &sh)).unwrap();
    for req in [
        "WRITE=lowervalve.opn:1",
        "READ=lock.hieght",
        "READ=lock.height,lock.hieght,",
    ] {
        let r = h(req);
        assert!(
            r.starts_with("ERR=") && r.contains("did you mean `"),
            "{req} -> {r}"
        );
    }
    assert!(h("WRITE=lowervalve.opn:1").contains("`lowervalve.open`"));
}

#[test]
fn random_bytes_never_panic_and_always_get_a_reply() {
    let (meta, sh) = setup();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let seeds: [&[u8]; 4] = [
        b"READ=lock.height,lowervalve.open,\0",
        b"WRITE=lowervalve.open:1.000000,uppergate.open:true\0",
        b"QUERY=\0",
        b"write=a:b:c,,:,\0",
    ];
    for i in 0..20_000 {
        let mut req: Vec<u8> = if i % 2 == 0 {
            (0..rng.below(300)).map(|_| rng.next() as u8).collect()
        } else {
            let mut s = seeds[rng.below(seeds.len())].to_vec();
            for _ in 0..1 + rng.below(4) {
                match rng.below(3) {
                    0 if !s.is_empty() => {
                        let at = rng.below(s.len());
                        s[at] = rng.next() as u8;
                    }
                    1 if !s.is_empty() => {
                        s.remove(rng.below(s.len()));
                    }
                    _ => s.insert(rng.below(s.len() + 1), b"=:,\0"[rng.below(4)]),
                }
            }
            s
        };
        req.truncate(1000);
        let r = handle(&req, &meta, &sh);
        assert!(is_reply(&r), "{req:?} -> {r:?}");
    }
}

/// Serve on a free port; another test may grab the port between the probe and the bind, so try a few.
fn serve_on(handler: impl Fn() -> transport::Handler) -> String {
    for _ in 0..20 {
        let ep = format!("tcp://127.0.0.1:{}", free_port());
        if transport::serve(&ep, handler()).is_ok() {
            return ep;
        }
    }
    panic!("no free port");
}

#[test]
fn a_panicking_handler_still_answers_and_the_next_request_works() {
    let ep = serve_on(|| {
        Box::new(|req| {
            if req.starts_with(b"BOOM") {
                panic!("handler bug");
            }
            b"ACK=\0".to_vec()
        })
    });
    let mut c = transport::requester(&ep, 2000).unwrap();
    let r = String::from_utf8(c.call(b"BOOM=lock.height").unwrap()).unwrap();
    assert!(
        r.starts_with("ERR=") && r.contains("BOOM=lock.height"),
        "{r}"
    );
    assert_eq!(c.call(b"READ=x").unwrap(), b"ACK=\0");
}

#[test]
fn a_multipart_request_gets_exactly_one_reply() {
    let ep = serve_on(|| Box::new(|_| b"ACK=\0".to_vec()));
    let s = zmq::Context::new().socket(zmq::REQ).unwrap();
    s.set_rcvtimeo(2000).unwrap();
    s.connect(&ep).unwrap();
    s.send_multipart(["QUERY=", "junk", "more"], 0).unwrap();
    let r = s.recv_multipart(0).unwrap();
    assert!(
        r.len() == 1 && r[0].starts_with(b"ERR=request has 3 frames"),
        "{r:?}"
    );
    s.send("QUERY=", 0).unwrap();
    assert_eq!(s.recv_bytes(0).unwrap(), b"ACK=\0");
}

#[test]
fn a_client_survives_a_lost_reply() {
    // a ROUTER stands in for a provider that swallows the first reply
    let ctx = zmq::Context::new();
    let router = ctx.socket(zmq::ROUTER).unwrap();
    router.bind("tcp://127.0.0.1:*").unwrap();
    let ep = router.get_last_endpoint().unwrap().unwrap();
    let t = std::thread::spawn(move || {
        for n in 0..2 {
            let m = router.recv_multipart(0).unwrap();
            if n == 1 {
                router
                    .send_multipart([&m[0][..], b"", b"ACK=7\0"], 0)
                    .unwrap();
            }
        }
    });
    let mut c = transport::requester(&ep, 300).unwrap();
    assert!(
        c.call(b"READ=a").is_err(),
        "lost reply: timeout, not a hang"
    );
    assert_eq!(
        c.call(b"READ=a").unwrap(),
        b"ACK=7\0",
        "next call is not wedged"
    );
    t.join().unwrap();
}

struct Counting(std::sync::atomic::AtomicUsize);
impl Publisher for Counting {
    fn publish(&self, _: &str) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn describe(&self) -> String {
        "count".into()
    }
}

#[test]
fn udp_send_errors_are_counted_and_never_block_or_panic() {
    // port 0 is refused by the kernel (EINVAL): the same path as ENOBUFS or an unreachable network
    let p = transport::publisher("udp://127.0.0.1:0").unwrap();
    let t = Instant::now();
    for _ in 0..50 {
        p.publish("lock.height:5,lowervalve.open:0,");
    }
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(p.dropped(), 50);
    let c = Counting(Default::default());
    assert_eq!(
        c.dropped(),
        0,
        "a publisher that cannot fail reports no drops"
    );
}

fn dgram(group: &str, body: &str) -> Vec<u8> {
    let mut b = body.as_bytes().to_vec();
    b.push(0);
    frame(group, &b)
}

#[test]
fn the_receiver_ignores_garbage_truncated_unknown_duplicate_and_old_datagrams() {
    let ep = "udp://239.0.0.1:40000";
    let g = group_for(ep);
    let mut r = Receiver::new(&g, ["a", "b"]);
    let ok = dgram(
        &g,
        "sim.t:1,a:1.5,b:true,x:9,_ff.seq:5,_ff.cyc:2,_ff.t:100,",
    );
    // unknown names are skipped, the pseudo-tags are consumed, true/false are 1/0
    assert_eq!(
        r.accept(&ok).unwrap(),
        vec![("a".to_string(), 1.5), ("b".to_string(), 1.0)]
    );
    assert_eq!(r.accept(&ok), Err(Reject::Stale), "duplicate");
    let old = dgram(&g, "a:2,_ff.seq:4,_ff.cyc:1,_ff.t:90,");
    assert_eq!(r.accept(&old), Err(Reject::Stale), "older cycle");
    let next = dgram(&g, "a:3,_ff.seq:6,_ff.cyc:3,_ff.t:110,");
    assert_eq!(r.accept(&next).unwrap(), vec![("a".to_string(), 3.0)]);
    // a provider restart: the counter starts over but the clock moved on
    let restart = dgram(&g, "a:4,_ff.seq:0,_ff.cyc:0,_ff.t:5000,");
    assert_eq!(r.accept(&restart).unwrap(), vec![("a".to_string(), 4.0)]);
    // not ours / not a datagram / cut short (no NUL) / empty
    assert_eq!(
        r.accept(&dgram("deadbeef", "a:1,")),
        Err(Reject::WrongGroup)
    );
    assert_eq!(r.accept(b"\xff\xff"), Err(Reject::Garbage));
    assert_eq!(r.accept(b""), Err(Reject::Garbage));
    let mut cut = dgram(&g, "a:123.5,_ff.seq:9,_ff.cyc:9,_ff.t:9999,");
    cut.pop();
    assert_eq!(r.accept(&cut), Err(Reject::Truncated));
    // bad values and half pairs are skipped, the good pairs of the same datagram survive
    let messy = dgram(&g, "a:zzz,b:,:3,,a,b:2,_ff.seq:10,_ff.cyc:9,_ff.t:9999,");
    assert_eq!(r.accept(&messy).unwrap(), vec![("b".to_string(), 2.0)]);
    // a stock provider sends no trailer: nothing to order by, everything is accepted
    let stock = dgram(&g, "a:7,");
    assert_eq!(r.accept(&stock).unwrap().len(), 1);
    assert_eq!(r.accept(&stock).unwrap().len(), 1);
}

fn chunk(g: &str, seq: u64, cyc: u64, body: &str) -> Vec<u8> {
    dgram(
        g,
        &format!("{body}_ff.seq:{seq},_ff.cyc:{cyc},_ff.t:{},", 1000 + seq),
    )
}

fn names(r: Result<Vec<(String, f64)>, Reject>) -> Vec<String> {
    r.unwrap().into_iter().map(|p| p.0).collect()
}

#[test]
fn reordered_chunks_of_one_cycle_all_apply_and_duplicates_do_not() {
    let g = group_for("udp://239.0.0.1:40000");
    let mut r = Receiver::new(&g, ["a", "b", "c"]);
    assert_eq!(names(r.accept(&chunk(&g, 2, 1, "c:3,"))), ["c"]);
    assert_eq!(
        names(r.accept(&chunk(&g, 0, 1, "a:1,"))),
        ["a"],
        "late, lower seq"
    );
    assert_eq!(names(r.accept(&chunk(&g, 1, 1, "b:2,"))), ["b"]);
    assert_eq!(
        r.accept(&chunk(&g, 1, 1, "b:2,")),
        Err(Reject::Stale),
        "repeat of a late seq"
    );
    assert_eq!(
        r.accept(&chunk(&g, 2, 1, "c:3,")),
        Err(Reject::Stale),
        "repeat of the newest seq"
    );
}

#[test]
fn a_late_old_cycle_does_not_overwrite_newer_values() {
    let g = group_for("udp://239.0.0.1:40000");
    let mut r = Receiver::new(&g, ["a", "b"]);
    r.accept(&chunk(&g, 2, 2, "a:20,")).unwrap();
    assert_eq!(r.accept(&chunk(&g, 0, 1, "a:10,")), Err(Reject::Stale));
    // per tag: b was never seen in cycle 2, so its cycle 1 value still counts
    assert_eq!(names(r.accept(&chunk(&g, 1, 1, "a:10,b:11,"))), ["b"]);
    // a seq far behind the newest is out of the window even if never seen
    r.accept(&chunk(&g, 10_000, 9, "a:90,")).unwrap();
    assert_eq!(r.accept(&chunk(&g, 5, 9, "b:1,")), Err(Reject::Stale));
}

#[test]
fn a_restarted_provider_is_still_detected_after_reordering() {
    let g = group_for("udp://239.0.0.1:40000");
    let mut r = Receiver::new(&g, ["a"]);
    r.accept(&chunk(&g, 7, 3, "a:1,")).unwrap();
    r.accept(&chunk(&g, 6, 3, "a:2,")).unwrap();
    // counters back at 0, clock moved on; seq 6 and cycle 3 are now new again
    let restart = dgram(&g, "a:9,_ff.seq:6,_ff.cyc:0,_ff.t:5000,");
    assert_eq!(names(r.accept(&restart)), ["a"]);
    assert_eq!(
        names(r.accept(&dgram(&g, "a:8,_ff.seq:7,_ff.cyc:1,_ff.t:5001,"))),
        ["a"]
    );
}

#[test]
fn shuffled_chunks_give_the_state_of_in_order_delivery() {
    let g = group_for("udp://239.0.0.1:40000");
    let tags = ["a", "b", "c", "d", "e", "f"];
    let all: Vec<Vec<u8>> = (0..12u64)
        .map(|seq| {
            let (cyc, i) = (seq / 3, (seq % 3) as usize * 2);
            let body = format!(
                "{}:{},{}:{},",
                tags[i],
                cyc * 10 + 1,
                tags[i + 1],
                cyc * 10 + 2
            );
            chunk(&g, seq, cyc, &body)
        })
        .collect();
    let state = |order: &[usize]| {
        let mut r = Receiver::new(&g, tags);
        let mut m = std::collections::BTreeMap::new();
        for &i in order {
            for (k, v) in r.accept(&all[i]).unwrap_or_default() {
                m.insert(k, v as i64);
            }
        }
        m
    };
    let in_order = state(&(0..12).collect::<Vec<_>>());
    let mut rng = Rng(7);
    for _ in 0..200 {
        // shuffle within a window of 4, as jitter does: never further than a cycle or two
        let mut order: Vec<usize> = (0..12).collect();
        for w in order.chunks_mut(4) {
            for i in (1..w.len()).rev() {
                w.swap(i, rng.below(i + 1));
            }
        }
        assert_eq!(state(&order), in_order, "{order:?}");
    }
}

#[test]
fn random_datagrams_never_panic_the_receiver() {
    let g = group_for("udp://239.0.0.1:40000");
    let mut r = Receiver::new(&g, ["a"]);
    let mut rng = Rng(42);
    for _ in 0..20_000 {
        let n = rng.below(120);
        let mut d: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if rng.below(2) == 0 {
            let mut f = frame(&g, b"a:1,_ff.seq:1,_ff.cyc:1,_ff.t:1,\0");
            let at = rng.below(f.len());
            f.truncate(at.max(1));
            d = f;
        }
        let _ = r.accept(&d);
    }
}

#[test]
fn shared_state_survives_a_poisoned_lock() {
    let (meta, sh) = setup();
    let sh = Arc::new(sh);
    let s2 = sh.clone();
    let _ = std::thread::spawn(move || {
        let _g = s2.lock().unwrap();
        panic!("died holding the lock");
    })
    .join();
    let r = handle(b"READ=lock.height", &meta, &sh);
    assert_eq!(r, b"ACK=5\0");
}

// ---- startup validation: the file, the key, the tag, and what was probably meant ----

/// stderr of a failed `ferrofire run` and the model path.
fn run_bad(name: &str, yaml: &str, extra: &[&str]) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("ff-robust-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.yaml");
    std::fs::write(&path, yaml).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ferrofire"))
        .arg("run")
        .arg(&path)
        .args(extra)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    (err, path.to_str().unwrap().to_string())
}

#[test]
fn a_misspelt_yaml_key_names_the_file_and_the_key() {
    let (err, path) = run_bad("key", "version: 1\ndt: 0.1\nstpes: 3\n", &[]);
    assert!(err.contains(&path) && err.contains("stpes"), "{err}");
    let (err, _) = run_bad("key2", "version: 1\ndt: 0.1\nsubstep: 3\n", &[]);
    assert!(err.contains("did you mean `substeps`"), "{err}");
}

#[test]
fn a_misspelt_name_in_an_expression_suggests_the_right_one() {
    let y = "version: 1\ndt: 0.1\ninputs:\n  valve_open: {type: analog}\noutputs:\n  flow: {type: analog, expr: 'valve_opn * 2'}\n";
    let (err, path) = run_bad("expr", y, &[]);
    assert!(
        err.contains(&path)
            && err.contains("valve_opn")
            && err.contains("did you mean `valve_open`"),
        "{err}"
    );
}

#[test]
fn a_misspelt_publish_order_or_slice_tag_suggests_the_right_one() {
    let y = "version: 1\ndt: 0.1\ninputs:\n  valve_open: {type: analog}\noutputs:\n  flow: {type: analog, expr: 'valve_open * 2'}\npublish_order: [flw]\n";
    let (err, _) = run_bad("order", y, &[]);
    assert!(
        err.contains("flw") && err.contains("did you mean `flow`"),
        "{err}"
    );
    let ok = "version: 1\ndt: 0.1\ninputs:\n  valve_open: {type: analog}\noutputs:\n  flow: {type: analog, expr: 'valve_open * 2'}\n";
    let (err, _) = run_bad(
        "slice",
        ok,
        &["--publish-tags", "udp://127.0.0.1:41000=flwo"],
    );
    assert!(
        err.contains("flwo") && err.contains("did you mean `flow`"),
        "{err}"
    );
}

#[test]
fn the_pull_bridge_names_a_misspelt_tag_with_a_suggestion() {
    use ferrofire::transport::Requester;
    struct Fake;
    impl Requester for Fake {
        fn call(&mut self, _: &[u8]) -> Result<Vec<u8>, String> {
            Ok(b"ACK=lock.height,lowervalve.open,\0".to_vec())
        }
    }
    let e = ferrofire::pull::validate(&mut Fake, &["lock.hieght".to_string()]).unwrap_err();
    assert!(
        e.contains("lock.hieght") && e.contains("did you mean `lock.height`"),
        "{e}"
    );
}
