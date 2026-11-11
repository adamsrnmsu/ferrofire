//! The ICCP server over real sockets with our thin ICCP client (tests/common/iccp_client.rs): the bilateral table, browse, Read, Write,
//! data sets, and transfer sets reporting by interval and by change.
#![cfg(feature = "proto-iccp")]

mod common;
#[path = "common/iccp_client.rs"]
mod iccp_client;

use ferrofire::fd::proto::iccp::Server;
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::Store;
use iccp_client::Iccp;
use iec61850_rs::proto::data::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(port: u16, peers: &str) -> String {
    let log = std::env::temp_dir()
        .join(format!("iccp-net-{port}.log"))
        .display()
        .to_string();
    format!(
        r#"<SCEPTRE><field-device><name>t</name><tags>
<external-tag><name>brkr</name><io>I0</io><type>binary</type></external-tag>
<external-tag><name>load-power</name><io>I1</io><type>analog</type></external-tag>
<external-tag><name>secret</name><io>I2</io><type>analog</type></external-tag>
<external-tag><name>toggle</name><io>O0</io><type>binary</type></external-tag>
<external-tag><name>setpoint</name><io>O1</io><type>analog</type></external-tag></tags>
<comms><iccp-server><endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>{log}</event-logging>
<bilateral-table><id>BLT_RANGE</id></bilateral-table>
<data-state><tag>brkr</tag><name>BRKR_1</name><variant>qtimetag</variant></data-state>
<data-real><tag>load-power</tag><name>MW_1</name><variant>QTimeTag</variant></data-real>
<data-real><tag>secret</tag><name>SECRET</name></data-real>
<data-state><tag>toggle</tag><name>TOGGLE</name><variant>plain</variant></data-state>
<data-discrete><tag>setpoint</tag><name>SP</name><variant>q</variant></data-discrete>
{peers}
</iccp-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>p.active</name></binary><analog><id>I1</id><name>p.mw</name></analog><analog><id>I2</id><name>p.secret</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>p.toggle</name></binary><analog><id>O1</id><name>p.sp</name></analog></output>
</field-device></SCEPTRE>"#
    )
}

const PEER: &str = "<peer><name>EMS1</name><domain>EMS1</domain><address>127.0.0.1</address><read>BRKR_1 MW_1 TOGGLE SP</read><write>TOGGLE SP</write></peer>";

