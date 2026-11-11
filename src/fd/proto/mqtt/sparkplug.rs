//! Sparkplug B 3.0 payloads (Eclipse Sparkplug specification, "Payloads" appendix, `sparkplug_b.proto`), written here from the
//! public wire description: the message numbers below are the spec's. Nothing is taken from Eclipse Tahu or sparkplug-rs (EPL-2.0);
//! Tahu runs only as a separate test peer. Only the fields an edge node needs are declared; prost skips the rest on decode
//! (metadata, properties, datasets, templates). `docs/sparkplug_b.proto` is the same schema as a .proto file.

use prost::Message;

pub const NAMESPACE: &str = "spBv1.0";
pub const BDSEQ: &str = "bdSeq";
pub const REBIRTH: &str = "Node Control/Rebirth";

/// Sparkplug data type codes (spec table "Data Types").
const INT64: u32 = 4;
const DOUBLE: u32 = 10;
const BOOLEAN: u32 = 11;

#[derive(Clone, PartialEq, Message)]
pub struct Payload {
    #[prost(uint64, optional, tag = "1")]
    pub timestamp: Option<u64>,
    #[prost(message, repeated, tag = "2")]
    pub metrics: Vec<Metric>,
    #[prost(uint64, optional, tag = "3")]
    pub seq: Option<u64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Metric {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(uint64, optional, tag = "2")]
    pub alias: Option<u64>,
    #[prost(uint64, optional, tag = "3")]
    pub timestamp: Option<u64>,
    #[prost(uint32, optional, tag = "4")]
    pub datatype: Option<u32>,
    #[prost(bool, optional, tag = "7")]
    pub is_null: Option<bool>,
    #[prost(oneof = "Value", tags = "10, 11, 12, 13, 14, 15")]
    pub value: Option<Value>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Value {
    #[prost(uint32, tag = "10")]
    Int(u32),
    #[prost(uint64, tag = "11")]
    Long(u64),
    #[prost(float, tag = "12")]
    Float(f32),
    #[prost(double, tag = "13")]
    Double(f64),
    #[prost(bool, tag = "14")]
    Boolean(bool),
    #[prost(string, tag = "15")]
    Text(String),
}

/// `spBv1.0/<group>/<message type>/<edge node>`.
pub fn topic(group: &str, kind: &str, edge: &str) -> String {
    format!("{NAMESPACE}/{group}/{kind}/{edge}")
}

/// A metric an edge node publishes: a tag's current value under its name and alias.
pub struct Sample<'a> {
    pub name: &'a str,
    pub alias: u64,
    pub value: crate::model::Value,
}

fn bd_seq(bd: u64, ts: Option<u64>) -> Metric {
    Metric {
        name: Some(BDSEQ.into()),
        timestamp: ts,
        datatype: Some(INT64),
        value: Some(Value::Long(bd)),
        ..Metric::default()
    }
}

fn metric(s: &Sample, ts: u64, with_name: bool) -> Metric {
    let (datatype, value) = match s.value {
        crate::model::Value::Analog(x) => (DOUBLE, Value::Double(x)),
        crate::model::Value::Binary(b) => (BOOLEAN, Value::Boolean(b)),
    };
    Metric {
        name: with_name.then(|| s.name.to_string()),
        alias: Some(s.alias),
        timestamp: Some(ts),
        datatype: Some(datatype),
        value: Some(value),
        ..Metric::default()
    }
}

/// NBIRTH: seq 0, `bdSeq`, the rebirth control, then every metric with its name and alias.
pub fn birth(ts: u64, bd: u64, samples: &[Sample]) -> Vec<u8> {
    let mut metrics = vec![
        bd_seq(bd, Some(ts)),
        Metric {
            name: Some(REBIRTH.into()),
            timestamp: Some(ts),
            datatype: Some(BOOLEAN),
            value: Some(Value::Boolean(false)),
            ..Metric::default()
        },
    ];
    metrics.extend(samples.iter().map(|s| metric(s, ts, true)));
    Payload {
        timestamp: Some(ts),
        metrics,
        seq: Some(0),
    }
    .encode_to_vec()
}

/// NDATA: the changed metrics, by alias only (the NBIRTH told the host the names).
pub fn data(ts: u64, seq: u64, samples: &[Sample]) -> Vec<u8> {
    Payload {
        timestamp: Some(ts),
        metrics: samples.iter().map(|s| metric(s, ts, false)).collect(),
        seq: Some(seq),
    }
    .encode_to_vec()
}

