//! `<modbus-client>` and `<dnp3-client>` on a serial endpoint (ww-y5q, plan F6): the client opens a pty slave, a bridge carries the
//! bytes to our own TCP server, so the framing on the port is what the stock transcripts show on the socket: MBAP without a slave
//! byte or CRC for Modbus, the same link frames for DNP3. `tests/serial_servers.rs` is the other way round.
#![cfg(feature = "proto-serial")]

mod common;

use common::clients::*;
use common::unhex;
use std::time::Duration;

const MODBUS_SERVER: &str = include_str!("../conformance/stock/server-modbus.xml");
const MODBUS_CLIENT: &str = include_str!("../conformance/stock/client-modbus.xml");
const DNP3_SERVER: &str = include_str!("../conformance/stock/server-dnp3.xml");
const DNP3_CLIENT: &str = include_str!("../conformance/stock/client-dnp3.xml");

#[tokio::test(flavor = "multi_thread")]
async fn the_modbus_client_talks_mbap_over_a_serial_port() {
    let (port, cmdif) = (free_port(), free_port());
    let server = modules(
        &MODBUS_SERVER
            .replace("tcp://127.0.0.1:5502", &format!("tcp://127.0.0.1:{port}"))
            .replace("rtu-1-modbus-server.log", "/dev/null"),
    );
    listening(port, true).await;
    let t = tap(port).await;
    let _client = modules(
        &MODBUS_CLIENT
            .replace("tcp://127.0.0.1:5502", &pty_to(t.port))
            .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}")),
    );
    for (cmd, reply) in [
        ("READ=load-power", "ACK=load-power:-1000.000000\\x00"),
        ("READ=gen-status", "ACK=gen-status:false\\x00"),
    ] {
        assert_eq!(shown(&command(cmdif, cmd).await), reply, "{cmd}");
    }
    for (point, x) in [
        ("load-1_bus-1.mw", 250.5),
        ("generator-1_bus-1.active", 1.0),
        ("load-1_bus-1.active", 0.0),
        ("bus-1.gen_mw", 12.5),
    ] {
        assert!(server.store.publish(point, x, 1), "{point}");
    }
    let want = "ACK=load-power:-248.523697\\x00";
    for _ in 0..50 {
        if shown(&command(cmdif, "READ=load-power").await) == want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(shown(&command(cmdif, "READ=load-power").await), want);
    // a request: transaction id, then protocol 0, length 6, unit 1, FC 4, address 0x7530, quantity 1; no slave byte, no CRC
    let sent = t.sent().concat();
    assert_eq!(sent.len() % 12, 0, "only 12-octet MBAP requests");
    assert_eq!(sent[2..12], unhex("00000006010475300001")[..]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dnp3_master_talks_the_same_frames_over_a_serial_port() {
    let (port, cmdif) = (free_port(), free_port());
    let server = modules(
        &DNP3_SERVER
            .replace("tcp://127.0.0.1:20000", &format!("tcp://127.0.0.1:{port}"))
            .replace("rtu-1-dnp3-outstation.log", "/dev/null"),
    );
    listening(port, true).await;
    let t = tap(port).await;
    let _client = modules(
        &DNP3_CLIENT
            .replace("tcp://127.0.0.1:20000", &pty_to(t.port))
            .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}"))
            .replace("<scan-rate>5</scan-rate>", "<scan-rate>1</scan-rate>"),
    );
    // the stock master's first frame (conformance/stock/client-dnp3-raw.golden): a class 0 read
    let first = unhex("05640bc40a000100acd1c0c0013c0106ff50");
    until("the first frame", Duration::from_secs(5), || {
        t.sent().concat().starts_with(&first)
    })
    .await;
    for (point, x) in [
        ("load-1_bus-1.mw", 250.5),
        ("generator-1_bus-1.active", 1.0),
    ] {
        assert!(server.store.publish(point, x, 1), "{point}");
    }
    for _ in 0..100 {
        if shown(&command(cmdif, "READ=load-power").await) == "ACK=load-power:250.500000\\x00" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the outstation's value never reached the master over the pty");
}
