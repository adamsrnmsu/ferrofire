//! The DNP3 outstation over real sockets: a master that frames requests by hand, one connection at a time, descriptor and
//! error handling (the bennu #79 / #81 shapes: closed sockets must really close, a failed accept must be retried).

mod common;

use ferrofire::fd::proto::dnp3::link::{self, Parse};
use ferrofire::fd::proto::dnp3::transport::{self, Reassembler};
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::Store;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn config(port: u16, log: &str) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>t</name><cycle-time>500</cycle-time>
<tags>
 <external-tag><name>bi</name><io>I0</io><type>binary</type></external-tag>
 <external-tag><name>bo</name><io>O0</io><type>binary</type></external-tag>
 <external-tag><name>ai</name><io>I1</io><type>analog</type></external-tag>
 <external-tag><name>ao</name><io>O1</io><type>analog</type></external-tag>
</tags>
<comms><dnp3-server><endpoint>tcp://127.0.0.1:{port}</endpoint><address>10</address><event-logging>{log}</event-logging>
 <binary-input><address>2</address><tag>bi</tag></binary-input>
 <binary-output><address>5</address><tag>bo</tag></binary-output>
 <analog-input><address>0</address><tag>ai</tag></analog-input>
 <analog-output><address>1</address><tag>ao</tag></analog-output>
</dnp3-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>pbi</name></binary><analog><id>I1</id><name>pai</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>pbo</name></binary><analog><id>O1</id><name>pao</name></analog></output>
</field-device></SCEPTRE>"#
    )
}

pub struct Device {
    pub store: Store,
    pub stop: CancellationToken,
    pub port: u16,
    pub log: String,
}

pub fn start() -> Device {
    let (port, log) = (
        free_port(),
        std::env::temp_dir()
            .join(format!("dnp3-net-{}.log", free_port()))
            .display()
            .to_string(),
    );
    let (out, store) = common::load(&config(port, &log));
    let stop = CancellationToken::new();
    tokio::spawn(out.run(store.clone(), stop.clone()));
    Device {
        store,
        stop,
        port,
        log,
    }
}

pub struct Master {
    pub s: TcpStream,
    seq: u8,
    app: u8,
    rx: Vec<u8>,
    re: Reassembler,
}

