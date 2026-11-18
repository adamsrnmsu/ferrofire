//! `<modbus-client>` in bennu mode, in one process with our own `<modbus-server>`: every command of the stock transcript
//! (conformance/stock/client-modbus.golden, recorded from the stock client against the stock server) must get the stock reply.
//! The bytes on the wire are compared in `make conformance PROTO=modbus-client`; here a scripted server checks the framing rules
//! that transcript cannot show: a response with another transaction id, and a server that never answers.

mod common;

use common::clients::*;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GOLDEN: &str = include_str!("../conformance/stock/client-modbus.golden");
const SERVER: &str = include_str!("../conformance/stock/server-modbus.xml");
const CLIENT: &str = include_str!("../conformance/stock/client-modbus.xml");

fn server_xml(port: u16) -> String {
    SERVER
        .replace("tcp://127.0.0.1:5502", &format!("tcp://127.0.0.1:{port}"))
        .replace("rtu-1-modbus-server.log", "/dev/null")
}

fn client_xml(port: u16, cmdif: u16) -> String {
    CLIENT
        .replace("tcp://127.0.0.1:5502", &format!("tcp://127.0.0.1:{port}"))
        .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_command_of_the_stock_transcript_gets_the_stock_reply() {
    let (port, cmdif) = (free_port(), free_port());
    let _client = modules(&client_xml(port, cmdif));
    let mut server = None;
    let mut n = 0;
    for line in transcript(GOLDEN) {
        match line {
            Line::Mark("server started") => {
                server = Some(modules(&server_xml(port)));
                listening(port, true).await;
            }
            Line::Mark("server stopped") => {
                server = None;
                listening(port, false).await;
            }
            Line::Mark("inputs published") => {
                let s = &server.as_ref().unwrap().store;
                for (point, x) in [
                    ("load-1_bus-1.mw", 250.5),
                    ("generator-1_bus-1.active", 1.0),
                    ("load-1_bus-1.active", 0.0),
                    ("bus-1.gen_mw", 12.5),
                ] {
                    assert!(s.publish(point, x, 1), "{point}");
                }
            }
            Line::Mark(_) | Line::Wire(_) => {}
            Line::Command(req, want) => {
                assert_eq!(shown(&command(cmdif, req).await), want, "reply to {req:?}");
                n += 1;
            }
        }
    }
    assert_eq!(n, 58, "commands replayed");
}

/// A server that answers each request with what `script` makes of it (`None`: nothing).
async fn scripted(
    script: fn(usize, &[u8]) -> Option<Vec<u8>>,
) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut n = 0;
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 260];
            while let Ok(got) = s.read(&mut buf).await {
                if got == 0 {
                    break;
                }
                let _ = tx.send(buf[..got].to_vec());
                if let Some(reply) = script(n, &buf[..got]) {
                    let _ = s.write_all(&reply).await;
                }
                n += 1;
            }
        }
    });
    (port, rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_transaction_id_is_3_and_a_response_with_another_id_is_discarded() {
    // the wrong id first (same request otherwise), then the right one, in one segment
    let (port, mut seen) = scripted(|_, req| {
        let mut wrong = req[..2].to_vec();
        wrong[1] ^= 0xff;
        let tail = [0, 0, 0, 5, 1, 3, 2];
        Some(
            [
                &wrong[..],
                &tail[..],
                &[0xff, 0xff],
                &req[..2],
                &tail[..],
                &[0x40, 0x20],
            ]
            .concat(),
        )
    })
    .await;
    let cmdif = free_port();
    let client = modules(&client_xml(port, cmdif));
    let want = "ACK=load-mw-setpoint:250.492096\\x00";
    assert_eq!(shown(&command(cmdif, "READ=load-mw-setpoint").await), want);
    assert_eq!(
        seen.recv().await.unwrap(),
        [0, 3, 0, 0, 0, 6, 1, 3, 0x9c, 0x40, 0, 1]
    );
    assert_eq!(shown(&command(cmdif, "READ=load-mw-setpoint").await), want);
    assert_eq!(seen.recv().await.unwrap()[..2], [0, 4]);
    // the tag holds what the client last read, once a scan cycle applies it
    client.store.apply_internal();
    let got = client.store.get_f64("load-mw-setpoint").unwrap();
    assert!((got - 250.492096).abs() < 1e-4, "{got}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_never_answers_costs_one_second_and_the_next_request_reconnects() {
    let (port, mut seen) =
        scripted(|n, req| (n > 0).then(|| [&req[..2], &[0, 0, 0, 4, 1, 2, 1, 1][..]].concat()))
            .await;
    let cmdif = free_port();
    let _client = modules(&client_xml(port, cmdif));
    let t = Instant::now();
    assert_eq!(
        shown(&command(cmdif, "READ=gen-status").await),
        "ERR=Failed reading tag 'gen-status': <text>\\x00"
    );
    let took = t.elapsed();
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_millis(3000),
        "{took:?}"
    );
    assert_eq!(
        shown(&command(cmdif, "READ=gen-status").await),
        "ACK=gen-status:true\\x00"
    );
    assert_eq!(seen.recv().await.unwrap()[..2], [0, 3]);
    assert_eq!(
        seen.recv().await.unwrap()[..2],
        [0, 4],
        "the id counts the request that failed"
    );
}
