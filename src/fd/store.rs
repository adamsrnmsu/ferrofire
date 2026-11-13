//! The field device's datastore: external points (named by the provider), tags (aliases onto points, or device-local),
//! and the updated-tag queue that protocol writes and logic results go into. Types are fixed at load: a `bool` point
//! never holds a number and vice versa. A write only queues; the value lands in the store when the provider ACKs
//! (external) or at the end of the scan cycle (internal), exactly as the stock device does (SPEC 3.2).

use crate::hint::lock;
use crate::model::{Kind, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a point the provider ACKed keeps the ACKed value against a stream value that differs (ww-1df.32). The provider ACKs
/// when it queues a write and applies it at its next engine tick, so datagrams published in between still carry the old value.
const ACK_HOLD: Duration = Duration::from_secs(2);

struct Point {
    value: Value,
    /// Unix seconds of the last provider update; 0 until the first.
    ts: u64,
    /// The ACKed value still waiting for the stream to show it, and when that wait ends.
    hold: Option<(Value, Instant)>,
}

/// The same value; a NaN is the same value as itself (a register of zero span unscales to NaN).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Analog(x), Value::Analog(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

#[derive(Default)]
struct Inner {
    points: HashMap<String, Point>,
    /// External tag -> point name.
    alias: HashMap<String, String>,
    internal: HashMap<String, Value>,
    /// Every tag in declaration order (what `snapshot` returns).
    order: Vec<String>,
    /// Point -> index of the `<output>` element that lists it.
    owner: HashMap<String, usize>,
    queue: BTreeMap<String, Value>,
    dropped: u64,
}

/// Why a protocol write was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteError {
    UnknownTag,
    WrongKind,
}

/// What a load builds the store from: points, tag aliases and device-local tags.
#[derive(Default)]
pub struct Layout {
    /// `(point name, kind, owning output index)`.
    pub points: Vec<(String, Kind, Option<usize>)>,
    /// `(tag, point name)`.
    pub external: Vec<(String, String)>,
    /// `(tag, initial value)`.
    pub internal: Vec<(String, Value)>,
}

/// Shared handle; clone it into every task.
#[derive(Clone)]
pub struct Store(Arc<Mutex<Inner>>);

fn zero(kind: Kind) -> Value {
    match kind {
        Kind::Binary => Value::Binary(false),
        Kind::Analog => Value::Analog(0.0),
    }
}

fn kind_of(v: Value) -> Kind {
    match v {
        Value::Binary(_) => Kind::Binary,
        Value::Analog(_) => Kind::Analog,
    }
}

fn from_wire(kind: Kind, x: f64) -> Option<Value> {
    match (kind, x) {
        (Kind::Analog, x) => Some(Value::Analog(x)),
        (Kind::Binary, x) if x == 0.0 || x == 1.0 => Some(Value::Binary(x == 1.0)),
        (Kind::Binary, _) => None,
    }
}

impl Store {
    /// A tag aliasing an unknown point must not reach here (the loader drops it); a point listed twice keeps the first kind.
    pub fn new(l: Layout) -> Store {
        let mut s = Inner::default();
        for (name, kind, owner) in l.points {
            s.points.entry(name.clone()).or_insert(Point {
                value: zero(kind),
                ts: 0,
                hold: None,
            });
            if let Some(o) = owner {
                s.owner.entry(name).or_insert(o);
            }
        }
        for (tag, point) in l.external {
            s.order.push(tag.clone());
            s.alias.insert(tag, point);
        }
        for (tag, v) in l.internal {
            s.order.push(tag.clone());
            s.internal.insert(tag, v);
        }
        Store(Arc::new(Mutex::new(s)))
    }

    pub fn get(&self, tag: &str) -> Option<Value> {
        lock(&self.0).get(tag)
    }

    pub fn kind(&self, tag: &str) -> Option<Kind> {
        self.get(tag).map(kind_of)
    }

    /// The value of a binary tag; `None` for an unknown tag or an analog one.
    pub fn get_bool(&self, tag: &str) -> Option<bool> {
        if let Some(Value::Binary(b)) = self.get(tag) {
            return Some(b);
        }
        None
    }

