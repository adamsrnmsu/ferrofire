//! The receiving side of ferrofire datagram v1 (docs/datagram-v1.md "Failure behaviour"): what a DISH-style
//! subscriber (the bench client, a future Rust field device) does with each datagram. Nothing a sender or the
//! network can do makes it panic; a datagram it cannot use is a `Reject`, never a crash.

use crate::bennu::unframe;
use std::collections::{HashMap, HashSet, VecDeque};

/// How many recent `_ff.seq` values are remembered for duplicate detection; a seq this far behind the newest is dropped unseen.
const WINDOW: u64 = 256;

/// Why a datagram was ignored.
#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    /// Not a RADIO frame, or the body is not UTF-8.
    Garbage,
    /// Another endpoint's group.
    WrongGroup,
    /// The body does not end in a comma and a NUL: cut short on the way.
    Truncated,
    /// A duplicate or an older datagram than one already seen.
    Stale,
}

/// How current a tag's held value is.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Quality {
    Good,
    /// Not refreshed for `stale_after` publish periods: the last value is held, not live.
    Stale,
}

pub struct Receiver {
    group: String,
    known: HashSet<String>,
    /// Newest `_ff.seq` and `_ff.t` seen.
    newest: Option<(u64, u64)>,
    /// The last `WINDOW` seqs accepted: a repeat of one is a duplicate.
    seen: VecDeque<u64>,
    /// Newest `_ff.cyc` applied per tag: an older cycle's value never overwrites it.
    cyc: HashMap<String, u64>,
    /// Newest `_ff.cyc` seen in any datagram: a tag's age is how far it is behind this.
    head: u64,
    stale_after: u64,
    /// Datagrams skipped between consecutive `_ff.seq` values (a late arrival of one takes it back).
    gaps: u64,
}

impl Receiver {
    pub fn new<'a>(group: &str, known: impl IntoIterator<Item = &'a str>) -> Receiver {
        Receiver {
            group: group.into(),
            known: known.into_iter().map(String::from).collect(),
            newest: None,
            seen: VecDeque::new(),
            cyc: HashMap::new(),
            head: 0,
            stale_after: 3,
            gaps: 0,
        }
    }

    /// Datagrams missing from the `_ff.seq` run so far.
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    /// The newest `_ff.cyc` seen in any datagram (0 before one with a trailer).
    pub fn head(&self) -> u64 {
        self.head
    }

    /// Missed publish periods after which a held value turns `Stale` (default 3).
    pub fn set_stale_after(&mut self, periods: u64) {
        self.stale_after = periods;
    }

    /// `(quality, age in publish periods)` of a tag; `None` until a value with a `_ff.cyc` has been applied.
    /// Time passing with no datagram at all does not advance the age: a caller that needs that watches for silence itself.
    pub fn quality(&self, tag: &str) -> Option<(Quality, u64)> {
        let age = self.head - self.cyc.get(tag)?;
        let q = if age >= self.stale_after {
            Quality::Stale
        } else {
            Quality::Good
        };
        Some((q, age))
    }

    /// The `(tag, value)` pairs this datagram contributes: only subscribed tags, `true`/`false` as 1/0, bad pairs skipped.
    /// Chunks of one cycle may arrive in any order. A datagram is stale when its `_ff.seq` was already seen (or is `WINDOW` behind
    /// the newest); a tag whose value is from an older `_ff.cyc` than the one applied is dropped. A lower seq with a newer `_ff.t`
    /// is a restarted provider: everything remembered is forgotten. Datagrams without the trailer (a stock provider) are never stale.
    pub fn accept(&mut self, d: &[u8]) -> Result<Vec<(String, f64)>, Reject> {
        let (seq, cyc, t, out) = self.points(self.body_of(d)?);
        match seq {
            Some(seq) if self.duplicate(seq, t) => Err(Reject::Stale),
            Some(_) => self.newest_cycle(cyc, out),
            None => Ok(out),
        }
    }

    /// The trailer (`_ff.seq`, `_ff.cyc`, `_ff.t`) and the subscribed `(tag, value)` pairs of a body.
    #[allow(clippy::type_complexity)]
    fn points(&self, body: &str) -> (Option<u64>, Option<u64>, u64, Vec<(String, f64)>) {
        let (mut seq, mut cyc, mut t, mut out) = (None, None, 0, vec![]);
        for (name, val) in body.split(',').filter_map(|p| p.split_once(':')) {
            match name {
                "_ff.seq" => seq = val.parse().ok(),
                "_ff.cyc" => cyc = val.parse().ok(),
                "_ff.t" => t = val.parse().unwrap_or(0),
                _ if self.known.contains(name) => {
                    out.extend(value(val).map(|v| (name.to_string(), v)))
                }
                _ => {}
            }
        }
        (seq, cyc, t, out)
    }

    /// Keep the values not older than the cycle already applied for their tag; some offered and none kept = stale.
    fn newest_cycle(
        &mut self,
        cyc: Option<u64>,
        mut out: Vec<(String, f64)>,
    ) -> Result<Vec<(String, f64)>, Reject> {
        let Some(cyc) = cyc else { return Ok(out) };
        self.head = self.head.max(cyc);
        let before = out.len();
        out.retain(|(tag, _)| self.cyc.get(tag).is_none_or(|&c| cyc >= c));
        self.cyc
            .extend(out.iter().map(|(tag, _)| (tag.clone(), cyc)));
        match out.is_empty() && before > 0 {
            true => Err(Reject::Stale),
            false => Ok(out),
        }
    }

    /// The body text of a datagram for our group: NUL-terminated, UTF-8, every point complete.
    fn body_of<'a>(&self, d: &'a [u8]) -> Result<&'a str, Reject> {
        let (group, body) = unframe(d).ok_or(Reject::Garbage)?;
        if group != self.group {
            return Err(Reject::WrongGroup);
        }
        let body = body.strip_suffix(&[0]).ok_or(Reject::Truncated)?;
        let body = std::str::from_utf8(body).map_err(|_| Reject::Garbage)?;
        match body.is_empty() || body.ends_with(',') {
            true => Ok(body),
            false => Err(Reject::Truncated),
        }
    }

    /// Has this seq been seen (or is it too far behind)? If not, remember it.
    fn duplicate(&mut self, seq: u64, t: u64) -> bool {
        if let Some((s, lt)) = self.newest {
            if seq <= s && t > lt {
                self.seen.clear();
                self.cyc.clear();
                self.head = 0;
                self.newest = None;
            }
        }
        let (s, lt) = self.newest.unwrap_or((seq, t));
        if seq + WINDOW <= s || self.seen.contains(&seq) {
            return true;
        }
        if seq > s + 1 {
            self.gaps += seq - s - 1;
        } else if seq < s {
            self.gaps = self.gaps.saturating_sub(1);
        }
        self.newest = Some((s.max(seq), lt.max(t)));
        self.seen.truncate(WINDOW as usize - 1);
        self.seen.push_front(seq);
        false
    }
}

fn value(s: &str) -> Option<f64> {
    match s {
        "true" => Some(1.0),
        "false" => Some(0.0),
        _ => s.parse().ok().filter(|v: &f64| v.is_finite()),
    }
}
