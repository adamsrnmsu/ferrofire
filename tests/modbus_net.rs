//! The Modbus server over real sockets: framing as clients see it, descriptor handling (the bennu #79 / #81 shapes: closed sockets
//! must really close, a bad or cut-short frame must not be answered, the listener must survive all of it), many connections, a bind
//! failure, and stopping. The replies themselves are checked byte for byte against stock in tests/modbus_golden.rs.

mod common;

use common::modbus::*;
use ferrofire::fd::proto::modbus::Server;
use ferrofire::fd::proto::Module;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

fn log_text(d: &Device) -> String {
    std::fs::read_to_string(&d.log).unwrap_or_default()
}

/// What a client of a server that refused it sees: nothing, and a closed connection.
async fn refused(port: u16, bytes: &[u8], half_close: bool) -> bool {
    let mut s = connect(port).await;
    let _ = s.write_all(bytes).await;
    if half_close {
        s.shutdown().await.unwrap();
    }
    drain(&mut s, Duration::from_secs(2)).await == (vec![], true)
}

#[tokio::test]
async fn a_client_reads_over_tcp_and_the_tid_and_unit_id_come_back() {
    let d = start();
    d.store.publish("pir", 250.5, 1);
    assert_eq!(
        ask(d.port, &good(9, &READ_IR)).await,
        [0, 9, 0, 0, 0, 5, 1, 4, 2, 0x40, 0x20]
    );
    let mut other_unit = good(0xABCD, &READ_IR);
    other_unit[6] = 77;
    assert_eq!(
        ask(d.port, &other_unit).await,
        [0xAB, 0xCD, 0, 0, 0, 5, 77, 4, 2, 0x40, 0x20]
    );
    d.stop.cancel();
}

