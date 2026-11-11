//! An independent Modbus client library (tokio-modbus, not ours, not bennu's) against our server: what it reads and writes must
//! agree with the datastore. The same is done with pymodbus and libmodbus' mbpoll in `make conformance PROTO=modbus`.

mod common;

use common::modbus::*;
use ferrofire::model::Value;
use tokio_modbus::prelude::*;

async fn client(port: u16) -> tokio_modbus::client::Context {
    let addr = ([127, 0, 0, 1], port).into();
    for _ in 0..50 {
        if let Ok(c) = tcp::connect_slave(addr, Slave(1)).await {
            return c;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the server never listened on {port}");
}

#[tokio::test]
async fn reads_agree_with_the_datastore() {
    let d = start();
    d.store.publish("pir", 250.5, 1);
    d.store.publish("pdi", 1.0, 1);
    let mut c = client(d.port).await;
    assert_eq!(
        c.read_input_registers(30000, 1).await.unwrap().unwrap(),
        [16416]
    );
    assert_eq!(
        c.read_discrete_inputs(10000, 1).await.unwrap().unwrap(),
        [true]
    );
    assert_eq!(c.read_coils(0, 2).await.unwrap().unwrap(), [false, false]);
    assert_eq!(
        c.read_holding_registers(40000, 2).await.unwrap().unwrap(),
        [0, 0]
    );
    d.stop.cancel();
}

#[tokio::test]
async fn writes_land_in_the_queue_with_the_scaled_value() {
    let d = start();
    let mut c = client(d.port).await;
    c.write_single_coil(0, true).await.unwrap().unwrap();
    c.write_multiple_coils(0, &[false, true])
        .await
        .unwrap()
        .unwrap();
    c.write_single_register(40000, 0x8000)
        .await
        .unwrap()
        .unwrap();
    c.write_multiple_registers(40000, &[0xFFFF, 0x4000])
        .await
        .unwrap()
        .unwrap();
    let q: std::collections::BTreeMap<_, _> = d
        .store
        .pending(0)
        .into_iter()
        .map(|(t, _, v)| (t, v))
        .collect();
    assert_eq!(
        q["co"],
        Value::Binary(false),
        "the later write wins here: stock keeps the first, see conformance/README.md"
    );
    assert_eq!(q["co2"], Value::Binary(true));
    let Value::Analog(hr) = q["hr"] else { panic!() };
    let Value::Analog(hr2) = q["hr2"] else {
        panic!()
    };
    assert_eq!((hr, (hr2 * 1000.0).round()), (1000.0, 250004.0));
    d.stop.cancel();
}

#[tokio::test]
async fn exceptions_come_back_as_the_library_names_them() {
    let d = start();
    let mut c = client(d.port).await;
    let miss = |r: Result<Result<Vec<u16>, ExceptionCode>, _>| r.unwrap().unwrap_err();
    assert_eq!(
        miss(c.read_holding_registers(40005, 1).await),
        ExceptionCode::IllegalDataAddress
    );
    assert_eq!(
        miss(c.read_holding_registers(40000, 126).await),
        ExceptionCode::IllegalDataValue
    );
    assert_eq!(
        c.read_coils(5, 1).await.unwrap().unwrap_err(),
        ExceptionCode::IllegalDataValue,
        "an unmapped coil answers 3, not 2 (SPEC Q3)"
    );
    d.stop.cancel();
}
