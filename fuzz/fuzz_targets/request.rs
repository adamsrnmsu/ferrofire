//! The REP request handler: any bytes in, exactly one NUL-terminated reply out, never a panic.
#![no_main]
use ferrofire::bennu::{handle, Meta, Shared};
use ferrofire::model::Model;
use libfuzzer_sys::fuzz_target;
use std::sync::{LazyLock, Mutex};

static STATE: LazyLock<(Meta, Mutex<Shared>)> = LazyLock::new(|| {
    let m = Model::load(ferrofire::WATERWAY_YAML).unwrap();
    let snap = m.snapshot().into_iter().map(|(t, v)| (t.to_string(), v)).collect();
    let sh = Shared { snap, queue: vec![] };
    (Meta::of(&m), Mutex::new(sh))
});

fuzz_target!(|req: &[u8]| {
    let (meta, sh) = &*STATE;
    let reply = handle(req, meta, sh);
    assert_eq!(reply.last(), Some(&0), "reply must end with NUL");
    sh.lock().unwrap().queue.clear();
});