#[tokio::test]
async fn writes_from_clients_go_to_the_store_queue_and_not_into_the_store() {
    let d = start();
    let mut s = connect(d.port).await;
    for (tid, pdu) in [
        (1u16, vec![5u8, 0, 0, 0xFF, 0]),
        (2, vec![6, 0x9c, 0x40, 0x80, 0]),
    ] {
        s.write_all(&good(tid, &pdu)).await.unwrap();
        let mut echo = vec![0u8; 12];
        s.read_exact(&mut echo).await.unwrap();
        assert_eq!(echo[7..], pdu[..]);
    }
    let q: Vec<_> = d.store.pending(0);
    assert_eq!(q.len(), 2, "{q:?}");
    assert_eq!(
        d.store.get_bool("co"),
        Some(false),
        "a write waits for the provider"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn malformed_frames_get_no_answer_and_the_connection_is_closed() {
    let d = start();
    let read = [4u8, 0x75, 0x30, 0, 1];
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("length 0", frame(1, 0, 0, &read)),
        ("length 1", frame(1, 0, 1, &[])),
        ("length 255", frame(1, 0, 255, &read)),
        ("length 256", frame(1, 0, 256, &read)),
        ("length 4000", frame(1, 0, 4000, &read)),
        ("length 65535", frame(1, 0, 65535, &read)),
        ("protocol id 1", frame(1, 1, 6, &read)),
        ("protocol id 0x0100", frame(1, 0x100, 6, &read)),
    ];
    cases.push(("garbage", vec![0xFF; 100_000]));
    for (name, bytes) in &cases {
        assert!(refused(d.port, bytes, false).await, "{name}");
    }
    assert!(
        answers(&d).await,
        "the server still answers after all of them"
    );
    d.stop.cancel();
}

async fn answers(d: &Device) -> bool {
    ask(d.port, &good(5, &READ_DI)).await[..2] == [0, 5]
}

#[tokio::test]
async fn a_frame_cut_short_is_never_answered_from_the_previous_request() {
    let d = start();
    let full = good(1, &READ_IR);
    // bennu #81: a request cut after the function code was answered from what the previous request left in the buffer
    for cut in [1, 5, 6, 7, 8, 10, full.len() - 1] {
        assert!(refused(d.port, &full[..cut], true).await, "cut at {cut}");
    }
    let mut s = connect(d.port).await;
    s.write_all(&full).await.unwrap();
    let mut r = [0u8; 11];
    s.read_exact(&mut r).await.unwrap();
    s.write_all(&full[..8]).await.unwrap();
    s.shutdown().await.unwrap();
    assert_eq!(drain(&mut s, Duration::from_secs(2)).await, (vec![], true));
    assert!(answers(&d).await);
    d.stop.cancel();
}

#[tokio::test]
async fn pipelined_split_and_trickled_frames_are_all_answered_in_order() {
    let d = start();
    let (a, b) = (good(1, &READ_IR), good(2, &READ_DI));
    let mut s = connect(d.port).await;
    s.write_all(&[a.clone(), b.clone()].concat()).await.unwrap();
    let (got, closed) = drain(&mut s, Duration::from_millis(300)).await;
    assert!(!closed);
    assert_eq!(
        (got.len(), got[..2].to_vec(), got[11..13].to_vec()),
        (11 + 10, vec![0, 1], vec![0, 2])
    );
    // one byte at a time
    for byte in &a {
        s.write_all(&[*byte]).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let (got, closed) = drain(&mut s, Duration::from_millis(300)).await;
    assert!(!closed && answers_a_read(&got, 1), "{got:?}");
    // a good frame then a bad one: the good one is answered, then the connection closes
    s.write_all(&a).await.unwrap();
    let mut r = [0u8; 11];
    s.read_exact(&mut r).await.unwrap();
    s.write_all(&frame(3, 9, 6, &READ_IR)).await.unwrap();
    assert_eq!(drain(&mut s, Duration::from_secs(2)).await, (vec![], true));
    d.stop.cancel();
}

fn answers_a_read(r: &[u8], tid: u16) -> bool {
    r.len() == 11 && r[..2] == tid.to_be_bytes() && r[4..6] == [0, 5] && r[7] == 4
}

#[tokio::test]
async fn any_number_of_clients_are_served_at_once() {
    let d = start();
    let mut held = vec![];
    for i in 0..300u16 {
        held.push((i, connect(d.port).await));
    }
    for (i, s) in held.iter_mut() {
        s.write_all(&good(*i, &READ_IR)).await.unwrap();
    }
    for (i, s) in held.iter_mut() {
        let mut r = [0u8; 11];
        timeout(Duration::from_secs(3), s.read_exact(&mut r))
            .await
            .expect("an answer")
            .unwrap();
        assert!(answers_a_read(&r, *i));
    }
    d.stop.cancel();
}

/// The bennu #79 shape: every connection must really close, so descriptors do not pile up under churn.
#[tokio::test]
async fn connection_churn_leaves_no_descriptors_behind() {
    let d = start();
    assert!(answers(&d).await);
    let before = open_fds();
    for i in 0..2000u16 {
        let mut s = connect(d.port).await;
        match i % 4 {
            0 => {
                s.write_all(&good(i, &READ_IR)).await.unwrap();
                let mut r = [0u8; 11];
                s.read_exact(&mut r).await.unwrap();
            }
            1 => s.write_all(&good(i, &READ_IR)[..9]).await.unwrap(), // cut short
            2 => s.write_all(&frame(i, 7, 6, &READ_IR)).await.unwrap(), // refused
            _ => {}                                                   // connect and go
        }
        drop(s);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(answers(&d).await);
    if let (Some(before), Some(after)) = (before, open_fds()) {
        assert!(after <= before + 8, "descriptors {before} -> {after}");
    }
    d.stop.cancel();
}

#[tokio::test]
async fn a_bind_failure_is_logged_and_does_not_panic() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let log = std::env::temp_dir().join(format!("modbus-bind-{port}.log"));
    let (out, store) =
        common::load_as::<Server>(&config(port, &log.display().to_string()), "modbus-server");
    timeout(
        Duration::from_secs(2),
        out.run(store, CancellationToken::new()),
    )
    .await
    .expect("run returns when it cannot bind");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains(",error,modbus-server,modbus TCP server init,"),
        "{text}"
    );
}

#[tokio::test]
async fn a_serial_endpoint_is_reported_not_served() {
    let log = std::env::temp_dir().join(format!("modbus-serial-{}.log", free_port()));
    let xml = config(1, &log.display().to_string()).replace("tcp://127.0.0.1:1", "/dev/ttyUSB0");
    let (out, store) = common::load_as::<Server>(&xml, "modbus-server");
    timeout(
        Duration::from_secs(2),
        out.run(store, CancellationToken::new()),
    )
    .await
    .unwrap();
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .contains("not supported yet"));
}

#[tokio::test]
async fn stop_ends_the_connections_and_the_listener() {
    let d = start();
    let mut s = connect(d.port).await;
    assert!(answers(&d).await);
    d.stop.cancel();
    assert_eq!(drain(&mut s, Duration::from_secs(2)).await, (vec![], true));
    timeout(Duration::from_secs(2), d.task)
        .await
        .expect("the server task ends")
        .unwrap();
    assert!(TcpStream::connect(("127.0.0.1", d.port)).await.is_err());
}

#[tokio::test]
async fn the_event_log_records_reads_writes_and_refusals() {
    let d = start();
    d.store.publish("pir", 1.0, 1);
    let mut s = connect(d.port).await;
    s.write_all(&good(1, &READ_IR)).await.unwrap();
    s.write_all(&good(2, &[5, 0, 9, 0xFF, 0])).await.unwrap();
    s.write_all(&frame(3, 1, 6, &READ_IR)).await.unwrap();
    let _ = drain(&mut s, Duration::from_millis(500)).await;
    let text = log_text(&d);
    for want in [
        ",info,modbus-server,read input registers,start address for read input registers request is 30000 and read 1 input registers.",
        ",error,modbus-server,write coils,Invalid write coils request address: 9",
        ",error,modbus-server,modbus connection,closing the connection: protocol id is not 0",
    ] {
        assert!(text.contains(want), "{want}\n{text}");
    }
    d.stop.cancel();
}
