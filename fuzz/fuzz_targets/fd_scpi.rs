//! SCPI server (docs/field-device-protocols-plan.md section 2 template): arbitrary bytes into one connection's line buffer, the
//! message parser and the command handlers, through the entry point the socket uses. Invariants: no panic, whatever arrives; the line
//! buffer never holds more than `max_line` bytes and the error queue never more than `ERR_CAP` entries; one `feed` never answers
//! with more than `OUT_CAP` bytes. The input is cut into chunks at 0xFF bytes so that lines split across reads too.
#![no_main]
use ferrofire::fd::proto::point::Point;
use ferrofire::fd::proto::scpi::device::{Instrument, Route, Session, ERR_CAP, OUT_CAP};
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::{Kind, Value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let route = |tag: &str, kind, writable, min, max| {
        let (name, k) = (tag.to_ascii_uppercase(), if kind == Kind::Analog { "ANALog" } else { "BINary" });
        Route {
            point: Point { tag: tag.into(), kind, writable, min, max },
            read: vec!["MEASure".into(), k.into(), name.clone()],
            write: writable.then(|| vec!["SOURce".into(), name]),
            default: Some(1.0),
        }
    };
    let ins = Instrument {
        idn: "ferrofire,fuzz,0,0".into(),
        routes: vec![
            route("lvl", Kind::Analog, true, Some(0.0), Some(10.0)),
            route("ro", Kind::Analog, false, None, None),
            route("sw", Kind::Binary, true, None, None),
            route("gone", Kind::Binary, true, None, None),
        ],
        max_line: 64,
    };
    let store = Store::new(Layout {
        points: vec![],
        external: vec![],
        internal: vec![
            ("lvl".into(), Value::Analog(2.0)),
            ("ro".into(), Value::Analog(f64::NAN)),
            ("sw".into(), Value::Binary(false)),
        ],
    });
    let mut s = Session::new();
    for chunk in data.split(|&b| b == 0xFF) {
        assert!(s.feed(&ins, &store, chunk).len() <= OUT_CAP);
        assert!(s.buffered() <= ins.max_line && s.queued_errors() <= ERR_CAP);
    }
    assert!(s.eof(&ins, &store).len() <= OUT_CAP);
});
