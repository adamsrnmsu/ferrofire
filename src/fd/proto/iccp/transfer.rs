//! DSTransfer_Set (IEC 60870-6-503, from recall, not the standard text: see docs/field-device-iccp.md): the structure a client
//! writes to arm periodic and by-exception reporting, and the schedule that decides when a report is due.
//!
//! Components, in order: Data_Set_Name {scope, domain, name}, Start_Time, Interval, TLE, Buffer_Time, Integrity_Check,
//! DSConditions_Requested (5 bits), Block_Data, Critical, RBE, All_Changes_Reported, Status, Event_Code_Requested.
//! The 1996 edition has no All_Changes_Reported (12 members, as in the Wireshark mms.pcap of 2005, where the fourth flag is always
//! true: it is Status): both layouts are accepted, and read back in the one the configured TASE2_Version names. The scope is unsigned in the field (both real captures); an integer scope is accepted too.
//! Supported: Interval and Integrity_Check timers, object change (held for Buffer_Time seconds after the first change), RBE,
//! Status (arm/disarm). Start_Time, TLE, All_Changes_Reported and Event_Code_Requested are stored and read back but have no
//! effect; Critical and Block_Data other than false are refused (object-value-invalid), because honouring them silently would be a lie.

use crate::model::Value as Datum;
use iec61850_rs::proto::data::Value;
use iec61850_rs::proto::mms::typespec::{Component, TypeSpec};

pub const INVALID: i64 = 11; // DataAccessError object-value-invalid
pub const INCONSISTENT: i64 = 7; // type-inconsistent

/// DSConditions bits, MSB first in the bit string.
pub const INTERVAL: u8 = 0x80;
pub const INTEGRITY: u8 = 0x40;
pub const CHANGE: u8 = 0x20;
/// Every condition this server can detect; the other two (operator request, external event) are never raised.
pub const SUPPORTED: u8 = INTERVAL | INTEGRITY | CHANGE;

/// `Transfer_Set_Name` / `Data_Set_Name`: {scope (1 = domain), domain, name}.
pub fn name_value(domain: &str, name: &str) -> Value {
    Value::Structure(vec![
        Value::Unsigned(1),
        Value::VisibleString(domain.into()),
        Value::VisibleString(name.into()),
    ])
}

pub fn name_type() -> TypeSpec {
    structure(vec![
        TypeSpec::Unsigned(8),
        TypeSpec::VisibleString(-32),
        TypeSpec::VisibleString(-32),
    ])
}

pub fn structure(parts: Vec<TypeSpec>) -> TypeSpec {
    let components = parts
        .into_iter()
        .map(|type_spec| Component {
            name: None,
            type_spec,
        })
        .collect();
    TypeSpec::Structure {
        packed: false,
        components,
    }
}

