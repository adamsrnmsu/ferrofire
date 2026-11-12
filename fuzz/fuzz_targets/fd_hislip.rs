//! HiSLIP (docs/field-device-scpi.md): arbitrary bytes into the frame reader and, frame by frame, into the synchronous channel's state
//! machine and the asynchronous channel's replies. Invariants: no panic, whatever arrives; a frame the reader returns has a payload within
//! `MAX_MESSAGE`; a response Data frame is never longer than the client's maximum message size less the header (272 until it says otherwise); the error
//! queue never holds more than `ERR_CAP` entries. The input is cut into reads at 0xFF bytes so that frames split across reads too.
#![no_main]
use ferrofire::fd::proto::point::Point;
use ferrofire::fd::proto::scpi::device::{shared, Instrument, Route, Session, ERR_CAP};
use ferrofire::fd::proto::scpi::hislip::{async_reply, t::*, Ctx, Link, Reader, Sync, HEADER, MAX_MESSAGE};
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::{Kind, Value};
use libfuzzer_sys::fuzz_target;

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
    let (link, mut sync, mut reader) = (Link::new(status.clone()), Sync::new(status.clone()), Reader::default());
    let watcher = Session::sharing(status);
    let ctx = Ctx { ins: &ins, store: &store, link: &link };
    for chunk in data.split(|&b| b == 0xFF) {
        reader.push(chunk);
        loop {
            match reader.take() {
                Ok(Some(f)) => {
                    assert!(f.payload.len() as u64 <= MAX_MESSAGE);
                    // the types of the synchronous channel go to the state machine, the rest to the asynchronous replies
                    let frames = match f.ty {
                        DATA | DATA_END | TRIGGER | DEVICE_CLEAR_COMPLETE | INITIALIZE => sync.recv(f, &ctx).unwrap_or_default(),
                        _ => vec![async_reply(&f, &link)],
                    };
                    let step = link.max_out().saturating_sub(HEADER as u64);
                    assert!(frames.iter().filter(|r| matches!(r.ty, DATA | DATA_END)).all(|r| r.payload.len() as u64 <= step));
                    assert!(watcher.queued_errors() <= ERR_CAP);
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
    sync.close();
});
