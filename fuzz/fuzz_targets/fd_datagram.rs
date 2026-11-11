//! Template for a frame-parser fuzz target (docs/field-device-protocols-plan.md section 2): feed arbitrary bytes to the
//! parser through the same entry point the network uses; the only failure is a panic. A protocol module copies this file,
//! swapping the call for its own `parse_frame(&[u8])`.
#![no_main]
use ferrofire::datagram::Receiver;
use ferrofire::fd::{input, store::{Layout, Store}};
use ferrofire::model::Kind;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let store = Store::new(Layout {
        points: vec![("a".into(), Kind::Analog, None), ("b".into(), Kind::Binary, None)],
        external: vec![("ta".into(), "a".into()), ("tb".into(), "b".into())],
        internal: vec![],
    });
    let mut rx = Receiver::new("g", ["a", "b"]);
    input::apply(&mut rx, &store, data, 0);
});
