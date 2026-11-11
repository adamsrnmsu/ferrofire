//! DNP3 application layer (IEEE 1815 clause 4): fragments, object headers and internal indications. Pure parsing and
//! encoding, no state, shared by the outstation and the later master.

/// Largest application fragment either side sends or accepts (opendnp3 `DEFAULT_MAX_APDU_SIZE`).
pub const MAX_APDU: usize = 2048;

/// Application control octet.
pub const FIR: u8 = 0x80;
pub const FIN: u8 = 0x40;
pub const CON: u8 = 0x20;
pub const UNS: u8 = 0x10;

/// Function codes the outstation answers.
pub mod func {
    pub const CONFIRM: u8 = 0;
    pub const READ: u8 = 1;
    pub const WRITE: u8 = 2;
    pub const SELECT: u8 = 3;
    pub const OPERATE: u8 = 4;
    pub const DIRECT_OPERATE: u8 = 5;
    pub const DIRECT_OPERATE_NO_ACK: u8 = 6;
    pub const IMMED_FREEZE: u8 = 7;
    pub const ASSIGN_CLASS: u8 = 22;
    pub const DELAY_MEASURE: u8 = 23;
    pub const RECORD_CURRENT_TIME: u8 = 24;
    pub const RESPONSE: u8 = 129;
}

/// Internal indications as one number: IIN1 in the low octet, IIN2 in the high one (the wire order).
pub mod iin {
    pub const BROADCAST: u16 = 0x0001;
    pub const CLASS1: u16 = 0x0002;
    pub const NEED_TIME: u16 = 0x0010;
    pub const RESTART: u16 = 0x0080;
    pub const NO_FUNC: u16 = 0x0100;
    pub const UNKNOWN_OBJECT: u16 = 0x0200;
    pub const PARAM_ERROR: u16 = 0x0400;
    pub const OVERFLOW: u16 = 0x0800;
}

/// Control status codes (clause 4.2.2.1.5.1).
pub mod status {
    pub const SUCCESS: u8 = 0;
    pub const NO_SELECT: u8 = 2;
    pub const OUT_OF_RANGE: u8 = 12;
}

/// A request: control octet, function code, and the object headers and data that follow.
#[derive(Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub ctl: u8,
    pub func: u8,
    pub objects: &'a [u8],
}

