//! Replays what the stock bennu Modbus server did (conformance/modbus/stock-*.trace, recorded by probe_*.py through
//! `conformance/differential.sh`) against our state machine, with no sockets. Every reply must be the same bytes (and the same
//! closed-or-open), what a stand-in provider was sent must be the same points and values, and the event log the same lines.
//! The one family of rows that differs on purpose is SPEC Q4 (a value outside its range): `q4` below states the rule, and the
//! test fails if a differing word is anything else. Frames our server refuses (`probe_divergent.py`) are asserted at the end.

mod common;

use ferrofire::fd::eventlog::EventLog;
use ferrofire::fd::proto::modbus::device::{Close, Conn, Device};
use ferrofire::fd::proto::modbus::Server;
use ferrofire::fd::scan::item;
use ferrofire::fd::store::Store;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Rig {
    dev: Device,
    store: Store,
    conn: Conn,
    log: EventLog,
    logged: Buf,
    /// A stand-in provider is answering: queued writes are acknowledged at every sleep.
    provider: bool,
    sent: Vec<String>,
    out: Vec<u8>,
    closed: bool,
    request: Vec<u8>,
}

fn rig(xml: &str) -> Rig {
    let logged = Buf::default();
    let (server, store) = common::load_as::<Server>(xml, "modbus-server");
    Rig {
        dev: server.device(),
        store,
        conn: Conn::new(),
        log: EventLog::to("modbus-server", Box::new(logged.clone())),
        logged,
        provider: false,
        sent: vec![],
        out: vec![],
        closed: false,
        request: vec![],
    }
}

impl Rig {
    fn connect(&mut self) {
        self.conn = Conn::new();
        self.out.clear();
        self.closed = false;
    }

    fn send(&mut self, bytes: &[u8]) {
        self.request = bytes.to_vec();
        match self.conn.feed(&self.dev, &self.store, &self.log, bytes) {
            Ok(o) => self.out.extend(o),
            Err(Close(_)) => self.closed = true,
        }
    }

    /// Time passes: with a provider answering, what is queued is sent to it and acknowledged.
    fn sleep(&mut self) {
        if self.provider {
            for (tag, point, v) in self.store.pending(0) {
                self.sent.push(item(&point, &v));
                self.store.ack(&tag, v);
            }
        }
    }

    fn publish(&self, body: &str) {
        for kv in body.split(',').filter(|kv| !kv.is_empty()) {
            let (name, v) = kv.split_once(':').unwrap();
            let x = match v {
                "true" => 1.0,
                "false" => 0.0,
                n => n.parse().unwrap(),
            };
            self.store.publish(name, x, 1_794_324_000);
        }
    }
}

