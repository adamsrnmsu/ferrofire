//! `<iec61850-rtu>` (ww-n29) on a real link: our `<iec61850-server>` publishes GOOSE on `lo`, and the subscriber in the same
//! device, configured from a SEL CID file as bennu's module is, turns a TRIP into a write. Needs CAP_NET_RAW (the test container
//! runs as root); without it the subscriber says so and the test is skipped.
#![cfg(feature = "proto-iec61850")]

mod common;

use common::clients::*;
use ferrofire::model::Value;
use std::time::Duration;

const CID: &str = r#"<?xml version="1.0"?>
<SCL xmlns="http://www.iec.ch/61850/2003/SCL" xmlns:esel="http://www.selinc.com/2006/61850"><IED name="SUB">
  <Private type="SEL_GooseSubscription"><esel:GooseSubscription iedName="simpleIO" ldInst="GenericIO" cbName="gcbAnalogValues"
    datSet="Events" datSetRef="simpleIOGenericIO/LLN0$Events" confRev="1" mAddr="01-0C-CD-01-00-01" APPID="1000">
    <esel:GooseRxEntry tdlString="VB001"/></esel:GooseSubscription></Private></IED></SCL>"#;

fn config(port: u16, cid: &str, log: &str) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>t</name><tags>
<external-tag><name>brkr</name><io>I0</io><type>binary</type></external-tag>
<external-tag><name>trip</name><io>O0</io><type>binary</type></external-tag></tags>
<comms><iec61850-server><endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging>
<binary-input><tag>brkr</tag></binary-input><goose><interface>lo</interface></goose></iec61850-server>
<iec61850-rtu><name>sub</name><configuration-file>{cid}</configuration-file><interface>lo</interface><event-logging>{log}</event-logging>
<entry><register-address>0</register-address><tag>trip</tag></entry></iec61850-rtu></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>p.brkr</name></binary></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>p.trip</name></binary></output>
</field-device></SCEPTRE>"#
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_goose_trip_on_the_wire_writes_false_to_the_entry_0_tag() {
    let port = free_port();
    let dir = std::env::temp_dir();
    let cid = dir.join(format!("iec61850-rtu-{port}.cid"));
    let log = dir.join(format!("iec61850-rtu-{port}.log"));
    std::fs::write(&cid, CID).unwrap();
    let device = modules(&config(
        port,
        &cid.display().to_string(),
        &log.display().to_string(),
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    if logged.contains("needs CAP_NET_RAW") {
        eprintln!("skipped: no CAP_NET_RAW ({logged})");
        return;
    }
    assert!(logged.contains("subscribing on lo"), "{logged}");
    assert_eq!(
        device.store.queued("trip"),
        None,
        "no trip while the breaker input is false"
    );
    assert!(device.store.publish("p.brkr", 1.0, 1));
    until("the trip write", Duration::from_secs(5), || {
        device.store.queued("trip") == Some(Value::Binary(false))
    })
    .await;
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains("Value at register address 0 was received"),
        "{logged}"
    );
    let _ = std::fs::remove_file(cid);
    let _ = std::fs::remove_file(log);
}

#[test]
fn a_missing_or_broken_configuration_file_is_a_config_error() {
    use ferrofire::fd::config::{Config, Opts};
    use ferrofire::fd::proto::REGISTRY;
    let err = |cid: &str| {
        Config::parse(&config(1, cid, "/dev/null"), REGISTRY, &Opts::default())
            .err()
            .expect("refused")
            .to_string()
    };
    assert!(
        err("/no/such.cid").contains("configuration-file"),
        "{}",
        err("/no/such.cid")
    );
    let bad = std::env::temp_dir().join("iec61850-rtu-broken.cid");
    std::fs::write(&bad, "<SCL").unwrap();
    assert!(err(&bad.display().to_string()).contains("configuration-file"));
    let _ = std::fs::remove_file(bad);
}