impl Master {
    pub async fn connect(port: u16) -> Master {
        for _ in 0..50 {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
                return Master {
                    s,
                    seq: 0,
                    app: 0,
                    rx: vec![],
                    re: Reassembler::new(4096),
                };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("outstation never listened on {port}");
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) {
        self.s.write_all(bytes).await.unwrap();
    }

    /// Send one request fragment (control octet and sequence are filled in) and return the first response fragment, or
    /// `None` when the outstation answered nothing or closed the connection.
    pub async fn request(&mut self, func: u8, objects: &[u8]) -> Option<Vec<u8>> {
        let mut frag = vec![0xC0 | self.app, func];
        frag.extend_from_slice(objects);
        self.app = (self.app + 1) & 15;
        for seg in transport::segment(&frag, &mut self.seq) {
            self.send_raw(&link::encode(0xC4, 10, 1, &seg)).await;
        }
        self.response(Duration::from_millis(1500)).await
    }

    pub async fn response(&mut self, wait: Duration) -> Option<Vec<u8>> {
        let mut buf = [0u8; 4096];
        timeout(wait, async {
            loop {
                while let Parse::Frame(f, used) = link::parse(&self.rx) {
                    self.rx.drain(..used);
                    if let Some(app) = self.re.push(&f.user).unwrap() {
                        return Some(app);
                    }
                }
                match self.s.read(&mut buf).await {
                    Ok(0) | Err(_) => return None,
                    Ok(n) => self.rx.extend_from_slice(&buf[..n]),
                }
            }
        })
        .await
        .unwrap_or(None)
    }

    /// True when the peer has closed (the next read returns end-of-file).
    pub async fn closed(&mut self) -> bool {
        let mut b = [0u8; 64];
        matches!(
            timeout(Duration::from_millis(1500), self.s.read(&mut b)).await,
            Ok(Ok(0)) | Ok(Err(_))
        )
    }
}

const INTEGRITY: [u8; 12] = [60, 2, 6, 60, 3, 6, 60, 4, 6, 60, 1, 6];
pub const CROB_LATCH_ON_5: [u8; 16] = [12, 1, 0x17, 1, 5, 3, 1, 100, 0, 0, 0, 0, 0, 0, 0, 0];

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

#[tokio::test]
async fn a_master_polls_and_operates_over_tcp() {
    let d = start();
    let mut m = Master::connect(d.port).await;
    let r = m.request(1, &INTEGRITY).await.unwrap();
    assert_eq!(r[1], 129, "a response");
    assert!(
        r.windows(3).any(|w| w == [1, 2, 0x00]),
        "static binary input present: {r:02x?}"
    );
    // CROB LATCH_ON on binary-output 5: status 0, and the tag is queued for the provider
    let r = m.request(5, &CROB_LATCH_ON_5).await.unwrap();
    assert_eq!(&r[4..], &{
        let mut e = CROB_LATCH_ON_5;
        e[15] = 0;
        e
    });
    assert!(
        d.store.is_queued("bo"),
        "the write reached the store's queue"
    );
    // a float command on analog-output 1
    let mut ao = vec![41, 3, 0x17, 1, 1];
    ao.extend_from_slice(&55.5f32.to_le_bytes());
    ao.push(0);
    let r = m.request(5, &ao).await.unwrap();
    assert_eq!(*r.last().unwrap(), 0);
    assert!(d.store.is_queued("ao"));
    d.stop.cancel();
    let log = std::fs::read_to_string(&d.log).unwrap();
    assert!(
        log.contains("Binary point command at address 5 with value 1."),
        "{log}"
    );
    assert!(
        log.contains("Analog point command at address 1 with value 55.5."),
        "{log}"
    );
}

#[tokio::test]
async fn provider_values_reach_masters_as_events_within_a_sweep() {
    let d = start();
    let mut m = Master::connect(d.port).await;
    tokio::time::sleep(Duration::from_millis(300)).await; // first sweep: the start-up events
    let r = m.request(1, &INTEGRITY).await.unwrap();
    let con = r[0] & 0x20 != 0;
    assert!(con, "a response with events asks for a confirm");
    d.store.publish("pbi", 1.0, 1);
    d.store.publish("pai", 12.5, 1);
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let r = m.request(1, &[60, 1, 6]).await.unwrap();
    let hex: String = r.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        hex.contains("0000000000002940"),
        "12.5 as a double in the static data: {hex}"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_second_master_is_closed_and_the_first_is_served_on() {
    let d = start();
    let mut first = Master::connect(d.port).await;
    assert!(first.request(1, &[60, 1, 6]).await.is_some());
    let mut second = Master::connect(d.port).await;
    assert!(
        second.closed().await,
        "CloseNew: the new connection is the one closed"
    );
    assert!(first.request(1, &[60, 1, 6]).await.is_some());
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut third = Master::connect(d.port).await;
    assert!(
        third.request(1, &[60, 1, 6]).await.is_some(),
        "the slot is free again"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_corrupt_frame_closes_the_connection() {
    let d = start();
    let mut m = Master::connect(d.port).await;
    let mut f = link::encode(0xC4, 10, 1, &[0xC0, 0xC0, 1, 60, 1, 6]);
    f[11] ^= 0xFF;
    m.send_raw(&f).await;
    assert!(m.closed().await);
    let mut again = Master::connect(d.port).await;
    assert!(
        again.request(1, &[60, 1, 6]).await.is_some(),
        "and the outstation took the next master"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn connection_churn_leaves_no_descriptors_behind() {
    let d = start();
    let mut warm = Master::connect(d.port).await;
    assert!(warm.request(1, &[60, 1, 6]).await.is_some());
    drop(warm);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = open_fds();
    for i in 0..300 {
        // the previous master's slot frees when its task sees the close; until then a new one is closed (CloseNew)
        let mut m = Master::connect(d.port).await;
        for _ in 0..50 {
            if i % 3 != 0 || m.request(1, &[60, 1, 6]).await.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            m = Master::connect(d.port).await;
        }
        if i % 3 == 1 {
            m.send_raw(&[0x05, 0x64, 0xFF]).await; // half a frame, then gone
        }
        drop(m);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = open_fds();
    assert!(
        after <= before + 2,
        "descriptors: {before} before the churn, {after} after"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_bind_failure_is_logged_and_does_not_panic() {
    let hold = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = hold.local_addr().unwrap().port();
    let log = std::env::temp_dir()
        .join(format!("dnp3-bind-{port}.log"))
        .display()
        .to_string();
    let (out, store) = common::load(&config(port, &log));
    let stop = CancellationToken::new();
    timeout(Duration::from_secs(3), out.run(store, stop))
        .await
        .expect("run returns when it cannot bind");
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .contains("failed to add TCPServer"));
}

#[tokio::test]
async fn a_serial_endpoint_is_reported_not_served() {
    let log = std::env::temp_dir()
        .join("dnp3-serial.log")
        .display()
        .to_string();
    let xml = config(1, &log).replace("tcp://127.0.0.1:1", "/dev/ttyS99");
    let (out, store) = common::load(&xml);
    timeout(
        Duration::from_secs(3),
        out.run(store, CancellationToken::new()),
    )
    .await
    .expect("returns");
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .contains("serial endpoint"));
}

#[tokio::test]
async fn stop_ends_the_connection() {
    let d = start();
    let mut m = Master::connect(d.port).await;
    assert!(m.request(1, &[60, 1, 6]).await.is_some());
    d.stop.cancel();
    assert!(m.closed().await);
}
