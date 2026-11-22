//! `<iec60870-5-104-client>` in `fep` mode, whole devices in one process: a FEP device polls an RTU device (ferrofire's own 104
//! server) with a GI every `<scan-rate>`, keeps the RTU's points in its tags, and writes local changes down as direct-execute
//! commands, never writing a polled value back (docs/field-device-clients.md, "`fep` mode"). A relay between the two (`Tap`) shows
//! what the FEP sent.

mod common;

use common::clients::*;
use ferrofire::fd::config::{Config, Opts};
use ferrofire::fd::proto::client::Mode;
use ferrofire::fd::proto::iec104::apci::{Decoder, Frame};
use ferrofire::fd::proto::iec104::asdu::{decode, Asdu, Item};
use ferrofire::fd::proto::iec104::client::{Conn, Master};
use ferrofire::fd::proto::iec104::server::Point;
use ferrofire::fd::proto::REGISTRY;
use ferrofire::model::Kind;
use ferrofire::model::Value::{Analog, Binary};
use std::sync::atomic::Ordering;
use std::time::Duration;

const S: fn(u64) -> Duration = Duration::from_secs;

/// An RTU: internal tags behind our 104 server, so a test sets "field values" with `store.write`. IOA 5 is an input there and an
/// output on the FEP: the RTU refuses a command to it.
fn rtu_xml(port: u16, subtype: &str) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>rtu</name><cycle-time>20</cycle-time>
<tags>
 <internal-tag><name>level</name><value>0</value></internal-tag>
 <internal-tag><name>setpoint</name><value>0</value></internal-tag>
 <internal-tag><name>pump</name><status>false</status></internal-tag>
 <internal-tag><name>alarm</name><status>false</status></internal-tag>
 <internal-tag><name>trip</name><status>false</status></internal-tag>
</tags>
<comms><iec60870-5-104-server><rpoll-rate>1</rpoll-rate><subtype>{subtype}</subtype>
 <endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging>
 <binary-input><address>100</address><tag>alarm</tag></binary-input>
 <binary-input><address>5</address><tag>trip</tag></binary-input>
 <binary-output><address>0</address><tag>pump</tag></binary-output>
 <analog-input><address>102</address><tag>level</tag></analog-input>
 <analog-output><address>2</address><tag>setpoint</tag></analog-output>
</iec60870-5-104-server></comms></field-device></SCEPTRE>"#
    )
}

/// A FEP: a GI every 100 ms, a 40 ms cycle.
fn fep_xml(rtu: u16) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>fep</name><cycle-time>40</cycle-time>
<comms><iec60870-5-104-client><mode>fep</mode>
 <iec60870-5-104-connection><endpoint>tcp://127.0.0.1:{rtu}</endpoint><scan-rate>0.1</scan-rate><status-tag>rtu-up</status-tag>
 <binary-input><address>100</address><tag>alarm</tag></binary-input>
 <binary-output><address>5</address><tag>trip</tag></binary-output>
 <binary-output><address>0</address><tag>pump</tag></binary-output>
 <analog-input><address>102</address><tag>level</tag></analog-input>
 <analog-output><address>2</address><tag>setpoint</tag></analog-output>
 </iec60870-5-104-connection>
</iec60870-5-104-client></comms></field-device></SCEPTRE>"#
    )
}

/// The ASDUs the FEP sent through the tap, in order.
fn asdus(t: &Tap) -> Vec<Asdu> {
    let mut d = Decoder::default();
    d.push(&t.sent().concat());
    let mut out = vec![];
    while let Ok(Some(f)) = d.next() {
        if let Frame::I { asdu, .. } = f {
            out.extend(decode(&asdu).ok());
        }
    }
    out
}

/// The commands among them (45, 46, 50).
fn commands(t: &Tap) -> Vec<Item> {
    let cmd = |a: &Asdu| [45, 46, 50].contains(&a.head.type_id);
    asdus(t)
        .into_iter()
        .filter(cmd)
        .flat_map(|a| a.items)
        .collect()
}

fn setpoints(t: &Tap) -> Vec<f32> {
    let v = |i: Item| match i {
        Item::SetFloat { v, select, .. } if !select => Some(v),
        _ => None,
    };
    commands(t).into_iter().filter_map(v).collect()
}

struct Rig {
    rtu: Device,
    fep: Device,
    tap: Tap,
    rtu_port: u16,
}

async fn rig(subtype: &str) -> Rig {
    let rtu_port = free_port();
    let rtu = device(&rtu_xml(rtu_port, subtype));
    listening(rtu_port, true).await;
    let tap = tap(rtu_port).await;
    let fep = device(&fep_xml(tap.port));
    Rig {
        rtu,
        fep,
        tap,
        rtu_port,
    }
}

