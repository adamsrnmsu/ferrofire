//! The outstation database: static points (binary and analog, keyed by DNP3 index), the event buffer, and the encoders that
//! turn both into object bytes in the variation a master asks for.

use std::collections::VecDeque;

/// Events kept per type when the config has no (or an unreadable) `<event-buffer-size>`: opendnp3 `EventBufferConfig::AllTypes(n)`.
pub const EVENT_CAP: u16 = 100;

pub const ONLINE: u8 = 0x01;
pub const RESTART: u8 = 0x02;
pub const OVER_RANGE: u8 = 0x20;
pub const STATE: u8 = 0x80;

/// A point. Only a point in Class 1-3 raises events: bennu v2026.11.03 defaults every point to Class 0 (static data only); the
/// config's `<class>` or a master's ASSIGN_CLASS moves it. `svar` and `evar` are the point's static and event variations (the ones
/// a READ of variation 0 and a class poll use), `deadband` the analog change an event needs and `last` the value of the last event.
#[derive(Debug, Clone, PartialEq)]
pub struct Point<V> {
    pub tag: String,
    pub sbo: bool,
    pub input: bool,
    pub class: u8,
    pub svar: u8,
    pub evar: u8,
    pub deadband: f64,
    pub last: V,
    pub value: V,
    pub flags: u8,
}

pub type BinPoint = Point<bool>;
pub type AnaPoint = Point<f64>;

/// The static variations a point answers `READ g?v0` with: inputs g1v2 / g30v6 (flags; double), outputs g10v2 / g40v1 (flags;
/// 32-bit integer and flags).
pub trait Static: Default + Copy {
    const INPUT_VAR: u8;
    const OUTPUT_VAR: u8;
    /// The event variations of an input and of an output.
    const INPUT_EVENT: u8;
    const OUTPUT_EVENT: u8;
}

impl Static for bool {
    const INPUT_VAR: u8 = 2;
    const OUTPUT_VAR: u8 = 2;
    const INPUT_EVENT: u8 = 2;
    const OUTPUT_EVENT: u8 = 1;
}

impl Static for f64 {
    const INPUT_VAR: u8 = 6;
    const OUTPUT_VAR: u8 = 1;
    const INPUT_EVENT: u8 = 6;
    const OUTPUT_EVENT: u8 = 1;
}

impl<V: Static> Point<V> {
    /// A point at power-up: value 0, flags RESTART.
    pub fn new(tag: &str, sbo: bool, input: bool) -> Point<V> {
        Point {
            tag: tag.into(),
            sbo,
            input,
            class: 0,
            svar: if input { V::INPUT_VAR } else { V::OUTPUT_VAR },
            evar: if input {
                V::INPUT_EVENT
            } else {
                V::OUTPUT_EVENT
            },
            deadband: 0.0,
            last: V::default(),
            value: V::default(),
            flags: RESTART,
        }
    }
}

impl BinPoint {
    /// Take the provider's value; `true` when that is an event.
    pub fn update(&mut self, value: bool) -> bool {
        let flags = ONLINE | if value { STATE } else { 0 };
        let changed = flags != self.flags;
        (self.value, self.flags) = (value, flags);
        changed && self.class > 0
    }
}

impl AnaPoint {
    /// Take the provider's value; `true` when that is an event: the flags changed, or the value is more than the deadband away from
    /// the value of the last event.
    pub fn update(&mut self, value: f64) -> bool {
        let changed = self.flags != ONLINE || (value - self.last).abs() > self.deadband;
        (self.value, self.flags) = (value, ONLINE);
        if changed {
            self.last = value;
        }
        changed && self.class > 0
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

/// Binary or analog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typ {
    Binary,
    Analog,
}

/// A static table. opendnp3 keeps inputs (g1, g30) and output status (g10, g40) apart, as bennu v2026.11.03 registers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table {
    pub typ: Typ,
    pub out: bool,
}

