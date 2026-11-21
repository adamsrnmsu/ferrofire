//! `<dnp3-client>` in bennu mode, in one process with our own `<dnp3-server>`: the startup frames and the commands must be the
//! stock master's bytes (conformance/stock/client-dnp3-raw.golden), and every command of the stock transcript
//! (client-dnp3.golden) must get the stock reply. A scripted outstation covers what ours never does: an unsolicited response,
//! a response with another sequence number, a refused select.

mod common;

use common::clients::*;
use common::unhex;
use ferrofire::fd::proto::dnp3::link::{self, Parse};
use ferrofire::fd::proto::dnp3::{app, master, transport};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GOLDEN: &str = include_str!("../conformance/stock/client-dnp3.golden");
const SERVER: &str = include_str!("../conformance/stock/server-dnp3.xml");
const CLIENT: &str = include_str!("../conformance/stock/client-dnp3.xml");
const SCAN: &str = "013c02063c03063c04063c0106";

fn server_xml(port: u16) -> String {
    SERVER
        .replace("tcp://127.0.0.1:20000", &format!("tcp://127.0.0.1:{port}"))
        .replace("rtu-1-dnp3-outstation.log", "/dev/null")
}

/// The stock client config with a 2 s scan, so a replay does not take a minute.
fn client_xml(port: u16, cmdif: u16) -> String {
    CLIENT
        .replace("tcp://127.0.0.1:20000", &format!("tcp://127.0.0.1:{port}"))
        .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}"))
        .replace("<scan-rate>5</scan-rate>", "<scan-rate>2</scan-rate>")
}

/// Every link frame in the chunks a tap or a scripted peer saw, as (control, dst, src, user data).
fn frames(chunks: &[Vec<u8>]) -> Vec<link::Frame> {
    let mut buf: Vec<u8> = chunks.concat();
    let mut out = vec![];
    while let Parse::Frame(f, used) = link::parse(&buf) {
        out.push(f);
        buf.drain(..used);
    }
    out
}

/// The application fragments the master sent, as hex, sequence numbers included.
fn sent(t: &Tap) -> Vec<String> {
    frames(&t.sent())
        .iter()
        .map(|f| f.user[1..].iter().map(|b| format!("{b:02x}")).collect())
        .collect()
}

fn scans(t: &Tap) -> usize {
    sent(t).iter().filter(|a| a[2..] == *SCAN).count()
}