async fn up(r: &Rig) {
    until("the first GI", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(true)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_value_on_the_rtu_appears_in_the_feps_tag() {
    let r = rig("single").await;
    r.rtu.store.write("level", Analog(250.5)).unwrap();
    r.rtu.store.write("alarm", Binary(true)).unwrap();
    r.rtu.store.write("setpoint", Analog(42.0)).unwrap();
    r.rtu.store.write("pump", Binary(true)).unwrap();
    let f = &r.fep.store;
    until("the GI's values", S(3), || {
        f.get_f64("level") == Some(250.5)
            && f.get_bool("alarm") == Some(true)
            && f.get_f64("setpoint") == Some(42.0)
            && f.get_bool("pump") == Some(true)
    })
    .await;
    assert_eq!(f.get_bool("rtu-up"), Some(true));
    // a new value comes with the next GI; every value came from a poll, so nothing was commanded
    r.rtu.store.write("level", Analog(7.25)).unwrap();
    until("the next GI", S(2), || f.get_f64("level") == Some(7.25)).await;
    let gis = asdus(&r.tap)
        .iter()
        .filter(|a| a.head.type_id == 100)
        .count();
    assert!(gis >= 2, "{gis} GIs");
    assert_eq!(commands(&r.tap), vec![]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_change_is_written_down_once_and_not_echoed() {
    let r = rig("single").await;
    up(&r).await;
    r.fep.store.write("setpoint", Analog(500.0)).unwrap();
    r.fep.store.write("pump", Binary(true)).unwrap();
    let rtu = &r.rtu.store;
    until("the writes on the RTU", S(2), || {
        rtu.get_f64("setpoint") == Some(500.0) && rtu.get_bool("pump") == Some(true)
    })
    .await;
    // several GIs later each point was commanded once, direct execute; a single-point server gets C_SC_NA_1
    tokio::time::sleep(Duration::from_millis(600)).await;
    let want = vec![
        Item::SetFloat {
            ioa: 2,
            v: 500.0,
            select: false,
            ql: 0,
        },
        Item::SingleCmd {
            ioa: 0,
            on: true,
            select: false,
            qu: 0,
        },
    ];
    let mut got = commands(&r.tap);
    got.sort_by_key(|i| matches!(i, Item::SingleCmd { .. }));
    assert_eq!(got, want);
    // a change made on the RTU comes up and is not written back
    rtu.write("setpoint", Analog(250.0)).unwrap();
    until("the RTU's own change on the FEP", S(2), || {
        r.fep.store.get_f64("setpoint") == Some(250.0)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(setpoints(&r.tap), [500.0]);
    // and the value sent before goes again when it is wanted again
    r.fep.store.write("setpoint", Analog(500.0)).unwrap();
    until("the second write", S(2), || setpoints(&r.tap).len() == 2).await;
    until("the second write on the RTU", S(2), || {
        rtu.get_f64("setpoint") == Some(500.0)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_tag_follows_the_session() {
    let mut r = rig("single").await;
    assert_eq!(
        r.fep.store.get_bool("rtu-up"),
        Some(false),
        "false before the first GI"
    );
    up(&r).await;
    r.rtu.store.write("level", Analog(100.0)).unwrap();
    until("level", S(2), || {
        r.fep.store.get_f64("level") == Some(100.0)
    })
    .await;
    r.rtu.stop();
    until("down", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(false)
    })
    .await;
    assert_eq!(
        r.fep.store.get_f64("level"),
        Some(100.0),
        "a lost session leaves the tags"
    );
    r.rtu = device(&rtu_xml(r.rtu_port, "single"));
    r.rtu.store.write("level", Analog(300.0)).unwrap();
    until("up again", S(10), || {
        r.fep.store.get_bool("rtu-up") == Some(true) && r.fep.store.get_f64("level") == Some(300.0)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_command_is_not_repeated() {
    let r = rig("single").await;
    up(&r).await;
    // IOA 5 is an input on the RTU: negative ACT_CON
    r.fep.store.write("trip", Binary(true)).unwrap();
    until("the command", S(2), || !commands(&r.tap).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        commands(&r.tap),
        vec![Item::SingleCmd {
            ioa: 5,
            on: true,
            select: false,
            qu: 0
        }]
    );
    assert_eq!(r.rtu.store.get_bool("trip"), Some(false));
    // a later GI puts the RTU's value back, and that is not sent down either
    until("the RTU's value", S(2), || {
        r.fep.store.get_bool("trip") == Some(false)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(commands(&r.tap).len(), 1);
    assert_eq!(r.fep.store.get_bool("rtu-up"), Some(true));
}

fn fep_master() -> Master {
    let point = |addr, tag: &str, kind| Point {
        addr,
        tag: tag.into(),
        kind,
        output: true,
    };
    let conn = Conn {
        endpoint: "tcp://127.0.0.1:1".into(),
        points: vec![point(0, "pump", Kind::Binary), point(2, "sp", Kind::Analog)],
        fep: None,
    };
    Master::new(&conn, Mode::Fep, false)
}

#[tokio::test(flavor = "multi_thread")]
async fn double_mode_server_gets_c_dc() {
    // the type follows the binaries of the last GI: C_SC_NA_1 before any
    let mut m = fep_master();
    let type_of = |m: &Master| m.command("pump", Binary(true)).unwrap().head.type_id;
    assert_eq!(type_of(&m), 45);
    let gi = |t: u8| decode(&[t, 1, 20, 0, 1, 0, 0, 0, 0, 2]).unwrap();
    m.on_asdu(&gi(3));
    assert_eq!(type_of(&m), 46);
    m.on_asdu(&gi(1));
    assert_eq!(type_of(&m), 45);
    // a spontaneous double point is not the GI
    m.on_asdu(&decode(&[3, 1, 3, 0, 1, 0, 0, 0, 0, 2]).unwrap());
    assert_eq!(type_of(&m), 45);
    // bennu mode is stock's: always C_DC_NA_1
    let conn = Conn {
        endpoint: "tcp://127.0.0.1:1".into(),
        points: vec![Point {
            addr: 0,
            tag: "pump".into(),
            kind: Kind::Binary,
            output: true,
        }],
        fep: None,
    };
    let mut bennu = Master::new(&conn, Mode::Bennu, false);
    bennu.on_asdu(&gi(1));
    assert_eq!(type_of(&bennu), 46);

    let r = rig("double").await;
    up(&r).await;
    r.fep.store.write("pump", Binary(true)).unwrap();
    until("the write on the RTU", S(2), || {
        r.rtu.store.get_bool("pump") == Some(true)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        commands(&r.tap),
        vec![Item::DoubleCmd {
            ioa: 0,
            dcs: 2,
            select: false,
            qu: 0
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nan_is_never_sent() {
    let r = rig("single").await;
    up(&r).await;
    r.fep.store.write("setpoint", Analog(f64::NAN)).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(setpoints(&r.tap), Vec::<f32>::new(), "NaN is not sent");
    r.fep.store.write("setpoint", Analog(7.0)).unwrap();
    until("the next value", S(2), || !setpoints(&r.tap).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(setpoints(&r.tap), [7.0]);
}

/// Review Focus 5: an RTU that is away is tried 1, 2, 4 ... s apart; local changes made meanwhile are not queued one by one,
/// the newest value of each goes down when it is back, before the GI.
#[tokio::test(flavor = "multi_thread")]
async fn rtu_away_backs_off_and_sends_the_newest_on_return() {
    let mut r = rig("single").await;
    up(&r).await;
    r.rtu.stop();
    until("down", S(3), || {
        r.fep.store.get_bool("rtu-up") == Some(false)
    })
    .await;
    // the tap takes each attempt and hangs up at once: count them
    let before = r.tap.accepted.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let n = r.tap.accepted.load(Ordering::SeqCst) - before;
    assert!(
        (1..=3).contains(&n),
        "{n} attempts in 2.5 s with a 0.1 s scan-rate"
    );
    r.tap.clear();
    r.fep.store.write("setpoint", Analog(100.0)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.fep.store.write("setpoint", Analog(200.0)).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.rtu = device(&rtu_xml(r.rtu_port, "single"));
    until("the newest value on the RTU", S(10), || {
        r.rtu.store.get_f64("setpoint") == Some(200.0)
    })
    .await;
    up(&r).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(setpoints(&r.tap), [200.0], "100 never left");
    let types: Vec<u8> = asdus(&r.tap).iter().map(|a| a.head.type_id).collect();
    assert_eq!(types[..2], [50, 100], "the write before the GI: {types:?}");
    assert_eq!(r.fep.store.get_f64("setpoint"), Some(200.0));
}

/// An RTU that stopped while its host stays up refuses the connect (no tap in between): after a good session the first retry is
/// at once, the next ones 1 s and 2 s apart, not in a tight loop.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_reconnect_after_a_good_session_backs_off() {
    let rtu_port = free_port();
    let mut rtu = device(&rtu_xml(rtu_port, "single"));
    listening(rtu_port, true).await;
    let fep = device(&fep_xml(rtu_port));
    until("up", S(3), || fep.store.get_bool("rtu-up") == Some(true)).await;
    rtu.stop();
    until("down", S(3), || fep.store.get_bool("rtu-up") == Some(false)).await;
    // refused at once and 1 s later; the next attempt is 3 s after the loss
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let l = tokio::net::TcpListener::bind(("127.0.0.1", rtu_port))
        .await
        .unwrap();
    let early = tokio::time::timeout(Duration::from_millis(700), l.accept()).await;
    assert!(early.is_err(), "a connect came within 2.2 s of the loss");
    let late = tokio::time::timeout(S(5), l.accept()).await;
    assert!(late.is_ok(), "no connect after the backoff");
}

#[test]
fn bennu_mode_scan_rate_is_a_warning_and_an_error_under_strict() {
    let sample = include_str!("../conformance/stock/client-iec104.xml");
    let strict = Opts {
        strict: true,
        ..Opts::default()
    };
    for key in ["<scan-rate>0.5</scan-rate>", "<status-tag>up</status-tag>"] {
        let xml = sample.replace(
            "<iec60870-5-104-connection>",
            &format!("<iec60870-5-104-connection>{key}"),
        );
        let cfg = Config::parse(&xml, REGISTRY, &Opts::default()).unwrap();
        assert_eq!(cfg.store.get("up"), None, "{key}: not declared");
        let e = Config::parse(&xml, REGISTRY, &strict).err().expect(key).0;
        assert!(e.contains("is fep-only, ignored"), "{key}: {e}");
    }
    // in fep mode both count, the status tag is declared false
    let fep = sample
        .replace(
            "<iec60870-5-104-client>",
            "<iec60870-5-104-client><mode>fep</mode>",
        )
        .replace(
            "<iec60870-5-104-connection>",
            "<iec60870-5-104-connection><scan-rate>0.5</scan-rate><status-tag>up</status-tag>",
        );
    let cfg = Config::parse(&fep, REGISTRY, &strict).unwrap();
    assert_eq!(cfg.store.get("up"), Some(Binary(false)));
}

/// By hand, against the stock server image (the conformance sample in double-point mode, its provider feeding it):
///
/// ```sh
/// docker run -d --rm --name ff-104-stock -v $PWD/conformance:/c:ro ffc-stock-2026.11.03:22.04 bash -c \
///   "cd /tmp && { python3 /c/bacnet/feed.py /tmp/provider.log 'bus-1.gen_mw:300.7,load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,load-1_bus-1.active:false,' & } \
///    && LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f /c/stock/server-iec104.xml"
/// docker run --rm --network container:ff-104-stock -v $PWD:/src -w /src -e CARGO_TARGET_DIR=/src/target-linux ff-rust:22.04 \
///   cargo test --test iec104_fep -- --ignored --nocapture
/// docker stop ff-104-stock
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the stock server image beside it (see the doc comment)"]
async fn against_the_stock_server() {
    let t = tap(2404).await;
    let xml = include_str!("../conformance/stock/client-iec104.xml")
        .replace(
            "tcp://127.0.0.1:2404",
            &format!("tcp://127.0.0.1:{}", t.port),
        )
        .replace(
            "<iec60870-5-104-client>",
            "<iec60870-5-104-client><mode>fep</mode>",
        )
        .replace(
            "<iec60870-5-104-connection>",
            "<iec60870-5-104-connection><scan-rate>0.5</scan-rate><status-tag>up</status-tag>",
        )
        .replace(
            "<command-interface>tcp://127.0.0.1:1330</command-interface>",
            "",
        );
    let fep = device(&xml);
    let f = &fep.store;
    until("the stock server's values", S(10), || {
        f.get_bool("up") == Some(true)
            && f.get_f64("I_bus-1.gen_mw") == Some(f64::from(300.7f32))
            && f.get_f64("I_load-1_bus-1.mw") == Some(250.5)
            && f.get_bool("I_generator-1_bus-1.active") == Some(true)
    })
    .await;
    f.write("O_bus-1.gen_mw", Analog(42.5)).unwrap();
    f.write("O_load-1_bus-1.active", Binary(true)).unwrap();
    // the provider logs each WRITE it gets from the stock server
    let mut log = String::new();
    for _ in 0..50 {
        log = shown(&command(5555, "LOG=").await);
        if log.contains("bus-1.gen_mw") && log.contains("load-1_bus-1.active") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        log.contains("bus-1.gen_mw") && log.contains("load-1_bus-1.active"),
        "{log}"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    // stock is in double-point mode: C_DC_NA_1, and each write went once
    let mut got = commands(&t);
    got.sort_by_key(|i| matches!(i, Item::DoubleCmd { .. }));
    assert_eq!(
        got,
        vec![
            Item::SetFloat {
                ioa: 2,
                v: 42.5,
                select: false,
                ql: 0
            },
            Item::DoubleCmd {
                ioa: 1,
                dcs: 2,
                select: false,
                qu: 0
            },
        ]
    );
    assert_eq!(f.get_bool("up"), Some(true));
}