    /// The value of an analog tag; `None` for an unknown tag or a binary one.
    pub fn get_f64(&self, tag: &str) -> Option<f64> {
        self.get(tag).and_then(|v| match v {
            Value::Analog(x) => Some(x),
            Value::Binary(_) => None,
        })
    }

    /// Unix seconds of the provider's last update of the point behind `tag`; `None` for an internal or unknown tag.
    pub fn timestamp(&self, tag: &str) -> Option<u64> {
        let s = lock(&self.0);
        Some(s.points.get(s.alias.get(tag)?)?.ts)
    }

    /// Queue a protocol write or logic result. Nothing changes in the store until the ACK / end of cycle.
    pub fn write(&self, tag: &str, v: Value) -> Result<(), WriteError> {
        let mut s = lock(&self.0);
        let cur = s.get(tag).ok_or(WriteError::UnknownTag)?;
        if kind_of(cur) != kind_of(v) {
            return Err(WriteError::WrongKind);
        }
        s.queue.insert(tag.to_string(), v);
        Ok(())
    }

    pub fn is_queued(&self, tag: &str) -> bool {
        lock(&self.0).queue.contains_key(tag)
    }

    /// A number the provider published for `point` (`true`/`false` arrive as 1/0). A number that is not 0 or 1 for a binary
    /// point, or any value for an unknown point, is dropped and counted.
    /// ponytail: the datagram receiver hands over f64, so `true` for an analog point lands as 1.0 instead of being dropped.
    ///
    /// A point the provider just ACKed keeps the ACKed value until the stream shows it (which ends the wait) or `ACK_HOLD` runs out;
    /// a different value in between is stale and ignored (neither value nor timestamp changes), though not counted as dropped.
    pub fn publish(&self, point: &str, x: f64, now: u64) -> bool {
        self.publish_at(point, x, now, Instant::now())
    }

    /// `publish` with the clock the hold runs on passed in.
    pub fn publish_at(&self, point: &str, x: f64, now: u64, at: Instant) -> bool {
        let mut s = lock(&self.0);
        let Some(p) = s.points.get_mut(point) else {
            s.dropped += 1;
            return false;
        };
        let Some(value) = from_wire(kind_of(p.value), x) else {
            s.dropped += 1;
            return false;
        };
        match p.hold.take() {
            Some((held, until)) if at < until && !same(&held, &value) => {
                p.hold = Some((held, until))
            }
            _ => {
                p.value = value;
                p.ts = now;
            }
        }
        true
    }

    /// Published values dropped so far (unknown point or wrong kind).
    pub fn dropped(&self) -> u64 {
        lock(&self.0).dropped
    }

    /// Every tag and its current value, in declaration order.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        let s = lock(&self.0);
        s.order
            .iter()
            .filter_map(|t| Some((t.clone(), s.get(t)?)))
            .collect()
    }

    /// Queued external writes for output `owner`: `(tag, point, value)` for the points it lists and the points no output lists.
    pub fn pending(&self, owner: usize) -> Vec<(String, String, Value)> {
        let s = lock(&self.0);
        s.queue
            .iter()
            .filter_map(|(tag, v)| {
                let point = s.alias.get(tag)?;
                let mine = s.owner.get(point).is_none_or(|&o| o == owner);
                mine.then(|| (tag.clone(), point.clone(), *v))
            })
            .collect()
    }

    /// The provider accepted `v` for `tag`: set the point, and clear the queue entry unless a newer write replaced it.
    /// The point then holds `v` against stale stream values (see `publish`); a newer ACK replaces the hold.
    pub fn ack(&self, tag: &str, v: Value) {
        self.ack_at(tag, v, Instant::now());
    }

    /// `ack` with the clock the hold runs on passed in.
    pub fn ack_at(&self, tag: &str, v: Value, at: Instant) {
        let mut s = lock(&self.0);
        if let Some(point) = s.alias.get(tag).cloned() {
            if let Some(p) = s.points.get_mut(&point) {
                p.value = v;
                p.hold = Some((v, at + ACK_HOLD));
            }
        }
        s.forget(tag, v);
    }

    /// The provider rejected `v` for `tag`: drop it from the queue unless a newer write replaced it.
    pub fn reject(&self, tag: &str, v: Value) {
        lock(&self.0).forget(tag, v);
    }

    /// End of cycle: queued writes to device-local tags take effect.
    pub fn apply_internal(&self) {
        let mut s = lock(&self.0);
        let queue = std::mem::take(&mut s.queue);
        for (tag, v) in queue {
            match s.internal.get_mut(&tag) {
                Some(slot) => *slot = v,
                None => {
                    s.queue.insert(tag, v);
                }
            }
        }
    }
}

