//! `fep` mode end to end, whole devices in one process: a FEP device polls an RTU device (our own server) into its tags, re-serves
//! them through its own server, writes local changes down, and never writes a polled value back (the echo rule). A relay between
//! the two (`Tap`) shows what the FEP sent. The Review Focus cases of the plan are here: a peer that is down or silent, a write
//! while it is down, NaN and out-of-range values.

mod common;

use common::clients::*;
use ferrofire::model::Value::{Analog, Binary};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const S: fn(u64) -> Duration = Duration::from_secs;

/// An RTU: internal tags behind a Modbus server, so a test sets "field values" with `store.write`.
fn rtu_xml(port: u16) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>rtu</name><cycle-time>20</cycle-time>
<tags>
 <internal-tag><name>level</name><value>0</value></internal-tag>
 <internal-tag><name>setpoint</name><value>0</value></internal-tag>
 <internal-tag><name>spare</name><value>0</value></internal-tag>
 <internal-tag><name>pump</name><status>false</status></internal-tag>
 <internal-tag><name>alarm</name><status>false</status></internal-tag>
</tags>
<comms><modbus-server><endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging>
 <input-register><address>30000</address><max-value>1000</max-value><min-value>0</min-value><tag>level</tag></input-register>
 <holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>setpoint</tag></holding-register>
 <holding-register><address>40001</address><max-value>1000</max-value><min-value>0</min-value><tag>spare</tag></holding-register>
 <coil><address>0</address><tag>pump</tag></coil>
 <discrete-input><address>10000</address><tag>alarm</tag></discrete-input>
</modbus-server></comms></field-device></SCEPTRE>"#
    )
}

const POINTS: &str = r#"
 <input-register><address>30000</address><max-value>1000</max-value><min-value>0</min-value><tag>level</tag></input-register>
 <holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>setpoint</tag></holding-register>
 <holding-register><address>40001</address><max-value>1000</max-value><min-value>0</min-value><tag>spare</tag></holding-register>
 <coil><address>0</address><tag>pump</tag></coil>
 <discrete-input><address>10000</address><tag>alarm</tag></discrete-input>"#;

/// A FEP: a `fep` mode client polling `rtu` every 100 ms, a 40 ms cycle, and its own server on `serve` over the same tags.
/// `more` goes into `<modbus-client>` (another connection, a command interface).
fn fep_xml(rtu: u16, serve: u16, more: &str) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>fep</name><cycle-time>40</cycle-time>
<comms><modbus-client><mode>fep</mode>
 <modbus-connection><endpoint>tcp://127.0.0.1:{rtu}</endpoint><unit-id>1</unit-id><scan-rate>0.1</scan-rate><status-tag>rtu-up</status-tag>{POINTS}
 </modbus-connection>{more}
</modbus-client>
<modbus-server><endpoint>tcp://127.0.0.1:{serve}</endpoint><event-logging>/dev/null</event-logging>{POINTS}
</modbus-server></comms></field-device></SCEPTRE>"#
    )
}

/// One Modbus request to 127.0.0.1:`port`; the reply PDU.
async fn modbus(port: u16, pdu: &[u8]) -> Vec<u8> {
    listening(port, true).await;
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut f = vec![0, 1, 0, 0, 0, pdu.len() as u8 + 1, 1];
    f.extend_from_slice(pdu);
    s.write_all(&f).await.unwrap();
    let mut head = [0u8; 7];
    s.read_exact(&mut head).await.unwrap();
    let mut body = vec![0u8; usize::from(head[5]) - 1];
    s.read_exact(&mut body).await.unwrap();
    body
}

/// The request PDUs the FEP sent through the tap with function code `fc`.
fn requests(t: &Tap, fc: u8) -> Vec<Vec<u8>> {
    t.sent()
        .into_iter()
        .map(|f| f[7..].to_vec())
        .filter(|p| p[0] == fc)
        .collect()
}

struct Rig {
    rtu: Device,
    fep: Device,
    tap: Tap,
    rtu_port: u16,
    serve: u16,
}

async fn rig(more: &str) -> Rig {
    let (rtu_port, serve) = (free_port(), free_port());
    let rtu = device(&rtu_xml(rtu_port));
    listening(rtu_port, true).await;
    let tap = tap(rtu_port).await;
    let fep = device(&fep_xml(tap.port, serve, more));
    Rig {
        rtu,
        fep,
        tap,
        rtu_port,
        serve,
    }
}

