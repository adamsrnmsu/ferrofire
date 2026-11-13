//! VXI-11 (docs/field-device-scpi.md): arbitrary bytes into the RPC record reader, each record into the portmapper and core channel
//! (the XDR decoder behind them), and datagrams into the UDP portmapper. Invariants: no panic, whatever arrives; a reply is a whole
//! number of XDR words; the error queue never holds more than `ERR_CAP` entries. The input is cut into reads and datagrams at 0xFF bytes.
#![no_main]
use ferrofire::fd::proto::point::Point;
use ferrofire::fd::proto::scpi::device::{shared, Instrument, Route, Scope, ERR_CAP};
use ferrofire::fd::proto::scpi::vxi11::{Records, Vxi11};
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::{Kind, Value};
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    let ins = Instrument {
        idn: "ferrofire,fuzz,0,0".into(),
        routes: vec![Route {
            point: Point { tag: "lvl".into(), kind: Kind::Analog, writable: true, min: Some(0.0), max: Some(10.0) },
            read: vec!["MEASure".into(), "SCALar".into(), "LVL".into()],
            write: Some(vec!["SOURce".into(), "LVL".into()]),
            default: Some(1.0),
        }],
        max_line: 64,
    };
    let store = Store::new(Layout { points: vec![], external: vec![], internal: vec![("lvl".into(), Value::Analog(2.0))] });
    let status = shared();
    let svc = Vxi11::new(Arc::new(ins), store, (Scope::Instrument, status.clone()), 4881, 4);
    let conn = svc.connection();
    let mut records = Records::default();
    for chunk in data.split(|&b| b == 0xFF) {
        if let Some(r) = svc.call_udp(chunk) {
            assert_eq!(r.len() % 4, 0);
        }
        records.push(chunk);
        loop {
            match records.take() {
                Ok(Some(rec)) => {
                    if let Some(r) = svc.call(conn, &rec) {
                        assert_eq!(r.len() % 4, 0);
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
    assert!(ferrofire::fd::proto::scpi::device::Session::sharing(status).queued_errors() <= ERR_CAP);
    svc.close(conn);
});
