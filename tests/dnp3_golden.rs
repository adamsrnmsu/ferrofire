//! Replays what the stock bennu outstation did (conformance/dnp3/stock-*.trace, recorded by probe_diff.py / probe_big.py
//! through `conformance/differential.sh`) against our state machine, with a virtual clock and no sockets. Every response must
//! be the same link frames and the same application bytes; the event log must be the same lines. Two things a wall clock decides
//! are masked on both sides: the 6-byte provider time inside time-bearing objects, and IIN1.4 NEED_TIME (its 30 s rule has
//! its own unit test).

mod common;

use ferrofire::fd::eventlog::EventLog;
use ferrofire::fd::proto::dnp3::outstation::{Conn, Device};
use ferrofire::fd::proto::dnp3::{link, transport, Commands};
use ferrofire::fd::store::Store;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    now: Duration,
    next_sweep: Duration,
    out: Vec<u8>,
}

/// The config loaded as the field device does, with the module's event log swapped for one we can read.
fn rig(xml: &str) -> Rig {
    let logged = Buf::default();
    let (out, store) = common::load(xml);
    let log = EventLog::to("dnp3-server", Box::new(logged.clone()));
    let dev = out.device();
    log.log(
        "info",
        "dnp3 server init",
        &format!(
            "Binary Size is {} and Analog Size is {}.",
            dev.bin.len(),
            dev.ana.len()
        ),
    );
    let mut r = Rig {
        dev,
        store,
        conn: Conn::new(),
        log,
        logged,
        now: Duration::ZERO,
        next_sweep: Duration::ZERO,
        out: vec![],
    };
    r.advance(6.0); // the probes wait for the stock device to settle: its first sweep made the initial events
    r
}

impl Rig {
    /// Move the virtual clock; the 1 Hz sweep runs at every whole second passed, as the stock update thread does.
    fn advance(&mut self, secs: f64) {
        let target = self.now + Duration::from_secs_f64(secs);
        while self.next_sweep <= target {
            self.now = self.next_sweep;
            self.conn.tick(&mut self.dev, self.now);
            // the provider acknowledges queued writes within a scan cycle (the probes run a stand-in REP socket that does)
            for (tag, _, v) in self.store.pending(0) {
                self.store.ack(&tag, v);
            }
            self.dev.refresh(&self.store);
            self.next_sweep += Duration::from_secs(1);
        }
        self.now = target;
        self.conn.tick(&mut self.dev, self.now);
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut sink = Commands::new(&self.store, &self.log);
        if let Ok(o) = self.conn.feed(&mut self.dev, &mut sink, bytes, self.now) {
            self.out.extend(o);
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

/// Zero what a wall clock decides: provider times (little-endian near 1.79e9, then two zero octets) and NEED_TIME.
fn mask(app: &[u8]) -> Vec<u8> {
    let mut b = app.to_vec();
    if b.len() >= 4 && b[1] == 0x81 {
        b[2] &= !0x10;
    }
    for i in 0..b.len().saturating_sub(5) {
        if matches!(b[i + 3], 0x6a | 0x6b) && b[i + 4] == 0 && b[i + 5] == 0 {
            b[i..i + 6].fill(0);
        }
    }
    b
}

/// Wire bytes as comparable text: each link frame's control and addresses, its transport octet, and every reassembled fragment.
fn describe(wire: &[u8]) -> Vec<String> {
    let (mut out, mut rest, mut re) = (vec![], wire, transport::Reassembler::new(1 << 20));
    while let link::Parse::Frame(f, used) = link::parse(rest) {
        rest = &rest[used..];
        let tp = f
            .user
            .first()
            .map(|t| format!(" tp={t:02x}"))
            .unwrap_or_default();
        out.push(format!(
            "frame ctrl={:02x} {}<-{}{tp}",
            f.ctrl, f.dst, f.src
        ));
        if let Some(app) = re.push(&f.user).unwrap() {
            out.push(format!(
                "app {}",
                mask(&app)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
    }
    if !rest.is_empty() {
        out.push(format!("unparsed {} bytes", rest.len()));
    }
    out
}

/// Where two descriptions first differ, with a window of bytes around the difference (replies can be 2 KB).
fn first_diff(a: &[String], b: &[String]) -> String {
    let n = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let (x, y) = (
        a.get(n).cloned().unwrap_or_default(),
        b.get(n).cloned().unwrap_or_default(),
    );
    let at = x
        .bytes()
        .zip(y.bytes())
        .position(|(p, q)| p != q)
        .unwrap_or(x.len().min(y.len()));
    let win = |s: &str| {
        s.get(at.saturating_sub(60)..(at + 60).min(s.len()))
            .unwrap_or("")
            .to_string()
    };
    format!(
        "item {n} of {}/{}, text offset {at}:\n  ours : ...{}...\n  stock: ...{}...",
        a.len(),
        b.len(),
        win(&x),
        win(&y)
    )
}

/// The log as `event,message` pairs: sequence numbers kept (they must match), timestamps dropped.
fn log_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.contains(",dnp3-server,"))
        .map(|l| {
            let f: Vec<&str> = l.splitn(6, ',').collect();
            format!("{},{},{},{}", f[0], f[2], f[4], f[5])
        })
        .collect()
}

fn replay(config: &str, trace: &str, stock_logs: &str, name: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/dnp3");
    let read =
        |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
    let xml = read(config)
        .replace("dnp3-diff.log", "/dev/null")
        .replace("dnp3-big.log", "/dev/null");
    let mut r = rig(&xml);
    let items: Vec<serde_json::Value> = read(trace)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let label_after = |i: usize| {
        items[i..]
            .iter()
            .find(|x| x[0] == "label")
            .map(|x| x[1].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    let mut recvs = 0;
    for (i, it) in items.iter().enumerate() {
        match it[0].as_str().unwrap() {
            "send" => r.send(&common::unhex(it[1].as_str().unwrap())),
            "sleep" => r.advance(it[1].as_f64().unwrap()),
            "publish" => r.publish(it[1].as_str().unwrap()),
            "connect" => {
                r.conn.cancel(&mut r.dev);
                r.conn = Conn::new();
            }
            "recv" => {
                recvs += 1;
                let (ours, stock) = (
                    describe(&r.out),
                    describe(&common::unhex(it[1].as_str().unwrap())),
                );
                assert!(
                    ours == stock,
                    "{name}: reply #{recvs} to `{}`: {}",
                    label_after(i),
                    first_diff(&ours, &stock)
                );
                r.out.clear();
            }
            _ => {}
        }
    }
    assert!(recvs > 20, "{name}: the trace has {recvs} replies");
    let stock = log_lines(&read(stock_logs));
    let ours = log_lines(&String::from_utf8(r.logged.0.lock().unwrap().clone()).unwrap());
    assert!(
        ours == stock,
        "{name}: event log\n ours : {ours:?}\n stock: {stock:?}"
    );
}

#[test]
fn matches_the_stock_outstation_on_the_differential_probe() {
    replay(
        "diff-server.xml",
        "stock-diff.trace",
        "stock-diff.logs",
        "diff",
    );
}

#[test]
fn matches_the_stock_outstation_on_the_big_probe() {
    replay(
        "diff-big-server.xml",
        "stock-big.trace",
        "stock-big.logs",
        "big",
    );
}