impl Inner {
    fn get(&self, tag: &str) -> Option<Value> {
        match self.alias.get(tag) {
            Some(point) => Some(self.points.get(point)?.value),
            None => self.internal.get(tag).copied(),
        }
    }

    /// Erase the queue entry iff it still holds `v` (compare-and-erase, SPEC 3.2). An entry that never compared equal (a NaN) would
    /// be sent to the provider for ever.
    fn forget(&mut self, tag: &str, v: Value) {
        if self.queue.get(tag).is_some_and(|q| same(q, &v)) {
            self.queue.remove(tag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new(Layout {
            points: vec![
                ("in.a".into(), Kind::Analog, None),
                ("in.b".into(), Kind::Binary, None),
                ("out.a".into(), Kind::Analog, Some(0)),
                ("out2.a".into(), Kind::Analog, Some(1)),
            ],
            external: vec![
                ("ta".into(), "in.a".into()),
                ("tb".into(), "in.b".into()),
                ("to".into(), "out.a".into()),
                ("to2".into(), "out2.a".into()),
            ],
            internal: vec![
                ("n".into(), Value::Analog(7.0)),
                ("s".into(), Value::Binary(true)),
            ],
        })
    }

    #[test]
    fn typed_reads() {
        let s = store();
        assert_eq!(s.get_f64("ta"), Some(0.0));
        assert_eq!(s.get_bool("tb"), Some(false));
        assert_eq!(s.get_f64("tb"), None);
        assert_eq!(s.get_bool("ta"), None);
        assert_eq!(s.get_f64("n"), Some(7.0));
        assert_eq!(s.get_bool("s"), Some(true));
        assert_eq!(s.get("nope"), None);
        assert_eq!(s.kind("tb"), Some(Kind::Binary));
        assert_eq!(s.kind("n"), Some(Kind::Analog));
        assert_eq!(s.kind("nope"), None);
    }

    #[test]
    fn publish_sets_value_and_time_and_drops_mismatches() {
        let s = store();
        assert!(s.publish("in.a", 2.5, 99));
        assert_eq!(s.get_f64("ta"), Some(2.5));
        assert_eq!(s.timestamp("ta"), Some(99));
        assert_eq!(s.timestamp("n"), None);
        assert_eq!(s.timestamp("nope"), None);
        assert!(!s.publish("in.b", 2.0, 100));
        assert!(!s.publish("in.b", 0.5, 100));
        assert!(!s.publish("nope", 1.0, 100));
        assert_eq!(s.dropped(), 3);
        assert_eq!(s.get_bool("tb"), Some(false));
        assert_eq!(s.get_f64("ta"), Some(2.5));
        assert_eq!(s.timestamp("ta"), Some(99));
        assert_eq!(s.timestamp("tb"), Some(0));
        assert!(s.publish("in.b", 1.0, 101));
        assert!(s.publish("in.b", 0.0, 102));
        assert_eq!(s.get_bool("tb"), Some(false));
        assert!(s.publish("in.b", 1.0, 103));
        assert_eq!(s.get_bool("tb"), Some(true));
    }

    #[test]
    fn write_queues_without_touching_the_store() {
        let s = store();
        assert_eq!(
            s.write("nope", Value::Analog(1.0)),
            Err(WriteError::UnknownTag)
        );
        assert_eq!(
            s.write("ta", Value::Binary(true)),
            Err(WriteError::WrongKind)
        );
        assert_eq!(
            s.write("n", Value::Binary(true)),
            Err(WriteError::WrongKind)
        );
        assert!(!s.is_queued("ta"));
        s.write("ta", Value::Analog(3.0)).unwrap();
        assert!(s.is_queued("ta"));
        assert_eq!(s.get_f64("ta"), Some(0.0));
    }

    #[test]
    fn ack_sets_the_point_and_erases_only_the_same_value() {
        let s = store();
        s.write("to", Value::Analog(1.0)).unwrap();
        s.write("to", Value::Analog(2.0)).unwrap();
        s.ack("to", Value::Analog(1.0)); // a stale send: the newer write stays queued
        assert_eq!(s.get_f64("to"), Some(1.0));
        assert!(s.is_queued("to"));
        s.ack("to", Value::Analog(2.0));
        assert_eq!(s.get_f64("to"), Some(2.0));
        assert!(!s.is_queued("to"));
        s.ack("n", Value::Analog(9.0)); // not an external tag: nothing to set
        assert_eq!(s.get_f64("n"), Some(7.0));
    }

    #[test]
    fn reject_drops_only_the_same_value() {
        let s = store();
        s.write("to", Value::Analog(1.0)).unwrap();
        s.reject("to", Value::Analog(5.0));
        assert!(s.is_queued("to"));
        s.reject("to", Value::Analog(1.0));
        assert!(!s.is_queued("to"));
        assert_eq!(s.get_f64("to"), Some(0.0));
    }

    #[test]
    fn pending_is_per_owner_plus_unowned_and_external_only() {
        let s = store();
        for t in ["ta", "to", "to2"] {
            s.write(t, Value::Analog(1.0)).unwrap();
        }
        s.write("n", Value::Analog(1.0)).unwrap();
        let names = |o| {
            s.pending(o)
                .into_iter()
                .map(|(t, p, _)| format!("{t}={p}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(names(0), ["ta=in.a", "to=out.a"]);
        assert_eq!(names(1), ["ta=in.a", "to2=out2.a"]);
        assert_eq!(s.pending(0)[1].2, Value::Analog(1.0));
    }

    #[test]
    fn apply_internal_moves_only_local_tags() {
        let s = store();
        s.write("n", Value::Analog(1.5)).unwrap();
        s.write("to", Value::Analog(4.0)).unwrap();
        s.apply_internal();
        assert_eq!(s.get_f64("n"), Some(1.5));
        assert!(!s.is_queued("n"));
        assert!(s.is_queued("to"));
        assert_eq!(s.get_f64("to"), Some(0.0));
    }

    #[test]
    fn snapshot_is_in_declaration_order() {
        let s = store();
        s.publish("in.b", 1.0, 1);
        let snap = s.snapshot();
        let names: Vec<_> = snap.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(names, ["ta", "tb", "to", "to2", "n", "s"]);
        assert_eq!(snap[1].1, Value::Binary(true));
    }

    #[test]
    fn shared_point_name_shares_one_value() {
        let s = Store::new(Layout {
            points: vec![
                ("p".into(), Kind::Analog, None),
                ("p".into(), Kind::Binary, Some(0)),
            ],
            external: vec![("i".into(), "p".into()), ("o".into(), "p".into())],
            internal: vec![],
        });
        s.publish("p", 4.0, 1);
        assert_eq!(s.get_f64("o"), Some(4.0));
        assert_eq!(s.pending(5).len(), 0);
    }

    #[test]
    fn a_nan_write_is_cleared_by_its_ack_but_a_newer_value_stays() {
        let s = store();
        s.write("ta", Value::Analog(f64::NAN)).unwrap();
        s.ack("ta", Value::Analog(f64::NAN));
        assert!(!s.is_queued("ta"));
        s.write("ta", Value::Analog(f64::NAN)).unwrap();
        s.write("ta", Value::Analog(1.0)).unwrap();
        s.ack("ta", Value::Analog(f64::NAN));
        assert!(s.is_queued("ta"));
        s.reject("ta", Value::Analog(1.0));
        assert!(!s.is_queued("ta"));
    }

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn a_stale_publish_after_the_ack_keeps_the_acked_value_and_timestamp() {
        let (s, t0) = (store(), Instant::now());
        s.publish("out.a", 1.0, 5);
        s.ack_at("to", Value::Analog(2.0), t0);
        assert!(s.publish_at("out.a", 1.0, 6, ms(t0, 100)));
        assert_eq!((s.get_f64("to"), s.timestamp("to")), (Some(2.0), Some(5)));
        assert_eq!(s.dropped(), 0);
        assert!(s.publish_at("out.a", 1.0, 7, ms(t0, 1999)));
        assert_eq!(s.get_f64("to"), Some(2.0));
    }

    #[test]
    fn a_confirming_publish_clears_the_hold() {
        let (s, t0) = (store(), Instant::now());
        s.ack_at("to", Value::Analog(2.0), t0);
        s.publish_at("out.a", 2.0, 6, ms(t0, 100));
        assert_eq!((s.get_f64("to"), s.timestamp("to")), (Some(2.0), Some(6)));
        s.publish_at("out.a", 3.0, 7, ms(t0, 200)); // another writer, not held any more
        assert_eq!(s.get_f64("to"), Some(3.0));
    }

    #[test]
    fn after_the_deadline_the_stream_wins() {
        let (s, t0) = (store(), Instant::now());
        s.ack_at("to", Value::Analog(2.0), t0);
        s.publish_at("out.a", 1.0, 6, ms(t0, 2000));
        assert_eq!((s.get_f64("to"), s.timestamp("to")), (Some(1.0), Some(6)));
        s.ack_at("to", Value::Analog(2.0), t0);
        s.publish_at("out.a", 1.0, 7, ms(t0, 1999));
        assert_eq!(s.get_f64("to"), Some(2.0));
    }

    #[test]
    fn a_newer_ack_replaces_the_hold() {
        let (s, t0) = (store(), Instant::now());
        s.ack_at("to", Value::Analog(2.0), t0);
        s.ack_at("to", Value::Analog(3.0), ms(t0, 1500));
        s.publish_at("out.a", 2.0, 6, ms(t0, 2500)); // the old hold would have ended; the new one has not
        assert_eq!(s.get_f64("to"), Some(3.0));
        s.publish_at("out.a", 2.0, 7, ms(t0, 3500));
        assert_eq!(s.get_f64("to"), Some(2.0));
    }

    #[test]
    fn a_nan_stream_value_confirms_a_nan_ack() {
        let (s, t0) = (store(), Instant::now());
        s.ack_at("to", Value::Analog(f64::NAN), t0);
        s.publish_at("out.a", f64::NAN, 6, ms(t0, 100));
        assert_eq!(s.timestamp("to"), Some(6));
        s.publish_at("out.a", 4.0, 7, ms(t0, 200));
        assert_eq!(s.get_f64("to"), Some(4.0));
    }

    #[test]
    fn a_binary_point_is_held_too_and_a_bad_value_is_still_dropped() {
        let s = Store::new(Layout {
            points: vec![("p".into(), Kind::Binary, Some(0))],
            external: vec![("c".into(), "p".into())],
            internal: vec![],
        });
        let t0 = Instant::now();
        s.ack_at("c", Value::Binary(true), t0);
        s.publish_at("p", 0.0, 1, ms(t0, 10));
        assert_eq!(s.get_bool("c"), Some(true));
        assert!(!s.publish_at("p", 7.0, 2, ms(t0, 10)));
        assert_eq!(s.dropped(), 1);
        assert_eq!(s.get_bool("c"), Some(true));
        s.publish_at("p", 1.0, 3, ms(t0, 20));
        s.publish_at("p", 0.0, 4, ms(t0, 30));
        assert_eq!(s.get_bool("c"), Some(false));
    }
}
