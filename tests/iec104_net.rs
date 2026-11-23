//! `<iec60870-5-104-server>` over real sockets: a master that frames APDUs by hand. The rules are stock's
//! (`conformance/iec104/STOCK.md`); `conformance/iec104` checks the same against the stock transcript.

mod common;
use common::ports::free_port;

use ferrofire::fd::proto::iec104::Server;
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::Store;
use ferrofire::model::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;

fn config(port: u16, rpoll: u32, log: &str) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>t</name><cycle-time>500</cycle-time>
<tags>
 <external-tag><name>bi</name><io>I0</io><type>binary</type></external-tag>
 <external-tag><name>bo</name><io>O0</io><type>binary</type></external-tag>
 <external-tag><name>ai</name><io>I1</io><type>analog</type></external-tag>
 <external-tag><name>ao</name><io>O1</io><type>analog</type></external-tag>
</tags>
<comms><iec60870-5-104-server><rpoll-rate>{rpoll}</rpoll-rate><subtype>single</subtype>
 <endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>{log}</event-logging>
 <binary-input><address>100</address><tag>bi</tag></binary-input>
 <binary-output><address>0</address><tag>bo</tag></binary-output>
 <analog-input><address>102</address><tag>ai</tag></analog-input>
 <analog-output><address>2</address><tag>ao</tag></analog-output>
</iec60870-5-104-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>pbi</name></binary><analog><id>I1</id><name>pai</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>pbo</name></binary><analog><id>O1</id><name>pao</name></analog></output>
</field-device></SCEPTRE>"#
    )
}

struct Device {
    store: Store,
    stop: CancellationToken,
    port: u16,
    log: String,
}

/// A server whose report comes every `rpoll` s (3600: once, at a connection's open, before it can be started).
fn start(rpoll: u32) -> Device {
    let port = free_port();
    let log = std::env::temp_dir()
        .join(format!("iec104-net-{port}.log"))
        .display()
        .to_string();
    let (server, store) =
        common::load_as::<Server>(&config(port, rpoll, &log), "iec60870-5-104-server");
    let stop = CancellationToken::new();
    tokio::spawn(server.run(store.clone(), stop.clone()));
    Device {
        store,
        stop,
        port,
        log,
    }
}

const STARTDT: u8 = 0x07;
const STOPDT: u8 = 0x13;
const TESTFR: u8 = 0x43;
const START_CON: [u8; 6] = [0x68, 4, 0x0B, 0, 0, 0];
const GI: [u8; 10] = [100, 1, 6, 0, 1, 0, 0, 0, 0, 20];

struct Master {
    s: TcpStream,
    ns: u16,
    nr: u16,
    rx: Vec<u8>,
    closed: bool,
}

