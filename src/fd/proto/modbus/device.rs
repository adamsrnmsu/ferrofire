//! The Modbus server without I/O: the four tables, the scaling, and one request PDU in, one reply PDU out. `Conn` (below) adds the
//! framing of one connection on top. Behaviour is stock bennu's, recorded in `conformance/modbus/` and replayed by
//! `tests/modbus_golden.rs`; the SPEC 3.6 quirks Q1-Q4 and Q9 are explained there.

use super::codec::{self, Parse};
use crate::fd::eventlog::EventLog;
use crate::fd::store::Store;
use crate::model::Value;
use std::collections::BTreeMap;

/// The range stock scales over when a register does not give both `<max-value>` and `<min-value>`.
pub const DEFAULT_RANGE: Range = Range {
    min: -1000.0,
    max: 2000.0,
};

/// An analog register is the tag's value scaled linearly onto 0..=65535 over `[min, max]` (SPEC 4.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
}

impl Range {
    fn slope(&self) -> f64 {
        65535.0 / (self.max - self.min)
    }

    /// Truncates; a value outside the range clamps to 0 or 65535 (NaN reads as 0). Stock converts through a 32-bit integer and keeps
    /// the low 16 bits, so 1000.5 over `[0,1000]` reads 31 there: SPEC Q4 decided on a clamp, and the golden replay lists the rows.
    pub fn scale(&self, v: f64) -> u16 {
        let slope = self.slope();
        // one rounding for `slope * v + intercept`, as the stock build computes it (fused multiply-add): it decides which side of an
        // integer a value near it lands on
        slope.mul_add(v, self.intercept(slope)) as u16
    }

    pub fn unscale(&self, raw: u16) -> f64 {
        let slope = self.slope();
        (f64::from(raw) - self.intercept(slope)) / slope
    }

