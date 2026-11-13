//! ww-1df.32: the Modbus read-back of a written point must not flap. The provider ACKs a WRITE when it queues it and applies it at its
//! next tick, so a datagram published in between still carries the old value. The device keeps the ACKed value until the stream
//! confirms it (or the hold runs out).

mod common;

use common::modbus::{ask, free_port, good};
use ferrofire::bennu::{frame, group_for};
use ferrofire::fd::config::{Config, Opts};
use ferrofire::fd::proto;
use std::net::UdpSocket;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const WRITE_COIL_ON: [u8; 5] = [5, 0, 0, 0xFF, 0];
const READ_COIL: [u8; 5] = [1, 0, 0, 0, 1];

fn device(modbus: u16, udp: u16, prov: u16) -> String {
    let log = std::env::temp_dir()
        .join(format!("fd-flap-{modbus}.log"))
        .display()
        .to_string();
    format!(
        "<SCEPTRE><field-device><name>d</name><cycle-time>20</cycle-time>\
        <tags><external-tag><name>co</name><io>O0</io><type>binary</type></external-tag></tags>\
        <comms><modbus-server><endpoint>tcp://127.0.0.1:{modbus}</endpoint><event-logging>{log}</event-logging>\
        <coil><address>0</address><tag>co</tag></coil></modbus-server></comms>\
        <input><endpoint>udp://127.0.0.1:{udp}</endpoint><binary><id>I0</id><name>p.co</name></binary></input>\
        <output><endpoint>tcp://127.0.0.1:{prov}</endpoint><binary><id>O0</id><name>p.co</name></binary></output>\
        </field-device></SCEPTRE>"
    )
}

/// A provider that ACKs every WRITE at once and never changes what it publishes (it applies the write one engine tick later).
fn provider(port: u16, stop: CancellationToken) -> std::thread::JoinHandle<()> {
    let rep = zmq::Context::new().socket(zmq::REP).unwrap();
    rep.set_rcvtimeo(100).unwrap();
    rep.bind(&format!("tcp://127.0.0.1:{port}")).unwrap();
    std::thread::spawn(move || {
        while !stop.is_cancelled() {
            if rep.recv_bytes(0).is_ok() {
                rep.send("ACK=\0", 0).unwrap();
            }
        }
    })
}

async fn coil(port: u16) -> bool {
    ask(port, &good(1, &READ_COIL)).await[9] == 1
}

#[tokio::test]
async fn a_stale_datagram_after_the_ack_does_not_flap_the_readback() {
    let (modbus, udp, prov) = (free_port(), free_port(), free_port());
    let stop = CancellationToken::new();
    let prov_thread = provider(prov, stop.clone());
    let cfg = Config::parse(
        &device(modbus, udp, prov),
        proto::REGISTRY,
        &Opts::default(),
    )
    .unwrap();
    let (stop2, dev) = (
        stop.clone(),
        std::thread::spawn({
            let stop = stop.clone();
            move || ferrofire::fd::run(cfg, stop, false)
        }),
    );
    // The old value (off), published all the time like a stream that has not caught up.
    let (done, ep) = (stop2.clone(), format!("udp://127.0.0.1:{udp}"));
    let sender = std::thread::spawn(move || {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        while !done.is_cancelled() {
            let _ = s.send_to(
                &frame(&group_for(&ep), b"p.co:false,\0"),
                ("127.0.0.1", udp),
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    });
    ask(modbus, &good(2, &WRITE_COIL_ON)).await;
    let t = Instant::now();
    while !coil(modbus).await && t.elapsed() < Duration::from_secs(5) {}
    assert!(coil(modbus).await, "the ACK never set the coil");
    // Well inside the hold, the read-back must stay on.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(800) {
        assert!(
            coil(modbus).await,
            "read-back flapped back to the old value"
        );
    }
    stop.cancel();
    dev.join().unwrap().unwrap();
    prov_thread.join().unwrap();
    sender.join().unwrap();
}