impl<'a> Request<'a> {
    pub fn parse(fragment: &'a [u8]) -> Option<Request<'a>> {
        let (&ctl, rest) = fragment.split_first()?;
        let (&func, objects) = rest.split_first()?;
        Some(Request { ctl, func, objects })
    }
    pub fn seq(&self) -> u8 {
        self.ctl & 0x0F
    }
    /// A request that is one whole fragment (the only kind the outstation takes).
    pub fn single(&self) -> bool {
        self.ctl & (FIR | FIN) == FIR | FIN
    }
    pub fn unsolicited(&self) -> bool {
        self.ctl & UNS != 0
    }
}

/// A response fragment: control (`flags` are FIR/FIN/CON), RESPONSE, IIN, then `objects`.
pub fn response(seq: u8, flags: u8, iin: u16, objects: &[u8]) -> Vec<u8> {
    let mut out = vec![(flags & (FIR | FIN | CON)) | (seq & 0x0F), func::RESPONSE];
    out.extend_from_slice(&iin.to_le_bytes());
    out.extend_from_slice(objects);
    out
}

/// Bytes before the objects in a response fragment.
pub const RESPONSE_HEADER: usize = 4;

/// What follows an object header: which points it addresses.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Range {
    All,
    /// Qualifier 0x00 / 0x01: start and stop index.
    StartStop(u32, u32),
    /// Qualifier 0x07 / 0x08: a limit on how many to return.
    Count(u32),
    /// Qualifier 0x17 / 0x28 on a READ: a list of indices.
    Indices(Vec<u32>),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Header {
    pub group: u8,
    pub var: u8,
    pub range: Range,
}

/// A request that cannot be parsed to its end.
#[derive(Debug, PartialEq, Eq)]
pub enum Malformed {
    Truncated,
    Qualifier(u8),
}

fn bytes(buf: &[u8], at: usize, n: usize) -> Result<&[u8], Malformed> {
    buf.get(at..at + n).ok_or(Malformed::Truncated)
}

fn le(b: &[u8]) -> u32 {
    b.iter().rev().fold(0, |a, &x| a << 8 | u32::from(x))
}

/// `w`-octet start and stop indices (qualifier 0x00 / 0x01).
fn start_stop(body: &[u8], w: usize) -> Result<(Range, usize), Malformed> {
    Ok((
        Range::StartStop(le(bytes(body, 0, w)?), le(bytes(body, w, w)?)),
        2 * w,
    ))
}

/// A `w`-octet limit (qualifier 0x07 / 0x08).
fn limit(body: &[u8], w: usize) -> Result<(Range, usize), Malformed> {
    Ok((Range::Count(le(bytes(body, 0, w)?)), w))
}

/// A `w`-octet count of `w`-octet indices (qualifier 0x17 / 0x28).
fn list(body: &[u8], w: usize) -> Result<(Range, usize), Malformed> {
    let n = le(bytes(body, 0, w)?) as usize;
    let idx = bytes(body, w, n * w)?;
    Ok((Range::Indices(idx.chunks(w).map(le).collect()), w + n * w))
}

/// How a qualifier addresses points: the form of its range and the width in octets of its numbers.
#[derive(Clone, Copy)]
enum Form {
    All,
    StartStop,
    Limit,
    List,
}

fn qualifier(q: u8) -> Option<(Form, usize)> {
    match q {
        0x06 => Some((Form::All, 0)),
        0x00 => Some((Form::StartStop, 1)),
        0x01 => Some((Form::StartStop, 2)),
        0x07 => Some((Form::Limit, 1)),
        0x08 => Some((Form::Limit, 2)),
        0x17 => Some((Form::List, 1)),
        0x28 => Some((Form::List, 2)),
        _ => None,
    }
}

fn range_of(form: Form, body: &[u8], w: usize) -> Result<(Range, usize), Malformed> {
    match form {
        Form::All => Ok((Range::All, 0)),
        Form::StartStop => start_stop(body, w),
        Form::Limit => limit(body, w),
        Form::List => list(body, w),
    }
}

/// Parse one object header of a READ request and return it with the bytes it used.
pub fn read_header(buf: &[u8]) -> Result<(Header, usize), Malformed> {
    let head = bytes(buf, 0, 3)?;
    let (form, w) = qualifier(head[2]).ok_or(Malformed::Qualifier(head[2]))?;
    let (range, used) = range_of(form, &buf[3..], w)?;
    Ok((
        Header {
            group: head[0],
            var: head[1],
            range,
        },
        3 + used,
    ))
}

/// Every object header of a READ request.
pub fn read_headers(mut buf: &[u8]) -> Result<Vec<Header>, Malformed> {
    let mut out = vec![];
    while !buf.is_empty() {
        let (h, used) = read_header(buf)?;
        out.push(h);
        buf = &buf[used..];
    }
    Ok(out)
}

/// An object in a control request: its index prefix and where its body and status octet are in the request.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Control {
    pub group: u8,
    pub var: u8,
    pub index: u16,
    /// Object body without the trailing status octet.
    pub body: Vec<u8>,
    /// Offset of the status octet in the request's object bytes.
    pub status_at: usize,
}

/// Size of a control object (body plus its status octet), `None` when the outstation does not know the object.
fn control_size(group: u8, var: u8) -> Option<usize> {
    match (group, var) {
        (12, 1) => Some(11),
        (41, 1) | (41, 3) => Some(5),
        (41, 2) => Some(3),
        (41, 4) => Some(9),
        _ => None,
    }
}

/// How a control request failed to parse.
#[derive(Debug, PartialEq, Eq)]
pub enum ControlError {
    Malformed,
    /// An object nothing here has heard of (group, variation).
    Unknown,
    /// A known object that is not a control object.
    NotControl,
}

/// Groups opendnp3 knows, so that a control request naming one is a parameter error and not an unknown object.
pub const KNOWN_GROUPS: &[u8] = &[
    1, 2, 3, 4, 10, 11, 20, 21, 22, 23, 30, 32, 40, 42, 50, 51, 52, 60, 80,
];

/// A prefixed control header: the objects' group, variation, index width, how many there are and how big each is with its prefix.
struct ControlHeader {
    group: u8,
    var: u8,
    width: usize,
    count: usize,
    item: usize,
    /// Octets of the header itself (group, variation, qualifier, count).
    len: usize,
}

