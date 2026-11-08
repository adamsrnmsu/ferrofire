//! Wire format of the bennu-simulink-provider <-> solver contract (see ../CONTRACT.md). Pure, no IPC.

use crate::model::Value;

pub const MAX_MSG_LEN: usize = 256;
pub const NUM_PUBLISH_POINTS_SHM_KEY: i32 = 10610;
pub const PUBLISH_POINTS_SHM_KEY: i32 = 10613;
pub const PUBLISH_SEM: &str = "publish_sem";
pub const UPDATES_SEM: &str = "updates_sem";
pub const UPDATES_FIFO: &str = "/tmp/updates_fifo";

/// "tag:BOOLEAN:1" / "tag:DOUBLE:1.500000", NUL padded to a 256-byte slot.
pub fn encode_dto(tag: &str, v: Value) -> [u8; MAX_MSG_LEN] {
    let s = match v {
        Value::Binary(b) => format!("{tag}:BOOLEAN:{}", b as u8),
        Value::Analog(d) => format!("{tag}:DOUBLE:{d:.6}"),
    };
    let mut slot = [0u8; MAX_MSG_LEN];
    let n = s.len().min(MAX_MSG_LEN - 1); // always leave a NUL
    slot[..n].copy_from_slice(&s.as_bytes()[..n]);
    slot
}

/// One 256-byte FIFO record "tag:TYPE:value" -> (tag, numeric value). Mirrors the C strtok/strtod/strtol
/// handling: BOOLEAN is parsed as base-2 (so "1"/"0"), anything else as a double.
pub fn decode_update(rec: &[u8]) -> Option<(String, f64)> {
    let end = rec.iter().position(|&b| b == 0).unwrap_or(rec.len());
    let s = std::str::from_utf8(&rec[..end]).ok()?;
    let mut it = s.split(':');
    let (tag, ty, val) = (it.next()?, it.next()?, it.next()?);
    if tag.is_empty() {
        return None;
    }
    let v = if ty == "BOOLEAN" {
        i64::from_str_radix(val.trim(), 2).ok()? as f64
    } else {
        val.trim().parse::<f64>().ok()?
    };
    Some((tag.to_string(), v))
}

/// publishPoints.txt: one tag per line (CRLF/blank tolerant).
pub fn parse_publish_points(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Content of the count segment: decimal text + NUL (segment is only 4 bytes).
pub fn encode_count(n: usize) -> Vec<u8> {
    let mut v = n.to_string().into_bytes();
    v.push(0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(slot: &[u8]) -> &str {
        std::str::from_utf8(&slot[..slot.iter().position(|&b| b == 0).unwrap()]).unwrap()
    }

    #[test]
    fn dto_encoding() {
        assert_eq!(
            text(&encode_dto("lowergate.open", Value::Binary(true))),
            "lowergate.open:BOOLEAN:1"
        );
        assert_eq!(
            text(&encode_dto("lock.height", Value::Analog(5.5))),
            "lock.height:DOUBLE:5.500000"
        );
        let long = "x".repeat(400);
        let s = encode_dto(&long, Value::Binary(false));
        assert_eq!(s[MAX_MSG_LEN - 1], 0);
    }

    #[test]
    fn update_decoding() {
        let mut rec = [0u8; MAX_MSG_LEN];
        rec[..24].copy_from_slice(b"lowervalve.open:BOOLEAN:");
        rec[24] = b'1';
        assert_eq!(decode_update(&rec), Some(("lowervalve.open".into(), 1.0)));
        let mut rec = [0u8; MAX_MSG_LEN];
        let m = b"lock.height:DOUBLE:2.5";
        rec[..m.len()].copy_from_slice(m);
        assert_eq!(decode_update(&rec), Some(("lock.height".into(), 2.5)));
        assert_eq!(decode_update(&[0u8; MAX_MSG_LEN]), None);
        assert_eq!(decode_update(b"junk"), None);
    }

    #[test]
    fn points_and_count() {
        assert_eq!(parse_publish_points("a.b\r\n\nc.d\n"), vec!["a.b", "c.d"]);
        assert_eq!(encode_count(15), b"15\0");
    }
}
