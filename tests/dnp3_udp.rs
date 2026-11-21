//! DNP3 over UDP (ww-8l7): `<dnp3-server>` and `<dnp3-connection>` on `udp://ip:port`. The frames are the TCP frames (the state
//! machines are the same ones `dnp3_golden.rs` replays against stock); what is new is the socket: one datagram per request and
//! per response, the reply to whoever sent, no master slot, and a master that takes a refused datagram for a lost connection.

mod common;

use common::clients::*;
use common::unhex;
use ferrofire::fd::config::{Config, Opts};
use ferrofire::fd::proto::dnp3::link::{self, Parse};
use ferrofire::fd::proto::REGISTRY;
use std::time::Duration;
use tokio::net::UdpSocket;

const SERVER: &str = include_str!("../conformance/stock/server-dnp3.xml");
const CLIENT: &str = include_str!("../conformance/stock/client-dnp3.xml");
const MODBUS_CLIENT: &str = include_str!("../conformance/stock/client-modbus.xml");

/// The stock master's first frame (conformance/stock/client-dnp3-raw.golden): a class 0 read from master 1 to outstation 10.
const CLASS0: &str = "05640bc40a000100acd1c0c0013c0106ff50";

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn server_xml(port: u16) -> String {
    SERVER
        .replace("tcp://127.0.0.1:20000", &format!("udp://127.0.0.1:{port}"))
        .replace("rtu-1-dnp3-outstation.log", "/dev/null")
}

fn client_xml(port: u16, cmdif: u16) -> String {
    CLIENT
        .replace("tcp://127.0.0.1:20000", &format!("udp://127.0.0.1:{port}"))
        .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}"))
        .replace("<scan-rate>5</scan-rate>", "<scan-rate>1</scan-rate>")
}

/// Send `req` from `sock` and wait for one datagram back.
async fn ask(sock: &UdpSocket, to: u16, req: &[u8]) -> Vec<u8> {
    sock.send_to(req, ("127.0.0.1", to)).await.unwrap();
    let mut buf = vec![0u8; 65536];
    let n = tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf))
        .await
        .expect("no reply datagram")
        .unwrap();
    buf.truncate(n);
    buf
}

/// Ask until the outstation answers (its socket is bound by a spawned task).
async fn first_reply(sock: &UdpSocket, to: u16, req: &[u8]) -> Vec<u8> {
    for _ in 0..50 {
        sock.send_to(req, ("127.0.0.1", to)).await.unwrap();
        let mut buf = vec![0u8; 65536];
        if let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(100), sock.recv(&mut buf)).await
        {
            buf.truncate(n);
            return buf;
        }
    }
    panic!("the outstation on udp port {to} never answered");
}

/// Every link frame in one datagram, and nothing left over.
fn frames(datagram: &[u8]) -> Vec<link::Frame> {
    let (mut buf, mut out) = (datagram.to_vec(), vec![]);
    while let Parse::Frame(f, used) = link::parse(&buf) {
        out.push(f);
        buf.drain(..used);
    }
    assert!(buf.is_empty(), "a datagram ends on a frame boundary");
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn the_outstation_answers_every_sender_with_one_datagram() {
    let port = free_udp_port();
    let _server = modules(&server_xml(port));
    let (a, b) = (
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
    );
    let reply = first_reply(&a, port, &unhex(CLASS0)).await;
    let f = frames(&reply);
    assert!(!f.is_empty());
    // from outstation 10 to master 1, a response (function code 0x81 after the transport and application control octets)
    assert_eq!((f[0].dst, f[0].src), (1, 10));
    assert_eq!(f[0].user[2], 0x81, "{:02x?}", f[0].user);
    // a second master is answered too, on its own address: UDP has no master slot to take
    let second = ask(&b, port, &unhex(CLASS0)).await;
    assert_eq!(frames(&second)[0].user[2], 0x81);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_broken_datagram_starts_a_fresh_session_and_the_next_is_answered() {
    let port = free_udp_port();
    let _server = modules(&server_xml(port));
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    first_reply(&sock, port, &unhex(CLASS0)).await;
    // the class 0 read with its data CRC broken: dropped, no reply
    let mut bad = unhex(CLASS0);
    *bad.last_mut().unwrap() ^= 0xff;
    sock.send_to(&bad, ("127.0.0.1", port)).await.unwrap();
    let mut buf = [0u8; 2048];
    let silent = tokio::time::timeout(Duration::from_millis(300), sock.recv(&mut buf)).await;
    assert!(silent.is_err(), "a datagram with a bad CRC is not answered");
    assert_eq!(
        frames(&ask(&sock, port, &unhex(CLASS0)).await)[0].user[2],
        0x81
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn master_and_outstation_read_and_command_over_udp() {
    let (port, cmdif) = (free_udp_port(), free_port());
    let server = modules(&server_xml(port));
    let _client = modules(&client_xml(port, cmdif));
    for (point, x) in [
        ("load-1_bus-1.mw", 250.5),
        ("generator-1_bus-1.active", 1.0),
    ] {
        assert!(server.store.publish(point, x, 1), "{point}");
    }
    let want = "ACK=load-power:250.500000\\x00";
    for _ in 0..100 {
        if shown(&command(cmdif, "READ=load-power").await) == want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(shown(&command(cmdif, "READ=load-power").await), want);
    // a select-before-operate CROB and a float32 setpoint, both down to the outstation's store
    for (cmd, reply) in [
        (
            "WRITE=gen-toggle:true",
            "ACK=Wrote tag gen-toggle -- true\\x00",
        ),
        (
            "WRITE=gen-mw-setpoint:7",
            "ACK=Wrote tag gen-mw-setpoint -- 7\\x00",
        ),
    ] {
        assert_eq!(shown(&command(cmdif, cmd).await), reply, "{cmd}");
    }
    until(
        "the commands at the outstation",
        Duration::from_secs(5),
        || server.store.is_queued("gen-toggle") && server.store.is_queued("gen-mw-setpoint"),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_master_finds_an_outstation_that_comes_up_later() {
    let (port, cmdif) = (free_udp_port(), free_port());
    let _client = modules(&client_xml(port, cmdif));
    // nothing listens yet: the master's datagrams are refused and it backs off
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let server = modules(&server_xml(port));
    assert!(server.store.publish("load-1_bus-1.mw", 42.0, 1));
    let want = "ACK=load-power:42.000000\\x00";
    for _ in 0..200 {
        if shown(&command(cmdif, "READ=load-power").await) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the master never reached the outstation that came up after it");
}

#[test]
fn modbus_refuses_a_udp_endpoint() {
    let xml = MODBUS_CLIENT.replace("tcp://127.0.0.1:5502", "udp://127.0.0.1:5502");
    let err = Config::parse(&xml, REGISTRY, &Opts::default())
        .err()
        .expect("a udp:// modbus connection is refused");
    assert!(err.to_string().contains("udp:// is DNP3 only"), "{err}");
}
