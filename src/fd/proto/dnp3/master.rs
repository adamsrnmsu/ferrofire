//! DNP3 master (`<dnp3-client>`, protocol plan row 9). This part is the codec: the requests the stock master (opendnp3 under
//! bennu) was seen to send, byte for byte (conformance/stock/client-dnp3-raw.golden), and the decoder for what an outstation
//! answers. The link, transport and application framing are the outstation's (`link`, `transport`, `app`).

use super::app::{self, func, CON, FIN, FIR, UNS};

/// The link control octet of every frame the master sends: DIR, PRM, unconfirmed user data.
pub const MASTER_DATA: u8 = 0xC4;

/// The object headers of a class poll: group 60, all objects, in the order given (stock's all-classes scan is 1, 2, 3, 0).
pub fn class_read(classes: &[u8]) -> Vec<u8> {
    classes
        .iter()
        .flat_map(|c| [60, if *c == 0 { 1 } else { c + 1 }, 0x06])
        .collect()
}

/// The object of the WRITE that clears IIN1.7: g80v1, index 7, value 0.
pub fn clear_restart() -> Vec<u8> {
    vec![80, 1, 0x00, 7, 7, 0]
}

/// A CROB as stock sends it: g12v1, one object with a two-octet index, LATCH_ON (3) or LATCH_OFF (4), count 1, 100 ms on and
/// off, status 0.
pub fn crob(idx: u16, on: bool) -> Vec<u8> {
    let mut o = vec![12, 1, 0x28, 1, 0];
    o.extend_from_slice(&idx.to_le_bytes());
    o.extend_from_slice(&[if on { 3 } else { 4 }, 1]);
    o.extend_from_slice(&100u32.to_le_bytes());
    o.extend_from_slice(&100u32.to_le_bytes());
    o.push(0);
    o
}

/// An analog output as stock sends it, whatever the point: g41v3 (float32), status 0.
pub fn ao_f32(idx: u16, v: f64) -> Vec<u8> {
    let mut o = vec![41, 3, 0x28, 1, 0];
    o.extend_from_slice(&idx.to_le_bytes());
    // stock's NaN leaves as 7f800001, not the quiet NaN a cast gives
    let bits = if v.is_nan() {
        0x7f80_0001
    } else {
        (v as f32).to_bits()
    };
    o.extend_from_slice(&bits.to_le_bytes());
    o.push(0);
    o
}

/// The object headers of an output-status read (`fep` mode only): every binary and analog output, default variation.
pub fn output_status() -> Vec<u8> {
    vec![10, 0, 0x06, 40, 0, 0x06]
}

/// The confirmation of a response that asked for one.
pub fn confirm(seq: u8, unsol: bool) -> Vec<u8> {
    vec![
        FIR | FIN | if unsol { UNS } else { 0 } | (seq & 0x0F),
        func::CONFIRM,
    ]
}

/// A point value in a response. Stock's handler takes `Binary` (groups 1 and 2) and `Analog` (30 and 32) and ignores the
/// rest; the output statuses (10, 11, 40, 42) are for `fep` mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seen {
    Binary(u16, bool),
    Analog(u16, f64),
    BinaryOutput(u16, bool),
    AnalogOutput(u16, f64),
}

/// A response that cannot be read to its end; nothing of it is used.
#[derive(Debug, PartialEq, Eq)]
pub enum Bad {
    Truncated,
    Qualifier(u8),
    /// A group and variation whose size is not known, so what follows cannot be found.
    Object(u8, u8),
    NotAResponse,
}

/// A decoded response fragment.
#[derive(Debug, PartialEq)]
pub struct Response {
    pub seq: u8,
    pub iin: u16,
    /// An unsolicited response (function 130).
    pub unsol: bool,
    /// The outstation wants a confirmation.
    pub con: bool,
    pub fin: bool,
    pub seen: Vec<Seen>,
    /// The object headers and data as received: a command's echo.
    pub objects: Vec<u8>,
}

impl Response {
    /// IIN1.7: the outstation restarted.
    pub fn restart(&self) -> bool {
        self.iin & app::iin::RESTART != 0
    }
}

/// How one object of a group and variation lies in a response.
enum Size {
    /// Packed, this many bits each.
    Bits(usize),
    Bytes(usize),
}

