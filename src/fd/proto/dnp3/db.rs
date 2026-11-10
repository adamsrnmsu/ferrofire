//! The outstation database: static points (binary and analog, keyed by DNP3 index), the event buffer, and the encoders that
//! turn both into object bytes in the variation a master asks for.

use std::collections::VecDeque;

/// Events kept per type (opendnp3 `EventBufferConfig::AllTypes(100)`, as bennu configures it).
pub const EVENT_CAP: usize = 100;

pub const ONLINE: u8 = 0x01;
pub const RESTART: u8 = 0x02;
pub const OVER_RANGE: u8 = 0x20;
pub const STATE: u8 = 0x80;

/// A point. Inputs raise events; outputs (reached through the same map, bennu quirk Q5) never do.
#[derive(Debug, Clone, PartialEq)]
pub struct Point<V> {
    pub tag: String,
    pub sbo: bool,
    pub input: bool,
    pub value: V,
    pub flags: u8,
}

pub type BinPoint = Point<bool>;
pub type AnaPoint = Point<f64>;

/// The static variations a point answers `READ g?v0` with: inputs g1v2 / g30v6 (flags; double), outputs g1v1 / g30v1 (packed bit;
/// 32-bit integer).
pub trait Static: Default {
    const INPUT_VAR: u8;
    const OUTPUT_VAR: u8;
}

impl Static for bool {
    const INPUT_VAR: u8 = 2;
    const OUTPUT_VAR: u8 = 1;
}

impl Static for f64 {
    const INPUT_VAR: u8 = 6;
    const OUTPUT_VAR: u8 = 1;
}

impl<V: Static> Point<V> {
    /// A point at power-up: value 0, flags RESTART.
    pub fn new(tag: &str, sbo: bool, input: bool) -> Point<V> {
        Point {
            tag: tag.into(),
            sbo,
            input,
            value: V::default(),
            flags: RESTART,
        }
    }
    pub fn default_var(&self) -> u8 {
        if self.input {
            V::INPUT_VAR
        } else {
            V::OUTPUT_VAR
        }
    }
}

impl BinPoint {
    /// Take the provider's value; `true` when that is an event.
    pub fn update(&mut self, value: bool) -> bool {
        let flags = ONLINE | if value { STATE } else { 0 };
        let changed = flags != self.flags;
        (self.value, self.flags) = (value, flags);
        changed && self.input
    }
}

impl AnaPoint {
    /// Take the provider's value; `true` when that is an event (flags changed, or any change: the deadband is 0).
    pub fn update(&mut self, value: f64) -> bool {
        let changed = self.flags != ONLINE || (value - self.value).abs() > 0.0;
        (self.value, self.flags) = (value, ONLINE);
        changed && self.input
    }
}

/// Static binary object in variation 1 (one packed bit) or 2 (flags octet).
pub fn binary_static(var: u8, flags: u8) -> Vec<u8> {
    match var {
        1 => vec![u8::from(flags & STATE != 0)],
        _ => vec![flags],
    }
}

/// An integer object's flag octet and bytes: the value clamped to the width, `OVER_RANGE` set when it did not fit.
fn int_body(wide: bool, value: f64, flags: u8) -> (u8, Vec<u8>) {
    let (min, max) = if wide {
        (f64::from(i32::MIN), f64::from(i32::MAX))
    } else {
        (f64::from(i16::MIN), f64::from(i16::MAX))
    };
    let over = value.is_nan() || value < min || value > max;
    let v = if value.is_nan() {
        0.0
    } else {
        value.clamp(min, max)
    };
    let bytes = if wide {
        (v as i32).to_le_bytes().to_vec()
    } else {
        (v as i16).to_le_bytes().to_vec()
    };
    (flags | if over { OVER_RANGE } else { 0 }, bytes)
}

fn time48(ts: u64) -> [u8; 6] {
    let b = ts.to_le_bytes();
    [b[0], b[1], b[2], b[3], b[4], b[5]]
}

/// An analog value as variation `var` of group 30 (`group32` false) or group 32: 1/2 integer with flag, 3/4 integer (group 30:
/// without flag), 5/6 float/double with flag; group 32 has a flag in every variation and adds a 48-bit time to 3, 4, 7 (float)
/// and 8 (double). Variation 0 is never passed.
pub fn analog_object(var: u8, value: f64, flags: u8, time: Option<u64>, group32: bool) -> Vec<u8> {
    let (flag, body) = match var {
        1..=4 => int_body(matches!(var, 1 | 3), value, flags),
        5 | 7 => (flags, (value as f32).to_le_bytes().to_vec()),
        _ => (flags, value.to_le_bytes().to_vec()),
    };
    let mut out = if group32 || !matches!(var, 3 | 4) {
        vec![flag]
    } else {
        vec![]
    };
    out.extend(body);
    if group32 && matches!(var, 3 | 4 | 7 | 8) {
        out.extend_from_slice(&time48(time.unwrap_or(0)));
    }
    out
}

