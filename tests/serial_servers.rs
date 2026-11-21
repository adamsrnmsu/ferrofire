//! `<modbus-server>` and `<dnp3-server>` on a serial endpoint (ww-z5w, plan F6 server half): the server opens a pty slave, and our
//! own TCP client reaches it through a bridge on the pty's master side, so the bytes on the port are the ones on the socket.
#![cfg(feature = "proto-serial")]

mod common;

use common::clients::*;
use std::time::Duration;

const MODBUS_SERVER: &str = include_str!("../conformance/stock/server-modbus.xml");
const MODBUS_CLIENT: &str = include_str!("../conformance/stock/client-modbus.xml");
const DNP3_SERVER: &str = include_str!("../conformance/stock/server-dnp3.xml");
const DNP3_CLIENT: &str = include_str!("../conformance/stock/client-dnp3.xml");

/// Poll `READ=<point>` on the client's command interface until it answers `want`.
async fn reads(cmdif: u16, cmd: &str, want: &str) {
    for _ in 0..100 {
        if shown(&command(cmdif, cmd).await) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(shown(&command(cmdif, cmd).await), want, "{cmd}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_modbus_server_answers_mbap_on_a_serial_port() {
    let ((path, port), cmdif) = (pty_from().await, free_port());
    let server = modules(
        &MODBUS_SERVER
            .replace("tcp://127.0.0.1:5502", &path)
            .replace("rtu-1-modbus-server.log", "/dev/null"),
    );
    let _client = modules(
        &MODBUS_CLIENT
            .replace("tcp://127.0.0.1:5502", &format!("tcp://127.0.0.1:{port}"))
            .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}")),
    );
    reads(cmdif, "READ=gen-status", "ACK=gen-status:false\\x00").await;
    for (point, x) in [
        ("load-1_bus-1.mw", 250.5),
        ("generator-1_bus-1.active", 1.0),
        ("load-1_bus-1.active", 0.0),
        ("bus-1.gen_mw", 12.5),
    ] {
        assert!(server.store.publish(point, x, 1), "{point}");
    }
    reads(cmdif, "READ=load-power", "ACK=load-power:-248.523697\\x00").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dnp3_outstation_answers_on_a_serial_port() {
    let ((path, port), cmdif) = (pty_from().await, free_port());
    let server = modules(
        &DNP3_SERVER
            .replace("tcp://127.0.0.1:20000", &path)
            .replace("rtu-1-dnp3-outstation.log", "/dev/null"),
    );
    let _client = modules(
        &DNP3_CLIENT
            .replace("tcp://127.0.0.1:20000", &format!("tcp://127.0.0.1:{port}"))
            .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}"))
            .replace("<scan-rate>5</scan-rate>", "<scan-rate>1</scan-rate>"),
    );
    assert!(server.store.publish("load-1_bus-1.mw", 250.5, 1));
    reads(cmdif, "READ=load-power", "ACK=load-power:250.500000\\x00").await;
}