fn size(group: u8, var: u8) -> Option<Size> {
    use Size::{Bits, Bytes};
    Some(match (group, var) {
        (1 | 10 | 80, 1) => Bits(1),
        (3, 1) => Bits(2),
        (1 | 3 | 10, 2) | (2 | 4 | 11 | 13, 1) => Bytes(1),
        (2 | 4 | 11 | 13, 2) => Bytes(7),
        (2 | 4, 3) => Bytes(3),
        (12, 1) => Bytes(11),
        // counters: flag and 32 or 16 bits, without the flag (20 and 21 only), with a time
        (20..=23, 1) => Bytes(5),
        (20..=23, 2) => Bytes(3),
        (20, 5) | (21, 9) => Bytes(4),
        (20, 6) | (21, 10) => Bytes(2),
        (21..=23, 5) => Bytes(11),
        (21..=23, 6) => Bytes(9),
        // analog inputs, their events, output status, output commands and output events
        (30 | 32 | 40 | 41 | 42, 1) | (30 | 32 | 42, 5) | (40 | 41, 3) => Bytes(5),
        (30 | 32 | 40 | 41 | 42, 2) => Bytes(3),
        (30, 3) => Bytes(4),
        (30, 4) => Bytes(2),
        (30 | 32 | 42, 6) | (32 | 42, 4) | (40 | 41, 4) => Bytes(9),
        (32 | 42, 3 | 7) => Bytes(11),
        (32 | 42, 8) => Bytes(15),
        (50 | 51, 1 | 2) => Bytes(6),
        (52, 1 | 2) => Bytes(2),
        (110 | 111, n) if n > 0 => Bytes(usize::from(n)),
        _ => return None,
    })
}

fn int(b: &[u8]) -> f64 {
    match b.len() {
        2 => f64::from(i16::from_le_bytes([b[0], b[1]])),
        _ => f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
    }
}

fn float(b: &[u8]) -> f64 {
    match b.len() {
        4 => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        _ => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
    }
}

/// The value in one byte-sized object `b` of group `g`, variation `v`, when it is a point value.
fn value(g: u8, v: u8, idx: u16, b: &[u8]) -> Option<Seen> {
    let state = b[0] & 0x80 != 0;
    let analog = match (g, v) {
        (30, 3) => int(&b[..4]),
        (30, 4) => int(&b[..2]),
        (40, 3) | (30 | 32 | 42, 5 | 7) => float(&b[1..5]),
        (40, 4) | (30 | 32 | 42, 6 | 8) => float(&b[1..9]),
        (30 | 32 | 40 | 42, 2) | (32 | 42, 4) => int(&b[1..3]),
        (30 | 32 | 40 | 42, _) => int(&b[1..5]),
        _ => 0.0,
    };
    match g {
        1 | 2 => Some(Seen::Binary(idx, state)),
        10 | 11 => Some(Seen::BinaryOutput(idx, state)),
        30 | 32 => Some(Seen::Analog(idx, analog)),
        40 | 42 => Some(Seen::AnalogOutput(idx, analog)),
        _ => None,
    }
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8], Bad> {
    if buf.len() < n {
        return Err(Bad::Truncated);
    }
    let (head, rest) = buf.split_at(n);
    *buf = rest;
    Ok(head)
}

fn number(buf: &mut &[u8], width: usize) -> Result<usize, Bad> {
    Ok(take(buf, width)?
        .iter()
        .rev()
        .fold(0, |a, &x| a << 8 | usize::from(x)))
}

/// One object header and its objects, off the front of `buf`.
fn objects(buf: &mut &[u8], seen: &mut Vec<Seen>) -> Result<(), Bad> {
    let head = take(buf, 3)?;
    let (g, v, q) = (head[0], head[1], head[2]);
    // (first index, count, octets of index prefix)
    let (start, count, prefix) = match q {
        0x00 | 0x01 => {
            let w = usize::from(q) + 1;
            let (start, stop) = (number(buf, w)?, number(buf, w)?);
            (
                start,
                (stop + 1).checked_sub(start).ok_or(Bad::Truncated)?,
                0,
            )
        }
        0x07 | 0x08 => (0, number(buf, usize::from(q) - 6)?, 0),
        0x17 => (0, number(buf, 1)?, 1),
        0x28 => (0, number(buf, 2)?, 2),
        other => return Err(Bad::Qualifier(other)),
    };
    match size(g, v).ok_or(Bad::Object(g, v))? {
        Size::Bits(_) if prefix > 0 => Err(Bad::Qualifier(q)),
        Size::Bits(each) => {
            let data = take(buf, (count * each).div_ceil(8))?;
            for i in 0..count {
                let on = data[i * each / 8] >> (i * each % 8) & 1 == 1;
                match g {
                    1 => seen.push(Seen::Binary((start + i) as u16, on)),
                    10 => seen.push(Seen::BinaryOutput((start + i) as u16, on)),
                    _ => {}
                }
            }
            Ok(())
        }
        Size::Bytes(each) => {
            for i in 0..count {
                let idx = match prefix {
                    0 => start + i,
                    w => number(buf, w)?,
                };
                seen.extend(value(g, v, idx as u16, take(buf, each)?));
            }
            Ok(())
        }
    }
}

