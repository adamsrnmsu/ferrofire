//! The MQTT module without a socket: what to publish when, and what a command message does to the store. `Engine` is driven by the
//! session loop in `mod.rs` (connect, tick every ~100 ms, one call per incoming message) and by the tests directly.
//!
//! Plain scheme: `<prefix>/<device>/<tag>` carries the value as text, `<prefix>/<device>/<tag>/set` takes a command,
//! `<prefix>/<device>/status` is `online` / `offline` (the will). Sparkplug B scheme: NBIRTH / NDATA / NDEATH out, NCMD in.

use super::sparkplug::{self as sp, Command, Key, Sample};
use crate::bennu::fmt_value;
pub use crate::fd::proto::point::{Num, Point};
use crate::fd::store::Store;
use crate::hint::show;
use crate::model::{Kind, Value};
use std::collections::HashMap;

/// One message to publish.
#[derive(Debug, PartialEq, Eq)]
pub struct Out {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    pub retain: bool,
}

pub enum Scheme {
    Plain {
        prefix: String,
        device: String,
        qos: u8,
        retain: bool,
    },
    Sparkplug {
        group: String,
        edge: String,
    },
}

pub struct Engine {
    scheme: Scheme,
    points: Vec<Point>,
    by_name: HashMap<String, usize>,
    /// What was last published per point.
    last: Vec<Option<Value>>,
    /// Milliseconds between full republications; 0 = never.
    refresh_ms: u64,
    last_full: u64,
    need_birth: bool,
    /// The bdSeq of the current session, and of the next one (0..=255, wrapping).
    bd: u64,
    next_bd: u64,
    seq: u64,
}

const WILL_QOS: u8 = 1;

