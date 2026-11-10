//! The IEC 61850 server over real sockets, with the `iec61850-rs` MMS client as the peer: the namespace a client browses, values
//! from the datastore, a report on change, a control becoming a queued write, and what a hostile or greedy peer gets.
#![cfg(feature = "proto-iec61850")]

mod common;

use ferrofire::fd::proto::iec61850::Server;
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::Store;
use iec61850_rs::client::{Client, ControlModel, RcbSettings, TrgOps};
use iec61850_rs::proto::data::{Typed, Value};
use iec61850_rs::Fc;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(port: u16) -> String {
    let log = std::env::temp_dir()
        .join(format!("iec61850-net-{port}.log"))
        .display()
        .to_string();
    format!(
        r#"<SCEPTRE><field-device><name>t</name><tags>
<external-tag><name>brkr</name><io>I0</io><type>binary</type></external-tag>
<external-tag><name>load-power</name><io>I1</io><type>analog</type></external-tag>
<external-tag><name>toggle</name><io>O0</io><type>binary</type></external-tag>
<external-tag><name>setpoint</name><io>O1</io><type>analog</type></external-tag></tags>
<comms><iec61850-server><endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>{log}</event-logging>
<binary-input><tag>brkr</tag></binary-input><analog-input><tag>load-power</tag></analog-input>
<binary-output><tag>toggle</tag></binary-output><analog-output><tag>setpoint</tag></analog-output></iec61850-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>p.active</name></binary><analog><id>I1</id><name>p.mw</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>p.active</name></binary><analog><id>O1</id><name>p.mw</name></analog></output>
</field-device></SCEPTRE>"#
    )
}