    fn intercept(&self, slope: f64) -> f64 {
        -slope * self.min
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reg {
    pub tag: String,
    pub range: Range,
}

/// What a connection reads and writes: address -> tag, per table.
#[derive(Clone, Default)]
pub struct Device {
    pub coils: BTreeMap<u16, String>,
    pub discretes: BTreeMap<u16, String>,
    pub holding: BTreeMap<u16, Reg>,
    pub input: BTreeMap<u16, Reg>,
}

/// The connection must be closed.
#[derive(Debug, PartialEq, Eq)]
pub struct Close(pub &'static str);

/// One of the four tables as a request sees it: how it is named in the log, and what an address with no mapping answers.
struct Spec {
    /// Log event name suffix: `read <name>`.
    name: &'static str,
    /// What the info line counts: stock says "registers" for discrete inputs.
    noun: &'static str,
    /// Largest quantity of one read.
    limit: u16,
    /// Exception code for an unmapped address (SPEC Q3: a coil answers 3, the rest 2).
    miss: u8,
    /// The error line has a colon before the address for everything but coils.
    colon: &'static str,
}

const COILS: Spec = Spec {
    name: "coils",
    noun: "coils",
    limit: 2000,
    miss: 3,
    colon: "",
};
const DISCRETES: Spec = Spec {
    name: "discrete inputs",
    noun: "registers",
    limit: 2000,
    miss: 2,
    colon: ":",
};
const HOLDING: Spec = Spec {
    name: "holding registers",
    noun: "holding registers",
    limit: 125,
    miss: 2,
    colon: ":",
};
const INPUT: Spec = Spec {
    name: "input registers",
    noun: "input registers",
    limit: 125,
    miss: 2,
    colon: ":",
};

fn exception(fc: u8, code: u8) -> Vec<u8> {
    vec![fc | 0x80, code]
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

/// Where a request's `addr + qty` may end: stock refuses a span whose end is past address 65535.
fn past_the_end(addr: u16, qty: u16) -> bool {
    u32::from(addr) + u32::from(qty) > 65535
}

/// The first address of `addr..addr+n` that `table` has no entry for.
fn first_gap<V>(table: &BTreeMap<u16, V>, addr: u16, n: u16) -> Option<u16> {
    (addr..addr + n).find(|a| !table.contains_key(a))
}

impl Device {
    /// One request PDU (function code first, never empty) to its reply PDU. An empty reply is the header-only frame of a function
    /// that is not served.
    pub fn handle(&self, store: &Store, log: &EventLog, pdu: &[u8]) -> Vec<u8> {
        let Some((&fc, body)) = pdu.split_first() else {
            return vec![]; // the framing never hands over an empty PDU
        };
        if body.len() < 4 {
            return vec![0x80, 4]; // stock reads address and quantity first, whatever the function
        }
        let (addr, n) = (be16(body, 0), be16(body, 2));
        match fc {
            1..=4 => self.read_fc(store, log, fc, addr, n),
            5 | 6 | 15 | 16 => self.write_fc(store, log, pdu, addr, n),
            _ => vec![],
        }
    }

    fn read_fc(&self, store: &Store, log: &EventLog, fc: u8, addr: u16, n: u16) -> Vec<u8> {
        match fc {
            1 => read(log, &COILS, &self.coils, fc, addr, n, || {
                bits(store, &self.coils, addr, n)
            }),
            2 => read(log, &DISCRETES, &self.discretes, fc, addr, n, || {
                bits(store, &self.discretes, addr, n)
            }),
            3 => read(log, &HOLDING, &self.holding, fc, addr, n, || {
                words(store, &self.holding, addr, n)
            }),
            _ => read(log, &INPUT, &self.input, fc, addr, n, || {
                words(store, &self.input, addr, n)
            }),
        }
    }

    /// `pdu` is the whole request, `addr` and `n` its first two words.
    fn write_fc(&self, store: &Store, log: &EventLog, pdu: &[u8], addr: u16, n: u16) -> Vec<u8> {
        match pdu[0] {
            5 => self.write_coil(store, log, pdu, addr, n),
            6 => self.write_reg(store, log, pdu, addr, n),
            15 => self.write_coils(store, log, &pdu[1..], addr, n),
            _ => self.write_regs(store, log, &pdu[1..], addr, n),
        }
    }

    /// FC5: any value with the high byte 0xFF is ON. The reply echoes the whole request.
    fn write_coil(
        &self,
        store: &Store,
        log: &EventLog,
        pdu: &[u8],
        addr: u16,
        value: u16,
    ) -> Vec<u8> {
        match self.coils.get(&addr).filter(|_| addr != u16::MAX) {
            Some(tag) => {
                let _ = store.write(tag, Value::Binary(value >= 0xFF00));
                written(log, "coils");
                pdu.to_vec()
            }
            None => refused(log, "coils", addr, 5, 2),
        }
    }

    /// FC6: the raw word is unscaled over the register's range. The reply echoes the whole request.
    fn write_reg(&self, store: &Store, log: &EventLog, pdu: &[u8], addr: u16, raw: u16) -> Vec<u8> {
        match self.holding.get(&addr).filter(|_| addr != u16::MAX) {
            Some(reg) => {
                let _ = store.write(&reg.tag, Value::Analog(reg.range.unscale(raw)));
                written(log, "holding registers");
                pdu.to_vec()
            }
            None => refused(log, "holding registers", addr, 6, 2),
        }
    }

    /// FC15: quantity, byte count, then the bits LSB first. Each coil is queued in order; an unmapped one answers exception 2 and
    /// leaves the ones before it queued (stock).
    fn write_coils(
        &self,
        store: &Store,
        log: &EventLog,
        body: &[u8],
        addr: u16,
        qty: u16,
    ) -> Vec<u8> {
        let Some(data) = write_data(body, qty, usize::from(qty).div_ceil(8), addr) else {
            return exception(15, 3);
        };
        for i in 0..qty {
            let Some(tag) = self.coils.get(&(addr + i)) else {
                return refused(log, "coils", addr + i, 15, 2);
            };
            let _ = store.write(
                tag,
                Value::Binary(data[usize::from(i / 8)] >> (i % 8) & 1 == 1),
            );
            written(log, "coils");
        }
        echo_span(15, addr, qty)
    }

    /// FC16: quantity, byte count, then big-endian words, unscaled one by one like FC6.
    fn write_regs(
        &self,
        store: &Store,
        log: &EventLog,
        body: &[u8],
        addr: u16,
        qty: u16,
    ) -> Vec<u8> {
        let Some(data) = write_data(body, qty, usize::from(qty) * 2, addr) else {
            return exception(16, 3);
        };
        for i in 0..qty {
            let Some(reg) = self.holding.get(&(addr + i)) else {
                return refused(log, "holding registers", addr + i, 16, 2);
            };
            let raw = be16(data, usize::from(i) * 2);
            let _ = store.write(&reg.tag, Value::Analog(reg.range.unscale(raw)));
            written(log, "holding registers");
        }
        echo_span(16, addr, qty)
    }
}

/// The data bytes of an FC15/FC16 request, or `None` (exception 3) when the quantity is 0, the span runs past address 65535, the
/// byte count is not `want`, or fewer data bytes arrived than the byte count says. Stock does not check the last one and writes
/// whatever its buffer holds after the frame; here the request is refused.
fn write_data(body: &[u8], qty: u16, want: usize, addr: u16) -> Option<&[u8]> {
    let count = usize::from(*body.get(4)?);
    let ok = qty != 0 && !past_the_end(addr, qty) && count == want;
    ok.then(|| body.get(5..5 + count)).flatten()
}

fn echo_span(fc: u8, addr: u16, qty: u16) -> Vec<u8> {
    let mut r = vec![fc];
    r.extend_from_slice(&addr.to_be_bytes());
    r.extend_from_slice(&qty.to_be_bytes());
    r
}

fn written(log: &EventLog, table: &str) {
    log.log(
        "info",
        &format!("write {table}"),
        "Data successfully written.",
    );
}

/// A write to an address that is not served: logged, and exception `code`.
fn refused(log: &EventLog, table: &str, addr: u16, fc: u8, code: u8) -> Vec<u8> {
    log.log(
        "error",
        &format!("write {table}"),
        &format!("Invalid write {table} request address: {addr}"),
    );
    exception(fc, code)
}

/// A read of `n` entries from `addr`: refuse a bad quantity (3), a span past the end (2) or an unmapped address (the table's code),
/// else log it and reply with the bytes `data` makes.
fn read<V>(
    log: &EventLog,
    spec: &Spec,
    table: &BTreeMap<u16, V>,
    fc: u8,
    addr: u16,
    n: u16,
    data: impl FnOnce() -> Vec<u8>,
) -> Vec<u8> {
    let event = format!("read {}", spec.name);
    if n == 0 || n > spec.limit {
        return exception(fc, 3);
    }
    if past_the_end(addr, n) {
        return exception(fc, 2);
    }
    if let Some(a) = first_gap(table, addr, n) {
        log.log(
            "error",
            &event,
            &format!(
                "Invalid read {} request address{} {a}",
                spec.name, spec.colon
            ),
        );
        return exception(fc, spec.miss);
    }
    log.log(
        "info",
        &event,
        &format!(
            "start address for read {} request is {addr} and read {n} {}.",
            spec.name, spec.noun
        ),
    );
    let bytes = data();
    let mut reply = vec![fc, bytes.len() as u8];
    reply.extend(bytes);
    reply
}

/// `n` coil or discrete values from `addr`, LSB first.
fn bits(store: &Store, table: &BTreeMap<u16, String>, addr: u16, n: u16) -> Vec<u8> {
    let mut out = vec![0u8; usize::from(n).div_ceil(8)];
    for i in 0..n {
        let on = table.get(&(addr + i)).and_then(|t| store.get_bool(t));
        out[usize::from(i / 8)] |= u8::from(on == Some(true)) << (i % 8);
    }
    out
}

/// `n` registers from `addr`, scaled, big-endian.
fn words(store: &Store, table: &BTreeMap<u16, Reg>, addr: u16, n: u16) -> Vec<u8> {
    (addr..addr + n)
        .filter_map(|a| table.get(&a))
        .flat_map(|r| {
            r.range
                .scale(store.get_f64(&r.tag).unwrap_or(0.0))
                .to_be_bytes()
        })
        .collect()
}

/// One client connection's receive buffer. Nothing is kept beyond one incomplete frame (at most 259 bytes), so a client that never
/// finishes a frame costs one small buffer, and a frame that cannot be Modbus ends the connection at its sixth byte.
#[derive(Default)]
pub struct Conn {
    rx: Vec<u8>,
}

impl Conn {
    pub fn new() -> Conn {
        Conn::default()
    }

    /// Take bytes from the client; returns the bytes to send back, one reply per whole frame, in order. `Err` means close the
    /// connection (a frame that is not Modbus/TCP); replies to the frames before it in the same call are not sent.
    pub fn feed(
        &mut self,
        dev: &Device,
        store: &Store,
        log: &EventLog,
        bytes: &[u8],
    ) -> Result<Vec<u8>, Close> {
        self.rx.extend_from_slice(bytes);
        let mut out = vec![];
        loop {
            match codec::parse(&self.rx) {
                Parse::Need => return Ok(out),
                Parse::Bad(why) => return Err(Close(why)),
                Parse::Frame {
                    tid,
                    uid,
                    pdu,
                    used,
                } => {
                    out.extend(codec::frame(tid, uid, &dev.handle(store, log, pdu)));
                    self.rx.drain(..used);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::store::Layout;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A device whose tables are all internal tags (so a write lands after `apply_internal`), and a readable log.
    struct Rig {
        dev: Device,
        store: Store,
        log: EventLog,
        out: Buf,
    }

    fn reg(tag: &str, min: f64, max: f64) -> Reg {
        Reg {
            tag: tag.into(),
            range: Range { min, max },
        }
    }

    /// `n` coils and discretes at 0.., `n` holding and input registers at 0.. over [0,1000], plus a coil and registers at 65534.
    fn rig(n: u16) -> Rig {
        let mut dev = Device::default();
        let mut internal = vec![];
        for a in (0..n).chain([65534]) {
            for (table, p) in [(&mut dev.coils, "c"), (&mut dev.discretes, "d")] {
                table.insert(a, format!("{p}{a}"));
                internal.push((format!("{p}{a}"), Value::Binary(false)));
            }
            for (table, p) in [(&mut dev.holding, "h"), (&mut dev.input, "i")] {
                table.insert(a, reg(&format!("{p}{a}"), 0.0, 1000.0));
                internal.push((format!("{p}{a}"), Value::Analog(0.0)));
            }
        }
        let out = Buf::default();
        Rig {
            dev,
            store: Store::new(Layout {
                points: vec![],
                external: vec![],
                internal,
            }),
            log: EventLog::to("modbus-server", Box::new(out.clone())),
            out,
        }
    }

    impl Rig {
        fn ask(&self, pdu: &[u8]) -> Vec<u8> {
            self.dev.handle(&self.store, &self.log, pdu)
        }

        /// The request `fc addr qty`.
        fn aq(&self, fc: u8, addr: u16, qty: u16) -> Vec<u8> {
            self.ask(&[&[fc][..], &addr.to_be_bytes(), &qty.to_be_bytes()].concat())
        }

        /// FC15 / FC16: `fc addr qty count data`.
        fn multi(&self, fc: u8, addr: u16, qty: u16, count: u8, data: &[u8]) -> Vec<u8> {
            let head = [&[fc][..], &addr.to_be_bytes(), &qty.to_be_bytes(), &[count]].concat();
            self.ask(&[head, data.to_vec()].concat())
        }

        fn land(&self) {
            self.store.apply_internal();
        }

        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.out.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|l| {
                    let f: Vec<&str> = l.splitn(6, ',').collect();
                    format!("{},{},{}", f[2], f[4], f[5])
                })
                .collect()
        }
    }

    #[test]
    fn scaling_is_linear_over_the_range_and_clamps_outside_it() {
        let r = Range {
            min: 0.0,
            max: 1000.0,
        };
        assert_eq!(
            (r.scale(0.0), r.scale(250.5), r.scale(1000.0)),
            (0, 0x4020, 65535)
        );
        assert_eq!(
            (r.scale(1000.5), r.scale(1e10), r.scale(f64::INFINITY)),
            (65535, 65535, 65535)
        );
        assert_eq!(
            (r.scale(-0.001), r.scale(-1e10), r.scale(f64::NAN)),
            (0, 0, 0)
        );
        let d = DEFAULT_RANGE;
        assert_eq!(
            (d.scale(-1000.0), d.scale(0.0), d.scale(2000.0)),
            (0, 0x5555, 65535)
        );
        let up = Range {
            min: 50.0,
            max: -50.0,
        };
        assert_eq!(
            (up.scale(0.0), up.scale(-50.0), up.scale(50.0)),
            (0x7fff, 65535, 0)
        );
        assert_eq!(
            Range {
                min: 100.0,
                max: 100.0
            }
            .scale(100.0),
            0,
            "a range of no width reads 0"
        );
        assert_eq!((r.unscale(0), r.unscale(65535)), (0.0, 1000.0));
        assert!((r.unscale(0x4020) - 250.5).abs() < 0.016);
        assert!((d.unscale(0) + 1000.0).abs() < 1e-9 && (d.unscale(65535) - 2000.0).abs() < 1e-9);
        assert!(Range {
            min: 100.0,
            max: 100.0
        }
        .unscale(5)
        .is_nan());
        for raw in [0u16, 1, 12345, 32768, 65535] {
            assert_eq!(
                r.scale(r.unscale(raw)) / 2,
                raw / 2,
                "a round trip loses at most the last bit"
            );
        }
    }

    #[test]
    fn a_short_body_is_exception_4_under_function_byte_0x80_for_any_function() {
        let rig = rig(4);
        for fc in [1u8, 2, 3, 4, 5, 6, 7, 15, 16, 43, 255] {
            for n in 0..4 {
                assert_eq!(
                    rig.ask(&[&[fc][..], &vec![0; n]].concat()),
                    [0x80, 4],
                    "FC{fc} with {n} bytes"
                );
            }
        }
        assert_eq!(
            rig.ask(&[7, 0, 0, 0, 1]),
            Vec::<u8>::new(),
            "four bytes is enough: the function is just not served"
        );
        assert_eq!(rig.ask(&[0, 0, 0, 0, 0]), Vec::<u8>::new());
        assert_eq!(rig.ask(&[]), Vec::<u8>::new());
    }

    #[test]
    fn reads_pack_bits_lsb_first_and_words_big_endian() {
        let rig = rig(12);
        for a in [0u16, 2, 3, 9, 11] {
            rig.store
                .write(&format!("c{a}"), Value::Binary(true))
                .unwrap();
            rig.store
                .write(&format!("d{a}"), Value::Binary(true))
                .unwrap();
        }
        rig.store.write("h1", Value::Analog(250.5)).unwrap();
        rig.store.write("i1", Value::Analog(1000.0)).unwrap();
        rig.land();
        assert_eq!(rig.aq(1, 0, 12), [1, 2, 0b0000_1101, 0b0000_1010]);
        assert_eq!(rig.aq(2, 2, 3), [2, 1, 0b011]);
        assert_eq!(rig.aq(1, 3, 1), [1, 1, 1]);
        assert_eq!(rig.aq(3, 0, 3), [3, 6, 0, 0, 0x40, 0x20, 0, 0]);
        assert_eq!(rig.aq(4, 1, 2), [4, 4, 0xff, 0xff, 0, 0]);
    }

    #[test]
    fn read_quantities_are_limited_per_table_and_checked_before_addresses() {
        let rig = rig(2001);
        let ok = |r: Vec<u8>, len: usize| assert_eq!((r[0] & 0x80, r.len()), (0, len), "{r:?}");
        ok(rig.aq(1, 0, 2000), 2 + 250);
        ok(rig.aq(2, 0, 2000), 2 + 250);
        ok(rig.aq(3, 0, 125), 2 + 250);
        ok(rig.aq(4, 0, 125), 2 + 250);
        for fc in [1u8, 2, 3, 4] {
            assert_eq!(rig.aq(fc, 0, 0), [fc | 0x80, 3], "FC{fc} x0");
        }
        assert_eq!(rig.aq(1, 0, 2001), [0x81, 3]);
        assert_eq!(rig.aq(2, 0, 2001), [0x82, 3]);
        assert_eq!(rig.aq(3, 0, 126), [0x83, 3]);
        assert_eq!(rig.aq(4, 0, 126), [0x84, 3]);
        assert_eq!(
            rig.aq(4, 5000, 126),
            [0x84, 3],
            "the quantity is judged before the address"
        );
        assert_eq!(rig.aq(4, 5000, 1), [0x84, 2]);
    }

    #[test]
    fn an_unmapped_address_answers_3_for_a_coil_and_2_for_the_rest_and_names_the_first_gap() {
        let rig = rig(3);
        assert_eq!(rig.aq(1, 1, 3), [0x81, 3]);
        assert_eq!(rig.aq(2, 1, 3), [0x82, 2]);
        assert_eq!(rig.aq(3, 1, 3), [0x83, 2]);
        assert_eq!(rig.aq(4, 1, 3), [0x84, 2]);
        assert_eq!(
            rig.lines(),
            [
                "error,read coils,Invalid read coils request address 3",
                "error,read discrete inputs,Invalid read discrete inputs request address: 3",
                "error,read holding registers,Invalid read holding registers request address: 3",
                "error,read input registers,Invalid read input registers request address: 3",
            ]
        );
    }

    #[test]
    fn a_span_that_ends_past_address_65535_answers_2_before_any_mapping_is_looked_at() {
        let rig = rig(1);
        assert_eq!(
            rig.aq(1, 65534, 1)[0],
            1,
            "one coil at 65534 is the last that can be read"
        );
        assert_eq!(rig.aq(1, 65534, 2), [0x81, 2]);
        assert_eq!(rig.aq(1, 65535, 1), [0x81, 2]);
        assert_eq!(rig.aq(4, 65534, 2), [0x84, 2]);
        assert_eq!(rig.aq(3, 65534, 1)[0], 3);
        assert_eq!(rig.aq(2, 65534, 1)[0], 2);
        assert!(rig.lines().len() == 3, "{:?}", rig.lines());
    }

    #[test]
    fn single_writes_echo_the_whole_request_and_queue_the_value() {
        let rig = rig(2);
        for (value, on) in [
            (0xFF00u16, true),
            (0xFFFF, true),
            (0xFEFF, false),
            (0, false),
            (1234, false),
            (0x00FF, false),
        ] {
            let req = [
                &[5u8][..],
                &1u16.to_be_bytes(),
                &value.to_be_bytes(),
                &[9, 9],
            ]
            .concat();
            assert_eq!(rig.ask(&req), req, "FC5 echoes everything");
            rig.land();
            assert_eq!(rig.store.get_bool("c1"), Some(on), "{value:#06x}");
        }
        let req = [
            &[6u8][..],
            &1u16.to_be_bytes(),
            &0x8000u16.to_be_bytes(),
            &[7],
        ]
        .concat();
        assert_eq!(rig.ask(&req), req);
        rig.land();
        let v = rig.store.get_f64("h1").unwrap();
        assert!((v - 500.0076).abs() < 1e-3, "{v}");
        assert_eq!(rig.aq(5, 7, 0xFF00), [0x85, 2]);
        assert_eq!(rig.aq(6, 7, 1), [0x86, 2]);
        assert_eq!(rig.aq(5, 65535, 0xFF00), [0x85, 2]);
        assert_eq!(rig.aq(6, 65535, 1), [0x86, 2]);
        assert_eq!(
            rig.aq(5, 65534, 0xFF00)[0],
            5,
            "the last coil below 65535 works"
        );
        assert_eq!(rig.aq(6, 65534, 1)[0], 6);
        assert_eq!(
            rig.lines().last().unwrap(),
            "info,write holding registers,Data successfully written."
        );
        assert_eq!(
            rig.lines()
                .iter()
                .filter(|l| l.starts_with("error,write"))
                .count(),
            4
        );
    }

    #[test]
    fn writing_several_coils_takes_the_bits_lsb_first_and_queues_those_before_an_unmapped_one() {
        let rig = rig(10);
        assert_eq!(
            rig.multi(15, 0, 10, 2, &[0b1100_1101, 0b10]),
            [15, 0, 0, 0, 10]
        );
        rig.land();
        let got: Vec<bool> = (0..10)
            .map(|a| rig.store.get_bool(&format!("c{a}")).unwrap())
            .collect();
        assert_eq!(
            got,
            [true, false, true, true, false, false, true, true, false, true]
        );
        assert_eq!(
            rig.multi(15, 8, 4, 1, &[0b1111]),
            [0x8f, 2],
            "address 10 is not mapped"
        );
        rig.land();
        assert_eq!(
            (rig.store.get_bool("c8"), rig.store.get_bool("c9")),
            (Some(true), Some(true))
        );
        assert_eq!(
            rig.lines().last().unwrap(),
            "error,write coils,Invalid write coils request address: 10"
        );
    }

    #[test]
    fn writing_several_registers_unscales_each_and_queues_those_before_an_unmapped_one() {
        let rig = rig(3);
        assert_eq!(
            rig.multi(16, 0, 3, 6, &[0, 0, 0x80, 0, 0xff, 0xff]),
            [16, 0, 0, 0, 3]
        );
        rig.land();
        let h: Vec<f64> = (0..3)
            .map(|a| rig.store.get_f64(&format!("h{a}")).unwrap())
            .collect();
        assert!(
            h[0] == 0.0 && (h[1] - 500.0076).abs() < 1e-3 && h[2] == 1000.0,
            "{h:?}"
        );
        assert_eq!(rig.multi(16, 2, 2, 4, &[0, 0, 0, 0]), [0x90, 2]);
        assert_eq!(
            rig.lines().last().unwrap(),
            "error,write holding registers,Invalid write holding registers request address: 3"
        );
    }

    #[test]
    fn multi_writes_refuse_a_bad_quantity_byte_count_short_data_or_a_span_past_the_end() {
        let rig = rig(10);
        let bad = |fc: u8, r: Vec<u8>| assert_eq!(r, [fc | 0x80, 3], "{r:?}");
        bad(15, rig.multi(15, 0, 0, 0, &[]));
        bad(15, rig.multi(15, 0, 9, 1, &[0xff]));
        bad(15, rig.multi(15, 0, 8, 2, &[0xff, 0xff]));
        bad(15, rig.multi(15, 0, 8, 1, &[]));
        bad(15, rig.ask(&[15, 0, 0, 0, 8]));
        bad(15, rig.multi(15, 65535, 1, 1, &[1]));
        bad(15, rig.multi(15, 65534, 2, 1, &[1]));
        bad(16, rig.multi(16, 0, 0, 0, &[]));
        bad(16, rig.multi(16, 0, 2, 2, &[0, 0]));
        bad(16, rig.multi(16, 0, 1, 4, &[0; 4]));
        bad(16, rig.multi(16, 0, 2, 4, &[0; 3]));
        bad(16, rig.ask(&[16, 0, 0, 0, 1]));
        bad(16, rig.multi(16, 65535, 1, 2, &[0, 0]));
        bad(16, rig.multi(16, 65534, 2, 4, &[0; 4]));
        assert_eq!(
            rig.multi(15, 65534, 1, 1, &[1]),
            [15, 0xff, 0xfe, 0, 1],
            "the last coil below 65535 works"
        );
        assert_eq!(rig.multi(16, 65534, 1, 2, &[0, 9]), [16, 0xff, 0xfe, 0, 1]);
        assert_eq!(
            rig.multi(16, 0, 1, 2, &[0, 0, 7, 7]),
            [16, 0, 0, 0, 1],
            "extra data is ignored"
        );
        assert!(
            rig.lines().iter().all(|l| l.starts_with("info,write")),
            "refused requests are not logged: {:?}",
            rig.lines()
        );
    }

    #[test]
    fn a_connection_answers_every_whole_frame_and_keeps_only_an_incomplete_one() {
        let rig = rig(2);
        let mut conn = Conn::new();
        let one = codec::frame(1, 1, &[2, 0, 0, 0, 1]);
        let two = codec::frame(2, 7, &[1, 0, 0, 0, 1]);
        let feed = |c: &mut Conn, b: &[u8]| c.feed(&rig.dev, &rig.store, &rig.log, b);
        let both = [one.clone(), two.clone()].concat();
        assert_eq!(
            feed(&mut conn, &both),
            Ok([
                codec::frame(1, 1, &[2, 1, 0]),
                codec::frame(2, 7, &[1, 1, 0])
            ]
            .concat())
        );
        assert_eq!(feed(&mut conn, &both[..5]), Ok(vec![]));
        assert_eq!(conn.rx.len(), 5);
        assert_eq!(
            feed(&mut conn, &both[5..one.len()]),
            Ok(codec::frame(1, 1, &[2, 1, 0]))
        );
        assert!(conn.rx.is_empty());
        // a frame that can never be answered is a Close, and a long wait for the rest of a big one never buffers more than it
        assert_eq!(
            feed(&mut conn, &[0, 1, 0, 9, 0, 6]),
            Err(Close("protocol id is not 0"))
        );
        // the largest frame, byte by byte: nothing but that one incomplete frame is ever held
        let mut conn = Conn::new();
        let mut big = vec![0, 1, 0, 0, 0, 254, 1, 77];
        big.resize(260, 0);
        for (i, byte) in big.iter().enumerate() {
            let out = feed(&mut conn, &[*byte]).unwrap();
            assert_eq!(conn.rx.len(), if i == 259 { 0 } else { i + 1 });
            assert_eq!(out.is_empty(), i != 259, "answered at the last byte only");
        }
    }
}
