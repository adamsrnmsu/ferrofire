//! DNP3 transport function (IEEE 1815 clause 8): a one-octet header (FIN, FIR, 6-bit sequence) in front of up to 249 bytes of
//! application fragment. `Reassembler` rebuilds fragments from segments; `segment` cuts one into segments.

/// Application bytes per segment: a link frame carries 250 user bytes, one of them the transport header.
pub const SEGMENT: usize = 249;
const FIN: u8 = 0x80;
const FIR: u8 = 0x40;

/// Cut `fragment` into transport segments; `seq` is the next sequence number and is advanced past the last segment.
pub fn segment(fragment: &[u8], seq: &mut u8) -> Vec<Vec<u8>> {
    let n = fragment.len().div_ceil(SEGMENT).max(1);
    let mut chunks: Vec<&[u8]> = fragment.chunks(SEGMENT).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let h = (*seq & 0x3F) | if i == 0 { FIR } else { 0 } | if i + 1 == n { FIN } else { 0 };
            *seq = (*seq + 1) & 0x3F;
            let mut s = vec![h];
            s.extend_from_slice(c);
            s
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fault {
    /// The fragment being rebuilt grew past the receive limit.
    TooLong,
}

/// Rebuilds application fragments from transport segments. A segment out of order, or a continuation with no first segment,
/// drops what was collected and is otherwise ignored (what the stock outstation does).
pub struct Reassembler {
    buf: Vec<u8>,
    next: Option<u8>,
    limit: usize,
}

impl Reassembler {
    pub fn new(limit: usize) -> Reassembler {
        Reassembler {
            buf: vec![],
            next: None,
            limit,
        }
    }

    /// `Ok(Some(fragment))` when `seg` completes one.
    pub fn push(&mut self, seg: &[u8]) -> Result<Option<Vec<u8>>, Fault> {
        let Some((&h, body)) = seg.split_first() else {
            return Ok(None);
        };
        let seq = h & 0x3F;
        if h & FIR != 0 {
            self.buf.clear();
        } else if self.next != Some(seq) {
            self.buf.clear();
            self.next = None;
            return Ok(None);
        }
        if self.buf.len() + body.len() > self.limit {
            self.buf.clear();
            self.next = None;
            return Err(Fault::TooLong);
        }
        self.buf.extend_from_slice(body);
        if h & FIN != 0 {
            self.next = None;
            return Ok(Some(std::mem::take(&mut self.buf)));
        }
        self.next = Some((seq + 1) & 0x3F);
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_fragment_is_one_segment_with_both_flags() {
        let mut seq = 0;
        assert_eq!(segment(&[1, 2, 3], &mut seq), vec![vec![0xC0, 1, 2, 3]]);
        assert_eq!(seq, 1);
        assert_eq!(segment(&[], &mut seq), vec![vec![0xC1]]);
    }

    #[test]
    fn a_long_fragment_is_cut_at_249_and_the_sequence_wraps_at_64() {
        let frag: Vec<u8> = (0..500).map(|i| i as u8).collect();
        let mut seq = 62;
        let segs = segment(&frag, &mut seq);
        assert_eq!(segs.len(), 3);
        assert_eq!(
            segs.iter().map(|s| s[0]).collect::<Vec<_>>(),
            vec![0x40 | 62, 63, 0x80]
        );
        assert_eq!(segs[0].len(), 250);
        assert_eq!(seq, 1);
        let exact: Vec<u8> = vec![9; 249];
        assert_eq!(segment(&exact, &mut 0).len(), 1);
        assert_eq!(segment(&vec![9; 250], &mut 0).len(), 2);
    }

    #[test]
    fn segments_reassemble_into_the_fragment() {
        let frag: Vec<u8> = (0..2000).map(|i| (i * 7) as u8).collect();
        let mut seq = 5;
        let mut r = Reassembler::new(2048);
        let mut out = None;
        for s in segment(&frag, &mut seq) {
            out = r.push(&s).unwrap();
        }
        assert_eq!(out, Some(frag));
    }

    #[test]
    fn a_gap_a_headless_continuation_and_an_empty_segment_are_dropped() {
        let mut r = Reassembler::new(2048);
        assert_eq!(r.push(&[3, 1]), Ok(None)); // no FIR
        assert_eq!(r.push(&[0x80 | 4, 1]), Ok(None)); // FIN after the dropped one
        assert_eq!(r.push(&[]), Ok(None));
        assert_eq!(r.push(&[0x40 | 1, 1, 2]), Ok(None));
        assert_eq!(r.push(&[0x80 | 3, 9]), Ok(None)); // seq 3, expected 2
        assert_eq!(r.push(&[0x80 | 2, 9]), Ok(None)); // the partial was dropped with it
        assert_eq!(r.push(&[0xC0, 7]), Ok(Some(vec![7])));
    }

    #[test]
    fn a_new_first_segment_restarts_and_the_limit_is_enforced() {
        let mut r = Reassembler::new(4);
        assert_eq!(r.push(&[0x40, 1, 2]), Ok(None));
        assert_eq!(r.push(&[0x40 | 9, 5]), Ok(None));
        assert_eq!(r.push(&[0x80 | 10, 6]), Ok(Some(vec![5, 6])));
        assert_eq!(r.push(&[0x40, 1, 2, 3]), Ok(None));
        assert_eq!(r.push(&[0x81, 4, 5]), Err(Fault::TooLong));
        assert_eq!(r.push(&[0xC0, 4]), Ok(Some(vec![4])));
        assert_eq!(r.push(&[0xC0, 1, 2, 3, 4]), Ok(Some(vec![1, 2, 3, 4])));
        assert_eq!(r.push(&[0xC0, 1, 2, 3, 4, 5]), Err(Fault::TooLong));
    }
}
