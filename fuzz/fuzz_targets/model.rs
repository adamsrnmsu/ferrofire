//! Model::load: any YAML loads or is an Err; a loaded model steps without panicking.
#![no_main]
use ferrofire::model::Model;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|yaml: &str| {
    if let Ok(mut m) = Model::load(yaml) {
        m.step(0.1);
        let _ = m.snapshot();
    }
});
