//! Shared by the Modbus integration tests: a server on a free port with a four-table config, and a raw MBAP client.
#![allow(dead_code)]

use super::load_as;
use ferrofire::fd::proto::modbus::Server;
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
 <external-tag><name>di</name><io>I0</io><type>binary</type></external-tag>
 <external-tag><name>co</name><io>O0</io><type>binary</type></external-tag>
 <external-tag><name>ir</name><io>I1</io><type>analog</type></external-tag>
 <external-tag><name>hr</name><io>O1</io><type>analog</type></external-tag>
 <external-tag><name>co2</name><io>O2</io><type>binary</type></external-tag>
 <external-tag><name>hr2</name><io>O3</io><type>analog</type></external-tag>
</tags>
<comms><modbus-server><endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>{log}</event-logging>
 <discrete-input><address>10000</address><tag>di</tag></discrete-input>
 <coil><address>0</address><tag>co</tag></coil>
 <coil><address>1</address><tag>co2</tag></coil>
 <input-register><address>30000</address><max-value>1000</max-value><min-value>0</min-value><tag>ir</tag></input-register>
 <holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>hr</tag></holding-register>
 <holding-register><address>40001</address><max-value>1000</max-value><min-value>0</min-value><tag>hr2</tag></holding-register>
</modbus-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>pdi</name></binary><analog><id>I1</id><name>pir</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>pco</name></binary><analog><id>O1</id><name>phr</name></analog><binary><id>O2</id><name>pco2</name></binary><analog><id>O3</id><name>phr2</name></analog></output>
</field-device></SCEPTRE>"#
    )
}

pub struct Device {
    pub store: Store,
    pub stop: CancellationToken,
    pub port: u16,
    pub log: String,
    pub task: tokio::task::JoinHandle<()>,
}

pub fn start() -> Device {
    let (port, log) = (
        free_port(),
        std::env::temp_dir()
            .join(format!("modbus-net-{}.log", free_port()))
            .display()
            .to_string(),
    );
    let (out, store) = load_as::<Server>(&config(port, &log), "modbus-server");
    let stop = CancellationToken::new();
    let task = tokio::spawn(out.run(store.clone(), stop.clone()));
    Device {
        store,
        stop,
        port,
        log,
        task,
    }
}

pub async fn connect(port: u16) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the server never listened on {port}");
}

/// An MBAP frame: transaction id, protocol id, length, unit id, PDU.
pub fn frame(tid: u16, pid: u16, length: u16, pdu: &[u8]) -> Vec<u8> {
    let mut f = [tid.to_be_bytes(), pid.to_be_bytes(), length.to_be_bytes()].concat();
    f.push(1);
    f.extend_from_slice(pdu);
    f
}

pub fn good(tid: u16, pdu: &[u8]) -> Vec<u8> {
    frame(tid, 0, pdu.len() as u16 + 1, pdu)
}

pub const READ_IR: [u8; 5] = [4, 0x75, 0x30, 0, 1]; // FC4 30000 x1
pub const READ_DI: [u8; 5] = [2, 0x27, 0x10, 0, 1]; // FC2 10000 x1

/// Everything the server sends until it closes or `wait` passes: the bytes, and whether it closed.
pub async fn drain(s: &mut TcpStream, wait: Duration) -> (Vec<u8>, bool) {
    let (mut got, mut buf) = (vec![], [0u8; 4096]);
    loop {
        match timeout(wait, s.read(&mut buf)).await {
            Err(_) => return (got, false),
            Ok(Ok(0)) | Ok(Err(_)) => return (got, true),
            Ok(Ok(n)) => got.extend_from_slice(&buf[..n]),
        }
    }
}

/// One request on a fresh connection; the whole reply frame (the MBAP length says how long it is).
pub async fn ask(port: u16, request: &[u8]) -> Vec<u8> {
    let mut s = connect(port).await;
    s.write_all(request).await.unwrap();
    let mut r = vec![0u8; 6];
    let wait = Duration::from_secs(2);
    timeout(wait, s.read_exact(&mut r))
        .await
        .expect("an answer")
        .unwrap();
    let rest = usize::from(u16::from_be_bytes([r[4], r[5]]));
    r.resize(6 + rest, 0);
    timeout(wait, s.read_exact(&mut r[6..]))
        .await
        .expect("the rest of the answer")
        .unwrap();
    r
}

pub fn open_fds() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}