impl Table {
    pub fn group(self) -> u8 {
        match (self.typ, self.out) {
            (Typ::Binary, false) => 1,
            (Typ::Binary, true) => 10,
            (Typ::Analog, false) => 30,
            (Typ::Analog, true) => 40,
        }
    }
    /// Events are kept per table: inputs as g2 and g32, output status as g11 and g42 (bennu v2026.11.03, probe_out.py).
    fn slot(self) -> usize {
        self.typ as usize * 2 + usize::from(self.out)
    }
    /// The group of this table's events.
    pub fn event_group(self) -> u8 {
        [2, 11, 32, 42][self.slot()]
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
    pub class: u8,
    /// An output point's event (g11, g42), not an input's.
    pub out: bool,
    /// The variation the point's config gives its events (what a READ of variation 0 and a class poll answer).
    pub var: u8,
    pub data: Data,
    pub state: State,
}

impl Event {
    pub fn table(&self) -> Table {
        let typ = match self.data {
            Data::Bin { .. } => Typ::Binary,
            Data::Ana { .. } => Typ::Analog,
        };
        Table { typ, out: self.out }
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

/// The event buffer: one queue in the order events happened, at most `cap` of each type (the oldest is dropped; a type with
/// room for none keeps none and flags no overflow).
#[derive(Debug)]
pub struct Events {
    q: VecDeque<Event>,
    next: u64,
    overflow: [bool; 4],
    cap: usize,
}

impl Default for Events {
    fn default() -> Self {
        Events::new(EVENT_CAP)
    }
}

impl Events {
    pub fn new(cap: u16) -> Events {
        Events {
            q: VecDeque::new(),
            next: 0,
            overflow: [false; 4],
            cap: usize::from(cap),
        }
    }

    pub fn push(&mut self, idx: u16, class: u8, out: bool, var: u8, data: Data) {
        if self.cap == 0 {
            return;
        }
        let ev = Event {
            id: self.next,
            idx,
            class,
            out,
            var,
            data,
            state: State::Unselected,
        };
        let t = ev.table();
        if self.count(t) >= self.cap {
            if let Some(at) = self.q.iter().position(|e| e.table() == t) {
                self.q.remove(at);
            }
            self.overflow[t.slot()] = true;
        }
        self.q.push_back(ev);
        self.next += 1;
    }

    fn count(&self, t: Table) -> usize {
        self.q.iter().filter(|e| e.table() == t).count()
    }

    /// Any event not yet selected, in class `class` (any when 0): IIN1.1-1.3.
    pub fn unselected(&self, class: u8) -> bool {
        self.q
            .iter()
            .any(|e| e.state == State::Unselected && (class == 0 || e.class == class))
    }

    pub fn overflowed(&self) -> bool {
        self.overflow.iter().any(|&o| o)
    }

    /// Select unselected events of `typ` and `class` (any when `None`), oldest first, at most `limit`; `var` 0 means the default.
    pub fn select(
        &mut self,
        table: Option<Table>,
        class: Option<u8>,
        var: u8,
        limit: usize,
    ) -> usize {
        let mut n = 0;
        for e in self.q.iter_mut().filter(|e| e.state == State::Unselected) {
            if n == limit
                || table.is_some_and(|t| t != e.table())
                || class.is_some_and(|c| c != e.class)
            {
                continue;
            }
            let v = if var == 0 { e.var } else { var };
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
            for out in [false, true] {
                let t = Table { typ, out };
                let full = self.count(t) >= self.cap;
                self.overflow[t.slot()] &= full;
            }
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

    const IN_BIN: Table = Table {
        typ: Typ::Binary,
        out: false,
    };
    const IN_ANA: Table = Table {
        typ: Typ::Analog,
        out: false,
    };
    const OUT_BIN: Table = Table {
        typ: Typ::Binary,
        out: true,
    };
    const OUT_ANA: Table = Table {
        typ: Typ::Analog,
        out: true,
    };

    #[test]
    fn point_events_only_in_a_class_and_only_on_change() {
        let mut p = BinPoint::new("t", false, true);
        p.class = 1;
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
        p.class = 1;
        assert!(p.update(0.0));
        assert!(!p.update(0.0));
        assert!(p.update(0.5));
        assert!(!p.update(0.5));
        assert!(p.update(f64::INFINITY));
        assert!(!p.update(f64::INFINITY)); // inf - inf is NaN: no event, no flood
        let mut o = AnaPoint::new("t", true, false);
        assert!(!o.update(3.0));
        assert_eq!((o.value, o.flags, o.svar), (3.0, ONLINE, 1));
        assert_eq!(p.svar, 6);
    }

    /// Stock (stock-cfg.golden): the reference is the value of the last event, a change of more than the deadband is an event (not
    /// equal to it), and the reference moves also while the point is in class 0.
    #[test]
    fn a_deadband_measures_from_the_last_event() {
        let mut p = AnaPoint::new("t", false, true);
        (p.class, p.deadband) = (1, 5.0);
        assert!(p.update(0.0)); // the flags change from RESTART
        let seq = [
            (3.0, false),
            (6.0, true),
            (11.0, false),
            (12.0, true),
            (7.0, false),
            (6.5, true),
        ];
        for (v, event) in seq {
            assert_eq!(p.update(v), event, "{v}");
        }
        let mut z = AnaPoint::new("t", false, true);
        z.deadband = 5.0;
        assert!(!z.update(0.0) && !z.update(8.0), "class 0: no event");
        z.class = 1;
        assert!(!z.update(9.0), "the reference moved to 8 while in class 0");
        assert!(z.update(14.0));
        let mut n = AnaPoint::new("t", false, true);
        (n.class, n.deadband) = (1, -1.0);
        assert!(
            n.update(0.0) && n.update(0.0),
            "a negative deadband makes every update an event"
        );
    }

    #[test]
    fn an_event_buffer_of_the_configured_size_and_one_of_none() {
        let mut ev = Events::new(3);
        for i in 0..5 {
            ev.push(i, 1, false, 2, Data::Bin { flags: ONLINE });
        }
        ev.select(None, None, 0, usize::MAX);
        let idx: Vec<u16> = ev.selected().map(|(e, _)| e.idx).collect();
        assert_eq!(idx, [2, 3, 4]);
        assert!(ev.overflowed());
        let mut none = Events::new(0);
        none.push(0, 1, false, 2, Data::Bin { flags: ONLINE });
        assert!(
            !none.overflowed() && !none.unselected(0),
            "no room, no event, no IIN2.3"
        );
    }

    #[test]
    fn a_point_event_carries_its_own_variation_unless_the_master_names_one() {
        let mut ev = Events::new(10);
        ev.push(0, 1, false, 3, Data::Bin { flags: ONLINE });
        ev.push(1, 1, false, 1, Data::Bin { flags: ONLINE });
        ev.select(None, None, 0, usize::MAX);
        let vars: Vec<u8> = ev.selected().map(|(_, v)| v).collect();
        assert_eq!(vars, [3, 1]);
        ev.reset();
        ev.select(None, None, 2, usize::MAX);
        assert!(ev.selected().all(|(_, v)| v == 2));
    }

    #[test]
    fn default_variations_follow_the_point_kind() {
        assert_eq!(BinPoint::new("t", false, true).svar, 2);
        assert_eq!(BinPoint::new("t", false, false).svar, 2);
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
            class: 1,
            out: false,
            var: 2,
            data: Data::Bin { flags: 0x81 },
            state: State::Unselected,
        };
        assert_eq!(b.encode(1), vec![2, 1, 0x81]);
        assert_eq!(b.encode(2), vec![2, 1, 0x81, 0, 0, 0, 0, 0, 0]);
        assert_eq!(b.encode(3), vec![2, 1, 0x81, 0, 0]);
        let a = Event {
            id: 0,
            idx: 3,
            class: 1,
            out: false,
            var: 6,
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
            ev.push(i, 1, false, 2, Data::Bin { flags: ONLINE });
        }
        for i in 0..ana {
            ev.push(i, 1, false, 2, Data::Ana { value: 0.0, ts: 0 });
        }
    }

    #[test]
    fn the_oldest_event_of_a_full_type_is_dropped_and_overflow_flagged() {
        let mut ev = Events::default();
        fill(&mut ev, 130, 5);
        assert!(ev.overflowed() && ev.unselected(0));
        ev.select(Some(IN_BIN), None, 0, usize::MAX);
        let idx: Vec<u16> = ev.selected().map(|(e, _)| e.idx).collect();
        assert_eq!((idx.len(), idx[0], idx[99]), (100, 30, 129));
        assert_eq!(ev.count(IN_ANA), 5);
    }

    #[test]
    fn overflow_ends_when_a_confirm_leaves_room() {
        let mut ev = Events::default();
        fill(&mut ev, 101, 100);
        assert!(ev.overflowed());
        ev.select(Some(IN_BIN), None, 0, 1);
        let ids: Vec<u64> = ev.selected().map(|(e, _)| e.id).collect();
        ev.mark_written(&ids);
        ev.clear_written();
        assert!(!ev.overflowed());
        // analog was never overflowed, but fill it past the cap now and clear one
        ev.push(0, 1, false, 6, Data::Ana { value: 1.0, ts: 0 });
        assert!(ev.overflowed());
        ev.select(Some(IN_BIN), None, 0, 1);
        let ids: Vec<u64> = ev.selected().map(|(e, _)| e.id).collect();
        ev.mark_written(&ids);
        ev.clear_written(); // frees binary only: the analog buffer is still full
        assert!(ev.overflowed());
    }

    #[test]
    fn select_honours_type_limit_variation_and_state() {
        let mut ev = Events::default();
        ev.push(1, 1, false, 6, Data::Ana { value: 1.0, ts: 0 });
        ev.push(2, 1, false, 2, Data::Bin { flags: ONLINE });
        ev.push(3, 1, false, 2, Data::Bin { flags: ONLINE });
        assert_eq!(ev.select(Some(IN_BIN), None, 3, 1), 1);
        let sel: Vec<_> = ev.selected().map(|(e, v)| (e.idx, v)).collect();
        assert_eq!(sel, vec![(2, 3)]);
        assert_eq!(ev.select(None, None, 0, usize::MAX), 2); // the rest, defaults; the selected one is not taken again
        let sel: Vec<_> = ev.selected().map(|(e, v)| (e.idx, v)).collect();
        assert_eq!(sel, vec![(1, 6), (2, 3), (3, 2)]);
        assert!(!ev.unselected(0));
        ev.reset();
        assert!(ev.unselected(0));
        assert_eq!(ev.selected().count(), 0);
        assert_eq!(ev.select(None, None, 0, 0), 0);
    }

    #[test]
    fn written_events_are_deleted_by_a_confirm_and_returned_by_a_reset() {
        let mut ev = Events::default();
        fill(&mut ev, 3, 0);
        ev.select(None, None, 0, usize::MAX);
        ev.mark_written(&[0, 1]);
        ev.reset();
        assert_eq!(ev.select(None, None, 0, usize::MAX), 3);
        ev.mark_written(&[0, 1]);
        ev.clear_written();
        let left: Vec<u16> = ev.selected().map(|(e, _)| e.idx).collect();
        assert_eq!(left, vec![2]);
    }

    #[test]
    fn event_groups_and_defaults() {
        let t = |typ, out| Table { typ, out };
        let all = [
            t(Typ::Binary, false),
            t(Typ::Binary, true),
            t(Typ::Analog, false),
            t(Typ::Analog, true),
        ];
        assert_eq!(all.map(Table::event_group), [2, 11, 32, 42]);
    }

    /// Input and output events are separate buffers: a full input buffer does not push out an output event.
    #[test]
    fn output_events_are_a_type_of_their_own() {
        let mut ev = Events::default();
        ev.push(5, 1, true, 1, Data::Bin { flags: ONLINE });
        fill(&mut ev, 101, 0);
        assert!(ev.overflowed());
        assert_eq!(ev.count(OUT_BIN), 1);
        assert_eq!(ev.select(Some(IN_BIN), None, 0, usize::MAX), 100);
        assert_eq!(ev.select(Some(OUT_BIN), None, 0, usize::MAX), 1);
        assert_eq!(ev.select(Some(OUT_ANA), None, 0, usize::MAX), 0);
    }
}