/// The name a `{scope, domain, name}` structure carries, if it is one.
fn name_of(v: &Value) -> Option<(String, String)> {
    match v.members()? {
        [Value::Integer(_) | Value::Unsigned(_), Value::VisibleString(d), Value::VisibleString(n)] => {
            Some((d.clone(), n.clone()))
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Cfg {
    pub domain: String,
    pub data_set: String,
    pub start: i64,
    pub interval: i64,
    pub tle: i64,
    pub buffer: i64,
    pub integrity: i64,
    pub conditions: u8,
    pub block: bool,
    pub critical: bool,
    pub rbe: bool,
    pub all_changes: bool,
    pub status: bool,
    pub event_code: i64,
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Unsigned(u) => i64::try_from(*u).ok(),
        _ => None,
    }
}

fn flag(v: &Value) -> Option<bool> {
    match v {
        Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

/// The type of a DSTransfer_Set: 13 members, or 12 (no All_Changes_Reported) when `v1996`.
pub fn type_spec(v1996: bool) -> TypeSpec {
    let int = || TypeSpec::Integer(32);
    let mut parts = vec![
        name_type(),
        int(),
        int(),
        int(),
        int(),
        int(),
        TypeSpec::BitString(5),
    ];
    // Block_Data, Critical, RBE, [All_Changes_Reported,] Status
    parts.extend(std::iter::repeat_with(|| TypeSpec::Boolean).take(if v1996 { 4 } else { 5 }));
    parts.push(TypeSpec::Integer(16));
    structure(parts)
}

/// The numbers of components `idx`, none negative (seconds); `None` if any is not an integer.
fn counts(m: &[Value], idx: [usize; 5]) -> Option<Vec<i64>> {
    idx.iter()
        .map(|i| int(m.get(*i)?).filter(|x| *x >= 0))
        .collect()
}

fn flags(m: &[Value], idx: [usize; 4]) -> Option<Vec<bool>> {
    idx.iter().map(|i| flag(m.get(*i)?)).collect()
}

impl Cfg {
    /// Acknowledged (critical) and blocked reports, and conditions we never detect, are not built.
    fn supported(&self) -> bool {
        !self.block && !self.critical && self.conditions & !SUPPORTED == 0
    }

    /// A DataAccessError code on refusal: type-inconsistent for the wrong shape, object-value-invalid for what is not supported.
    pub fn from_value(v: &Value) -> Result<Cfg, i64> {
        let m = v
            .members()
            .filter(|m| matches!(m.len(), 12 | 13))
            .ok_or(INCONSISTENT)?;
        let (domain, data_set) = name_of(&m[0]).ok_or(INCONSISTENT)?;
        // start, interval, tle, buffer, integrity (seconds); block, critical, rbe and status (the one before Event_Code); 1996 has no
        // All_Changes_Reported
        let n = counts(m, [1, 2, 3, 4, 5]).ok_or(INCONSISTENT)?;
        let b = flags(m, [7, 8, 9, m.len() - 2]).ok_or(INCONSISTENT)?;
        let all_changes = if m.len() == 13 {
            flag(&m[10])
        } else {
            Some(false)
        };
        let Value::BitString { bytes, .. } = &m[6] else {
            return Err(INCONSISTENT);
        };
        let cfg = Cfg {
            domain,
            data_set,
            start: n[0],
            interval: n[1],
            tle: n[2],
            buffer: n[3],
            integrity: n[4],
            conditions: bytes.first().copied().unwrap_or(0) & 0xF8,
            block: b[0],
            critical: b[1],
            rbe: b[2],
            all_changes: all_changes.ok_or(INCONSISTENT)?,
            status: b[3],
            event_code: int(&m[m.len() - 1]).ok_or(INCONSISTENT)?,
        };
        if cfg.supported() {
            Ok(cfg)
        } else {
            Err(INVALID)
        }
    }

    /// As written: 13 members, or 12 (no All_Changes_Reported) when `v1996`.
    pub fn to_value(&self, v1996: bool) -> Value {
        let int = Value::Integer;
        let mut m = vec![
            name_value(&self.domain, &self.data_set),
            int(self.start),
            int(self.interval),
            int(self.tle),
            int(self.buffer),
            int(self.integrity),
            conditions(self.conditions),
            Value::Boolean(self.block),
            Value::Boolean(self.critical),
            Value::Boolean(self.rbe),
        ];
        if !v1996 {
            m.push(Value::Boolean(self.all_changes));
        }
        m.push(Value::Boolean(self.status));
        m.push(int(self.event_code));
        Value::Structure(m)
    }
}

/// A 5-bit DSConditions bit string.
pub fn conditions(bits: u8) -> Value {
    Value::BitString {
        unused: 3,
        bytes: vec![bits & 0xF8],
    }
}

/// What a report carries: the conditions detected and the data set members (by index) it lists.
#[derive(Debug, PartialEq, Eq)]
pub struct Due {
    pub conditions: u8,
    pub members: Vec<usize>,
}

/// One transfer set of one association: its configuration and where its timers and change baseline stand.
#[derive(Default)]
pub struct Ts {
    pub cfg: Option<Cfg>,
    armed: bool,
    seen: Vec<Option<Datum>>,
    next_interval: u64,
    next_integrity: u64,
    /// When the first change not yet reported was seen (Buffer_Time holds the report back from here).
    pending: Option<u64>,
}

impl Ts {
    /// A write of `cfg`. Arming (status true) takes `current` as the baseline, so only later changes are reported, and starts
    /// both timers at `now_ms`.
    pub fn set(&mut self, cfg: Cfg, now_ms: u64, current: Vec<Option<Datum>>) {
        let arm = cfg.status
            && (!self.armed || self.cfg.as_ref().map(|c| &c.data_set) != Some(&cfg.data_set));
        if arm {
            self.seen = current;
        }
        if cfg.status
            && (arm
                || self.cfg.as_ref().map(|c| (c.interval, c.integrity))
                    != Some((cfg.interval, cfg.integrity)))
        {
            self.next_interval = now_ms + secs(cfg.interval);
            self.next_integrity = now_ms + secs(cfg.integrity);
        }
        self.armed = cfg.status;
        self.pending = None;
        self.cfg = Some(cfg);
    }

    /// The timer conditions that have run out at `now_ms`; each timer that fired starts again.
    fn timers(&mut self, now_ms: u64) -> u8 {
        let cfg = self.cfg.clone().unwrap_or_default();
        let mut hit = 0;
        for (every, next, bit) in [
            (cfg.interval, &mut self.next_interval, INTERVAL),
            (cfg.integrity, &mut self.next_integrity, INTEGRITY),
        ] {
            if every > 0 && cfg.conditions & bit != 0 && now_ms >= *next {
                *next = now_ms + secs(every);
                hit |= bit;
            }
        }
        hit
    }

    /// Is a report due at `now_ms` given the members' `current` values? Taking it moves the baseline and the timers.
    pub fn poll(&mut self, now_ms: u64, current: &[Option<Datum>]) -> Option<Due> {
        let cfg = self.cfg.as_ref().filter(|_| self.armed)?;
        let (watch_change, rbe, hold) = (cfg.conditions & CHANGE != 0, cfg.rbe, secs(cfg.buffer));
        let mut hit = self.timers(now_ms);
        let changed: Vec<usize> = (0..current.len())
            .filter(|i| self.seen.get(*i) != Some(&current[*i]))
            .collect();
        if watch_change && !changed.is_empty() {
            let since = *self.pending.get_or_insert(now_ms);
            if now_ms >= since + hold {
                hit |= CHANGE;
            }
        } else {
            self.pending = None;
        }
        if hit == 0 {
            return None;
        }
        self.pending = None;
        self.seen = current.to_vec();
        let only_changes = rbe && hit == CHANGE;
        Some(Due {
            conditions: hit,
            members: if only_changes {
                changed
            } else {
                (0..current.len()).collect()
            },
        })
    }
}

fn secs(s: i64) -> u64 {
    u64::try_from(s).unwrap_or(0).saturating_mul(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Cfg {
        Cfg {
            domain: "EMS1".into(),
            data_set: "DS1".into(),
            interval: 2,
            integrity: 0,
            conditions: INTERVAL | CHANGE,
            status: true,
            ..Cfg::default()
        }
    }

    fn a(x: f64) -> Option<Datum> {
        Some(Datum::Analog(x))
    }

    #[test]
    fn a_structure_round_trips_and_shape_errors_are_inconsistent() {
        let c = cfg();
        assert_eq!(Cfg::from_value(&c.to_value(false)), Ok(c.clone()));
        assert_eq!(Cfg::from_value(&Value::Integer(1)), Err(INCONSISTENT));
        let mut short = c.to_value(false);
        if let Value::Structure(m) = &mut short {
            m.pop();
        }
        assert_eq!(Cfg::from_value(&short), Err(INCONSISTENT));
        let mut bad = c.to_value(false);
        if let Value::Structure(m) = &mut bad {
            m[2] = Value::Boolean(true);
        }
        assert_eq!(Cfg::from_value(&bad), Err(INCONSISTENT));
        let mut neg = c.to_value(false);
        if let Value::Structure(m) = &mut neg {
            m[2] = Value::Integer(-1);
        }
        assert_eq!(
            Cfg::from_value(&neg),
            Err(INCONSISTENT),
            "no negative seconds"
        );
        assert_ne!(type_spec(false), type_spec(true));
    }

    #[test]
    fn what_is_not_supported_is_refused_not_ignored() {
        for (i, v) in [
            (7, Value::Boolean(true)), // Block_Data
            (8, Value::Boolean(true)), // Critical
            (6, conditions(0x10)),     // operator request
            (6, conditions(0x08)),     // external event
        ] {
            let mut x = cfg().to_value(false);
            if let Value::Structure(m) = &mut x {
                m[i] = v;
            }
            assert_eq!(Cfg::from_value(&x), Err(INVALID), "component {i}");
        }
    }

    #[test]
    fn the_scope_is_unsigned_out_and_integer_or_unsigned_in() {
        let c = cfg();
        let v = c.to_value(false);
        assert_eq!(
            v.members().unwrap()[0].members().unwrap()[0],
            Value::Unsigned(1)
        );
        let mut x = v;
        if let Value::Structure(m) = &mut x {
            m[0] = Value::Structure(vec![
                Value::Integer(1),
                Value::VisibleString("EMS1".into()),
                Value::VisibleString("DS1".into()),
            ]);
        }
        assert_eq!(Cfg::from_value(&x), Ok(c));
        assert_eq!(
            name_type(),
            structure(vec![
                TypeSpec::Unsigned(8),
                TypeSpec::VisibleString(-32),
                TypeSpec::VisibleString(-32)
            ])
        );
    }

    #[test]
    fn the_1996_layout_has_twelve_members_without_all_changes_reported() {
        let mut c = cfg();
        c.all_changes = true;
        let v = c.to_value(true);
        let back = Cfg::from_value(&v).unwrap();
        assert!(
            !back.all_changes && back.status,
            "the flag before Event_Code is Status"
        );
        c.all_changes = false;
        let v = c.to_value(true);
        assert_eq!(v.members().unwrap().len(), 12);
        assert_eq!(c.to_value(false).members().unwrap().len(), 13);
        assert_eq!(Cfg::from_value(&v), Ok(c.clone()));
        let mut off = c.clone();
        off.status = false;
        assert_eq!(
            Cfg::from_value(&off.to_value(true)),
            Ok(off),
            "a 1996 client can stop a set"
        );
        let TypeSpec::Structure { components, .. } = type_spec(true) else {
            panic!()
        };
        assert_eq!(components.len(), 12);
        let mut r = c.to_value(false);
        if let Value::Structure(m) = &mut r {
            m.truncate(11); // 11 members: neither layout
        }
        assert_eq!(Cfg::from_value(&r), Err(INCONSISTENT));
    }

    #[test]
    fn buffer_time_holds_a_change_report_back_and_gathers_changes() {
        let mut c = cfg();
        c.interval = 0;
        c.buffer = 2;
        c.rbe = true;
        let mut t = Ts::default();
        t.set(c, 0, vec![a(1.0), a(2.0)]);
        assert_eq!(
            t.poll(1_000, &[a(5.0), a(2.0)]),
            None,
            "first change starts the buffer"
        );
        assert_eq!(t.poll(2_999, &[a(5.0), a(7.0)]), None, "still buffering");
        assert_eq!(
            t.poll(3_000, &[a(5.0), a(7.0)]),
            Some(Due {
                conditions: CHANGE,
                members: vec![0, 1]
            }),
            "both changes in one report, two seconds after the first"
        );
        assert_eq!(t.poll(9_000, &[a(5.0), a(7.0)]), None);
        // a change that reverts inside the buffer is never reported, and the next change starts a fresh buffer
        assert_eq!(t.poll(10_000, &[a(6.0), a(7.0)]), None);
        assert_eq!(t.poll(11_000, &[a(5.0), a(7.0)]), None);
        assert_eq!(t.poll(20_000, &[a(6.0), a(7.0)]), None);
        assert!(t.poll(22_000, &[a(6.0), a(7.0)]).is_some());
    }

    #[test]
    fn rewriting_a_set_restarts_the_buffer() {
        let mut c = cfg();
        c.interval = 0;
        c.buffer = 2;
        let mut t = Ts::default();
        t.set(c.clone(), 0, vec![a(1.0)]);
        assert_eq!(t.poll(1_000, &[a(2.0)]), None, "buffer starts at 1 s");
        t.set(c, 2_000, vec![a(1.0)]);
        assert_eq!(
            t.poll(3_000, &[a(2.0)]),
            None,
            "restarted at 3 s, not due at 3 s"
        );
        assert!(t.poll(5_000, &[a(2.0)]).is_some());
    }

    #[test]
    fn interval_reports_everything_when_the_time_comes() {
        let mut t = Ts::default();
        t.set(cfg(), 10_000, vec![a(1.0), a(2.0)]);
        let now = [a(1.0), a(2.0)];
        assert_eq!(t.poll(11_999, &now), None);
        assert_eq!(
            t.poll(12_000, &now),
            Some(Due {
                conditions: INTERVAL,
                members: vec![0, 1]
            })
        );
        assert_eq!(t.poll(13_000, &now), None, "the timer restarted");
        assert!(t.poll(14_000, &now).is_some());
    }

    #[test]
    fn a_change_is_reported_once_and_whole_without_rbe() {
        let mut t = Ts::default();
        t.set(cfg(), 0, vec![a(1.0), a(2.0)]);
        let due = t.poll(100, &[a(1.0), a(3.0)]).unwrap();
        assert_eq!(
            due,
            Due {
                conditions: CHANGE,
                members: vec![0, 1]
            }
        );
        assert_eq!(t.poll(200, &[a(1.0), a(3.0)]), None, "the baseline moved");
    }

    #[test]
    fn rbe_lists_only_what_changed_and_an_interval_hit_lists_all() {
        let mut c = cfg();
        c.rbe = true;
        let mut t = Ts::default();
        t.set(c, 0, vec![a(1.0), a(2.0), a(3.0)]);
        let due = t.poll(100, &[a(1.0), a(9.0), a(3.0)]).unwrap();
        assert_eq!(
            due,
            Due {
                conditions: CHANGE,
                members: vec![1]
            }
        );
        let both = t.poll(2_000, &[a(5.0), a(9.0), a(3.0)]).unwrap();
        assert_eq!(
            both,
            Due {
                conditions: INTERVAL | CHANGE,
                members: vec![0, 1, 2]
            }
        );
    }

    #[test]
    fn integrity_is_its_own_condition_and_only_when_requested() {
        let mut c = cfg();
        c.interval = 0;
        c.integrity = 3;
        c.conditions = INTEGRITY;
        let mut t = Ts::default();
        t.set(c.clone(), 0, vec![a(1.0)]);
        assert_eq!(t.poll(2_999, &[a(1.0)]), None);
        assert_eq!(t.poll(3_000, &[a(1.0)]).unwrap().conditions, INTEGRITY);
        c.conditions = CHANGE; // integrity time set but not requested
        let mut t = Ts::default();
        t.set(c, 0, vec![a(1.0)]);
        assert_eq!(t.poll(9_000, &[a(1.0)]), None);
    }

    #[test]
    fn disarmed_or_unset_never_reports_and_arming_baselines_the_current_values() {
        let mut t = Ts::default();
        assert_eq!(t.poll(0, &[a(1.0)]), None, "no configuration");
        let mut off = cfg();
        off.status = false;
        t.set(off, 0, vec![a(1.0)]);
        assert_eq!(t.poll(10_000, &[a(2.0)]), None, "disarmed");
        t.set(cfg(), 20_000, vec![a(2.0)]);
        assert_eq!(t.poll(20_100, &[a(2.0)]), None, "2.0 is the baseline");
        assert_eq!(t.poll(21_999, &[a(2.0)]), None, "timer counts from arming");
        assert!(t.poll(22_000, &[a(2.0)]).is_some());
    }

    #[test]
    fn rewriting_an_armed_set_keeps_its_baseline_and_timers_unless_they_changed() {
        let mut t = Ts::default();
        t.set(cfg(), 0, vec![a(1.0)]);
        let mut again = cfg();
        again.tle = 60;
        t.set(again, 1_500, vec![a(7.0)]);
        assert!(
            t.poll(1_600, &[a(7.0)]).is_some(),
            "7.0 is still a change against the first baseline"
        );
        let mut slower = cfg();
        slower.interval = 10;
        t.set(slower, 5_000, vec![a(7.0)]);
        assert_eq!(t.poll(14_999, &[a(7.0)]), None);
        assert!(t.poll(15_000, &[a(7.0)]).is_some());
    }

    #[test]
    fn a_member_list_shorter_than_the_baseline_does_not_panic() {
        let mut t = Ts::default();
        t.set(cfg(), 0, vec![a(1.0), a(2.0), a(3.0)]);
        assert!(t.poll(10, &[a(1.0)]).is_none());
        assert!(t.poll(10, &[a(1.0), a(2.0), a(3.0), a(4.0)]).is_some());
    }
}
