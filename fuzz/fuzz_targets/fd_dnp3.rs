//! DNP3 outstation (docs/field-device-protocols-plan.md section 2 template): arbitrary bytes into the link, transport and
//! application layers through the same entry point the socket uses. Invariant: no panic, whatever arrives; a framing
//! violation is a `Close`, never a crash. The input is cut into chunks at 0xFF bytes so that frames split across reads too.
#![no_main]
use ferrofire::fd::proto::dnp3::db::{AnaPoint, BinPoint};
use ferrofire::fd::proto::dnp3::outstation::{Conn, Device, Sink};
use libfuzzer_sys::fuzz_target;
use std::time::Duration;

struct Null;
impl Sink for Null {
    fn binary(&mut self, _: &str, _: u16, _: bool) {}
    fn analog(&mut self, _: &str, _: u16, _: f32) {}
}

fuzz_target!(|data: &[u8]| {
    let bin = [(2, BinPoint::new("bi", false, true)), (5, BinPoint::new("bo", true, false))];
    let ana = [(0, AnaPoint::new("ai", false, true)), (1, AnaPoint::new("ao", true, false))];
    let mut dev = Device::new(10, 1, bin.into_iter().collect(), ana.into_iter().collect());
    let mut conn = Conn::new();
    for (i, chunk) in data.split(|&b| b == 0xFF).enumerate() {
        let now = Duration::from_millis(500 * i as u64);
        if conn.feed(&mut dev, &mut Null, chunk, now).is_err() {
            break;
        }
        conn.tick(&mut dev, now);
    }
});
