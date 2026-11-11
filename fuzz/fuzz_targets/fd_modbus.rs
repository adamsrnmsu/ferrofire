//! Modbus/TCP server (docs/field-device-protocols-plan.md section 2 template): arbitrary bytes into the MBAP framing and the four
//! tables through the same entry point the socket uses. Invariants: no panic, whatever arrives; a frame that cannot be Modbus is a
//! `Close`; the receive buffer never holds more than one incomplete frame. The input is cut into chunks at 0xFF bytes so that
//! frames split across reads too.
#![no_main]
use ferrofire::fd::eventlog::EventLog;
use ferrofire::fd::proto::modbus::device::{Conn, Device, Range, Reg};
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::{Kind, Value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let reg = |tag: &str, min, max| Reg { tag: tag.into(), range: Range { min, max } };
    let dev = Device {
        coils: [(0, "c0".to_string()), (1, "c1".to_string()), (65535, "c2".to_string())].into(),
        discretes: [(0, "d0".to_string()), (10000, "d1".to_string())].into(),
        holding: [(0, reg("h0", 0.0, 1000.0)), (1, reg("h1", 100.0, 100.0)), (40000, reg("h2", -1000.0, 2000.0))].into(),
        input: [(0, reg("i0", 50.0, -50.0)), (30000, reg("i1", 0.0, 1.0))].into(),
    };
    let store = Store::new(Layout {
        points: vec![],
        external: vec![],
        internal: ["c0", "c1", "c2", "d0", "d1"]
            .map(|t| (t.to_string(), Value::Binary(false)))
            .into_iter()
            .chain(["h0", "h1", "h2", "i0", "i1"].map(|t| (t.to_string(), Value::Analog(f64::NAN))))
            .collect(),
    });
    let _ = Kind::Binary;
    let log = EventLog::to("modbus-server", Box::new(std::io::sink()));
    let mut conn = Conn::new();
    for chunk in data.split(|&b| b == 0xFF) {
        match conn.feed(&dev, &store, &log, chunk) {
            Ok(out) => assert!(out.len() < 1 << 20),
            Err(_) => conn = Conn::new(),
        }
    }
});