fn same(a: &Option<Value>, b: &Option<Value>) -> bool {
    match (a, b) {
        (Some(Value::Analog(x)), Some(Value::Analog(y))) => x == y || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

/// A plain-scheme command payload for a tag of `kind`: binary takes true/false/1/0/on/off, analog a finite number.
fn parse_plain(kind: Kind, payload: &[u8]) -> Option<Num> {
    let s = std::str::from_utf8(payload)
        .ok()?
        .trim()
        .to_ascii_lowercase();
    match kind {
        Kind::Binary => match s.as_str() {
            "true" | "1" | "on" => Some(Num::Bool(true)),
            "false" | "0" | "off" => Some(Num::Bool(false)),
            _ => None,
        },
        Kind::Analog => s.parse().ok().map(Num::Number),
    }
}

impl Engine {
    pub fn new(scheme: Scheme, points: Vec<Point>, refresh_ms: u64) -> Engine {
        let by_name = points
            .iter()
            .enumerate()
            .map(|(i, p)| (p.tag.clone(), i))
            .collect();
        Engine {
            scheme,
            last: vec![None; points.len()],
            points,
            by_name,
            refresh_ms,
            last_full: 0,
            need_birth: true,
            bd: 0,
            next_bd: 0,
            seq: 0,
        }
    }

    fn base(&self, what: &str) -> String {
        match &self.scheme {
            Scheme::Plain { prefix, device, .. } => format!("{prefix}/{device}/{what}"),
            Scheme::Sparkplug { group, edge } => sp::topic(group, what, edge),
        }
    }

    /// Start a session: the will to register in CONNECT. Sparkplug advances `bdSeq` here so that the NDEATH and the NBIRTH agree.
    pub fn begin_session(&mut self) -> Out {
        self.need_birth = true;
        self.bd = self.next_bd;
        self.next_bd = (self.next_bd + 1) % 256;
        self.goodbye()
    }

    /// What a clean shutdown publishes before DISCONNECT (a will is only sent on an unclean one).
    pub fn goodbye(&self) -> Out {
        match self.scheme {
            Scheme::Plain { .. } => Out {
                topic: self.base("status"),
                payload: b"offline".to_vec(),
                qos: WILL_QOS,
                retain: true,
            },
            Scheme::Sparkplug { .. } => Out {
                topic: self.base("NDEATH"),
                payload: sp::death(self.bd),
                qos: WILL_QOS,
                retain: false,
            },
        }
    }

    /// Topic filters to subscribe to, with QoS (commands are QoS 1 in the plain scheme).
    pub fn subscriptions(&self) -> Vec<(String, u8)> {
        match self.scheme {
            // ponytail: `+` matches one level, so a tag name with a `/` in it cannot be commanded
            Scheme::Plain { .. } => vec![(self.base("+/set"), 1)],
            Scheme::Sparkplug { .. } => vec![(self.base("NCMD"), 1)],
        }
    }

    /// A publication was lost (queue full): republish everything as a new birth, so a Sparkplug host sees `seq` restart.
    pub fn lost(&mut self) {
        self.need_birth = true;
    }

    fn samples<'a>(
        &'a self,
        now: &'a [Option<Value>],
        only: impl Fn(usize) -> bool,
    ) -> Vec<Sample<'a>> {
        self.points
            .iter()
            .enumerate()
            .filter(|(i, _)| only(*i))
            .filter_map(|(i, p)| {
                Some(Sample {
                    name: &p.tag,
                    alias: i as u64 + 1,
                    value: now[i]?,
                })
            })
            .collect()
    }

    fn birth(&mut self, now: &[Option<Value>], ts: u64) -> Vec<Out> {
        self.seq = 0;
        match &self.scheme {
            Scheme::Plain { qos, retain, .. } => {
                let (qos, retain) = (*qos, *retain);
                let mut out = vec![Out {
                    topic: self.base("status"),
                    payload: b"online".to_vec(),
                    qos: WILL_QOS,
                    retain: true,
                }];
                out.extend(self.values(now, |_| true, qos, retain));
                out
            }
            Scheme::Sparkplug { .. } => vec![Out {
                topic: self.base("NBIRTH"),
                payload: sp::birth(ts, self.bd, &self.samples(now, |_| true)),
                qos: 0,
                retain: false,
            }],
        }
    }

    fn values(
        &self,
        now: &[Option<Value>],
        only: impl Fn(usize) -> bool,
        qos: u8,
        retain: bool,
    ) -> Vec<Out> {
        self.points
            .iter()
            .enumerate()
            .filter(|(i, _)| only(*i))
            .filter_map(|(i, p)| {
                Some(Out {
                    topic: self.base(&p.tag),
                    payload: fmt_value(now[i]?).into_bytes(),
                    qos,
                    retain,
                })
            })
            .collect()
    }

    /// Publish what is due at `now_ms` (a monotonic clock; `ts_ms`, unix milliseconds, goes into Sparkplug payloads): a birth after a
    /// connect, otherwise the points that changed since last time, and everything once per refresh period.
    pub fn tick(&mut self, store: &Store, now_ms: u64, ts_ms: u64) -> Vec<Out> {
        let now: Vec<Option<Value>> = self.points.iter().map(|p| store.get(&p.tag)).collect();
        if std::mem::take(&mut self.need_birth) {
            self.last = now.clone();
            self.last_full = now_ms;
            return self.birth(&now, ts_ms);
        }
        let full = self.refresh_ms > 0 && now_ms.saturating_sub(self.last_full) >= self.refresh_ms;
        let due_at: Vec<bool> = (0..now.len())
            .map(|i| full || !same(&self.last[i], &now[i]))
            .collect();
        let due = |i: usize| due_at[i];
        let out = match &self.scheme {
            Scheme::Plain { qos, retain, .. } => self.values(&now, due, *qos, *retain),
            Scheme::Sparkplug { .. } => {
                let samples = self.samples(&now, due);
                if samples.is_empty() {
                    vec![]
                } else {
                    let payload = sp::data(ts_ms, (self.seq + 1) % 256, &samples);
                    self.seq = (self.seq + 1) % 256;
                    vec![Out {
                        topic: self.base("NDATA"),
                        payload,
                        qos: 0,
                        retain: false,
                    }]
                }
            }
        };
        if full {
            self.last_full = now_ms;
        }
        for i in (0..due_at.len()).filter(|&i| due_at[i]) {
            self.last[i] = now[i];
        }
        out
    }

    /// An incoming message. Returns one line per command that was refused (for the event log); an accepted write is queued in the
    /// store like any protocol's. A Sparkplug rebirth request makes the next `tick` publish a new NBIRTH.
    pub fn message(&mut self, topic: &str, payload: &[u8], store: &Store) -> Vec<String> {
        match self.scheme {
            Scheme::Plain { .. } => self
                .plain_command(topic, payload, store)
                .into_iter()
                .collect(),
            Scheme::Sparkplug { .. } => self.sparkplug_command(topic, payload, store),
        }
    }

    fn plain_command(&mut self, topic: &str, payload: &[u8], store: &Store) -> Option<String> {
        let tag = topic.strip_prefix(&self.base(""))?.strip_suffix("/set")?;
        let Some(&i) = self.by_name.get(tag) else {
            return Some(format!("command for unknown tag `{}` skipped", show(tag)));
        };
        let kind = self.points[i].kind;
        match parse_plain(kind, payload) {
            Some(n) => self.write(i, n, store).err(),
            None => Some(format!(
                "command for `{}`: payload `{}` is not a {kind:?} value",
                show(tag),
                show(&String::from_utf8_lossy(payload))
            )),
        }
    }

    fn sparkplug_command(&mut self, topic: &str, payload: &[u8], store: &Store) -> Vec<String> {
        if topic != self.base("NCMD") {
            return vec![];
        }
        match sp::decode_command(payload) {
            Ok(cmds) => cmds
                .into_iter()
                .filter_map(|c| self.apply(c, store))
                .collect(),
            Err(e) => vec![format!("undecodable NCMD payload: {e}")],
        }
    }

    /// The point an NCMD metric addresses: by name, or by alias (1-based position in the point list).
    fn lookup(&self, key: &Key) -> Option<usize> {
        match key {
            Key::Name(n) => self.by_name.get(n).copied(),
            Key::Alias(a) => usize::try_from(*a)
                .ok()
                .and_then(|a| a.checked_sub(1))
                .filter(|&i| i < self.points.len()),
        }
    }

    /// One NCMD command; `Some` is the line to log when it was refused.
    fn apply(&mut self, c: Command, store: &Store) -> Option<String> {
        let Command::Write { key, value } = c else {
            self.need_birth = true;
            return None;
        };
        match self.lookup(&key) {
            Some(i) => self.write(i, value, store).err(),
            None => Some(format!("NCMD metric {key:?} is unknown, skipped")),
        }
    }

    /// The checks every protocol's write passes (`Point::check`). `Err` is the line to log.
    fn write(&self, i: usize, n: Num, store: &Store) -> Result<(), String> {
        let p = &self.points[i];
        let why = |m: &dyn std::fmt::Display| format!("write to `{}` refused: {m}", show(&p.tag));
        let v = p.check(n).map_err(|r| why(&r))?;
        store.write(&p.tag, v).map_err(|e| why(&format!("{e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::proto::mqtt::sparkplug;
    use crate::fd::store::Layout;
    use prost::Message;

    fn store() -> Store {
        Store::new(Layout {
            points: vec![],
            external: vec![],
            internal: vec![
                ("lvl".into(), Value::Analog(2.5)),
                ("pump".into(), Value::Binary(false)),
                ("ro".into(), Value::Analog(1.0)),
            ],
        })
    }

    fn points() -> Vec<Point> {
        let p = |tag: &str, kind, writable, min, max| Point {
            tag: tag.into(),
            kind,
            writable,
            min,
            max,
        };
        vec![
            p("lvl", Kind::Analog, true, Some(0.0), Some(10.0)),
            p("pump", Kind::Binary, true, None, None),
            p("ro", Kind::Analog, false, None, None),
        ]
    }

    fn plain(refresh_ms: u64) -> Engine {
        let scheme = Scheme::Plain {
            prefix: "ff".into(),
            device: "rtu".into(),
            qos: 1,
            retain: true,
        };
        Engine::new(scheme, points(), refresh_ms)
    }

    fn spb(refresh_ms: u64) -> Engine {
        Engine::new(
            Scheme::Sparkplug {
                group: "g".into(),
                edge: "e".into(),
            },
            points(),
            refresh_ms,
        )
    }

    fn set(s: &Store, tag: &str, v: Value) {
        s.write(tag, v).unwrap();
        s.apply_internal();
    }

    fn topics(o: &[Out]) -> Vec<String> {
        o.iter()
            .map(|o| format!("{}={}", o.topic, String::from_utf8_lossy(&o.payload)))
            .collect()
    }

    #[test]
    fn plain_publishes_a_birth_then_only_changes_and_the_refresh() {
        let (s, mut e) = (store(), plain(1000));
        let will = e.begin_session();
        assert_eq!(
            will,
            Out {
                topic: "ff/rtu/status".into(),
                payload: b"offline".to_vec(),
                qos: 1,
                retain: true
            }
        );
        let first = e.tick(&s, 0, 0);
        assert_eq!(
            topics(&first),
            [
                "ff/rtu/status=online",
                "ff/rtu/lvl=2.5",
                "ff/rtu/pump=false",
                "ff/rtu/ro=1"
            ]
        );
        assert_eq!((first[0].qos, first[0].retain), (1, true));
        assert_eq!((first[1].qos, first[1].retain), (1, true));
        assert_eq!(e.tick(&s, 500, 0), []);
        set(&s, "pump", Value::Binary(true));
        assert_eq!(topics(&e.tick(&s, 600, 0)), ["ff/rtu/pump=true"]);
        assert_eq!(e.tick(&s, 999, 0), []);
        assert_eq!(
            e.tick(&s, 1000, 0).len(),
            3,
            "the refresh publishes everything"
        );
        assert_eq!(e.tick(&s, 1999, 0), []);
        assert_eq!(e.tick(&s, 2000, 0).len(), 3);
    }

    #[test]
    fn refresh_zero_never_republishes_and_a_nan_is_not_a_change() {
        let (s, mut e) = (store(), plain(0));
        e.tick(&s, 0, 0);
        assert_eq!(e.tick(&s, 10_000_000, 0), []);
        set(&s, "ro", Value::Analog(f64::NAN));
        assert_eq!(topics(&e.tick(&s, 1, 0)), ["ff/rtu/ro=NaN"]);
        assert_eq!(e.tick(&s, 2, 0), []);
    }

    #[test]
    fn a_lost_publication_or_a_new_session_births_again() {
        let (s, mut e) = (store(), plain(0));
        e.tick(&s, 0, 0);
        e.lost();
        assert_eq!(e.tick(&s, 1, 0).len(), 4);
        assert_eq!(e.tick(&s, 2, 0), []);
        e.begin_session();
        assert_eq!(e.tick(&s, 3, 0).len(), 4);
    }

    #[test]
    fn plain_goodbye_is_offline_and_subscribes_to_every_set_topic() {
        let e = plain(0);
        assert_eq!(topics(&[e.goodbye()]), ["ff/rtu/status=offline"]);
        assert_eq!(e.subscriptions(), [("ff/rtu/+/set".to_string(), 1)]);
    }

    fn cmd(e: &mut Engine, s: &Store, tag: &str, payload: &str) -> Vec<String> {
        e.message(&format!("ff/rtu/{tag}/set"), payload.as_bytes(), s)
    }

    #[test]
    fn plain_commands_are_validated_and_queued() {
        let (s, mut e) = (store(), plain(0));
        assert_eq!(cmd(&mut e, &s, "lvl", " 7.5\n"), Vec::<String>::new());
        assert!(s.is_queued("lvl"));
        assert_eq!(
            s.get_f64("lvl"),
            Some(2.5),
            "a write waits for the end of the cycle"
        );
        s.apply_internal();
        assert_eq!(s.get_f64("lvl"), Some(7.5));
        for (word, want) in [
            ("TRUE", true),
            ("1", true),
            ("on", true),
            ("false", false),
            ("0", false),
            ("Off", false),
        ] {
            assert!(cmd(&mut e, &s, "pump", word).is_empty(), "{word}");
            s.apply_internal();
            assert_eq!(s.get_bool("pump"), Some(want), "{word}");
        }
        assert_eq!(cmd(&mut e, &s, "lvl", "10").len(), 0);
        assert_eq!(cmd(&mut e, &s, "lvl", "0").len(), 0);
    }

    #[test]
    fn plain_commands_that_break_a_rule_are_refused_with_a_reason() {
        let (s, mut e) = (store(), plain(0));
        let why = |e: &mut Engine, tag, p: &str| cmd(e, &s, tag, p).join("|");
        assert!(why(&mut e, "ro", "5").contains("not writable"));
        assert!(why(&mut e, "lvl", "10.5").contains("outside the limits"));
        assert!(why(&mut e, "lvl", "-0.5").contains("outside the limits"));
        assert!(why(&mut e, "lvl", "inf").contains("not a finite number"));
        assert!(why(&mut e, "lvl", "NaN").contains("not a finite number"));
        assert!(why(&mut e, "lvl", "high").contains("not a Analog value"));
        assert!(why(&mut e, "pump", "2").contains("not a Binary value"));
        assert!(why(&mut e, "nope", "1").contains("unknown tag `nope`"));
        assert!(e
            .message("ff/rtu/lvl/set", &[0xff, 0xfe], &s)
            .join("")
            .contains("not a Analog value"));
        assert!(!s.is_queued("lvl") && !s.is_queued("ro") && !s.is_queued("pump"));
        assert_eq!(
            e.message("other/rtu/lvl/set", b"1", &s),
            Vec::<String>::new()
        );
        assert_eq!(e.message("ff/rtu/lvl", b"1", &s), Vec::<String>::new());
        assert!(!s.is_queued("lvl"));
    }

    fn decoded(o: &Out) -> sparkplug::Payload {
        sparkplug::Payload::decode(o.payload.as_slice()).unwrap()
    }

    #[test]
    fn sparkplug_will_and_birth_agree_on_bdseq_which_advances_and_wraps() {
        let (s, mut e) = (store(), spb(0));
        for expect in [0u64, 1, 2] {
            let will = e.begin_session();
            assert_eq!(
                (will.topic.as_str(), will.qos, will.retain),
                ("spBv1.0/g/NDEATH/e", 1, false)
            );
            assert_eq!(
                decoded(&will).metrics[0].value,
                Some(sparkplug::Value::Long(expect))
            );
            let birth = e.tick(&s, 0, 77);
            assert_eq!(birth.len(), 1);
            assert_eq!(
                (birth[0].topic.as_str(), birth[0].qos, birth[0].retain),
                ("spBv1.0/g/NBIRTH/e", 0, false)
            );
            let p = decoded(&birth[0]);
            assert_eq!((p.seq, p.timestamp), (Some(0), Some(77)));
            assert_eq!(p.metrics[0].value, Some(sparkplug::Value::Long(expect)));
            assert_eq!(p.metrics.len(), 5);
        }
        for _ in 0..253 {
            e.begin_session();
        }
        assert_eq!(
            decoded(&e.begin_session()).metrics[0].value,
            Some(sparkplug::Value::Long(0)),
            "bdSeq wraps after 255"
        );
        assert_eq!(
            decoded(&e.goodbye()).metrics[0].value,
            Some(sparkplug::Value::Long(0))
        );
    }

    #[test]
    fn sparkplug_data_carries_changes_by_alias_with_a_wrapping_sequence() {
        let (s, mut e) = (store(), spb(0));
        e.begin_session();
        e.tick(&s, 0, 0);
        assert_eq!(e.tick(&s, 1, 0), []);
        set(&s, "pump", Value::Binary(true));
        let out = e.tick(&s, 2, 9);
        assert_eq!(out[0].topic, "spBv1.0/g/NDATA/e");
        let p = decoded(&out[0]);
        assert_eq!((p.seq, p.timestamp, p.metrics.len()), (Some(1), Some(9), 1));
        assert_eq!(
            (p.metrics[0].alias, p.metrics[0].name.clone()),
            (Some(2), None)
        );
        assert_eq!(p.metrics[0].value, Some(sparkplug::Value::Boolean(true)));
        let mut last = 1;
        for i in 0..300 {
            set(&s, "lvl", Value::Analog(f64::from(i)));
            last = decoded(&e.tick(&s, 3, 0)[0]).seq.unwrap();
        }
        assert_eq!(last, (1 + 300) % 256, "seq runs 0..255 and wraps");
    }

    #[test]
    fn sparkplug_refresh_sends_every_metric_in_one_ndata() {
        let (s, mut e) = (store(), spb(1000));
        e.begin_session();
        e.tick(&s, 0, 0);
        assert_eq!(e.tick(&s, 999, 0), []);
        assert_eq!(decoded(&e.tick(&s, 1000, 0)[0]).metrics.len(), 3);
    }

    fn ncmd(metrics: Vec<sparkplug::Metric>) -> Vec<u8> {
        sparkplug::Payload {
            metrics,
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn by_name(name: &str, v: sparkplug::Value) -> sparkplug::Metric {
        sparkplug::Metric {
            name: Some(name.into()),
            value: Some(v),
            ..Default::default()
        }
    }

    #[test]
    fn an_ncmd_rebirth_makes_a_new_nbirth_with_seq_0_and_the_same_bdseq() {
        let (s, mut e) = (store(), spb(0));
        e.begin_session();
        e.begin_session();
        e.tick(&s, 0, 0);
        set(&s, "lvl", Value::Analog(1.0));
        e.tick(&s, 1, 0);
        let rb = ncmd(vec![by_name(
            sparkplug::REBIRTH,
            sparkplug::Value::Boolean(true),
        )]);
        assert_eq!(e.message("spBv1.0/g/NCMD/e", &rb, &s), Vec::<String>::new());
        let out = e.tick(&s, 2, 0);
        assert_eq!(out[0].topic, "spBv1.0/g/NBIRTH/e");
        let p = decoded(&out[0]);
        assert_eq!(
            (p.seq, p.metrics[0].value.clone()),
            (Some(0), Some(sparkplug::Value::Long(1)))
        );
        assert_eq!(
            decoded(&{
                set(&s, "lvl", Value::Analog(3.0));
                e.tick(&s, 3, 0).remove(0)
            })
            .seq,
            Some(1)
        );
        assert_eq!(e.subscriptions(), [("spBv1.0/g/NCMD/e".to_string(), 1)]);
    }

    #[test]
    fn an_ncmd_writes_by_name_or_alias_through_the_same_checks() {
        let (s, mut e) = (store(), spb(0));
        let go =
            |e: &mut Engine, m: Vec<sparkplug::Metric>| e.message("spBv1.0/g/NCMD/e", &ncmd(m), &s);
        use sparkplug::Value as W;
        assert!(go(&mut e, vec![by_name("lvl", W::Double(4.0))]).is_empty());
        assert!(s.is_queued("lvl"));
        let alias2 = sparkplug::Metric {
            alias: Some(2),
            value: Some(W::Boolean(true)),
            ..Default::default()
        };
        assert!(go(&mut e, vec![alias2]).is_empty());
        assert!(s.is_queued("pump"));
        s.apply_internal();
        assert_eq!(
            (s.get_f64("lvl"), s.get_bool("pump")),
            (Some(4.0), Some(true))
        );
        let as_number = sparkplug::Metric {
            alias: Some(2),
            value: Some(W::Long(0)),
            ..Default::default()
        };
        assert!(go(&mut e, vec![as_number]).is_empty());
        s.apply_internal();
        assert_eq!(s.get_bool("pump"), Some(false));
        let bad = |e: &mut Engine, m| go(e, vec![m]).join("|");
        assert!(bad(&mut e, by_name("lvl", W::Double(99.0))).contains("outside the limits"));
        assert!(bad(&mut e, by_name("ro", W::Double(1.0))).contains("not writable"));
        assert!(bad(&mut e, by_name("pump", W::Double(2.0))).contains("wrong type"));
        assert!(bad(&mut e, by_name("lvl", W::Boolean(true))).contains("wrong type"));
        assert!(bad(&mut e, by_name("nope", W::Double(1.0))).contains("unknown"));
        for a in [0u64, 4, u64::MAX] {
            let m = sparkplug::Metric {
                alias: Some(a),
                value: Some(W::Double(1.0)),
                ..Default::default()
            };
            assert!(bad(&mut e, m).contains("unknown"), "alias {a}");
        }
        assert!(e
            .message("spBv1.0/g/NCMD/e", &[0x0a, 0xff], &s)
            .join("")
            .contains("undecodable"));
        assert!(e
            .message(
                "spBv1.0/g/DCMD/e",
                &ncmd(vec![by_name("lvl", W::Double(1.0))]),
                &s
            )
            .is_empty());
        assert!(!s.is_queued("ro"));
    }
}