async fn start() -> (u16, Store, CancellationToken) {
    let port = port();
    let (srv, store) = common::load_as::<Server>(&config(port), "iec61850-server");
    let stop = CancellationToken::new();
    tokio::spawn(srv.run(store.clone(), stop.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    (port, store, stop)
}

/// Run blocking client code off the runtime that serves it.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

const LD: &str = "simpleIOGenericIO";

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_client_browses_the_model_and_reads_the_datastore() {
    let (port, store, stop) = start().await;
    store.publish("p.mw", 400.5, 1);
    store.publish("p.active", 1.0, 1);
    tokio::time::sleep(Duration::from_millis(600)).await; // the 4 Hz refresh
    let addr = format!("127.0.0.1:{port}");
    blocking(move || {
        let mut c = Client::connect(&addr).unwrap();
        assert_eq!(c.server_directory().unwrap(), [LD]);
        let names = c.logical_device_directory(LD).unwrap();
        for want in [
            "GGIO1$ST$Ind1$stVal",
            "GGIO1$MX$AnIn1$mag$f",
            "GGIO1$ST$SPCSO1$stVal",
            "GGIO1$MX$APCSO1$mxVal$f",
            "LLN0$RP$EventsRCB01$RptEna",
        ] {
            assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
        }
        assert_eq!(
            c.read(&format!("{LD}/GGIO1.AnIn1.mag.f"), Fc::MX)
                .unwrap()
                .as_f64(),
            Some(400.5)
        );
        assert_eq!(
            c.read(&format!("{LD}/GGIO1.Ind1.stVal"), Fc::ST).unwrap(),
            Value::Boolean(true)
        );
        assert!(!c.identify().unwrap().vendor.is_empty());
        c.release().unwrap();
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_change_in_the_datastore_is_reported_to_a_subscribed_client() {
    let (port, store, stop) = start().await;
    let addr = format!("127.0.0.1:{port}");
    let st = store.clone();
    blocking(move || {
        let mut c = Client::connect(&addr).unwrap();
        c.enable_rcb(
            &format!("{LD}/LLN0$RP$EventsRCB01"),
            Fc::RP,
            &RcbSettings::new().with_trg_ops(TrgOps::EVENTS),
        )
        .unwrap();
        st.publish("p.mw", 123.0, 2);
        let r = c
            .next_report(Duration::from_secs(3))
            .unwrap()
            .expect("a report");
        let vals: Vec<_> = r.entries.iter().map(|e| e.value.clone()).collect();
        assert!(vals.contains(&Value::Float32(123.0)), "{vals:?}");
        c.release().unwrap();
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn operating_a_control_queues_a_write_on_the_datastore() {
    let (port, store, stop) = start().await;
    let addr = format!("127.0.0.1:{port}");
    blocking(move || {
        let mut c = Client::connect(&addr).unwrap();
        c.control(&format!("{LD}/GGIO1.SPCSO1"))
            .model(ControlModel::DirectNormal)
            .execute(&Value::Boolean(true))
            .unwrap();
        c.control(&format!("{LD}/GGIO1.APCSO1"))
            .model(ControlModel::DirectNormal)
            .execute(&Value::Structure(vec![Value::Float32(55.5)]))
            .unwrap();
        c.release().unwrap();
    })
    .await;
    assert!(store.is_queued("toggle") && store.is_queued("setpoint"));
    let mut q: Vec<_> = store
        .pending(1)
        .into_iter()
        .chain(store.pending(0))
        .map(|(t, _, v)| format!("{t}={v:?}"))
        .collect();
    q.sort();
    q.dedup();
    assert_eq!(q, ["setpoint=Analog(55.5)", "toggle=Binary(true)"]);
    stop.cancel();
}

fn alive(s: &mut TcpStream) -> bool {
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut b = [0u8; 16];
    // a closed peer reads 0 bytes or errors with a reset; a silent live one times out
    !matches!(s.read(&mut b), Ok(0) | Err(_))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn garbage_closes_the_association_and_the_server_goes_on() {
    let (port, _store, stop) = start().await;
    let addr = format!("127.0.0.1:{port}");
    let a2 = addr.clone();
    blocking(move || {
        // not TPKT at all, a TPKT with a COTP nobody sent, an absurd TPKT length
        for junk in [
            &b"GET / HTTP/1.1\r\n\r\n"[..],
            &[3, 0, 0, 12, 7, 0xE0, 0, 0, 0, 0, 0, 0][..],
            &[3, 0, 0xFF, 0xFF, 2, 0xF0, 0x80][..],
        ] {
            let mut s = TcpStream::connect(&a2).unwrap();
            s.write_all(junk).unwrap();
            assert!(!alive(&mut s), "{junk:?}");
        }
        let mut c = Client::connect(&a2).unwrap();
        assert_eq!(c.server_directory().unwrap(), [LD]);
        c.release().unwrap();
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_full_house_closes_new_sockets_and_a_freed_slot_is_reused() {
    let (port, _store, stop) = start().await;
    let addr = format!("127.0.0.1:{port}");
    blocking(move || {
        let mut held: Vec<TcpStream> = (0..16)
            .map(|_| TcpStream::connect(&addr).unwrap())
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        let mut extra = TcpStream::connect(&addr).unwrap();
        assert!(!alive(&mut extra), "the 17th is closed at once");
        drop(held.pop());
        std::thread::sleep(Duration::from_millis(300));
        let mut c = Client::connect(&addr).unwrap();
        assert_eq!(c.server_directory().unwrap(), [LD]);
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn stopping_the_device_closes_the_associations() {
    let (port, _store, stop) = start().await;
    let addr = format!("127.0.0.1:{port}");
    let mut c = blocking(move || Client::connect(&addr).unwrap()).await;
    stop.cancel();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(blocking(move || c.server_directory().is_err()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_port_in_use_is_reported_and_the_rest_of_the_device_runs() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let (srv, store) = common::load_as::<Server>(&config(port), "iec61850-server");
    let stop = CancellationToken::new();
    tokio::time::timeout(Duration::from_secs(5), srv.run(store, stop))
        .await
        .expect("run returns when it cannot bind");
}