/// NDEATH (the will message): only `bdSeq`, equal to the one in the NBIRTH of the same session.
pub fn death(bd: u64) -> Vec<u8> {
    Payload {
        metrics: vec![bd_seq(bd, None)],
        ..Payload::default()
    }
    .encode_to_vec()
}

/// What an NCMD asks of the edge node.
#[derive(Debug, PartialEq)]
pub enum Command {
    Rebirth,
    Write { key: Key, value: Num },
}

#[derive(Debug, PartialEq)]
pub enum Key {
    Name(String),
    Alias(u64),
}

pub use crate::fd::proto::point::Num;

/// An integer from the 32-bit field, signed or not as the data type says: Int8/16/32 are stored sign-extended.
fn int32(x: u32, datatype: Option<u32>) -> f64 {
    match datatype {
        Some(1) => f64::from(x as u8 as i8),
        Some(2) => f64::from(x as u16 as i16),
        Some(3) => f64::from(x as i32),
        _ => f64::from(x),
    }
}

/// A metric's value as a number, honouring the signedness the data type names; Int64 is two's complement in the 64-bit field.
fn num(m: &Metric) -> Option<Num> {
    Some(match m.value.as_ref()? {
        Value::Boolean(b) => Num::Bool(*b),
        Value::Int(x) => Num::Number(int32(*x, m.datatype)),
        Value::Long(x) if m.datatype == Some(INT64) => Num::Number(*x as i64 as f64),
        Value::Long(x) => Num::Number(*x as f64),
        Value::Float(x) => Num::Number(f64::from(*x)),
        Value::Double(x) => Num::Number(*x),
        Value::Text(_) => return None,
    })
}