fn control_header(buf: &[u8]) -> Result<ControlHeader, ControlError> {
    let [group, var, qual] = buf
        .get(..3)
        .and_then(|s| <[u8; 3]>::try_from(s).ok())
        .ok_or(ControlError::Malformed)?;
    let Some(size) = control_size(group, var) else {
        return Err(if KNOWN_GROUPS.contains(&group) {
            ControlError::NotControl
        } else {
            ControlError::Unknown
        });
    };
    let width = match qual {
        0x17 => 1,
        0x28 => 2,
        _ => return Err(ControlError::Malformed),
    };
    let count = le(buf.get(3..3 + width).ok_or(ControlError::Malformed)?) as usize;
    Ok(ControlHeader {
        group,
        var,
        width,
        count,
        item: width + size,
        len: 3 + width,
    })
}

/// Parse the objects of SELECT / OPERATE / DIRECT OPERATE: prefixed (qualifier 0x17 / 0x28) control objects.
pub fn control_objects(mut buf: &[u8]) -> Result<Vec<Control>, ControlError> {
    let (mut out, mut base) = (vec![], 0);
    while !buf.is_empty() {
        let h = control_header(buf)?;
        let total = h.len + h.count * h.item;
        let items = buf.get(h.len..total).ok_or(ControlError::Malformed)?;
        for (i, it) in items.chunks(h.item).enumerate() {
            out.push(Control {
                group: h.group,
                var: h.var,
                index: le(&it[..h.width]) as u16,
                body: it[h.width..h.item - 1].to_vec(),
                status_at: base + h.len + i * h.item + h.item - 1,
            });
        }
        base += total;
        buf = &buf[total..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_fields() {
        let r = Request::parse(&[0xC3, 1, 60, 1, 6]).unwrap();
        assert_eq!((r.seq(), r.func, r.objects), (3, 1, &[60, 1, 6][..]));
        assert!(r.single() && !r.unsolicited());
        assert_eq!(Request::parse(&[0xC3]), None);
        assert_eq!(Request::parse(&[]), None);
        assert!(!Request::parse(&[0x80, 1]).unwrap().single());
        assert!(!Request::parse(&[0x40, 1]).unwrap().single());
        assert!(Request::parse(&[0xD0, 1]).unwrap().unsolicited());
    }

    #[test]
    fn response_header_is_ctl_function_and_little_endian_iin() {
        assert_eq!(
            response(3, FIR | FIN, 0x0410, &[9]),
            vec![0xC3, 129, 0x10, 0x04, 9]
        );
        assert_eq!(response(0x1F, FIR | CON, 0, &[]), vec![0xAF, 129, 0, 0]);
        assert_eq!(RESPONSE_HEADER, response(0, 0, 0, &[]).len());
    }

    fn h(g: u8, v: u8, range: Range) -> Header {
        Header {
            group: g,
            var: v,
            range,
        }
    }

    #[test]
    fn every_read_qualifier() {
        let ok = |b: &[u8]| read_header(b);
        assert_eq!(ok(&[1, 2, 6]), Ok((h(1, 2, Range::All), 3)));
        assert_eq!(
            ok(&[1, 2, 0, 3, 9]),
            Ok((h(1, 2, Range::StartStop(3, 9)), 5))
        );
        assert_eq!(
            ok(&[1, 2, 1, 44, 1, 45, 1]),
            Ok((h(1, 2, Range::StartStop(300, 301)), 7))
        );
        assert_eq!(ok(&[2, 0, 7, 5]), Ok((h(2, 0, Range::Count(5)), 4)));
        assert_eq!(ok(&[2, 0, 8, 0, 1]), Ok((h(2, 0, Range::Count(256)), 5)));
        assert_eq!(
            ok(&[1, 2, 0x17, 2, 10, 2]),
            Ok((h(1, 2, Range::Indices(vec![10, 2])), 6))
        );
        assert_eq!(
            ok(&[1, 2, 0x28, 2, 0, 1, 1, 255, 255, 9]),
            Ok((h(1, 2, Range::Indices(vec![257, 65535])), 9))
        );
        assert_eq!(
            ok(&[1, 2, 0x17, 0]),
            Ok((h(1, 2, Range::Indices(vec![])), 4))
        );
    }

    #[test]
    fn truncation_and_unknown_qualifiers_are_malformed() {
        for b in [
            &[][..],
            &[1],
            &[1, 2],
            &[1, 2, 0],
            &[1, 2, 0, 1],
            &[1, 2, 1, 1, 0, 1],
            &[1, 2, 7],
            &[1, 2, 8, 1],
            &[1, 2, 0x17],
            &[1, 2, 0x17, 2, 1],
            &[1, 2, 0x28, 1, 0, 1],
            &[1, 2, 0x28, 255, 255, 1],
        ] {
            assert_eq!(read_header(b), Err(Malformed::Truncated), "{b:?}");
        }
        assert_eq!(
            read_header(&[1, 2, 0x99, 1]),
            Err(Malformed::Qualifier(0x99))
        );
        assert_eq!(read_header(&[1, 2, 0x0B]), Err(Malformed::Qualifier(0x0B)));
    }

    #[test]
    fn several_headers_and_a_bad_tail() {
        assert_eq!(
            read_headers(&[60, 2, 6, 60, 1, 6]),
            Ok(vec![h(60, 2, Range::All), h(60, 1, Range::All)])
        );
        assert_eq!(read_headers(&[]), Ok(vec![]));
        assert_eq!(read_headers(&[60, 2, 6, 1]), Err(Malformed::Truncated));
    }

    const CROB: [u8; 11] = [3, 1, 100, 0, 0, 0, 0, 0, 0, 0, 0];

    fn crob_request(idx: u8) -> Vec<u8> {
        let mut v = vec![12, 1, 0x17, 1, idx];
        v.extend_from_slice(&CROB);
        v
    }

    #[test]
    fn control_objects_carry_index_body_and_status_offset() {
        let c = control_objects(&crob_request(5)).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].group, c[0].var, c[0].index), (12, 1, 5));
        assert_eq!(c[0].body, CROB[..10].to_vec());
        assert_eq!(c[0].status_at, 15);
        let two = [crob_request(5), crob_request(9)].concat();
        let c = control_objects(&two).unwrap();
        assert_eq!(c.iter().map(|x| x.index).collect::<Vec<_>>(), vec![5, 9]);
        assert_eq!(c[1].status_at, two.len() - 1);
        let mut multi = vec![41, 3, 0x28, 2, 0];
        for i in 0..2u16 {
            multi.extend_from_slice(&(300 + i).to_le_bytes());
            multi.extend_from_slice(&[0, 0, 0xC0, 0x3F, 0]);
        }
        let c = control_objects(&multi).unwrap();
        assert_eq!(
            c.iter().map(|x| x.index).collect::<Vec<_>>(),
            vec![300, 301]
        );
        assert_eq!(c[0].body.len(), 4);
        assert_eq!((c[0].status_at, c[1].status_at), (11, 18));
        assert_eq!(control_objects(&[]), Ok(vec![]));
    }

    #[test]
    fn every_control_variation_has_its_size() {
        for (g, v, size) in [(12, 1, 11), (41, 1, 5), (41, 2, 3), (41, 3, 5), (41, 4, 9)] {
            let mut b = vec![g, v, 0x17, 1, 7];
            b.extend(vec![0; size]);
            let c = control_objects(&b).unwrap();
            assert_eq!(
                (c[0].body.len(), c[0].status_at),
                (size - 1, 4 + size),
                "g{g}v{v}"
            );
            b.pop();
            assert_eq!(control_objects(&b), Err(ControlError::Malformed));
        }
    }

    #[test]
    fn control_errors() {
        assert_eq!(
            control_objects(&[12, 2, 0x17, 1, 5, 0]),
            Err(ControlError::Unknown)
        );
        assert_eq!(
            control_objects(&[99, 1, 0x17, 1, 5, 0]),
            Err(ControlError::Unknown)
        );
        assert_eq!(
            control_objects(&[10, 2, 0x17, 1, 5, 0x81]),
            Err(ControlError::NotControl)
        );
        assert_eq!(
            control_objects(&[12, 1, 0x06]),
            Err(ControlError::Malformed)
        );
        assert_eq!(control_objects(&[12, 1]), Err(ControlError::Malformed));
        assert_eq!(
            control_objects(&[12, 1, 0x17]),
            Err(ControlError::Malformed)
        );
        assert_eq!(
            control_objects(&[12, 1, 0x28, 1]),
            Err(ControlError::Malformed)
        );
        let mut huge = vec![12, 1, 0x28, 255, 255];
        huge.extend(vec![0; 20]);
        assert_eq!(control_objects(&huge), Err(ControlError::Malformed));
    }
}
