//! DNP3 data link layer (IEEE 1815 clause 9): FT3 frames of a 10-byte header and up to 250 user bytes in 16-byte blocks, each
//! block closed by a CRC. Pure functions over byte slices, shared by the outstation and the later master.

/// Largest user-data field of one frame (the `LEN` octet counts 5 header octets plus this).
pub const MAX_USER: usize = 250;
const START: [u8; 2] = [0x05, 0x64];

/// CRC-16/DNP: polynomial 0x3D65 reflected, initial 0, complemented.
pub fn crc(data: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for &b in data {
        c ^= u16::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
                (c >> 1) ^ 0xA6BC
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Bytes a frame with `user` data octets occupies on the wire.
pub fn wire_len(user: usize) -> usize {
    10 + user + 2 * user.div_ceil(16)
}

/// Control octet fields.
pub const DIR: u8 = 0x80;
pub const PRM: u8 = 0x40;
pub const FCB: u8 = 0x20;
pub const FCV: u8 = 0x10;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Frame {
    pub ctrl: u8,
    pub dst: u16,
    pub src: u16,
    pub user: Vec<u8>,
}

impl Frame {
    pub fn func(&self) -> u8 {
        self.ctrl & 0x0F
    }
    /// A primary-station frame (a request): the PRM bit.
    pub fn primary(&self) -> bool {
        self.ctrl & PRM != 0
    }
}

/// Encode a frame; `user` longer than `MAX_USER` is cut (callers segment first).
pub fn encode(ctrl: u8, dst: u16, src: u16, user: &[u8]) -> Vec<u8> {
    let user = &user[..user.len().min(MAX_USER)];
    let mut out = Vec::with_capacity(wire_len(user.len()));
    out.extend_from_slice(&START);
    out.push((5 + user.len()) as u8);
    out.push(ctrl);
    out.extend_from_slice(&dst.to_le_bytes());
    out.extend_from_slice(&src.to_le_bytes());
    let h = crc(&out);
    out.extend_from_slice(&h.to_le_bytes());
    for block in user.chunks(16) {
        out.extend_from_slice(block);
        out.extend_from_slice(&crc(block).to_le_bytes());
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parse {
    /// The buffer holds a frame prefix; at least this many more bytes are needed.
    Need(usize),
    /// A whole frame and how many bytes it used.
    Frame(Frame, usize),
    /// A framing violation: the stream cannot be trusted any more.
    Bad(&'static str),
}

/// The start bytes, checked as far as `buf` goes.
fn start_ok(buf: &[u8]) -> Result<(), Parse> {
    for (i, want) in START.iter().enumerate() {
        match buf.get(i) {
            Some(b) if b != want => return Err(Parse::Bad("bad start bytes")),
            None => return Err(Parse::Need(10 - buf.len())),
            _ => {}
        }
    }
    Ok(())
}

/// The user data of the blocks that follow the header, each CRC checked.
fn user_data(blocks: &[u8]) -> Result<Vec<u8>, Parse> {
    let mut user = Vec::with_capacity(blocks.len());
    for block in blocks.chunks(18) {
        let (data, sum) = block.split_at(block.len() - 2);
        if crc(data) != u16::from_le_bytes([sum[0], sum[1]]) {
            return Err(Parse::Bad("data CRC"));
        }
        user.extend_from_slice(data);
    }
    Ok(user)
}

fn parse_checked(buf: &[u8]) -> Result<(Frame, usize), Parse> {
    start_ok(buf)?;
    if buf.len() < 10 {
        return Err(Parse::Need(10 - buf.len()));
    }
    if crc(&buf[..8]) != u16::from_le_bytes([buf[8], buf[9]]) {
        return Err(Parse::Bad("header CRC"));
    }
    let user_len = (buf[2] as usize)
        .checked_sub(5)
        .ok_or(Parse::Bad("length below 5"))?;
    let total = wire_len(user_len);
    if buf.len() < total {
        return Err(Parse::Need(total - buf.len()));
    }
    let user = user_data(&buf[10..total])?;
    let (dst, src) = (
        u16::from_le_bytes([buf[4], buf[5]]),
        u16::from_le_bytes([buf[6], buf[7]]),
    );
    Ok((
        Frame {
            ctrl: buf[3],
            dst,
            src,
            user,
        },
        total,
    ))
}

/// Parse one frame from the start of `buf`.
pub fn parse(buf: &[u8]) -> Parse {
    match parse_checked(buf) {
        Ok((f, used)) => Parse::Frame(f, used),
        Err(p) => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard_check_value() {
        // IEEE 1815 / IEC 60870-5-1: CRC of "123456789" is 0xEA82.
        assert_eq!(crc(b"123456789"), 0xEA82);
        assert_eq!(crc(&[]), 0xFFFF);
    }

    #[test]
    fn encodes_the_stock_frame_for_a_link_status_reply() {
        // Observed on the stock outstation: LINK_STATUS from 10 to 1 is `05 64 05 0b 01 00 0a 00 <crc>`.
        let f = encode(0x0B, 1, 10, &[]);
        assert_eq!(&f[..8], &[0x05, 0x64, 0x05, 0x0B, 0x01, 0x00, 0x0A, 0x00]);
        assert_eq!(f.len(), 10);
        assert_eq!(u16::from_le_bytes([f[8], f[9]]), crc(&f[..8]));
    }

    #[test]
    fn round_trips_every_user_length() {
        for n in 0..=MAX_USER {
            let user: Vec<u8> = (0..n).map(|i| i as u8 ^ 0x5A).collect();
            let wire = encode(0x44, 1, 10, &user);
            assert_eq!(wire.len(), wire_len(n));
            let Parse::Frame(f, used) = parse(&wire) else {
                panic!("n={n}")
            };
            assert_eq!(used, wire.len());
            assert_eq!(f.user, user);
            assert_eq!((f.ctrl, f.dst, f.src), (0x44, 1, 10));
        }
    }

    #[test]
    fn oversized_user_data_is_cut_to_a_legal_frame() {
        let wire = encode(0x44, 1, 10, &[7; 300]);
        assert!(matches!(parse(&wire), Parse::Frame(f, _) if f.user.len() == MAX_USER));
    }

    #[test]
    fn a_prefix_asks_for_exactly_the_missing_bytes() {
        let wire = encode(
            0xC4,
            10,
            1,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17],
        );
        for cut in 0..wire.len() {
            assert_eq!(
                parse(&wire[..cut]),
                Parse::Need(if cut < 10 { 10 - cut } else { wire.len() - cut }),
                "cut={cut}"
            );
        }
    }

    #[test]
    fn framing_violations_are_named() {
        let good = encode(0xC4, 10, 1, &[1, 2, 3]);
        let mut x = good.clone();
        x[0] = 0x06;
        assert_eq!(parse(&x), Parse::Bad("bad start bytes"));
        let mut x = good.clone();
        x[1] = 0x65;
        assert_eq!(parse(&x), Parse::Bad("bad start bytes"));
        let mut x = good.clone();
        x[3] ^= 1;
        assert_eq!(parse(&x), Parse::Bad("header CRC"));
        let mut x = good.clone();
        x[10] ^= 1;
        assert_eq!(parse(&x), Parse::Bad("data CRC"));
        let mut x = vec![0x05, 0x64, 4, 0xC4, 10, 0, 1, 0];
        let c = crc(&x);
        x.extend_from_slice(&c.to_le_bytes());
        assert_eq!(parse(&x), Parse::Bad("length below 5"));
        assert_eq!(parse(&[0x05]), Parse::Need(9));
        assert_eq!(parse(&[]), Parse::Need(10));
        assert_eq!(parse(&[0x06]), Parse::Bad("bad start bytes"));
    }

    #[test]
    fn control_fields() {
        let f = Frame {
            ctrl: 0xC9,
            dst: 0,
            src: 0,
            user: vec![],
        };
        assert_eq!(f.func(), 9);
        assert!(f.primary());
        assert!(!Frame { ctrl: 0x0B, ..f }.primary());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        let mut x: u32 = 12345;
        for _ in 0..20000 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let n = (x >> 24) as usize % 40;
            let buf: Vec<u8> = (0..n).map(|i| (x >> (i % 24)) as u8).collect();
            let _ = parse(&buf);
        }
    }
}
