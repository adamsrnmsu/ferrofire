//! MQTT client (docs/field-device-protocols-plan.md section 2 template): arbitrary bytes as a Sparkplug B NCMD payload and as a plain
//! command, through the same `Engine::message` the session loop calls, plus the bare payload decoder. Invariants: no panic, whatever
//! arrives; a refusal is a log line, never a store change that the config forbids (`ro` is not writable and must never be queued);
//!
#![no_main]
use ferrofire::fd::proto::mqtt::engine::{Engine, Point, Scheme};
use ferrofire::fd::proto::mqtt::sparkplug;
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::{Kind, Value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sparkplug::decode_command(data);
    let point = |tag: &str, kind, writable, min, max| Point { tag: tag.into(), kind, writable, min, max };
    let points = vec![
        point("lvl", Kind::Analog, true, Some(0.0), Some(100.0)),
        point("pump", Kind::Binary, true, None, None),
        point("ro", Kind::Analog, false, None, None),
    ];
    let store = Store::new(Layout {
        points: vec![],
        external: vec![],
        internal: vec![
            ("lvl".into(), Value::Analog(0.0)),
            ("pump".into(), Value::Binary(false)),
            ("ro".into(), Value::Analog(0.0)),
        ],
    });
    let plain = Scheme::Plain { prefix: "ff".into(), device: "d".into(), qos: 1, retain: true };
    let spb = Scheme::Sparkplug { group: "g".into(), edge: "e".into() };
    for (scheme, topic) in [(plain, "ff/d/lvl/set"), (spb, "spBv1.0/g/NCMD/e")] {
        let mut e = Engine::new(scheme, points.clone(), 0);
        let _ = e.message(topic, data, &store);
        let _ = e.tick(&store, 0, 0);
    }
    // nothing the config forbids was queued; what was queued lands inside the limits
    assert!(!store.is_queued("ro"));
    store.apply_internal();
    assert!((0.0..=100.0).contains(&store.get_f64("lvl").unwrap()));
    assert_eq!(store.get_f64("ro"), Some(0.0));
});