impl Master {
    async fn connect(port: u16) -> Master {
        for _ in 0..50 {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
                return Master {
                    s,
                    ns: 0,
                    nr: 0,
                    rx: vec![],
                    closed: false,
                };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("server never listened on {port}");
    }

    async fn raw(&mut self, b: &[u8]) {
        let _ = self.s.write_all(b).await;
    }

    async fn u(&mut self, code: u8) {
        self.raw(&[0x68, 4, code, 0, 0, 0]).await;
    }

    async fn i(&mut self, asdu: &[u8]) {
        let (ns, nr) = (self.ns << 1, self.nr << 1);
        let mut f = vec![0x68, 4 + asdu.len() as u8];
        f.extend(ns.to_le_bytes());
        f.extend(nr.to_le_bytes());
        f.extend(asdu);
        self.ns += 1;
        self.raw(&f).await;
    }

    async fn ack(&mut self) {
        let nr = (self.nr << 1).to_le_bytes();
        self.raw(&[0x68, 4, 1, 0, nr[0], nr[1]]).await;
    }

    /// The APDUs received within `wait` (until the server closes, which sets `closed`).
    async fn read(&mut self, wait: Duration) -> Vec<Vec<u8>> {
        let (end, mut out, mut buf) = (Instant::now() + wait, vec![], [0u8; 4096]);
        while !self.closed {
            match timeout(end - Instant::now(), self.s.read(&mut buf)).await {
                Err(_) => break,
                Ok(Ok(0)) | Ok(Err(_)) => self.closed = true,
                Ok(Ok(n)) => self.rx.extend_from_slice(&buf[..n]),
            }
            while self.rx.len() >= 2 && self.rx.len() >= 2 + self.rx[1] as usize {
                let f: Vec<u8> = self.rx.drain(..2 + self.rx[1] as usize).collect();
                self.nr += u16::from(f[2] & 1 == 0);
                out.push(f);
            }
        }
        out
    }

    /// The ASDUs of the I-frames received within `wait`, acked at the end (as `probe_104x.py` does) so the k window never fills.
    async fn asdus(&mut self, wait: Duration) -> Vec<Vec<u8>> {
        let got: Vec<_> = self
            .read(wait)
            .await
            .into_iter()
            .filter(|f| f[2] & 1 == 0)
            .map(|f| f[6..].to_vec())
            .collect();
        if !got.is_empty() && !self.closed {
            self.ack().await;
        }
        got
    }

    /// STARTDT act, confirmed first; a report that follows within the wait is acked.
    async fn startdt(&mut self) {
        self.u(STARTDT).await;
        let got = self.read(Duration::from_millis(500)).await;
        assert_eq!(got.first(), Some(&START_CON.to_vec()));
        if got.iter().any(|f| f[2] & 1 == 0) {
            self.ack().await;
        }
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Type id and COT byte of each ASDU.
fn heads(asdus: &[Vec<u8>]) -> Vec<(u8, u8)> {
    asdus.iter().map(|a| (a[0], a[2])).collect()
}

#[tokio::test]
async fn startdt_gi_command_over_tcp() {
    let d = start(3600);
    let mut m = Master::connect(d.port).await;
    m.startdt().await;
    m.i(&GI).await;
    // G:13: ACT_CON, the binaries (M_SP_NA_1), the analogs (M_ME_NC_1), ACT_TERM
    assert_eq!(
        heads(&m.asdus(ms(500)).await),
        [(100, 7), (1, 20), (13, 20), (100, 10)]
    );
    m.i(&[45, 1, 6, 0, 1, 0, 0, 0, 0, 1]).await;
    assert_eq!(
        m.asdus(ms(500)).await,
        [
            vec![45, 1, 7, 0, 1, 0, 0, 0, 0, 1],
            vec![45, 1, 10, 0, 1, 0, 0, 0, 0, 1]
        ]
    );
    assert_eq!(d.store.queued("bo"), Some(Value::Binary(true)));
    let f = 77.5f32.to_le_bytes();
    m.i(&[50, 1, 6, 0, 1, 0, 2, 0, 0, f[0], f[1], f[2], f[3], 0])
        .await;
    assert_eq!(heads(&m.asdus(ms(500)).await), [(50, 7), (50, 10)]);
    assert_eq!(d.store.queued("ao"), Some(Value::Analog(77.5)));
    // a command at an input: negative ACT_CON, no write
    m.i(&[45, 1, 6, 0, 1, 0, 100, 0, 0, 1]).await;
    assert_eq!(heads(&m.asdus(ms(500)).await), [(45, 0x47)]);
    assert!(!d.store.is_queued("bi"));
    let log = std::fs::read_to_string(&d.log).unwrap();
    let init = format!(
        "info,iec60870-5-104-server,iec60870-5-104 server start,Initialized IEC60870-5-104 server: tcp://127.0.0.1:{} (select-before-operate disabled)",
        d.port
    );
    assert!(log.contains(&init), "{log}");
    assert!(
        log.contains(
            "iec60870-5-104 Server writeBinary,Binary point command at address 0 with value 1."
        ),
        "{log}"
    );
    d.stop.cancel();
}

/// Whether a report (an I-frame with COT 1) reached each master in a 2 s window, after what was in flight is drained.
async fn reported(ms_: &mut [&mut Master]) -> Vec<bool> {
    for m in ms_.iter_mut() {
        m.asdus(ms(300)).await;
    }
    tokio::time::sleep(ms(1700)).await;
    let mut out = vec![];
    for m in ms_.iter_mut() {
        out.push(m.asdus(ms(300)).await.iter().any(|a| a[2] & 0x3F == 1));
    }
    out
}

/// Single redundancy group (STOCK.md "two connections"): a STARTDT deactivates every other connection; the report goes to the
/// connection opened last while it is started; any close stops it until a connection opens.
#[tokio::test]
async fn two_masters_periodic_goes_to_the_last_opened() {
    let d = start(1);
    let mut a = Master::connect(d.port).await;
    a.startdt().await;
    assert_eq!(reported(&mut [&mut a]).await, [true]);
    let mut b = Master::connect(d.port).await;
    assert_eq!(
        reported(&mut [&mut a, &mut b]).await,
        [false, false],
        "B opened, not started"
    );
    b.startdt().await;
    assert_eq!(reported(&mut [&mut a, &mut b]).await, [false, true]);
    a.i(&GI).await;
    a.read(ms(500)).await;
    assert!(a.closed, "a deactivated connection's I-frame closes it");
    // a close of a connection that is not the last opened stops the report too
    let mut c = Master::connect(d.port).await;
    c.startdt().await;
    assert_eq!(reported(&mut [&mut c]).await, [true]);
    drop(b);
    assert_eq!(reported(&mut [&mut c]).await, [false], "B's close stops it");
    // the last opened closes: nobody gets it, even a connection started again
    let mut e = Master::connect(d.port).await;
    e.startdt().await;
    assert_eq!(reported(&mut [&mut c, &mut e]).await, [false, true]);
    drop(e);
    c.startdt().await;
    assert_eq!(reported(&mut [&mut c]).await, [false]);
    // a new connection, once started, gets it
    let mut f = Master::connect(d.port).await;
    f.startdt().await;
    assert_eq!(reported(&mut [&mut c, &mut f]).await, [false, true]);
    m_gi_on_the_connection_that_asked(&mut c).await;
    d.stop.cancel();
}

/// The GI is answered on the connection that asked, report or not (G:157); C was deactivated by F's STARTDT, so start it first.
async fn m_gi_on_the_connection_that_asked(c: &mut Master) {
    c.startdt().await;
    c.i(&GI).await;
    assert_eq!(heads(&c.asdus(ms(500)).await).len(), 4);
}

#[tokio::test]
async fn a_silent_or_pre_startdt_master_is_handled_as_stock() {
    let d = start(3600);
    // G:2-7: TESTFR is answered before STARTDT; an I- or S-frame closes
    let mut m = Master::connect(d.port).await;
    m.u(TESTFR).await;
    assert_eq!(m.read(ms(300)).await, [[0x68, 4, 0x83, 0, 0, 0]]);
    m.i(&GI).await;
    assert!(m.read(ms(500)).await.is_empty() && m.closed);
    let mut m = Master::connect(d.port).await;
    m.ack().await;
    assert!(m.read(ms(500)).await.is_empty() && m.closed);
    // source-only closes: an ASDU under 6 bytes, a GI at COT 6 or 8 with no object; at another COT it is answered
    for (asdu, closes) in [
        (&[45, 1, 6][..], true),
        (&[100, 1, 6, 0, 1, 0][..], true),
        (&[100, 1, 8, 0, 1, 0, 0][..], true),
        (&[100, 1, 3, 0, 1, 0][..], false),
    ] {
        let mut m = Master::connect(d.port).await;
        m.startdt().await;
        m.i(asdu).await;
        let got = m.asdus(ms(500)).await;
        assert_eq!(m.closed, closes, "{asdu:?}");
        if !closes {
            assert_eq!(heads(&got), [(100, 0x6D), (100, 0x6C)]);
        }
    }
    // STOPDT: an S-frame, then STOPDT con; an I-frame while stopped closes
    let mut m = Master::connect(d.port).await;
    m.startdt().await;
    m.u(STOPDT).await;
    let got = m.read(ms(300)).await;
    assert_eq!(got.len(), 2);
    assert_eq!(
        (got[0][2], got[1].as_slice()),
        (1, &[0x68, 4, 0x23, 0, 0, 0][..])
    );
    m.i(&GI).await;
    m.read(ms(500)).await;
    assert!(m.closed);
    // a master that never acks: k = 12 I-frames, the rest wait and go, in order, once acked
    let mut m = Master::connect(d.port).await;
    m.startdt().await;
    for _ in 0..4 {
        m.i(&GI).await;
    }
    let first = m.read(ms(800)).await;
    assert_eq!(first.len(), 12);
    m.ack().await;
    let rest: Vec<_> = m
        .read(ms(500))
        .await
        .into_iter()
        .map(|f| f[6..].to_vec())
        .collect();
    assert_eq!(heads(&rest), [(100, 7), (1, 20), (13, 20), (100, 10)]);
    d.stop.cancel();
}

/// lib60870 serves at most 5 masters (`CONFIG_CS104_MAX_CLIENT_CONNECTIONS`): the 6th is accepted and closed at once.
#[tokio::test]
async fn a_sixth_master_is_closed() {
    let d = start(3600);
    let mut five = vec![];
    for _ in 0..5 {
        let mut m = Master::connect(d.port).await;
        m.u(TESTFR).await;
        assert_eq!(m.read(ms(300)).await.len(), 1);
        five.push(m);
    }
    let mut sixth = Master::connect(d.port).await;
    sixth.read(ms(500)).await;
    assert!(sixth.closed);
    five.pop();
    tokio::time::sleep(ms(200)).await;
    let mut again = Master::connect(d.port).await;
    again.u(TESTFR).await;
    assert_eq!(again.read(ms(300)).await.len(), 1);
    assert!(!again.closed);
    d.stop.cancel();
}

#[tokio::test]
async fn stop_closes_listener_and_connections() {
    let d = start(3600);
    let mut m = Master::connect(d.port).await;
    m.startdt().await;
    d.stop.cancel();
    m.read(ms(1000)).await;
    assert!(m.closed);
    tokio::time::sleep(ms(200)).await;
    assert!(TcpStream::connect(("127.0.0.1", d.port)).await.is_err());
}
