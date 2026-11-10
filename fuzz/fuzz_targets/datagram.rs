//! datagram::Receiver::accept: any bytes are accepted or rejected, never a panic; a receiver stays usable.
#![no_main]
use ferrofire::datagram::Receiver;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|d: &[u8]| {
    let mut r = Receiver::new("g", ["a", "b", "valve.open"]);
    // twice: the second pass exercises the duplicate/old-seq paths on the state the first one left
    let _ = r.accept(d);
    let _ = r.accept(d);
    let _ = (r.gaps(), r.head(), r.quality("a"));
});
