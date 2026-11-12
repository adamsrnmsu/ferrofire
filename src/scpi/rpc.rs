//! ONC RPC (RFC 5531) record marking and XDR (RFC 4506) as far as VXI-11 needs them, shared by the field device's server
//! (`fd::proto::scpi::vxi11`) and the `ferrofire scpi` client: `Xdr` reads, `put` / `put_opaque` write, `Records` cuts a TCP stream into
//! RPC records and `record` frames one.

pub const PMAP_PROG: u32 = 100_000;
pub const CORE_PROG: u32 = 0x0607AF;
pub const ABORT_PROG: u32 = 0x0607B0;
/// The largest RPC record accepted (a device_write of 1 MiB and its headers fit).
pub const MAX_RECORD: usize = 2 << 20;

/// A reader over XDR-encoded bytes; every getter is `None` when the bytes run out.
pub struct Xdr<'a>(pub &'a [u8]);

impl<'a> Xdr<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    pub fn i32(&mut self) -> Option<i32> {
        self.u32().map(|x| x as i32)
    }

    /// Variable-length opaque data (or a string): a length, the bytes, padding to a multiple of 4. At most `max` bytes.
    pub fn opaque(&mut self, max: usize) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        if len > max {
            return None;
        }
        let data = self.take(len)?;
        self.take(len.wrapping_neg() & 3)?;
        Some(data)
    }
}

pub fn put(out: &mut Vec<u8>, x: u32) {
    out.extend(x.to_be_bytes());
}

pub fn put_opaque(out: &mut Vec<u8>, data: &[u8]) {
    put(out, data.len() as u32);
    out.extend(data);
    out.extend(std::iter::repeat_n(0, data.len().wrapping_neg() & 3));
}

/// The TCP stream cut into RPC records (RFC 5531 section 11, record marking: fragments with a length and a last-fragment bit).
#[derive(Default)]
pub struct Records {
    buf: Vec<u8>,
    record: Vec<u8>,
}

impl Records {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole record; `None` while more is needed, `Err` for one longer than `MAX_RECORD`.
    pub fn take(&mut self) -> Result<Option<Vec<u8>>, &'static str> {
        loop {
            let Some(head) = self.buf.first_chunk::<4>() else {
                return Ok(None);
            };
            let head = u32::from_be_bytes(*head);
            let len = (head & 0x7FFF_FFFF) as usize;
            if self.record.len() + len > MAX_RECORD {
                return Err("RPC record too long");
            }
            if self.buf.len() < 4 + len {
                return Ok(None);
            }
            self.record.extend(&self.buf[4..4 + len]);
            self.buf.drain(..4 + len);
            if head >> 31 == 1 {
                return Ok(Some(std::mem::take(&mut self.record)));
            }
        }
    }
}

/// A record as one last fragment.
pub fn record(body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() as u32) | 0x8000_0000).to_be_bytes().to_vec();
    out.extend(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdr_opaque_is_padded_to_four_bytes_and_bounded() {
        let mut out = vec![];
        for (d, total) in [(&b""[..], 4), (b"a", 8), (b"abcd", 8), (b"abcde", 12)] {
            out.clear();
            put_opaque(&mut out, d);
            assert_eq!(out.len(), total);
            assert_eq!(Xdr(&out).opaque(10), Some(d));
        }
        out.clear();
        put_opaque(&mut out, b"abcdef");
        assert_eq!(Xdr(&out).opaque(5), None, "over the maximum");
        assert_eq!(Xdr(&out[..10]).opaque(10), None, "cut short");
        assert_eq!(Xdr(&out[..8]).opaque(10), None, "padding missing");
        assert_eq!(Xdr(&[0, 0, 0, 1][..]).u32(), Some(1));
        assert_eq!(Xdr(&[0xFF; 4][..]).i32(), Some(-1));
        assert_eq!(Xdr(&[0; 3][..]).u32(), None);
    }

    #[test]
    fn records_are_cut_from_fragments() {
        let mut r = Records::default();
        let mut stream = vec![0, 0, 0, 2, 1, 2];
        stream.extend(record(b"xyz"));
        stream.extend(record(b""));
        for b in &stream[..stream.len() - 1] {
            r.push(&[*b]);
            let _ = r.take();
        }
        let mut r = Records::default();
        r.push(&stream);
        assert_eq!(
            r.take(),
            Ok(Some(vec![1, 2, b'x', b'y', b'z'])),
            "two fragments, one record"
        );
        assert_eq!(r.take(), Ok(Some(vec![])));
        assert_eq!(r.take(), Ok(None));
        r.push(&[0, 0, 0, 5, 1]);
        assert_eq!(r.take(), Ok(None), "wait for the rest of the fragment");
        assert_eq!(record(b"ab"), [0x80, 0, 0, 2, b'a', b'b']);
        let mut r = Records::default();
        r.push(&(MAX_RECORD as u32 + 1).to_be_bytes());
        assert!(r.take().is_err());
        let mut r = Records::default();
        r.push(&[0, 0x10, 0, 0]);
        r.push(&vec![0; 0x100000]);
        assert_eq!(r.take(), Ok(None), "a fragment of 1 MiB is fine");
        r.push(&[0, 0x10, 0, 0, 0]);
        r.push(&vec![0; 0x100000]);
        assert!(r.take().is_ok());
        r.push(&[0, 0x10, 0, 0]);
        assert!(r.take().is_err(), "but not three of them in one record");
    }
}