fn near(got: Option<f64>, want: f64) -> bool {
    got.is_some_and(|g| (g - want).abs() < 0.02)
}

#[tokio::test(flavor = "multi_thread")]
async fn gateway_a_value_on_the_rtu_appears_on_the_feps_own_server() {
    let r = rig("").await;
    assert_eq!(
        r.fep.store.get_bool("rtu-up"),
        Some(false),
        "false before the first connection"
    );
    r.rtu.store.write("level", Analog(250.0)).unwrap();
    r.rtu.store.write("alarm", Binary(true)).unwrap();
    r.rtu.store.write("setpoint", Analog(100.0)).unwrap();
    let f = &r.fep.store;
    until("the polled values", S(3), || {
        near(f.get_f64("level"), 250.0)
            && f.get_bool("alarm") == Some(true)
            && near(f.get_f64("setpoint"), 100.0)
    })
    .await;
    assert_eq!(f.get_bool("rtu-up"), Some(true));
    // through the FEP's own server: input register 30000 = 250 over 0..1000
    let reply = modbus(r.serve, &[4, 0x75, 0x30, 0, 1]).await;
    let raw = u16::from_be_bytes([reply[2], reply[3]]);
    assert!((i32::from(raw) - 16383).abs() <= 2, "{raw}");
    assert_eq!(modbus(r.serve, &[2, 0x27, 0x10, 0, 1]).await, [2, 1, 1]);
    // contiguous holding registers are one request; nothing was written down: every value came from a poll
    assert!(
        requests(&r.tap, 3).contains(&vec![3, 0x9c, 0x40, 0, 2]),
        "{:?}",
        requests(&r.tap, 3)
    );
    assert_eq!(requests(&r.tap, 6), Vec::<Vec<u8>>::new());
    assert_eq!(requests(&r.tap, 5), Vec::<Vec<u8>>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_to_the_feps_server_reaches_the_rtu_once_and_is_not_echoed() {
    let r = rig("").await;
    until("the first poll", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(true)
    })
    .await;
    // FC6 40000 = 0x8000 (500.0) and FC5 coil 0 on, to the FEP
    assert_eq!(
        modbus(r.serve, &[6, 0x9c, 0x40, 0x80, 0]).await,
        [6, 0x9c, 0x40, 0x80, 0]
    );
    assert_eq!(
        modbus(r.serve, &[5, 0, 0, 0xff, 0]).await,
        [5, 0, 0, 0xff, 0]
    );
    let rtu = &r.rtu.store;
    until("the write on the RTU", S(2), || {
        near(rtu.get_f64("setpoint"), 500.0) && rtu.get_bool("pump") == Some(true)
    })
    .await;
    // several polls later the RTU has still been written exactly once per point
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(requests(&r.tap, 6), [vec![6, 0x9c, 0x40, 0x80, 0]]);
    assert_eq!(requests(&r.tap, 5), [vec![5, 0, 0, 0xff, 0]]);
    assert!(near(r.fep.store.get_f64("setpoint"), 500.0));
    // a change made on the RTU itself comes up and is not written back either
    r.rtu.store.write("setpoint", Analog(250.0)).unwrap();
    until("the RTU's own change on the FEP", S(2), || {
        near(r.fep.store.get_f64("setpoint"), 250.0)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(requests(&r.tap, 6).len(), 1);
    // and the value sent before is sent again when it is wanted again
    r.fep.store.write("setpoint", Analog(500.0)).unwrap();
    until("the second write", S(2), || requests(&r.tap, 6).len() == 2).await;
    until("the second write on the RTU", S(2), || {
        near(rtu.get_f64("setpoint"), 500.0)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_rtu_turns_the_status_false_polling_backs_off_and_a_restart_recovers() {
    let (rtu_port, serve) = (free_port(), free_port());
    let mut rtu = device(&rtu_xml(rtu_port));
    listening(rtu_port, true).await;
    let fep = device(&fep_xml(rtu_port, serve, ""));
    until("up", S(3), || fep.store.get_bool("rtu-up") == Some(true)).await;
    rtu.store.write("level", Analog(100.0)).unwrap();
    until("level", S(2), || near(fep.store.get_f64("level"), 100.0)).await;
    rtu.stop();
    until("down", S(3), || fep.store.get_bool("rtu-up") == Some(false)).await;
    assert!(
        near(fep.store.get_f64("level"), 100.0),
        "a failed poll leaves the tags"
    );
    // while it is down, connection attempts come at 1 s, 2 s, ... apart, not every scan: count them on a listener that hangs up
    let l = tokio::net::TcpListener::bind(("127.0.0.1", rtu_port))
        .await
        .unwrap();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (a2, counting) = (attempts.clone(), tokio_util::sync::CancellationToken::new());
    let c2 = counting.clone();
    let counter = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = c2.cancelled() => return,
                r = l.accept() => { if r.is_ok() { a2.fetch_add(1, Ordering::SeqCst); } }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let n = attempts.load(Ordering::SeqCst);
    assert!(
        (1..=3).contains(&n),
        "{n} attempts in 2.5 s with a 0.1 s scan-rate"
    );
    counting.cancel();
    counter.await.unwrap();
    let rtu = device(&rtu_xml(rtu_port));
    rtu.store.write("level", Analog(300.0)).unwrap();
    until("recovery", S(10), || {
        fep.store.get_bool("rtu-up") == Some(true) && near(fep.store.get_f64("level"), 300.0)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_peer_fails_within_the_deadline_and_the_other_connection_keeps_polling() {
    // a listener that accepts and never answers
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = vec![];
        while let Ok((s, _)) = silent.accept().await {
            held.push(s);
        }
    });
    let second = format!(
        r#"<modbus-connection><endpoint>tcp://127.0.0.1:{silent_port}</endpoint><status-tag>silent-up</status-tag>
           <coil><address>0</address><tag>other</tag></coil></modbus-connection>"#
    );
    let r = rig(&second).await;
    until("the live connection", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(true)
    })
    .await;
    // the silent one never comes up; meanwhile the live one keeps taking new values within a scan or two
    for (i, v) in [100.0, 200.0, 300.0, 400.0].into_iter().enumerate() {
        r.rtu.store.write("level", Analog(v)).unwrap();
        until(
            "a fresh value from the live RTU",
            Duration::from_millis(700),
            || near(r.fep.store.get_f64("level"), v),
        )
        .await;
        assert_eq!(r.fep.store.get_bool("silent-up"), Some(false), "round {i}");
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    assert_eq!(r.fep.store.get_bool("rtu-up"), Some(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_while_the_rtu_is_down_is_kept_newest_only_and_sent_after_it_returns() {
    let mut r = rig("").await;
    until("up", S(3), || r.fep.store.get_bool("rtu-up") == Some(true)).await;
    r.rtu.stop();
    until("down", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(false)
    })
    .await;
    r.tap.clear();
    r.fep.store.write("setpoint", Analog(100.0)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.fep.store.write("setpoint", Analog(200.0)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.rtu = device(&rtu_xml(r.rtu_port));
    until("the newest value on the RTU", S(10), || {
        near(r.rtu.store.get_f64("setpoint"), 200.0)
    })
    .await;
    until("up again", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(true)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    // 200 over 0..1000 is 13107; 100 never left, and nothing was echoed afterwards
    assert_eq!(requests(&r.tap, 6), [vec![6, 0x9c, 0x40, 0x33, 0x33]]);
    assert!(near(r.fep.store.get_f64("setpoint"), 200.0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nan_is_never_sent_and_a_value_past_the_range_is_clamped() {
    let r = rig("").await;
    until("up", S(3), || r.fep.store.get_bool("rtu-up") == Some(true)).await;
    r.fep.store.write("setpoint", Analog(f64::NAN)).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        requests(&r.tap, 6),
        Vec::<Vec<u8>>::new(),
        "NaN is not sent"
    );
    r.fep.store.write("setpoint", Analog(1e9)).unwrap();
    until("the clamped write", S(2), || {
        !requests(&r.tap, 6).is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(requests(&r.tap, 6), [vec![6, 0x9c, 0x40, 0xff, 0xff]]);
    // the RTU holds the range's end; on the wire that is what was asked for, so the tag keeps the asked value
    until("the clamped value on the RTU", S(2), || {
        near(r.rtu.store.get_f64("setpoint"), 1000.0)
    })
    .await;
    assert_eq!(r.fep.store.get_f64("setpoint"), Some(1e9));
    r.fep.store.write("setpoint", Analog(-1e9)).unwrap();
    until("the low clamp", S(2), || requests(&r.tap, 6).len() == 2).await;
    assert_eq!(requests(&r.tap, 6)[1], [6, 0x9c, 0x40, 0, 0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn exception_02_on_a_merged_read_splits_that_block_from_the_next_poll_on() {
    // the FEP maps holding 40002, which the RTU lacks: the merged read 40000..40002 is refused with exception 2
    let more = r#"<modbus-connection><endpoint>tcp://127.0.0.1:PORT</endpoint><unit-id>1</unit-id><scan-rate>0.1</scan-rate>
       <status-tag>second-up</status-tag>
       <holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>a</tag></holding-register>
       <holding-register><address>40001</address><max-value>1000</max-value><min-value>0</min-value><tag>b</tag></holding-register>
       <holding-register><address>40002</address><max-value>1000</max-value><min-value>0</min-value><tag>missing</tag></holding-register>
       </modbus-connection>"#;
    let (rtu_port, serve) = (free_port(), free_port());
    let rtu = device(&rtu_xml(rtu_port));
    listening(rtu_port, true).await;
    let t = tap(rtu_port).await;
    let fep = device(&fep_xml(
        rtu_port,
        serve,
        &more.replace("PORT", &t.port.to_string()),
    ));
    rtu.store.write("setpoint", Analog(100.0)).unwrap();
    rtu.store.write("spare", Analog(200.0)).unwrap();
    until("the points that exist", S(3), || {
        near(fep.store.get_f64("a"), 100.0) && near(fep.store.get_f64("b"), 200.0)
    })
    .await;
    let reads = requests(&t, 3);
    assert_eq!(reads[0], [3, 0x9c, 0x40, 0, 3], "merged first");
    assert_eq!(
        reads.iter().filter(|r| r[3..] == [0, 3]).count(),
        1,
        "the merged read is not tried again: {reads:?}"
    );
    for addr in [0x40u8, 0x41, 0x42] {
        assert!(reads.contains(&vec![3, 0x9c, addr, 0, 1]), "{reads:?}");
    }
    // the device answers, so the session is up; the missing register is the config's problem
    assert_eq!(fep.store.get_bool("second-up"), Some(true));
    assert_eq!(fep.store.get_f64("missing"), Some(0.0));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_interface_in_fep_mode_reads_the_tag_and_writes_through_the_echo_rule() {
    let cmdif = free_port();
    let r = rig(&format!(
        "<command-interface>tcp://127.0.0.1:{cmdif}</command-interface>"
    ))
    .await;
    r.rtu.store.write("level", Analog(250.0)).unwrap();
    until("level", S(3), || near(r.fep.store.get_f64("level"), 250.0)).await;
    assert_eq!(
        shown(&command(cmdif, "QUERY=").await),
        "ACK=alarm,level,pump,setpoint,spare,\\x00"
    );
    let reply = shown(&command(cmdif, "READ=level").await);
    // 250 over 0..1000 is register 16383, which unscales to 249.9886
    assert_eq!(reply, "ACK=level:249.988556\\x00");
    assert_eq!(
        shown(&command(cmdif, "READ=pump").await),
        "ACK=pump:false\\x00"
    );
    assert_eq!(
        shown(&command(cmdif, "WRITE=setpoint:500").await),
        "ACK=Wrote tag setpoint -- 500\\x00"
    );
    assert_eq!(
        shown(&command(cmdif, "WRITE=pump:true").await),
        "ACK=Wrote tag pump -- true\\x00"
    );
    let rtu = &r.rtu.store;
    until("the writes on the RTU", S(2), || {
        near(rtu.get_f64("setpoint"), 500.0) && rtu.get_bool("pump") == Some(true)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(requests(&r.tap, 6).len(), 1);
    assert_eq!(requests(&r.tap, 5).len(), 1);
    // what the tag cannot take is refused, and so is an input
    for bad in [
        "WRITE=setpoint:true",
        "WRITE=pump:1",
        "WRITE=level:5",
        "WRITE=alarm:true",
    ] {
        let reply = shown(&command(cmdif, bad).await);
        assert!(
            reply.starts_with("ERR=Failed writing tag '"),
            "{bad}: {reply}"
        );
    }
    assert_eq!(requests(&r.tap, 6).len(), 1);
}

/// The gateway config in docs/field-device-clients.md loads as written, under `--strict` too.
#[test]
fn the_gateway_example_in_the_docs_loads() {
    use ferrofire::fd::config::{Config, Opts};
    let doc = include_str!("../docs/field-device-clients.md");
    let from = doc.find("<SCEPTRE>").expect("the example");
    let to = from + doc[from..].find("</SCEPTRE>").unwrap() + "</SCEPTRE>".len();
    // the server would open its default event log in the working directory
    let xml = doc[from..to].replace(
        "<dnp3-server>",
        "<dnp3-server><event-logging>/dev/null</event-logging>",
    );
    let cfg = Config::parse(&xml, ferrofire::fd::proto::REGISTRY, &Opts { strict: true }).unwrap();
    let names: Vec<_> = cfg.modules.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["modbus-client", "dnp3-server"]);
    assert_eq!(cfg.store.get("rtu-2-up"), Some(Binary(false)));
    assert_eq!(cfg.cycle_ms, 100);
}

// ---- the same over DNP3 -----------------------------------------------------------------------------------------------------

mod dnp3 {
    use super::*;
    use ferrofire::fd::proto::dnp3::link::{self, Parse};
    use ferrofire::fd::proto::dnp3::master::{self, Seen};

    const POINTS: &str = r#"
 <binary-input><address>2</address><tag>alarm</tag></binary-input>
 <binary-output><address>5</address><tag>pump</tag></binary-output>
 <analog-input><address>0</address><tag>level</tag></analog-input>
 <analog-output><address>1</address><tag>setpoint</tag><sbo>1</sbo></analog-output>"#;

    fn rtu_xml(port: u16) -> String {
        format!(
            r#"<SCEPTRE><field-device><name>rtu</name><cycle-time>20</cycle-time>
<tags>
 <internal-tag><name>level</name><value>0</value></internal-tag>
 <internal-tag><name>setpoint</name><value>0</value></internal-tag>
 <internal-tag><name>pump</name><status>false</status></internal-tag>
 <internal-tag><name>alarm</name><status>false</status></internal-tag>
</tags>
<comms><dnp3-server><endpoint>tcp://127.0.0.1:{port}</endpoint><address>10</address><event-logging>/dev/null</event-logging>{POINTS}
</dnp3-server></comms></field-device></SCEPTRE>"#
        )
    }

    /// A FEP: a `fep` mode master scanning `rtu` every second, a 40 ms cycle, and its own outstation on `serve` over the same
    /// tags. `more` goes into `<dnp3-client>`.
    fn fep_xml(rtu: u16, serve: u16, mode: &str, more: &str) -> String {
        format!(
            r#"<SCEPTRE><field-device><name>fep</name><cycle-time>40</cycle-time>
<comms><dnp3-client><address>1</address><scan-rate>1</scan-rate>{mode}
 <dnp3-connection><endpoint>tcp://127.0.0.1:{rtu}</endpoint><address>10</address><status-tag>rtu-up</status-tag>{POINTS}
 </dnp3-connection>{more}
</dnp3-client>
<dnp3-server><endpoint>tcp://127.0.0.1:{serve}</endpoint><address>10</address><event-logging>/dev/null</event-logging>{POINTS}
</dnp3-server></comms></field-device></SCEPTRE>"#
        )
    }

    struct Rig {
        rtu: Device,
        fep: Device,
        tap: Tap,
        rtu_port: u16,
        serve: u16,
    }

    async fn rig(more: &str) -> Rig {
        let (rtu_port, serve) = (free_port(), free_port());
        let rtu = device(&rtu_xml(rtu_port));
        listening(rtu_port, true).await;
        let tap = tap(rtu_port).await;
        let fep = device(&fep_xml(tap.port, serve, "<mode>fep</mode>", more));
        Rig {
            rtu,
            fep,
            tap,
            rtu_port,
            serve,
        }
    }

    /// The application fragments (function code first, the control octet dropped) the FEP's master sent through the tap.
    fn requests(t: &Tap) -> Vec<Vec<u8>> {
        let mut buf: Vec<u8> = t.sent().concat();
        let mut out = vec![];
        while let Parse::Frame(f, used) = link::parse(&buf) {
            out.push(f.user[2..].to_vec());
            buf.drain(..used);
        }
        out
    }

    /// The control requests among them: select (3), operate (4), direct operate (5).
    fn controls(t: &Tap) -> Vec<Vec<u8>> {
        requests(t)
            .into_iter()
            .filter(|r| (3..=5).contains(&r[0]))
            .collect()
    }

    /// One class 0 read of the outstation on `port`, as a master at address 1: the points in the response.
    async fn class0(port: u16) -> Vec<Seen> {
        listening(port, true).await;
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(&link::encode(
            master::MASTER_DATA,
            10,
            1,
            &[0xC0, 0xC0, 1, 60, 1, 6],
        ))
        .await
        .unwrap();
        let mut buf = vec![];
        loop {
            let mut chunk = [0u8; 512];
            // an outstation serves one master at a time: a connection made while the last one is still being let go is closed
            let Ok(Ok(n @ 1..)) = tokio::time::timeout(S(2), s.read(&mut chunk)).await else {
                return vec![];
            };
            buf.extend_from_slice(&chunk[..n]);
            if let Parse::Frame(f, _) = link::parse(&buf) {
                return master::decode(&f.user[1..]).unwrap().seen;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dnp3_gateway_a_value_on_the_rtu_appears_on_the_feps_own_outstation() {
        let r = rig("").await;
        assert_eq!(
            r.fep.store.get_bool("rtu-up"),
            Some(false),
            "false before the first connection"
        );
        r.rtu.store.write("level", Analog(250.5)).unwrap();
        r.rtu.store.write("alarm", Binary(true)).unwrap();
        r.rtu.store.write("setpoint", Analog(42.0)).unwrap();
        let f = &r.fep.store;
        until("the scanned values", S(6), || {
            f.get_f64("level") == Some(250.5)
                && f.get_bool("alarm") == Some(true)
                && f.get_f64("setpoint") == Some(42.0)
        })
        .await;
        assert_eq!(f.get_bool("rtu-up"), Some(true));
        // the FEP's outstation refreshes its database from the tags every 100 ms
        let mut seen = vec![];
        for _ in 0..20 {
            seen = class0(r.serve).await;
            if seen.contains(&Seen::Analog(0, 250.5)) && seen.contains(&Seen::Binary(2, true)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            seen.contains(&Seen::Analog(0, 250.5)) && seen.contains(&Seen::Binary(2, true)),
            "{seen:?}"
        );
        // the scan results came in through polls: nothing was commanded
        assert_eq!(controls(&r.tap), Vec::<Vec<u8>>::new());
        // fep mode also reads the output status, after each scan
        let status = [&[1u8][..], &master::output_status()].concat();
        assert!(requests(&r.tap).contains(&status), "{:?}", requests(&r.tap));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dnp3_bennu_mode_never_reads_the_output_status() {
        let (rtu_port, serve) = (free_port(), free_port());
        let _rtu = device(&rtu_xml(rtu_port));
        listening(rtu_port, true).await;
        let t = tap(rtu_port).await;
        let xml = fep_xml(t.port, serve, "", "").replace("<status-tag>rtu-up</status-tag>", "");
        let _fep = device(&xml);
        until("two scans", S(6), || {
            requests(&t).iter().filter(|r| r.len() == 13).count() >= 2
        })
        .await;
        assert!(
            requests(&t)
                .iter()
                .all(|r| !r.ends_with(&master::output_status())),
            "{:?}",
            requests(&t)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dnp3_a_local_change_is_commanded_once_and_not_echoed() {
        let r = rig("").await;
        until("up", S(6), || r.fep.store.get_bool("rtu-up") == Some(true)).await;
        r.fep.store.write("pump", Binary(true)).unwrap();
        // whole numbers: our outstation reports an analog output back as a 32-bit integer (g30v1)
        r.fep.store.write("setpoint", Analog(43.0)).unwrap();
        let rtu = &r.rtu.store;
        until("the commands on the RTU", S(3), || {
            rtu.get_bool("pump") == Some(true) && rtu.get_f64("setpoint") == Some(43.0)
        })
        .await;
        // three scans later: one direct operate (the CROB), one select and one operate (the sbo analog output), no more
        tokio::time::sleep(Duration::from_millis(3200)).await;
        let c = controls(&r.tap);
        let crob = [&[5u8][..], &master::crob(5, true)].concat();
        let ao = master::ao_f32(1, 43.0);
        let (select, operate) = ([&[3u8][..], &ao].concat(), [&[4u8][..], &ao].concat());
        assert_eq!(c.len(), 3, "{c:?}");
        assert!(
            c.contains(&crob) && c.contains(&select) && c.contains(&operate),
            "{c:?}"
        );
        assert_eq!(r.fep.store.get_f64("setpoint"), Some(43.0));
        // a change made on the RTU comes up and is not commanded back
        r.rtu.store.write("setpoint", Analog(7.0)).unwrap();
        until("the RTU's own change", S(4), || {
            r.fep.store.get_f64("setpoint") == Some(7.0)
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(controls(&r.tap).len(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dnp3_a_stopped_outstation_turns_the_status_false_and_a_restart_recovers() {
        let mut r = rig("").await;
        r.rtu.store.write("level", Analog(100.0)).unwrap();
        until("up", S(6), || {
            r.fep.store.get_bool("rtu-up") == Some(true)
                && r.fep.store.get_f64("level") == Some(100.0)
        })
        .await;
        r.rtu.stop();
        until("down", S(4), || {
            r.fep.store.get_bool("rtu-up") == Some(false)
        })
        .await;
        assert_eq!(
            r.fep.store.get_f64("level"),
            Some(100.0),
            "the tags keep their last values"
        );
        let before = r.tap.accepted.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let attempts = r.tap.accepted.load(Ordering::SeqCst) - before;
        assert!(attempts <= 3, "{attempts} connection attempts in 2.5 s");
        r.rtu = device(&rtu_xml(r.rtu_port));
        r.rtu.store.write("level", Analog(300.0)).unwrap();
        until("recovery", S(20), || {
            r.fep.store.get_bool("rtu-up") == Some(true)
                && r.fep.store.get_f64("level") == Some(300.0)
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dnp3_a_silent_outstation_never_comes_up_and_the_other_connection_keeps_scanning() {
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = vec![];
            while let Ok((s, _)) = silent.accept().await {
                held.push(s);
            }
        });
        let second = format!(
            r#"<dnp3-connection><endpoint>tcp://127.0.0.1:{silent_port}</endpoint><address>11</address><status-tag>silent-up</status-tag>
               <binary-input><address>0</address><tag>other</tag></binary-input></dnp3-connection>"#
        );
        let r = rig(&second).await;
        until("the live connection", S(6), || {
            r.fep.store.get_bool("rtu-up") == Some(true)
        })
        .await;
        for v in [100.0, 200.0, 300.0] {
            r.rtu.store.write("level", Analog(v)).unwrap();
            until(
                "a fresh value from the live RTU",
                Duration::from_millis(2500),
                || r.fep.store.get_f64("level") == Some(v),
            )
            .await;
            assert_eq!(r.fep.store.get_bool("silent-up"), Some(false));
        }
        assert_eq!(r.fep.store.get_bool("rtu-up"), Some(true));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dnp3_a_change_while_the_outstation_is_down_is_sent_newest_only_after_it_returns() {
        let mut r = rig("").await;
        until("up", S(6), || r.fep.store.get_bool("rtu-up") == Some(true)).await;
        r.rtu.stop();
        until("down", S(4), || {
            r.fep.store.get_bool("rtu-up") == Some(false)
        })
        .await;
        r.tap.clear();
        r.fep.store.write("setpoint", Analog(1.0)).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        r.fep.store.write("setpoint", Analog(2.0)).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        r.rtu = device(&rtu_xml(r.rtu_port));
        until("the newest value on the RTU", S(20), || {
            r.rtu.store.get_f64("setpoint") == Some(2.0)
        })
        .await;
        until("up again", S(4), || {
            r.fep.store.get_bool("rtu-up") == Some(true)
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let ao = master::ao_f32(1, 2.0);
        assert_eq!(
            controls(&r.tap),
            [[&[3u8][..], &ao].concat(), [&[4u8][..], &ao].concat()],
            "1.0 never left"
        );
        assert_eq!(r.fep.store.get_f64("setpoint"), Some(2.0));
    }
}

/// Final review, finding 4: `<logic>` (here: the test) keeps a setpoint the register cannot carry exactly. It goes down once,
/// the tag keeps the wanted value, and nothing more is written however often it is assigned.
#[tokio::test(flavor = "multi_thread")]
async fn a_setpoint_off_the_register_grid_is_written_once_and_the_tag_does_not_flap() {
    let r = rig("").await;
    until("up", S(3), || r.fep.store.get_bool("rtu-up") == Some(true)).await;
    let end = tokio::time::Instant::now() + Duration::from_millis(1500);
    let mut other = 0;
    while tokio::time::Instant::now() < end {
        match r.fep.store.get_f64("setpoint") {
            Some(3.3333) => {}
            _ => {
                other += 1;
                r.fep.store.write("setpoint", Analog(3.3333)).unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // 3.3333 over 0..1000 is register 218
    assert_eq!(requests(&r.tap, 6), [vec![6, 0x9c, 0x40, 0, 218]]);
    assert!(other <= 12, "the tag left the wanted value {other} times");
    assert_eq!(r.fep.store.get_f64("setpoint"), Some(3.3333));
}

const REGS3: &str = r#"<modbus-connection><endpoint>tcp://127.0.0.1:PORT</endpoint><unit-id>1</unit-id><scan-rate>0.1</scan-rate>
       <holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>a</tag></holding-register>
       <holding-register><address>40001</address><max-value>1000</max-value><min-value>0</min-value><tag>b</tag></holding-register>
       <holding-register><address>40002</address><max-value>1000</max-value><min-value>0</min-value><tag>missing</tag></holding-register>
       </modbus-connection>"#;

#[tokio::test(flavor = "multi_thread")]
async fn outputs_changed_in_one_cycle_go_down_as_one_fc16_and_fall_back_on_exception_02() {
    let (rtu_port, serve) = (free_port(), free_port());
    let rtu = device(&rtu_xml(rtu_port));
    listening(rtu_port, true).await;
    let t = tap(rtu_port).await;
    // setpoint and spare (40000, 40001) are also tags of the main connection; a, b, missing are a second view of 40000..40002
    let fep = device(&fep_xml(
        rtu_port,
        serve,
        &REGS3.replace("PORT", &t.port.to_string()),
    ));
    until("the first poll", S(3), || fep.store.get_f64("a").is_some()).await;
    fep.store.write("a", Analog(100.0)).unwrap();
    fep.store.write("b", Analog(200.0)).unwrap();
    fep.store.write("missing", Analog(300.0)).unwrap();
    // the RTU keeps the registers before the unmapped one, though it answers 2: wait for the fallback, not for the values
    until("the fallback to single writes", S(4), || {
        requests(&t, 6).len() >= 2
    })
    .await;
    assert!(near(rtu.store.get_f64("setpoint"), 100.0) && near(rtu.store.get_f64("spare"), 200.0));
    let multi = requests(&t, 16);
    // merged first: 40000 x3, 100.0 / 200.0 / 300.0 over 0..1000 as registers; refused (the RTU lacks 40002), then alone
    assert_eq!(multi.len(), 1, "{multi:?}");
    assert_eq!(multi[0][..6], [16, 0x9c, 0x40, 0, 3, 6]);
    let singles = requests(&t, 6);
    for addr in [0x40u8, 0x41] {
        assert!(singles.iter().any(|r| r[2] == addr), "{singles:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_outputs_with_a_gap_between_them_are_two_single_writes() {
    let r = rig("").await;
    until("the first poll", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(true)
    })
    .await;
    // setpoint 40000 and coil 0: different tables, so never one request
    r.fep.store.write("setpoint", Analog(500.0)).unwrap();
    r.fep.store.write("pump", Binary(true)).unwrap();
    until("both on the RTU", S(3), || {
        near(r.rtu.store.get_f64("setpoint"), 500.0) && r.rtu.store.get_bool("pump") == Some(true)
    })
    .await;
    assert_eq!(requests(&r.tap, 16).len() + requests(&r.tap, 15).len(), 0);
}
