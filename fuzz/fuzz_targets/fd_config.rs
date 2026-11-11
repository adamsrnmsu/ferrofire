//! The config loader and the logic compiler on arbitrary text.
#![no_main]
use ferrofire::fd::config::{Config, Opts};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(xml) = std::str::from_utf8(data) {
        let _ = Config::parse(xml, &[], &Opts::default());
        let _ = Config::parse(xml, &[], &Opts { strict: true });
    }
});