async fn start_with(peers: &str) -> (String, Store, CancellationToken) {
    let port = port();
    let (srv, store) = common::load_as::<Server>(&config(port, peers), "iccp-server");
    let stop = CancellationToken::new();
    tokio::spawn(srv.run(store.clone(), stop.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    (format!("127.0.0.1:{port}"), store, stop)
}

async fn start() -> (String, Store, CancellationToken) {
    start_with(PEER).await
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_peer_not_in_the_bilateral_table_cannot_associate() {
    let (addr, _s, stop) =
        start_with("<peer><name>EMS1</name><address>10.9.9.9</address></peer>").await;
    let e = blocking(move || Iccp::connect(&addr).err()).await;
    assert!(e.is_some(), "loopback is not 10.9.9.9");
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_peer_browses_only_its_own_domain_and_what_it_may_see() {
    let (addr, _s, stop) = start().await;
    blocking(move || {
        let mut c = Iccp::connect(&addr).unwrap();
        assert_eq!(c.identify().unwrap(), "ferrofire iccp-server");
        assert_eq!(c.domains().unwrap(), ["EMS1"]);
        let vars = c.variables("EMS1").unwrap();
        assert_eq!(
            vars,
            [
                "BRKR_1",
                "Bilateral_Table_ID",
                "MW_1",
                "Next_DSTransfer_Set",
                "SP",
                "TOGGLE"
            ],
            "SECRET is not on EMS1's table"
        );
        assert!(c.variables("OTHER").is_err(), "no such domain");
        let vmd = c
            .name_list(0, iec61850_rs::proto::mms::ObjectScope::VmdSpecific)
            .unwrap();
        assert_eq!(
            vmd,
            ["Bilateral_Table_ID", "Supported_Features", "TASE2_Version"]
        );
        assert!(c
            .type_of(Some("EMS1"), "MW_1")
            .unwrap()
            .contains("FloatingPoint"));
        assert!(c.type_of(Some("EMS1"), "SECRET").is_err());
        c.release().unwrap();
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn reads_come_from_the_datastore_and_the_table_decides_what_is_readable() {
    let (addr, store, stop) = start().await;
    store.publish("p.mw", 400.5, 1_700_000_000);
    store.publish("p.active", 1.0, 1_700_000_000);
    blocking(move || {
        let mut c = Iccp::connect(&addr).unwrap();
        let r = c
            .read(
                Some("EMS1"),
                &["MW_1", "BRKR_1", "SECRET", "NOPE", "Bilateral_Table_ID"],
            )
            .unwrap();
        let mw = r[0].clone().unwrap();
        let m = mw.members().unwrap();
        assert_eq!(
            (&m[0], &m[1]),
            (&Value::Integer(1_700_000_000), &Value::Float32(400.5))
        );
        let br = r[1].clone().unwrap();
        assert_eq!(
            br.members().unwrap()[1],
            Value::BitString {
                unused: 0,
                bytes: vec![0x80]
            },
            "on, valid"
        );
        assert_eq!(r[2], Err(10), "SECRET is invisible, not denied");
        assert_eq!(r[3], Err(10));
        assert_eq!(r[4], Ok(Value::VisibleString("BLT_RANGE".into())));
        let v = c
            .read(
                None,
                &["Supported_Features", "TASE2_Version", "Bilateral_Table_ID"],
            )
            .unwrap();
        assert_eq!(
            v[0],
            Ok(Value::BitString {
                unused: 7,
                bytes: vec![0xC0, 0]
            })
        );
        assert_eq!(
            v[1],
            Ok(Value::Structure(vec![
                Value::Integer(1996),
                Value::Integer(8)
            ]))
        );
        assert_eq!(v[2], Ok(Value::VisibleString("BLT_RANGE".into())));
        assert_eq!(
            c.read(Some("OTHER"), &["MW_1"]).unwrap()[0],
            Err(10),
            "a domain that is not ours"
        );
        c.release().unwrap();
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_write_needs_the_table_and_becomes_a_queued_datastore_write() {
    let (addr, store, stop) = start().await;
    let st = store.clone();
    blocking(move || {
        let mut c = Iccp::connect(&addr).unwrap();
        let q = |x: i64| {
            Value::Structure(vec![
                Value::Integer(x),
                Value::BitString {
                    unused: 0,
                    bytes: vec![0],
                },
            ])
        };
        assert_eq!(c.write("EMS1", "SP", &q(250)).unwrap(), Ok(()));
        assert!(st.is_queued("setpoint"));
        let on = Value::BitString {
            unused: 6,
            bytes: vec![0x80],
        };
        assert_eq!(c.write("EMS1", "TOGGLE", &on).unwrap(), Ok(()));
        assert!(st.is_queued("toggle"));
        assert_eq!(
            c.write("EMS1", "MW_1", &Value::Float32(1.0)).unwrap(),
            Err(3),
            "readable, not writable"
        );
        assert_eq!(
            c.write("EMS1", "SP", &Value::Boolean(true)).unwrap(),
            Err(7),
            "wrong type"
        );
        let invalid = Value::BitString {
            unused: 6,
            bytes: vec![0xC0],
        };
        assert_eq!(
            c.write("EMS1", "TOGGLE", &invalid).unwrap(),
            Err(11),
            "state invalid is not a command"
        );
        assert_eq!(
            c.write("EMS1", "SECRET", &Value::Float32(1.0)).unwrap(),
            Err(10)
        );
        c.release().unwrap();
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn data_sets_are_defined_read_listed_and_deleted_per_association() {
    let (addr, store, stop) = start().await;
    store.publish("p.mw", 12.0, 5);
    blocking(move || {
        let mut c = Iccp::connect(&addr).unwrap();
        c.define_set("EMS1", "DS1", &["MW_1", "BRKR_1"]).unwrap();
        assert!(
            c.define_set("EMS1", "DS1", &["MW_1"])
                .unwrap_err()
                .contains("service error 2 5"),
            "exists"
        );
        assert!(
            c.define_set("EMS1", "DS2", &["SECRET"])
                .unwrap_err()
                .contains("service error 2 1"),
            "not on the table"
        );
        assert!(c.define_set("OTHER", "DS3", &["MW_1"]).is_err());
        assert!(c.define_set("EMS1", "bad name", &["MW_1"]).is_err());
        assert!(
            c.define_set("EMS1", "MW_1", &["MW_1"]).is_err(),
            "a set cannot take an object's name"
        );
        let sets = c
            .name_list(
                2,
                iec61850_rs::proto::mms::ObjectScope::DomainSpecific("EMS1"),
            )
            .unwrap();
        assert_eq!(sets, ["DS1"]);
        assert_eq!(c.set_members("EMS1", "DS1").unwrap(), ["MW_1", "BRKR_1"]);
        let vals = c.read_set("EMS1", "DS1").unwrap();
        assert_eq!(vals.len(), 2);
        assert_eq!(
            vals[0].clone().unwrap().members().unwrap()[1],
            Value::Float32(12.0)
        );
        assert!(c.read_set("EMS1", "NOPE").is_err());
        assert_eq!(c.delete_set("EMS1", "DS1").unwrap(), (1, 1));
        assert_eq!(c.delete_set("EMS1", "DS1").unwrap(), (0, 0));
        c.release().unwrap();
        // a second association starts with none
        let mut d = Iccp::connect(&addr).unwrap();
        assert!(d
            .name_list(
                2,
                iec61850_rs::proto::mms::ObjectScope::DomainSpecific("EMS1")
            )
            .unwrap()
            .is_empty());
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_transfer_set_reports_by_interval_and_on_change() {
    let (addr, store, stop) = start().await;
    store.publish("p.mw", 10.0, 1);
    let st = store.clone();
    blocking(move || {
        let mut c = Iccp::connect(&addr).unwrap();
        c.define_set("EMS1", "DS1", &["MW_1", "BRKR_1"]).unwrap();
        let ts = c.next_transfer_set("EMS1").unwrap();
        assert_eq!(ts, "DSTrans_1");
        assert_eq!(
            c.next_transfer_set("EMS1").unwrap(),
            ts,
            "the same name until it is configured"
        );
        assert!(c.variables("EMS1").unwrap().contains(&ts));
        assert_eq!(
            c.read(Some("EMS1"), &[&ts]).unwrap()[0],
            Err(2),
            "not configured yet"
        );
        // 0xA0 = interval timeout + object change
        assert_eq!(
            c.arm("EMS1", &ts, "DS1", (1, 0xA0, false, true)).unwrap(),
            Ok(())
        );
        let r = c
            .next_report(Duration::from_secs(3))
            .unwrap()
            .expect("an interval report");
        assert_eq!((r.transfer_set.as_str(), r.conditions), ("DSTrans_1", 0x80));
        assert_eq!(
            r.values.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["MW_1", "BRKR_1"]
        );
        assert!(r.time > 1_600_000_000);
        st.publish("p.mw", 99.0, 2);
        let r = loop {
            let r = c
                .next_report(Duration::from_secs(3))
                .unwrap()
                .expect("a change report");
            if r.conditions & 0x20 != 0 {
                break r;
            }
        };
        assert_eq!(r.values[0].1.members().unwrap()[1], Value::Float32(99.0));
        // stop it: no more reports
        assert_eq!(
            c.arm("EMS1", &ts, "DS1", (1, 0xA0, false, false)).unwrap(),
            Ok(())
        );
        while c.next_report(Duration::from_millis(300)).unwrap().is_some() {}
        assert!(c
            .next_report(Duration::from_millis(2500))
            .unwrap()
            .is_none());
        // what is refused
        assert_eq!(
            c.arm("EMS1", &ts, "NOPE", (1, 0xA0, false, true)).unwrap(),
            Err(10)
        );
        c.release().unwrap();
    })
    .await;
    stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn rbe_reports_only_what_changed() {
    let (addr, store, stop) = start().await;
    store.publish("p.mw", 10.0, 1);
    let st = store.clone();
    blocking(move || {
        let mut c = Iccp::connect(&addr).unwrap();
        c.define_set("EMS1", "DS1", &["MW_1", "BRKR_1"]).unwrap();
        let ts = c.next_transfer_set("EMS1").unwrap();
        assert_eq!(
            c.arm("EMS1", &ts, "DS1", (0, 0x20, true, true)).unwrap(),
            Ok(())
        );
        std::thread::sleep(Duration::from_millis(300));
        st.publish("p.mw", 11.0, 2);
        let r = c
            .next_report(Duration::from_secs(3))
            .unwrap()
            .expect("a report");
        assert_eq!(r.conditions, 0x20);
        assert_eq!(r.values.len(), 1, "BRKR_1 did not change");
        assert_eq!(r.values[0].0, "MW_1");
    })
    .await;
    stop.cancel();
}
