//! Modbus/TCP framing (MBAP): `tid(2) pid(2) length(2) uid(1) pdu`, where `length` counts the unit id and the PDU. Pure functions on
//! byte slices, shared by the server's state machine and the fuzz target. A PDU is at most 253 bytes, so a length is 2..=254.

/// Largest MBAP `length`: unit id plus a 253 byte PDU.
pub const MAX_LENGTH: usize = 254;
/// Bytes before the PDU: transaction id, protocol id, length, unit id.
pub const HEADER: usize = 7;

#[derive(Debug, PartialEq, Eq)]
pub enum Parse<'a> {
    /// Not a whole frame yet.
    Need,
    /// The stream is not Modbus/TCP (or a hostile frame): close the connection.
    Bad(&'static str),
    /// One frame and the bytes it took from the front of the buffer.
    Frame {
        tid: u16,
        uid: u8,
        pdu: &'a [u8],
        used: usize,
    },
}

/// Split the first frame off `buf`. The header is checked as soon as its six bytes are here, so a bad length never makes the
/// caller wait for (or buffer) the bytes it promises.
pub fn parse(buf: &[u8]) -> Parse<'_> {
    if buf.len() < 6 {
        return Parse::Need;
    }
    if buf[2] != 0 || buf[3] != 0 {
        return Parse::Bad("protocol id is not 0");
    }
    let length = usize::from(u16::from_be_bytes([buf[4], buf[5]]));
    if !(2..=MAX_LENGTH).contains(&length) {
        return Parse::Bad("MBAP length is outside 2..=254");
    }
    let used = 6 + length;
    if buf.len() < used {
        return Parse::Need;
    }
    Parse::Frame {
        tid: u16::from_be_bytes([buf[0], buf[1]]),
        uid: buf[6],
        pdu: &buf[HEADER..used],
        used,
    }
}

/// A reply frame. An empty `pdu` is the header-only frame stock sends for a function it does not serve (SPEC Q2).
pub fn frame(tid: u16, uid: u8, pdu: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + pdu.len());
    out.extend_from_slice(&tid.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&((pdu.len() + 1) as u16).to_be_bytes());
    out.push(uid);
    out.extend_from_slice(pdu);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(tid: u16, pid: u16, len: u16, rest: &[u8]) -> Vec<u8> {
        let mut v = [tid.to_be_bytes(), pid.to_be_bytes(), len.to_be_bytes()].concat();
        v.extend_from_slice(rest);
        v
    }

    #[test]
    fn a_whole_frame_is_split_off_with_its_ids() {
        let mut b = req(0x1234, 0, 6, &[7, 3, 0x9c, 0x40, 0, 1]);
        b.extend_from_slice(b"next");
        assert_eq!(
            parse(&b),
            Parse::Frame {
                tid: 0x1234,
                uid: 7,
                pdu: &[3, 0x9c, 0x40, 0, 1],
                used: 12
            }
        );
    }

    #[test]
    fn a_frame_not_yet_complete_needs_more_at_every_cut() {
        let b = req(1, 0, 6, &[1, 3, 0, 0, 0, 1]);
        for n in 0..b.len() {
            assert_eq!(parse(&b[..n]), Parse::Need, "{n} bytes");
        }
        assert!(matches!(parse(&b), Parse::Frame { .. }));
    }

    #[test]
    fn length_bounds_are_checked_from_the_header_alone() {
        for (len, ok) in [
            (0, false),
            (1, false),
            (2, true),
            (254, true),
            (255, false),
            (65535, false),
        ] {
            let b = req(1, 0, len, &[]);
            let p = parse(&b);
            match ok {
                true => assert_eq!(p, Parse::Need, "length {len}"),
                false => assert!(matches!(p, Parse::Bad(_)), "length {len}: {p:?}"),
            }
        }
        let mut full = req(1, 0, 254, &[1]);
        full.resize(6 + 254, 0);
        assert!(matches!(parse(&full), Parse::Frame { used: 260, .. }));
    }

    #[test]
    fn a_protocol_id_other_than_zero_is_bad_in_either_byte() {
        for pid in [1u16, 0x100, 0xFFFF] {
            assert_eq!(
                parse(&req(1, pid, 6, &[1, 3, 0, 0, 0, 1])),
                Parse::Bad("protocol id is not 0")
            );
        }
    }

    #[test]
    fn a_reply_frame_counts_the_unit_id_and_may_have_no_pdu() {
        assert_eq!(
            frame(0xABCD, 7, &[4, 2, 0x40, 0x20]),
            [0xAB, 0xCD, 0, 0, 0, 5, 7, 4, 2, 0x40, 0x20]
        );
        assert_eq!(frame(1, 255, &[]), [0, 1, 0, 0, 0, 1, 255]);
    }
}