/// Which kind of event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typ {
    Binary,
    Analog,
}

impl Typ {
    /// `(group, default variation)`: g2v2 and g32v6 when the master does not name a variation.
    const EVENT: [(u8, u8); 2] = [(2, 2), (32, 6)];

    fn slot(self) -> usize {
        self as usize
    }
    pub fn default_var(self) -> u8 {
        Self::EVENT[self.slot()].1
    }
    pub fn group(self) -> u8 {
        Self::EVENT[self.slot()].0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Bin { flags: u8 },
    Ana { value: f64, ts: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unselected,
    /// Chosen by a READ, to be written as this variation.
    Selected(u8),
    /// Sent in a fragment that waits for its CONFIRM.
    Written,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: u64,
    pub idx: u16,
    pub data: Data,
    pub state: State,
}

impl Event {
    pub fn typ(&self) -> Typ {
        match self.data {
            Data::Bin { .. } => Typ::Binary,
            Data::Ana { .. } => Typ::Analog,
        }
    }
    /// Index prefix and object bytes in variation `var`.
    pub fn encode(&self, var: u8) -> Vec<u8> {
        let mut out = self.idx.to_le_bytes().to_vec();
        match self.data {
            Data::Bin { flags } => {
                out.push(flags);
                match var {
                    2 => out.extend_from_slice(&[0; 6]),
                    3 => out.extend_from_slice(&[0; 2]),
                    _ => {}
                }
            }
            Data::Ana { value, ts } => {
                out.extend(analog_object(var, value, ONLINE, Some(ts), true));
            }
        }
        out
    }
}

/// The event buffer: one queue in the order events happened, at most `EVENT_CAP` of each type (the oldest is dropped).
#[derive(Debug, Default)]
pub struct Events {
    q: VecDeque<Event>,
    next: u64,
    overflow: [bool; 2],
}

impl Events {
    pub fn push(&mut self, idx: u16, data: Data) {
        let typ = Event {
            id: 0,
            idx,
            data: data.clone(),
            state: State::Unselected,
        }
        .typ();
        if self.count(typ) >= EVENT_CAP {
            if let Some(at) = self.q.iter().position(|e| e.typ() == typ) {
                self.q.remove(at);
            }
            self.overflow[typ.slot()] = true;
        }
        self.q.push_back(Event {
            id: self.next,
            idx,
            data,
            state: State::Unselected,
        });
        self.next += 1;
    }

    fn count(&self, typ: Typ) -> usize {
        self.q.iter().filter(|e| e.typ() == typ).count()
    }

    /// Any event not yet selected: IIN1.1 (class 1 events).
    pub fn unselected(&self) -> bool {
        self.q.iter().any(|e| e.state == State::Unselected)
    }

    pub fn overflowed(&self) -> bool {
        self.overflow.iter().any(|&o| o)
    }

    /// Select unselected events of `typ` (any when `None`), oldest first, at most `limit`; `var` 0 means the default.
    pub fn select(&mut self, typ: Option<Typ>, var: u8, limit: usize) -> usize {
        let mut n = 0;
        for e in self.q.iter_mut().filter(|e| e.state == State::Unselected) {
            if n == limit || typ.is_some_and(|t| t != e.typ()) {
                continue;
            }
            let v = if var == 0 { e.typ().default_var() } else { var };
            e.state = State::Selected(v);
            n += 1;
        }
        n
    }

    /// Selected events in the order they happened.
    pub fn selected(&self) -> impl Iterator<Item = (&Event, u8)> {
        self.q.iter().filter_map(|e| match e.state {
            State::Selected(v) => Some((e, v)),
            _ => None,
        })
    }

    pub fn mark_written(&mut self, ids: &[u64]) {
        for e in self.q.iter_mut().filter(|e| ids.contains(&e.id)) {
            e.state = State::Written;
        }
    }

    /// A master confirmed: drop what was written, and end an overflow whose type has room again.
    pub fn clear_written(&mut self) {
        self.q.retain(|e| e.state != State::Written);
        for typ in [Typ::Binary, Typ::Analog] {
            let full = self.count(typ) >= EVENT_CAP;
            self.overflow[typ.slot()] &= full;
        }
    }

    /// A new request, a timeout or a disconnect: what was selected or written goes back to waiting.
    pub fn reset(&mut self) {
        for e in self.q.iter_mut() {
            e.state = State::Unselected;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_point_events_only_for_inputs_and_only_on_change() {
        let mut p = BinPoint::new("t", false, true);
        assert!(p.update(false)); // RESTART -> ONLINE
        assert!(!p.update(false));
        assert!(p.update(true));
        assert_eq!((p.value, p.flags), (true, ONLINE | STATE));
        assert!(!p.update(true));
        assert!(p.update(false));
        let mut o = BinPoint::new("t", false, false);
        assert!(!o.update(true));
        assert_eq!(o.flags, 0x81);
    }

    #[test]
    fn analog_point_events_on_first_update_and_any_change() {
        let mut p = AnaPoint::new("t", false, true);
        assert!(p.update(0.0));
        assert!(!p.update(0.0));
        assert!(p.update(0.5));
        assert!(!p.update(0.5));
        assert!(p.update(f64::INFINITY));
        assert!(!p.update(f64::INFINITY)); // inf - inf is NaN: no event, no flood
        let mut o = AnaPoint::new("t", true, false);
        assert!(!o.update(3.0));
        assert_eq!((o.value, o.flags, o.default_var()), (3.0, ONLINE, 1));
        assert_eq!(p.default_var(), 6);
    }

    #[test]
    fn default_variations_follow_the_point_kind() {
        assert_eq!(BinPoint::new("t", false, true).default_var(), 2);
        assert_eq!(BinPoint::new("t", false, false).default_var(), 1);
    }

    #[test]
    fn static_binary_variations() {
        assert_eq!(binary_static(1, 0x81), vec![1]);
        assert_eq!(binary_static(1, 0x01), vec![0]);
        assert_eq!(binary_static(2, 0x81), vec![0x81]);
    }

    /// Stock bytes (conformance/dnp3/stock-diff.golden) for pa2 = 70000.25 and pa7 = 1e10, read as every variation.
    #[test]
    fn analog_static_variations_match_the_stock_device() {
        let o = |var, v| analog_object(var, v, ONLINE, None, false);
        assert_eq!(o(1, 70000.25), vec![0x01, 0x70, 0x11, 0x01, 0x00]);
        assert_eq!(o(1, 1e10), vec![0x21, 0xFF, 0xFF, 0xFF, 0x7F]);
        assert_eq!(o(1, -3.0), vec![0x01, 0xFD, 0xFF, 0xFF, 0xFF]);
        assert_eq!(o(1, -2.99), vec![0x01, 0xFE, 0xFF, 0xFF, 0xFF]);
        assert_eq!(o(2, 70000.25), vec![0x21, 0xFF, 0x7F]);
        assert_eq!(o(2, -1e10), vec![0x21, 0x00, 0x80]);
        assert_eq!(o(2, -3.0), vec![0x01, 0xFD, 0xFF]);
        assert_eq!(o(3, 70000.25), vec![0x70, 0x11, 0x01, 0x00]);
        assert_eq!(o(3, 1e10), vec![0xFF, 0xFF, 0xFF, 0x7F]);
        assert_eq!(o(4, 1e10), vec![0xFF, 0x7F]);
        assert_eq!(o(5, 12.5), vec![0x01, 0x00, 0x00, 0x48, 0x41]);
        assert_eq!(o(6, 12.5), vec![0x01, 0, 0, 0, 0, 0, 0, 0x29, 0x40]);
        assert_eq!(o(1, f64::NAN), vec![0x21, 0, 0, 0, 0]);
        assert_eq!(o(2, 32767.0), vec![0x01, 0xFF, 0x7F]);
        assert_eq!(o(2, 32768.0), vec![0x21, 0xFF, 0x7F]);
        assert_eq!(o(2, -32768.0), vec![0x01, 0x00, 0x80]);
        assert_eq!(o(2, -32769.0), vec![0x21, 0x00, 0x80]);
    }

    #[test]
    fn event_bytes_per_variation() {
        let b = Event {
            id: 0,
            idx: 0x0102,
            data: Data::Bin { flags: 0x81 },
            state: State::Unselected,
        };
        assert_eq!(b.encode(1), vec![2, 1, 0x81]);
        assert_eq!(b.encode(2), vec![2, 1, 0x81, 0, 0, 0, 0, 0, 0]);
        assert_eq!(b.encode(3), vec![2, 1, 0x81, 0, 0]);
        let a = Event {
            id: 0,
            idx: 3,
            data: Data::Ana {
                value: 33.5,
                ts: 0x0000_6AF3_3692,
            },
            state: State::Unselected,
        };
        assert_eq!(a.encode(1), vec![3, 0, 0x01, 33, 0, 0, 0]);
        assert_eq!(
            a.encode(3),
            vec![3, 0, 0x01, 33, 0, 0, 0, 0x92, 0x36, 0xF3, 0x6A, 0, 0]
        );
        assert_eq!(
            a.encode(4),
            vec![3, 0, 0x01, 33, 0, 0x92, 0x36, 0xF3, 0x6A, 0, 0]
        );
        assert_eq!(a.encode(5), vec![3, 0, 0x01, 0, 0, 0x06, 0x42]);
        assert_eq!(a.encode(7)[7..], [0x92, 0x36, 0xF3, 0x6A, 0, 0]);
        assert_eq!(a.encode(8).len(), 2 + 1 + 8 + 6);
        assert_eq!(a.encode(6).len(), 2 + 1 + 8);
        assert_eq!(a.encode(2), vec![3, 0, 0x01, 33, 0]);
    }

    fn fill(ev: &mut Events, bin: u16, ana: u16) {
        for i in 0..bin {
            ev.push(i, Data::Bin { flags: ONLINE });
        }
        for i in 0..ana {
            ev.push(i, Data::Ana { value: 0.0, ts: 0 });
        }
    }

    #[test]
    fn the_oldest_event_of_a_full_type_is_dropped_and_overflow_flagged() {
        let mut ev = Events::default();
        fill(&mut ev, 130, 5);
        assert!(ev.overflowed() && ev.unselected());
        ev.select(Some(Typ::Binary), 0, usize::MAX);
        let idx: Vec<u16> = ev.selected().map(|(e, _)| e.idx).collect();
        assert_eq!((idx.len(), idx[0], idx[99]), (100, 30, 129));
        assert_eq!(ev.count(Typ::Analog), 5);
    }

    #[test]
    fn overflow_ends_when_a_confirm_leaves_room() {
        let mut ev = Events::default();
        fill(&mut ev, 101, 100);
        assert!(ev.overflowed());
        ev.select(Some(Typ::Binary), 0, 1);
        let ids: Vec<u64> = ev.selected().map(|(e, _)| e.id).collect();
        ev.mark_written(&ids);
        ev.clear_written();
        assert!(!ev.overflowed());
        // analog was never overflowed, but fill it past the cap now and clear one
        ev.push(0, Data::Ana { value: 1.0, ts: 0 });
        assert!(ev.overflowed());
        ev.select(Some(Typ::Binary), 0, 1);
        let ids: Vec<u64> = ev.selected().map(|(e, _)| e.id).collect();
        ev.mark_written(&ids);
        ev.clear_written(); // frees binary only: the analog buffer is still full
        assert!(ev.overflowed());
    }

    #[test]
    fn select_honours_type_limit_variation_and_state() {
        let mut ev = Events::default();
        ev.push(1, Data::Ana { value: 1.0, ts: 0 });
        ev.push(2, Data::Bin { flags: ONLINE });
        ev.push(3, Data::Bin { flags: ONLINE });
        assert_eq!(ev.select(Some(Typ::Binary), 3, 1), 1);
        let sel: Vec<_> = ev.selected().map(|(e, v)| (e.idx, v)).collect();
        assert_eq!(sel, vec![(2, 3)]);
        assert_eq!(ev.select(None, 0, usize::MAX), 2); // the rest, defaults; the selected one is not taken again
        let sel: Vec<_> = ev.selected().map(|(e, v)| (e.idx, v)).collect();
        assert_eq!(sel, vec![(1, 6), (2, 3), (3, 2)]);
        assert!(!ev.unselected());
        ev.reset();
        assert!(ev.unselected());
        assert_eq!(ev.selected().count(), 0);
        assert_eq!(ev.select(None, 0, 0), 0);
    }

    #[test]
    fn written_events_are_deleted_by_a_confirm_and_returned_by_a_reset() {
        let mut ev = Events::default();
        fill(&mut ev, 3, 0);
        ev.select(None, 0, usize::MAX);
        ev.mark_written(&[0, 1]);
        ev.reset();
        assert_eq!(ev.select(None, 0, usize::MAX), 3);
        ev.mark_written(&[0, 1]);
        ev.clear_written();
        let left: Vec<u16> = ev.selected().map(|(e, _)| e.idx).collect();
        assert_eq!(left, vec![2]);
    }

    #[test]
    fn typ_groups_and_defaults() {
        assert_eq!((Typ::Binary.group(), Typ::Analog.group()), (2, 32));
        assert_eq!(
            (Typ::Binary.default_var(), Typ::Analog.default_var()),
            (2, 6)
        );
    }
}
