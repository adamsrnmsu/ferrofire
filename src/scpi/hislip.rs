//! The HiSLIP (IVI-6.1) wire format, shared by the field device's server (`fd::proto::scpi::hislip`) and the `ferrofire scpi` client:
//! the 16-byte header (`HS`, type, control code, message parameter, payload length), the message types, and `Reader`, which cuts a
//! byte stream into frames.

use t::{ERROR, FATAL_ERROR};

pub const HEADER: usize = 16;
/// The largest payload accepted in one frame; sent to the client as the server's maximum message size.
pub const MAX_MESSAGE: u64 = 1 << 20;
/// Message types (IVI-6.1 table 5).
pub mod t {
    pub const INITIALIZE: u8 = 0;
    pub const INITIALIZE_RESPONSE: u8 = 1;
    pub const FATAL_ERROR: u8 = 2;
    pub const ERROR: u8 = 3;
    pub const ASYNC_LOCK: u8 = 4;
    pub const ASYNC_LOCK_RESPONSE: u8 = 5;
    pub const DATA: u8 = 6;
    pub const DATA_END: u8 = 7;
    pub const DEVICE_CLEAR_COMPLETE: u8 = 8;
    pub const DEVICE_CLEAR_ACK: u8 = 9;
    pub const ASYNC_REMOTE_LOCAL: u8 = 10;
    pub const ASYNC_REMOTE_LOCAL_RESPONSE: u8 = 11;
    pub const TRIGGER: u8 = 12;
    pub const INTERRUPTED: u8 = 13;
    pub const ASYNC_MAX_SIZE: u8 = 15;
    pub const ASYNC_MAX_SIZE_RESPONSE: u8 = 16;
    pub const ASYNC_INITIALIZE: u8 = 17;
    pub const ASYNC_INITIALIZE_RESPONSE: u8 = 18;
    pub const ASYNC_DEVICE_CLEAR: u8 = 19;
    pub const ASYNC_STATUS_QUERY: u8 = 21;
    pub const ASYNC_STATUS_RESPONSE: u8 = 22;
    pub const ASYNC_DEVICE_CLEAR_ACK: u8 = 23;
    pub const ASYNC_LOCK_INFO: u8 = 24;
    pub const ASYNC_LOCK_INFO_RESPONSE: u8 = 25;
}
/// Control code bit 0 of Data, DataEND, Trigger and AsyncStatusQuery: the client has read the last response to the end.
pub const RMT_DELIVERED: u8 = 1;

/// One HiSLIP message.
#[derive(Debug, PartialEq, Clone)]
pub struct Frame {
    pub ty: u8,
    pub ctl: u8,
    pub param: u32,
    pub payload: Vec<u8>,
}

pub fn frame(ty: u8, ctl: u8, param: u32, payload: &[u8]) -> Frame {
    Frame {
        ty,
        ctl,
        param,
        payload: payload.to_vec(),
    }
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = b"HS".to_vec();
        b.extend([self.ty, self.ctl]);
        b.extend(self.param.to_be_bytes());
        b.extend((self.payload.len() as u64).to_be_bytes());
        b.extend(&self.payload);
        b
    }
}

/// A FatalError: the session cannot go on. Codes: 0 unidentified, 1 poorly formed header, 2 a channel is missing, 3 bad
/// initialization sequence, 4 server refused the connection.
#[derive(Debug, PartialEq)]
pub struct Fatal(pub u8, pub &'static str);

impl Fatal {
    pub fn frame(&self) -> Frame {
        frame(FATAL_ERROR, self.0, 0, self.1.as_bytes())
    }
}

/// An Error (the session goes on): 0 unidentified, 1 unrecognized message type, 2 unrecognized control code, 4 message too large.
pub fn error(code: u8, text: &str) -> Frame {
    frame(ERROR, code, 0, text.as_bytes())
}

/// Bytes from a channel, cut into frames.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
}

impl Reader {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame; `None` while more bytes are needed. A header that is not one (`HS`, a payload within `MAX_MESSAGE`) is
    /// fatal at once: after it there is no telling where the next message starts.
    pub fn take(&mut self) -> Result<Option<Frame>, Fatal> {
        let bad = || Fatal(1, "poorly formed message header");
        if self.buf.len() >= 2 && &self.buf[..2] != b"HS" {
            return Err(bad());
        }
        if self.buf.len() < HEADER {
            return Ok(None);
        }
        let len = u64::from_be_bytes(self.buf[8..16].try_into().expect("8 bytes"));
        if len > MAX_MESSAGE {
            return Err(Fatal(1, "payload longer than the maximum message size"));
        }
        let total = HEADER + len as usize;
        if self.buf.len() < total {
            return Ok(None);
        }
        let param = u32::from_be_bytes(self.buf[4..8].try_into().expect("4 bytes"));
        let f = frame(self.buf[2], self.buf[3], param, &self.buf[HEADER..total]);
        self.buf.drain(..total);
        Ok(Some(f))
    }
}

#[cfg(test)]
mod tests {
    use super::t::*;
    use super::*;

    #[test]
    fn frames_are_cut_out_of_the_byte_stream_and_encode_back() {
        let f = frame(DATA_END, 1, 0xFFFF_FF00, b"*IDN?\n");
        let bytes = f.encode();
        assert_eq!(&bytes[..4], b"HS\x07\x01");
        assert_eq!(&bytes[4..8], &[0xFF, 0xFF, 0xFF, 0x00]);
        assert_eq!(&bytes[8..16], &6u64.to_be_bytes());
        let mut r = Reader::default();
        for b in &bytes[..bytes.len() - 1] {
            r.push(&[*b]);
            assert_eq!(r.take(), Ok(None));
        }
        r.push(&bytes[bytes.len() - 1..]);
        r.push(&bytes);
        assert_eq!(r.take(), Ok(Some(f.clone())));
        assert_eq!(r.take(), Ok(Some(f)));
        assert_eq!(r.take(), Ok(None));
    }

    #[test]
    fn a_bad_prologue_or_a_payload_over_the_maximum_is_fatal() {
        let mut r = Reader::default();
        r.push(b"H");
        assert_eq!(r.take(), Ok(None), "one byte may still be HS");
        r.push(b"X");
        assert_eq!(r.take().unwrap_err().0, 1);
        let mut r = Reader::default();
        r.push(b"GET / HTTP/1.1\r\n");
        assert_eq!(r.take().unwrap_err().0, 1);
        let mut big = frame(DATA, 0, 0, &[]).encode();
        big[8..16].copy_from_slice(&(MAX_MESSAGE + 1).to_be_bytes());
        let mut r = Reader::default();
        r.push(&big);
        assert_eq!(r.take().unwrap_err().0, 1);
        big[8..16].copy_from_slice(&MAX_MESSAGE.to_be_bytes());
        let mut r = Reader::default();
        r.push(&big);
        assert_eq!(
            r.take(),
            Ok(None),
            "the maximum itself is allowed: wait for its payload"
        );
        assert_eq!(Fatal(2, "x").frame(), frame(FATAL_ERROR, 2, 0, b"x"));
        assert_eq!(error(1, "y"), frame(ERROR, 1, 0, b"y"));
    }
}