/// The commands in an NCMD payload. Metrics that are null, valueless, textual or unnamed are not commands and are skipped.
pub fn decode_command(bytes: &[u8]) -> Result<Vec<Command>, prost::DecodeError> {
    let p = Payload::decode(bytes)?;
    Ok(p.metrics
        .iter()
        .filter(|m| m.is_null != Some(true))
        .filter_map(|m| {
            let value = num(m)?;
            let key = match (&m.name, m.alias) {
                (Some(n), _) if n == REBIRTH => {
                    return (value == Num::Bool(true)).then_some(Command::Rebirth)
                }
                (Some(n), _) => Key::Name(n.clone()),
                (None, Some(a)) => Key::Alias(a),
                (None, None) => return None,
            };
            Some(Command::Write { key, value })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Value as V;

    fn decoded(b: &[u8]) -> Payload {
        Payload::decode(b).unwrap()
    }

    fn sample(name: &str, alias: u64, value: V) -> Sample<'_> {
        Sample { name, alias, value }
    }

    #[test]
    fn topics_follow_the_namespace() {
        assert_eq!(topic("g", "NBIRTH", "e"), "spBv1.0/g/NBIRTH/e");
    }

    #[test]
    fn a_birth_has_seq_0_bdseq_rebirth_and_every_metric_with_name_and_alias() {
        let p = decoded(&birth(
            1000,
            7,
            &[
                sample("level", 1, V::Analog(2.5)),
                sample("pump", 2, V::Binary(true)),
            ],
        ));
        assert_eq!((p.timestamp, p.seq), (Some(1000), Some(0)));
        let m = &p.metrics;
        assert_eq!(m.len(), 4);
        assert_eq!(m[0].name.as_deref(), Some("bdSeq"));
        assert_eq!(
            (m[0].timestamp, m[1].timestamp),
            (Some(1000), Some(1000)),
            "every NBIRTH metric has a timestamp"
        );
        assert_eq!(m[0].datatype, Some(4));
        assert_eq!(m[0].value, Some(Value::Long(7)));
        assert_eq!(m[1].name.as_deref(), Some("Node Control/Rebirth"));
        assert_eq!(m[1].value, Some(Value::Boolean(false)));
        assert_eq!(m[2].name.as_deref(), Some("level"));
        assert_eq!((m[2].alias, m[2].datatype), (Some(1), Some(10)));
        assert_eq!(
            (m[2].timestamp, &m[2].value),
            (Some(1000), &Some(Value::Double(2.5)))
        );
        assert_eq!((m[3].alias, m[3].datatype), (Some(2), Some(11)));
        assert_eq!(m[3].value, Some(Value::Boolean(true)));
    }

    #[test]
    fn data_names_nothing_and_carries_the_sequence_number() {
        let p = decoded(&data(5, 9, &[sample("level", 1, V::Analog(1.0))]));
        assert_eq!((p.timestamp, p.seq), (Some(5), Some(9)));
        assert_eq!(p.metrics.len(), 1);
        assert_eq!(
            (p.metrics[0].name.as_deref(), p.metrics[0].alias),
            (None, Some(1))
        );
    }

    #[test]
    fn a_death_holds_only_the_bdseq() {
        let p = decoded(&death(3));
        assert_eq!((p.timestamp, p.seq), (None, None));
        assert_eq!(p.metrics.len(), 1);
        assert_eq!(p.metrics[0].value, Some(Value::Long(3)));
    }

    fn cmd(m: Vec<Metric>) -> Vec<u8> {
        Payload {
            metrics: m,
            ..Payload::default()
        }
        .encode_to_vec()
    }

    fn named(name: &str, value: Value) -> Metric {
        Metric {
            name: Some(name.into()),
            value: Some(value),
            ..Metric::default()
        }
    }

    #[test]
    fn rebirth_is_a_command_only_when_true() {
        let c = |b| decode_command(&cmd(vec![named(REBIRTH, Value::Boolean(b))])).unwrap();
        assert_eq!(c(true), [Command::Rebirth]);
        assert_eq!(c(false), []);
    }

    #[test]
    fn writes_are_addressed_by_name_or_alias_and_typed_by_the_data_type() {
        let alias = Metric {
            alias: Some(4),
            value: Some(Value::Double(1.5)),
            ..Metric::default()
        };
        let int32 = Metric {
            datatype: Some(3),
            ..named("a", Value::Int(0xFFFF_FFFE))
        };
        let int8 = Metric {
            datatype: Some(1),
            ..named("b", Value::Int(0xFF))
        };
        let int16 = Metric {
            datatype: Some(2),
            ..named("h", Value::Int(0xFFFE))
        };
        let uint32 = Metric {
            datatype: Some(7),
            ..named("c", Value::Int(0xFFFF_FFFE))
        };
        let int64 = Metric {
            datatype: Some(4),
            ..named("d", Value::Long(u64::MAX))
        };
        let uint64 = Metric {
            datatype: Some(8),
            ..named("e", Value::Long(1 << 40))
        };
        let f = named("f", Value::Float(0.5));
        let t = named("g", Value::Boolean(true));
        let got = decode_command(&cmd(vec![
            alias, int32, int8, int16, uint32, int64, uint64, f, t,
        ]))
        .unwrap();
        let w = |k: Key, n: Num| Command::Write { key: k, value: n };
        let n = |s: &str| Key::Name(s.into());
        assert_eq!(
            got,
            [
                w(Key::Alias(4), Num::Number(1.5)),
                w(n("a"), Num::Number(-2.0)),
                w(n("b"), Num::Number(-1.0)),
                w(n("h"), Num::Number(-2.0)),
                w(n("c"), Num::Number(4294967294.0)),
                w(n("d"), Num::Number(-1.0)),
                w(n("e"), Num::Number(1099511627776.0)),
                w(n("f"), Num::Number(0.5)),
                w(n("g"), Num::Bool(true)),
            ]
        );
    }

    #[test]
    fn nulls_text_valueless_and_unnamed_metrics_are_not_commands() {
        let null = Metric {
            is_null: Some(true),
            ..named("x", Value::Double(1.0))
        };
        let text = named("y", Value::Text("1".into()));
        let none = Metric {
            name: Some("z".into()),
            ..Metric::default()
        };
        let unnamed = Metric {
            value: Some(Value::Double(1.0)),
            ..Metric::default()
        };
        assert_eq!(
            decode_command(&cmd(vec![null, text, none, unnamed])).unwrap(),
            []
        );
    }

    #[test]
    fn garbage_is_a_decode_error_and_unknown_fields_are_skipped() {
        assert!(decode_command(&[0x0a, 0xff]).is_err());
        let mut b = cmd(vec![named("x", Value::Double(1.0))]);
        b.extend([0x92, 0x03, 0x01, 0x00]); // field 50, length-delimited: not ours
        assert_eq!(decode_command(&b).unwrap().len(), 1);
    }
}