/// The reference for SPEC Q4: the scaled value before the conversion to 16 bits, and what each side does with it.
fn q4(dev: &Device, request: &[u8], store: &Store, ours: &[u8], stock: &[u8]) -> bool {
    // an MBAP request for FC3/FC4 and its reply; every register word that differs must be an out-of-range value
    if request.len() < 12
        || !matches!(request[7], 3 | 4)
        || ours.len() != stock.len()
        || ours.len() < 9
    {
        return false;
    }
    let (fc, addr) = (request[7], u16::from_be_bytes([request[8], request[9]]));
    let table = if fc == 3 { &dev.holding } else { &dev.input };
    let (o, s) = (&ours[9..], &stock[9..]);
    (0..o.len() / 2).all(|i| {
        let (a, b) = (
            u16::from_be_bytes([o[2 * i], o[2 * i + 1]]),
            u16::from_be_bytes([s[2 * i], s[2 * i + 1]]),
        );
        if a == b {
            return true;
        }
        let reg = &table[&(addr + i as u16)];
        let (slope, v) = (
            65535.0 / (reg.range.max - reg.range.min),
            store.get_f64(&reg.tag).unwrap(),
        );
        let x = slope * v + -slope * reg.range.min;
        // stock: saturating conversion to 32 bits, low 16 kept (arm64); ours: clamp
        b == (x as u32) as u16
            && (x > 65535.0 || x < 0.0)
            && a == if x > 65535.0 { 65535 } else { 0 }
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The log as `level,event,message` with the sequence number: timestamps dropped.
fn log_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.contains(",modbus-server,"))
        .map(|l| {
            let f: Vec<&str> = l.splitn(6, ',').collect();
            format!("{},{},{},{}", f[0], f[2], f[4], f[5])
        })
        .collect()
}

fn replay(config: &str, set: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/modbus");
    let read =
        |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
    let xml = read(config)
        .replace("rtu-1-modbus-server.log", "/dev/null")
        .replace("rtu-1-modbus-tcp-server.log", "/dev/null")
        .replace("edges-modbus-server.log", "/dev/null");
    let mut r = rig(&xml);
    let items: Vec<serde_json::Value> = read(&format!("stock-{set}.trace"))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let (mut label, mut rows, mut diverged, mut saw) = (String::new(), 0, 0, 0);
    for it in &items {
        match it[0].as_str().unwrap() {
            "provider" => r.provider = true,
            "label" => label = it[1].as_str().unwrap().to_string(),
            "connect" => r.connect(),
            "send" => r.send(&common::unhex(it[1].as_str().unwrap())),
            "sleep" => r.sleep(),
            "publish" => r.publish(it[1].as_str().unwrap()),
            "provider-saw" => {
                saw += 1;
                let mut want: Vec<String> = it[1]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect();
                want.sort();
                r.sent.sort();
                assert_eq!(
                    r.sent, want,
                    "{set}: what the provider was sent after `{label}`"
                );
                r.sent.clear();
            }
            "recv" => {
                rows += 1;
                let stock = common::unhex(it[1].as_str().unwrap());
                let eof = it[2].as_bool().unwrap();
                if r.out != stock {
                    assert!(
                        q4(&r.dev, &r.request, &r.store, &r.out, &stock),
                        "{set}: reply to `{label}`\n ours : {}\n stock: {}",
                        hex(&r.out),
                        hex(&stock)
                    );
                    diverged += 1;
                }
                assert_eq!(
                    r.closed, eof,
                    "{set}: `{label}`: connection closed by us / by stock"
                );
                r.out.clear();
            }
            _ => {}
        }
    }
    println!("{set}: {rows} replies, {diverged} differ in SPEC Q4 (out-of-range) words, {saw} provider checks");
    assert!(rows > 20, "{set}: the trace has {rows} replies");
    let stock = log_lines(&read(&format!("stock-{set}.logs")));
    let ours = log_lines(&String::from_utf8(r.logged.0.lock().unwrap().clone()).unwrap());
    assert!(
        ours == stock,
        "{set}: event log differs\n{}",
        first_diff(&ours, &stock)
    );
}

fn first_diff(a: &[String], b: &[String]) -> String {
    let n = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    format!(
        "line {n} of {}/{}:\n ours : {:?}\n stock: {:?}",
        a.len(),
        b.len(),
        a.get(n),
        b.get(n)
    )
}

#[test]
fn matches_stock_on_the_spec_transcript_and_the_function_code_sweep() {
    replay("diff-server.xml", "diff");
}

#[test]
fn matches_stock_on_the_waterway_rtu() {
    replay("diff-waterway-server.xml", "waterway");
}

#[test]
fn matches_stock_on_scaling_bit_packing_address_corners_and_writes() {
    replay("diff-edges-server.xml", "edges");
}

#[test]
fn out_of_range_values_differ_from_stock_only_by_the_clamp_of_spec_q4() {
    replay("diff-edges-server.xml", "q4");
}

/// `conformance/modbus/stock-divergent.golden` is what stock did with these; this is what we do instead (SPEC 3.6 Q9 and the notes in
/// conformance/README.md). The cut-short frames get no answer here because they are not whole frames: the socket layer closes when the
/// client does (tests/modbus_net.rs).
#[test]
fn frames_that_stock_answers_with_exception_4_or_garbage_or_dies_on_are_refused() {
    enum Out {
        Closed,
        Waits,
        Reply(&'static str),
    }
    let xml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/conformance/modbus/diff-server.xml"
    ))
    .unwrap()
    .replace("rtu-1-modbus-server.log", "/dev/null");
    let r = rig(&xml);
    let head = |pid: u16, len: u16| {
        let mut h = vec![0, 1];
        h.extend(pid.to_be_bytes());
        h.extend(len.to_be_bytes());
        h.push(1);
        h
    };
    let read = [3u8, 0x9c, 0x40, 0, 1];
    let long = |len: usize| [head(0, len as u16), read.to_vec(), vec![0; len - 6]].concat();
    let cases: Vec<(&str, Vec<u8>, Out)> = vec![
        (
            "length 0",
            [head(0, 0), read.to_vec()].concat(),
            Out::Closed,
        ),
        ("length 1", head(0, 1), Out::Closed),
        (
            "length 1 and a read",
            [head(0, 1), read.to_vec()].concat(),
            Out::Closed,
        ),
        (
            "protocol id 1",
            [head(1, 6), read.to_vec()].concat(),
            Out::Closed,
        ),
        (
            "protocol id 0xff00",
            [head(0xFF00, 6), read.to_vec()].concat(),
            Out::Closed,
        ),
        ("length 255", long(255), Out::Closed),
        ("length 256", long(256), Out::Closed),
        ("length 1000", long(1000), Out::Closed),
        ("length 1018", long(1018), Out::Closed),
        ("length 65535 declared", head(0, 65535), Out::Closed),
        (
            "length 254 is the largest",
            long(254),
            Out::Reply("0001000000050103020000"),
        ),
        ("header only", head(0, 6), Out::Waits),
        (
            "cut after the function code",
            [head(0, 6), vec![3]].concat(),
            Out::Waits,
        ),
        (
            "cut one byte short",
            [head(0, 6), read[..4].to_vec()].concat(),
            Out::Waits,
        ),
        (
            "header cut at 5 bytes",
            head(0, 6)[..5].to_vec(),
            Out::Waits,
        ),
        (
            "FC16 bytecount 2 with no data",
            [head(0, 7), vec![16, 0x9c, 0x40, 0, 1, 2]].concat(),
            Out::Reply("000100000003019003"),
        ),
        (
            "FC16 bytecount 2 with 1 data byte",
            [head(0, 8), vec![16, 0x9c, 0x40, 0, 1, 2, 0xff]].concat(),
            Out::Reply("000100000003019003"),
        ),
        (
            "FC15 2 coils, bytecount 1, no data",
            [head(0, 7), vec![15, 0, 0, 0, 2, 1]].concat(),
            Out::Reply("000100000003018f03"),
        ),
    ];
    for (name, bytes, want) in cases {
        let mut conn = Conn::new();
        let got = conn.feed(&r.dev, &r.store, &r.log, &bytes);
        match (want, got) {
            (Out::Closed, Err(_)) => {}
            (Out::Waits, Ok(out)) if out.is_empty() => {}
            (Out::Reply(hex_want), Ok(out)) => assert_eq!(hex(&out), hex_want, "{name}"),
            (_, got) => panic!("{name}: {got:?}"),
        }
    }
}