/// Wait for the master's next all-classes scan and its response, so what follows falls between two scans.
async fn after_a_scan(t: &Tap) {
    let n = scans(t);
    until("the next class scan", Duration::from_secs(20), || {
        scans(t) > n
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_startup_frames_and_the_commands_are_the_stock_masters_bytes() {
    let (port, cmdif) = (free_port(), free_port());
    let _server = modules(&server_xml(port));
    listening(port, true).await;
    let t = tap(port).await;
    let _client = modules(&client_xml(t.port, cmdif));
    until("the startup sequence", Duration::from_secs(5), || {
        frames(&t.sent()).len() >= 4
    })
    .await;
    // conformance/stock/client-dnp3-raw.golden, after "# server started": integrity poll, clear restart, integrity poll, scan
    let raw: Vec<Vec<u8>> = [
        "05640bc40a000100acd1c0c0013c0106ff50",
        "05640ec40a0001002529c1c102500100070700ff81",
        "05640bc40a000100acd1c2c2013c01064430",
        "056414c40a0001008fedc3c3013c02063c03063c04063c0106b2a0",
    ]
    .iter()
    .map(|h| unhex(h))
    .collect();
    assert_eq!(t.sent().concat()[..raw.concat().len()], raw.concat());
    // no enable- or disable-unsolicited request, ever
    assert!(
        sent(&t).iter().all(|a| !["14", "15"].contains(&&a[2..4])),
        "{:?}",
        sent(&t)
    );
    // direct operate, select then operate (consecutive sequence numbers, the same object), and a float32 analog output
    after_a_scan(&t).await;
    let before = sent(&t).len();
    for (cmd, reply) in [
        (
            "WRITE=load-breaker-toggle:true",
            "ACK=Wrote tag load-breaker-toggle -- true\\x00",
        ),
        (
            "READ=load-breaker-toggle",
            "ACK=load-breaker-toggle:true\\x00",
        ),
        (
            "WRITE=gen-toggle:true",
            "ACK=Wrote tag gen-toggle -- true\\x00",
        ),
        (
            "WRITE=gen-mw-setpoint:7",
            "ACK=Wrote tag gen-mw-setpoint -- 7\\x00",
        ),
        ("READ=gen-mw-setpoint", "ACK=gen-mw-setpoint:7.000000\\x00"),
    ] {
        assert_eq!(shown(&command(cmdif, cmd).await), reply, "{cmd}");
    }
    until("the commands on the wire", Duration::from_secs(2), || {
        sent(&t).len() >= before + 4
    })
    .await;
    let s = sent(&t);
    let n = usize::from_str_radix(&s[before][1..2], 16).unwrap();
    let want = [
        (n, "050c0128010005000301640000006400000000"),
        (n + 1, "030c0128010006000301640000006400000000"),
        (n + 2, "040c0128010006000301640000006400000000"),
        (n + 3, "05290328010002000000e04000"),
    ];
    for (got, (seq, body)) in s[before..].iter().zip(want) {
        assert_eq!(*got, format!("c{:x}{body}", seq % 16));
    }
    // the transport sequence counts every segment, on past 15
    let tseq: Vec<u8> = frames(&t.sent()).iter().map(|f| f.user[0]).collect();
    for (i, h) in tseq.iter().enumerate() {
        assert_eq!(*h, 0xC0 | (i as u8 & 0x3F));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_command_of_the_stock_transcript_gets_the_stock_reply() {
    let (port, cmdif) = (free_port(), free_port());
    let t = tap(port).await;
    let _client = modules(&client_xml(t.port, cmdif));
    let mut server = None;
    let mut n = 0;
    for line in transcript(GOLDEN) {
        match line {
            Line::Mark("server started") => {
                server = Some(modules(&server_xml(port)));
                listening(port, true).await;
                after_a_scan(&t).await;
            }
            Line::Mark("server stopped") => {
                server = None;
                listening(port, false).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Line::Mark("inputs published") => {
                let s = &server.as_ref().unwrap().store;
                for (point, x) in [
                    ("load-1_bus-1.mw", 250.5),
                    ("generator-1_bus-1.active", 1.0),
                    ("load-1_bus-1.active", 1.0),
                    ("bus-1.gen_mw", 12.5),
                ] {
                    assert!(s.publish(point, x, 1), "{point}");
                }
                // one scan may have been under way; the one after it has the values
                after_a_scan(&t).await;
                after_a_scan(&t).await;
            }
            Line::Mark("a mapping the command does not fit")
            | Line::Mark("after the next scan") => after_a_scan(&t).await,
            Line::Mark(_) | Line::Wire(_) => {}
            Line::Command(req, want) => {
                assert_eq!(
                    shown(&command(cmdif, req).await),
                    want,
                    "reply to {req:?} (command {n})"
                );
                n += 1;
            }
        }
    }
    assert_eq!(n, GOLDEN.lines().filter(|l| l.contains('\t')).count());
    assert!(n > 50);
}

/// A scripted outstation at address 10 for a master at address 1: every request fragment goes to `script`, which answers with
/// fragments (control octet first). What the master sent is kept.
struct Scripted {
    port: u16,
    got: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

async fn scripted(script: fn(usize, &[u8]) -> Vec<Vec<u8>>) -> Scripted {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let s = Scripted {
        port: l.local_addr().unwrap().port(),
        got: Default::default(),
    };
    let got = s.got.clone();
    tokio::spawn(async move {
        let (mut sock, _) = l.accept().await.unwrap();
        let (mut buf, mut chunk, mut tseq) = (vec![], [0u8; 1024], 0u8);
        loop {
            while let Parse::Frame(f, used) = link::parse(&buf) {
                buf.drain(..used);
                let n = {
                    let mut g = got.lock().unwrap();
                    g.push(f.user[1..].to_vec());
                    g.len() - 1
                };
                for fragment in script(n, &f.user[1..]) {
                    for seg in transport::segment(&fragment, &mut tseq) {
                        sock.write_all(&link::encode(0x44, 1, 10, &seg))
                            .await
                            .unwrap();
                    }
                }
            }
            match sock.read(&mut chunk).await {
                Ok(n @ 1..) => buf.extend_from_slice(&chunk[..n]),
                _ => return,
            }
        }
    });
    s
}

impl Scripted {
    fn got(&self) -> Vec<Vec<u8>> {
        self.got.lock().unwrap().clone()
    }
}

/// An empty response to request `req` (IIN clear), plus `objects`.
fn answer(req: &[u8], objects: &str) -> Vec<u8> {
    app::response(req[0] & 0x0F, app::FIR | app::FIN, 0, &unhex(objects))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsolicited_response_reaches_the_tag_and_is_confirmed() {
    // the first request (the integrity poll) is answered, and an unsolicited response follows it: g2v1, index 2, on, CON set
    let o = scripted(|n, req| match (n, req[1]) {
        (_, 0) => vec![],
        (0, _) => vec![
            answer(req, ""),
            unhex("f58200000201170102 81".replace(' ', "").as_str()),
        ],
        _ => vec![answer(req, "")],
    })
    .await;
    let cmdif = free_port();
    let client = modules(&client_xml(o.port, cmdif));
    until("the confirm", Duration::from_secs(5), || {
        o.got().contains(&master::confirm(5, true))
    })
    .await;
    assert_eq!(
        shown(&command(cmdif, "READ=brkr").await),
        "ACK=brkr:true\\x00"
    );
    client.store.apply_internal();
    assert_eq!(client.store.get_bool("brkr"), Some(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_with_another_sequence_number_changes_nothing() {
    // the integrity poll (sequence 0) gets a response numbered 9 that says brkr is on, then the real one that says nothing
    let o = scripted(|n, req| match n {
        0 => vec![
            unhex("c98100000102000202 81".replace(' ', "").as_str()),
            answer(req, ""),
        ],
        _ => vec![answer(req, "")],
    })
    .await;
    let cmdif = free_port();
    let client = modules(&client_xml(o.port, cmdif));
    // the master goes on to its scan, so the mismatch did not stall it
    until("the scan after startup", Duration::from_secs(5), || {
        o.got().len() >= 2
    })
    .await;
    assert_eq!(
        shown(&command(cmdif, "READ=brkr").await),
        "ACK=brkr:false\\x00"
    );
    client.store.apply_internal();
    assert_eq!(client.store.get_bool("brkr"), Some(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_select_is_not_operated_and_the_cache_is_optimistic_anyway() {
    // a select is answered with status 4 (NOT_SUPPORTED) in the echo; everything else with an echo of the request's objects
    let o = scripted(|_, req| match req[1] {
        3 => {
            let mut echo = req[2..].to_vec();
            *echo.last_mut().unwrap() = 4;
            vec![app::response(req[0] & 0x0F, app::FIR | app::FIN, 0, &echo)]
        }
        4 | 5 => vec![app::response(
            req[0] & 0x0F,
            app::FIR | app::FIN,
            0,
            &req[2..],
        )],
        _ => vec![answer(req, "")],
    })
    .await;
    let cmdif = free_port();
    let _client = modules(&client_xml(o.port, cmdif));
    until("startup", Duration::from_secs(5), || o.got().len() >= 2).await;
    assert_eq!(
        shown(&command(cmdif, "WRITE=gen-toggle:true").await),
        "ACK=Wrote tag gen-toggle -- true\\x00"
    );
    assert_eq!(
        shown(&command(cmdif, "READ=gen-toggle").await),
        "ACK=gen-toggle:true\\x00"
    );
    assert_eq!(
        shown(&command(cmdif, "WRITE=load-breaker-toggle:true").await),
        "ACK=Wrote tag load-breaker-toggle -- true\\x00"
    );
    until("the direct operate", Duration::from_secs(2), || {
        o.got().iter().any(|f| f[1] == 5)
    })
    .await;
    let funcs: Vec<u8> = o.got().iter().map(|f| f[1]).collect();
    assert!(
        funcs.contains(&3) && !funcs.contains(&4),
        "select without operate: {funcs:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn while_nothing_is_awaited_an_unsolicited_response_is_taken_and_anything_else_discarded() {
    // after the answer to the first scan (request 1) come, unasked: a response numbered 9 that says load-power is 7, a fragment
    // too short to be anything, and an unsolicited response (CON set) that says brkr is on
    let o = scripted(|n, req| match (n, req[1]) {
        (_, 0) => vec![],
        (1, _) => vec![
            answer(req, ""),
            unhex("c98100001e03000000 07000000".replace(' ', "").as_str()),
            vec![0xc0],
            unhex("f28200000201170102 81".replace(' ', "").as_str()),
        ],
        _ => vec![answer(req, "")],
    })
    .await;
    let cmdif = free_port();
    let _client = modules(&client_xml(o.port, cmdif));
    until("the confirm", Duration::from_secs(5), || {
        o.got().contains(&master::confirm(2, true))
    })
    .await;
    assert_eq!(
        shown(&command(cmdif, "READ=brkr").await),
        "ACK=brkr:true\\x00"
    );
    assert_eq!(
        shown(&command(cmdif, "READ=load-power").await),
        "ACK=load-power:0.000000\\x00"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_that_is_not_dnp3_is_closed_and_the_master_connects_again() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a2 = accepted.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            a2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                let _ = s.read(&mut buf).await;
                // a frame for another master first (stepped over), then bytes that are no link frame
                let _ = s
                    .write_all(&link::encode(0x44, 7, 10, &[0xC0, 0xC0, 0x81, 0, 0]))
                    .await;
                let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
                // the master must hang up: read until it does
                while let Ok(1..) = s.read(&mut buf).await {}
            });
        }
    });
    let cmdif = free_port();
    let _client = modules(&client_xml(port, cmdif));
    let n = || accepted.load(std::sync::atomic::Ordering::SeqCst);
    until("a second connection", Duration::from_secs(6), || n() >= 2).await;
    assert_eq!(
        shown(&command(cmdif, "READ=brkr").await),
        "ACK=brkr:false\\x00"
    );
}

/// The first `n` requests the scripted outstation got that are not CONFIRMs.
fn requests(o: &Scripted) -> Vec<Vec<u8>> {
    o.got().into_iter().filter(|r| r[1] != 0).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_response_in_two_fragments_the_next_request_counts_both() {
    // ww-0sm: opendnp3 increments its sequence for every response fragment it accepts; the integrity poll (0) is answered by
    // fragments 0 (CON, not the last) and 1, so the next request carries 2
    let o = scripted(|n, req| match (n, req[1]) {
        (_, 0) => vec![],
        (0, _) => vec![
            app::response(0, app::FIR | app::CON, 0, &[]),
            app::response(1, app::FIN, 0, &[]),
        ],
        _ => vec![answer(req, "")],
    })
    .await;
    let _client = modules(&client_xml(o.port, free_port()));
    until("the next request", Duration::from_secs(5), || {
        requests(&o).len() >= 2
    })
    .await;
    assert_eq!(requests(&o)[1][0] & 0x0F, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_response_that_stops_midway_the_next_request_counts_the_fragment_and_the_timeout() {
    // ww-a12: opendnp3 adds one to its sequence for each response fragment it accepts and one more when the response times
    // out. The integrity poll (0) gets fragment 0 (CON, not the last) and then silence: the next request carries 2, so a late
    // fragment numbered 1 is not taken for its answer
    let o = scripted(|n, req| match (n, req[1]) {
        (_, 0) => vec![],
        (0, _) => vec![app::response(0, app::FIR | app::CON, 0, &[])],
        _ => vec![answer(req, "")],
    })
    .await;
    let _client = modules(&client_xml(o.port, free_port()));
    until("the next request", Duration::from_secs(12), || {
        requests(&o).len() >= 2
    })
    .await;
    assert_eq!(requests(&o)[1][0] & 0x0F, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_masters_link_layer_answers_its_outstations_requests_as_opendnp3_does() {
    // ww-0sm: an outstation (address 10) to the master (1): REQUEST LINK STATUS, RESET LINK STATES, TEST LINK STATES
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _client = modules(&client_xml(l.local_addr().unwrap().port(), free_port()));
    let (mut sock, _) = l.accept().await.unwrap();
    let mut buf = vec![];
    // what the master sent up to now is application data; the frame we wait for is the first with PRM clear
    let mut ask = async |ctrl: u8| -> Option<u8> {
        sock.write_all(&link::encode(ctrl, 1, 10, &[]))
            .await
            .unwrap();
        let end = tokio::time::Instant::now() + Duration::from_millis(600);
        let mut chunk = [0u8; 256];
        loop {
            while let Parse::Frame(f, used) = link::parse(&buf) {
                buf.drain(..used);
                if !f.primary() {
                    return Some(f.ctrl);
                }
            }
            match tokio::time::timeout_at(end, sock.read(&mut chunk)).await {
                Ok(Ok(n @ 1..)) => buf.extend_from_slice(&chunk[..n]),
                _ => return None,
            }
        }
    };
    assert_eq!(ask(0x49).await, Some(0x8B), "link status: LINK STATUS");
    assert_eq!(
        ask(0x72).await,
        None,
        "TEST LINK STATES before a reset is ignored"
    );
    assert_eq!(ask(0x40).await, Some(0x80), "RESET LINK STATES: ACK");
    assert_eq!(ask(0x52).await, None, "a wrong frame count bit is ignored");
    assert_eq!(
        ask(0x72).await,
        Some(0x80),
        "the expected frame count bit: ACK"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_outstation_that_never_answers_the_status_read_is_not_asked_again_and_not_dropped() {
    // ww-3oa 4: a fep master reads the output status after each scan; this outstation answers everything but that
    let o = scripted(|_, req| match (req[1], &req[2..]) {
        (0, _) | (1, [10, 0, 6, 40, 3, 6]) => vec![],
        _ => vec![answer(req, "")],
    })
    .await;
    let xml = format!(
        "<SCEPTRE><field-device><name>fep</name><cycle-time>40</cycle-time><comms><dnp3-client><address>1</address>\
         <scan-rate>1</scan-rate><mode>fep</mode><dnp3-connection><endpoint>tcp://127.0.0.1:{}</endpoint><address>10</address>\
         <status-tag>up</status-tag><binary-output><address>5</address><tag>pump</tag></binary-output></dnp3-connection>\
         </dnp3-client></comms></field-device></SCEPTRE>",
        o.port
    );
    let client = modules(&xml);
    let reads = |o: &Scripted| {
        requests(o)
            .iter()
            .filter(|r| r[2..] == [10, 0, 6, 40, 3, 6])
            .count()
    };
    until("three scans", Duration::from_secs(12), || {
        requests(&o)
            .iter()
            .filter(|r| r[1..] == *unhex(SCAN))
            .count()
            >= 3
    })
    .await;
    client.store.apply_internal();
    assert_eq!(reads(&o), 1, "asked once");
    assert_eq!(
        client.store.get_bool("up"),
        Some(true),
        "and the connection stayed"
    );
}
