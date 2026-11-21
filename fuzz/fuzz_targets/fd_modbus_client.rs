//! Modbus/TCP client (`<modbus-client>`): arbitrary bytes as what a server answers, through the framing (`codec::parse`, and
//! `take`, which also discards frames of another transaction) and the response decoder. Invariants: no panic, whatever arrives;
//! a response that is accepted has exactly the items the request asked for. The first byte picks the request.
#![no_main]
use ferrofire::fd::proto::modbus::client::{response, take};
use ferrofire::fd::proto::modbus::codec::{self, Parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let fc = [1, 2, 3, 4, 5, 6][usize::from(pick % 6)];
    let qty = u16::from(pick / 6) * 50 + 1; // 1..=2051: past the protocol limits too
    let check = |pdu: &[u8]| {
        if let Ok(values) = response(fc, qty, pdu) {
            assert_eq!(values.len(), if fc > 4 { 1 } else { usize::from(qty) });
            assert!(fc > 2 || values.iter().all(|v| *v <= 1));
        }
    };
    check(rest);
    if let Parse::Frame { pdu, .. } = codec::parse(rest) {
        check(pdu);
    }
    // as a stream: every frame for transaction 3 is split off; the buffer only shrinks
    let mut buf = rest.to_vec();
    let mut before = buf.len() + 1;
    while let Some(Ok(pdu)) = take(&mut buf, 3) {
        assert!(buf.len() < before);
        before = buf.len();
        check(&pdu);
    }
});