/// Decode a response fragment (solicited or unsolicited). All of it must parse: a fragment with a truncated object, a count
/// past its end or an object of unknown size gives nothing. Never panics on input.
pub fn decode(fragment: &[u8]) -> Result<Response, Bad> {
    let mut buf = fragment;
    let head = take(&mut buf, 2)?;
    let unsol = match head[1] {
        func::RESPONSE => false,
        130 => true,
        _ => return Err(Bad::NotAResponse),
    };
    let iin = take(&mut buf, 2)?;
    let mut r = Response {
        seq: head[0] & 0x0F,
        iin: u16::from_le_bytes([iin[0], iin[1]]),
        unsol,
        con: head[0] & CON != 0,
        fin: head[0] & FIN != 0,
        seen: vec![],
        objects: buf.to_vec(),
    };
    while !buf.is_empty() {
        objects(&mut buf, &mut r.seen)?;
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::proto::dnp3::{link, transport};

    fn unhex(s: &str) -> Vec<u8> {
        let s = s.replace(' ', "");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The user data of the one link frame in a golden line: transport octet, then the application fragment.
    fn user(frame: &str) -> Vec<u8> {
        match link::parse(&unhex(frame)) {
            link::Parse::Frame(f, _) => f.user,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_startup_requests_are_the_stock_masters_bytes() {
        // conformance/stock/client-dnp3-raw.golden, the four frames after "# server started"
        let integrity = app::request(0, app::func::READ, &class_read(&[0]));
        assert_eq!(integrity, unhex("c0013c0106"));
        let mut seq = 0;
        let seg = transport::segment(&integrity, &mut seq);
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &seg[0]),
            unhex("05640bc40a000100acd1c0c0013c0106ff50")
        );
        let clear = app::request(1, app::func::WRITE, &clear_restart());
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &transport::segment(&clear, &mut seq)[0]),
            unhex("05640ec40a0001002529c1c102500100070700ff81")
        );
        let again = app::request(2, app::func::READ, &class_read(&[0]));
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &transport::segment(&again, &mut seq)[0]),
            unhex("05640bc40a000100acd1c2c2013c01064430")
        );
        let scan = app::request(3, app::func::READ, &class_read(&[1, 2, 3, 0]));
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &transport::segment(&scan, &mut seq)[0]),
            unhex("056414c40a0001008fedc3c3013c02063c03063c04063c0106b2a0")
        );
    }

    #[test]
    fn the_commands_are_the_stock_masters_bytes() {
        // WRITE=load-breaker-toggle:true / :false (direct operate), WRITE=gen-toggle:true (select, then operate)
        let f = |seq, func, objects: &[u8]| app::request(seq, func, objects);
        assert_eq!(
            f(8, app::func::DIRECT_OPERATE, &crob(5, true)),
            user("05641ac40a0001008a1cc8c8050c0128010005000301640000005ef26400000000005b")[1..]
        );
        assert_eq!(
            f(9, app::func::DIRECT_OPERATE, &crob(5, false))[2..],
            unhex("0c0128010005000401640000006400000000")
        );
        assert_eq!(
            f(10, app::func::SELECT, &crob(6, true)),
            user("05641ac40a0001008a1ccaca030c01280100060003016400000060096400000000005b")[1..]
        );
        assert_eq!(
            f(11, app::func::OPERATE, &crob(6, true)),
            user("05641ac40a0001008a1ccbcb040c01280100060003016400000024926400000000005b")[1..]
        );
        // WRITE=gen-mw-setpoint:7 and WRITE=load-mw-setpoint:42.5 / 1e39 / 0.1 / nan / -2.5: always AnalogOutputFloat32
        assert_eq!(
            f(13, app::func::DIRECT_OPERATE, &ao_f32(2, 7.0)),
            unhex("cd05290328010002000000e04000")
        );
        assert_eq!(ao_f32(1, 42.5), unhex("2903280100010000002a4200"));
        assert_eq!(ao_f32(1, 1e39), unhex("290328010001000000807f00"));
        assert_eq!(ao_f32(1, 0.1), unhex("29032801000100cdcccc3d00"));
        assert_eq!(ao_f32(1, f64::NAN), unhex("290328010001000100807f00"));
        assert_eq!(ao_f32(1, -2.5), unhex("29032801000100000020c000"));
        assert_eq!(output_status(), unhex("0a00062800 06"));
        assert_eq!(app::request(5, app::func::CONFIRM, &[]), [0xc5, 0]);
        assert_eq!(confirm(5, true), [0xd5, 0]);
        assert_eq!(confirm(21, false), [0xc5, 0]);
    }

    #[test]
    fn the_stock_outstations_responses_decode_to_the_points_they_carry() {
        // the first integrity response: IIN1.7 (restart) and IIN1.4 (need time)
        let r = decode(&user("0564344401000a00285dc0c08190000102000202011e0600000082b90100000000000000000a020005060101a19d2801000102010000000001000000000540")[1..]).unwrap();
        assert_eq!(
            (r.seq, r.iin, r.unsol, r.con, r.fin),
            (0, 0x0090, false, false, true)
        );
        assert!(r.restart());
        use Seen::{Analog, AnalogOutput, Binary, BinaryOutput};
        assert_eq!(
            r.seen,
            [
                Binary(2, false),
                Analog(0, 0.0),
                BinaryOutput(5, false),
                BinaryOutput(6, false),
                AnalogOutput(1, 0.0),
                AnalogOutput(2, 0.0)
            ]
        );
        // a scan after the inputs changed
        let r = decode(&user("0564344401000a00285dc7c78110000102000202811e060000002292010000000000506f400a020005068181492e280100010201fa000000010c000000fe5c")[1..]).unwrap();
        assert_eq!((r.seq, r.iin), (7, 0x0010));
        assert!(!r.restart());
        assert_eq!(
            r.seen,
            [
                Binary(2, true),
                Analog(0, 250.5),
                BinaryOutput(5, true),
                BinaryOutput(6, true),
                AnalogOutput(1, 250.0),
                AnalogOutput(2, 12.0)
            ]
        );
        // the answer to the clear-restart write has no objects; a command echo is kept as bytes and has no points
        let r = decode(&unhex("c1811000")).unwrap();
        assert_eq!((r.seq, r.seen.len(), r.objects.len()), (1, 0, 0));
        let r = decode(
            &user("05641c4401000a007636c8c88110000c01280100050003016400961900006400000000005b")
                [1..],
        )
        .unwrap();
        assert_eq!(r.objects, crob(5, true));
        assert_eq!(r.seen, []);
    }

    fn resp(objects: &str) -> Result<Vec<Seen>, Bad> {
        decode(&[&[0xc0, 0x81, 0, 0][..], &unhex(objects)].concat()).map(|r| r.seen)
    }

    #[test]
    fn every_variation_an_outstation_may_choose_is_read() {
        use Seen::{Analog, AnalogOutput, Binary, BinaryOutput};
        // g1v1: packed bits 3..=12
        assert_eq!(
            resp("010100030ca502").unwrap(),
            [0xa5u16, 0x02]
                .iter()
                .flat_map(|b| (0..8).map(move |i| b >> i & 1 == 1))
                .take(10)
                .enumerate()
                .map(|(i, b)| Binary(3 + i as u16, b))
                .collect::<Vec<_>>()
        );
        // g2 events with a one-octet and a two-octet index prefix; v2 carries a time, v3 a relative time
        assert_eq!(
            resp("0201170207810900").unwrap(),
            [Binary(7, true), Binary(9, false)]
        );
        assert_eq!(
            resp("020228010034128100000000000 0").unwrap(),
            [Binary(0x1234, true)]
        );
        assert_eq!(resp("02031701058 10000").unwrap(), [Binary(5, true)]);
        // g30: 32 and 16 bit, with and without flag, float and double; two-octet start and stop
        assert_eq!(resp("1e01000101 01 2a000000").unwrap(), [Analog(1, 42.0)]);
        assert_eq!(
            resp("1e02010001 0001 01 feff").unwrap(),
            [Analog(256, -2.0)]
        );
        assert_eq!(resp("1e03000000 d6ffffff").unwrap(), [Analog(0, -42.0)]);
        assert_eq!(resp("1e04000000 0700").unwrap(), [Analog(0, 7.0)]);
        assert_eq!(resp("1e05000000 01 0000e040").unwrap(), [Analog(0, 7.0)]);
        // g32 events: v1 (i32), v5 (float), v7 (float with time), v8 (double with time)
        assert_eq!(resp("2001170103 01 2a000000").unwrap(), [Analog(3, 42.0)]);
        assert_eq!(resp("2005170103 01 0000e040").unwrap(), [Analog(3, 7.0)]);
        assert_eq!(
            resp("2007170103 01 0000e040 000000000000").unwrap(),
            [Analog(3, 7.0)]
        );
        assert_eq!(
            resp("2008170103 01 0000000000506f40 000000000000").unwrap(),
            [Analog(3, 250.5)]
        );
        // output status: g10v1 packed, g40v2 (i16), v3 (float), v4 (double)
        assert_eq!(
            resp("0a01000001 02").unwrap(),
            [BinaryOutput(0, false), BinaryOutput(1, true)]
        );
        assert_eq!(resp("2802000000 01 0500").unwrap(), [AnalogOutput(0, 5.0)]);
        assert_eq!(
            resp("2803000000 01 0000e040").unwrap(),
            [AnalogOutput(0, 7.0)]
        );
        assert_eq!(
            resp("2804000000 01 0000000000506f40").unwrap(),
            [AnalogOutput(0, 250.5)]
        );
        // what the stock handler ignores is stepped over: double-bit inputs, counters, a time, then a binary
        assert_eq!(
            resp("0302000000 81 1401000000 01 05000000 3201070 1 010203040506 0102000202 81")
                .unwrap(),
            [Binary(2, true)]
        );
        assert_eq!(resp("").unwrap(), []);
    }

    #[test]
    fn a_malformed_response_is_refused_whole() {
        // cut anywhere, the golden scan response is truncated, never a panic and never half a result
        let whole = user("0564344401000a00285dc7c78110000102000202811e060000002292010000000000506f400a020005068181492e280100010201fa000000010c000000fe5c");
        let whole = &whole[1..];
        let ends = [10, 24, 31]; // the fragment is complete after each object header's data
        for n in 0..whole.len() {
            let r = decode(&whole[..n]);
            match n >= 4 && (n == 4 || ends.contains(&n)) {
                true => assert!(r.is_ok(), "{n}: {r:?}"),
                false => assert!(r.is_err(), "{n}: {r:?}"),
            }
        }
        // a count past the end, a stop before the start, an unknown qualifier, an object nobody can size
        assert_eq!(resp("1e05000009 010000e040"), Err(Bad::Truncated));
        assert_eq!(resp("010200ff00"), Err(Bad::Truncated));
        assert_eq!(resp("0201280 0ff").map(|_| ()), Err(Bad::Truncated));
        assert_eq!(resp("01020502"), Err(Bad::Qualifier(5)));
        assert_eq!(
            resp("01020602"),
            Err(Bad::Qualifier(6)),
            "all-objects is a request's qualifier"
        );
        assert_eq!(resp("7001000000aa"), Err(Bad::Object(0x70, 1)));
        assert_eq!(resp("1e09000000aa"), Err(Bad::Object(30, 9)));
        // a request is not a response
        assert_eq!(
            decode(&unhex("c0013c0106")).map(|_| ()),
            Err(Bad::NotAResponse)
        );
        assert_eq!(decode(&[0xc0, 0x81, 0]).map(|_| ()), Err(Bad::Truncated));
        assert_eq!(decode(&[]).map(|_| ()), Err(Bad::Truncated));
        // an unsolicited response is one, and says so
        let r = decode(&unhex("f38200000201170102 81")).unwrap();
        assert_eq!((r.seq, r.unsol, r.con), (3, true, true));
        assert_eq!(r.seen, [Seen::Binary(2, true)]);
    }
}
