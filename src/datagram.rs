//! The receiving side of ferrofire datagram v1 (docs/datagram-v1.md "Failure behaviour"): what a DISH-style
//! subscriber (the bench client, a future Rust field device) does with each datagram. Nothing a sender or the
//! network can do makes it panic; a datagram it cannot use is a `Reject`, never a crash.

use crate::bennu::unframe;
use std::collections::HashSet;

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

pub struct Receiver {
    group: String,
    known: HashSet<String>,
    /// (`_ff.seq`, `_ff.t`) of the newest datagram accepted.
    last: Option<(u64, u64)>,
}

impl Receiver {
    pub fn new<'a>(group: &str, known: impl IntoIterator<Item = &'a str>) -> Receiver {
        Receiver {
            group: group.into(),
            known: known.into_iter().map(String::from).collect(),
            last: None,
        }
    }

    /// The `(tag, value)` pairs this datagram contributes: only subscribed tags, `true`/`false` as 1/0, bad pairs skipped.
    /// A datagram is stale when its `_ff.seq` is not above the newest seen and its `_ff.t` is not newer either (a restarted
    /// provider counts from 0 again but its clock has moved on). Datagrams without the trailer (a stock provider) are never stale.
    pub fn accept(&mut self, d: &[u8]) -> Result<Vec<(String, f64)>, Reject> {
        let body = self.body_of(d)?;
        let (mut seq, mut t, mut out) = (None, 0, vec![]);
        for (name, val) in body.split(',').filter_map(|p| p.split_once(':')) {
            match name {
                "_ff.seq" => seq = val.parse().ok(),
                "_ff.t" => t = val.parse().unwrap_or(0),
                _ if self.known.contains(name) => {
                    out.extend(value(val).map(|v| (name.to_string(), v)))
                }
                _ => {}
            }
        }
        match seq {
            Some(seq) if !self.newer(seq, t) => Err(Reject::Stale),
            _ => Ok(out),
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

    /// Is `(seq, t)` newer than anything seen? If so it becomes the newest.
    fn newer(&mut self, seq: u64, t: u64) -> bool {
        let stale = self.last.is_some_and(|(s, lt)| seq <= s && t <= lt);
        if !stale {
            self.last = Some((seq, t));
        }
        !stale
    }
}

fn value(s: &str) -> Option<f64> {
    match s {
        "true" => Some(1.0),
        "false" => Some(0.0),
        _ => s.parse().ok().filter(|v: &f64| v.is_finite()),
    }
}
