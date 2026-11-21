//! DNP3 master (`<dnp3-client>`): arbitrary bytes as what an outstation answers. Once as an application fragment straight into
//! `master::decode`, once as a byte stream through the link framing and the transport reassembly first, the way the socket
//! feeds it. Invariants: no panic, whatever arrives; a fragment that decodes cannot carry more points than it has bits.
#![no_main]
use ferrofire::fd::proto::dnp3::app::MAX_APDU;
use ferrofire::fd::proto::dnp3::link::{self, Parse};
use ferrofire::fd::proto::dnp3::master::decode;
use ferrofire::fd::proto::dnp3::transport::Reassembler;
use libfuzzer_sys::fuzz_target;

fn fragment(data: &[u8]) {
    if let Ok(r) = decode(data) {
        assert!(r.seen.len() <= data.len() * 8);
        assert_eq!(r.objects, data[4..]);
    }
}

fuzz_target!(|data: &[u8]| {
    fragment(data);
    let mut asm = Reassembler::new(MAX_APDU);
    let mut buf = data.to_vec();
    while let Parse::Frame(f, used) = link::parse(&buf) {
        buf.drain(..used);
        if let Ok(Some(whole)) = asm.push(&f.user) {
            fragment(&whole);
        }
    }
});
