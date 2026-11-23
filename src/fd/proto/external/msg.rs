//! ffmod/1 messages: one JSON object per line, field `t` is the type. Unknown fields are ignored.

use crate::model::{Kind, Value};
use serde_json::{json, Map, Value as J};

/// The longest line either side accepts, newline excluded.
pub const MAX_LINE: usize = 65536;

/// The longest line the device accepts from a module until `ready` has arrived: `ready.uses` grows with the point count.
pub const MAX_READY_LINE: usize = 16 << 20;

/// Device to module.
#[derive(Clone, Debug, PartialEq)]
pub enum ToModule {
    /// First message: every tag with its kind and current value.
    Hello {
        device: String,
        strict: bool,
        config: String,
        points: Vec<(String, Value)>,
    },
    /// Tags that changed since the last `Values`.
    Values(Vec<(String, Value)>),
    /// `Store::write` queued the write with this id.
    Ack(u64),
    /// `Store::write` refused the write with this id, and why.
    Reject(u64, String),
    /// Client role: the command interface wrote `tag`; the module sends it on and answers with `done` (same `id`).
    Put { id: u64, tag: String, value: Value },
    /// Shut down.
    Stop,
}

/// Module to device.
#[derive(Clone, Debug, PartialEq)]
pub enum FromModule {
    /// The tags the module maps, after `hello`.
    Ready {
        role: Role,
        uses: Vec<Use>,
    },
    /// A value from the module's peer.
    Write {
        id: u64,
        tag: String,
        value: Value,
    },
    /// Client role: the peer answered (`ok`) or refused the write of output `tag`.
    Sent {
        tag: String,
        value: Value,
        ok: bool,
    },
    /// Client role: the peer answered (`ok`) or refused the `put` with this `id`; `err` says why.
    Done {
        id: u64,
        ok: bool,
        err: String,
    },
    Log {
        level: String,
        msg: String,
    },
    /// The module cannot run.
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Server,
    Client,
}

/// One tag a module maps and which side supplies it.
#[derive(Clone, Debug, PartialEq)]
pub struct Use {
    pub tag: String,
    pub kind: Kind,
    pub dir: Dir,
}

/// `In`: the module's peer supplies the value. `Out`: the device does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    In,
    Out,
}

fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Binary(_) => "binary",
        Value::Analog(_) => "analog",
    }
}

/// A non-finite analog has no JSON form; it goes out as `null`.
fn value_json(v: &Value) -> J {
    match v {
        Value::Binary(b) => json!(b),
        Value::Analog(f) => json!(f),
    }
}

/// One `ToModule` as a single line ending in `\n`.
pub fn encode(m: &ToModule) -> String {
    let j = match m {
        ToModule::Hello {
            device,
            strict,
            config,
            points,
        } => {
            let pts: Vec<J> = points
                .iter()
                .map(|(t, v)| json!({"tag": t, "kind": kind_name(v), "value": value_json(v)}))
                .collect();
            json!({"t": "hello", "v": 1, "device": device, "opts": {"strict": strict}, "config": config, "points": pts})
        }
        ToModule::Values(ch) => {
            let ch: Vec<J> = ch.iter().map(|(t, v)| json!([t, value_json(v)])).collect();
            json!({"t": "values", "changes": ch})
        }
        ToModule::Ack(id) => json!({"t": "ack", "id": id}),
        ToModule::Reject(id, err) => json!({"t": "reject", "id": id, "err": err}),
        ToModule::Put { id, tag, value } => {
            json!({"t": "put", "id": id, "tag": tag, "value": value_json(value)})
        }
        ToModule::Stop => json!({"t": "stop"}),
    };
    format!("{j}\n")
}

fn field<'a>(o: &'a Map<String, J>, k: &str) -> Result<&'a J, String> {
    o.get(k).ok_or_else(|| format!("missing field `{k}`"))
}

fn text(o: &Map<String, J>, k: &str) -> Result<String, String> {
    field(o, k)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("field `{k}` is not a string"))
}

fn id(o: &Map<String, J>) -> Result<u64, String> {
    field(o, "id")?
        .as_u64()
        .ok_or_else(|| "field `id` is not an unsigned integer".to_owned())
}

fn value(j: &J) -> Result<Value, String> {
    match j {
        J::Bool(b) => Ok(Value::Binary(*b)),
        J::Number(n) => n
            .as_f64()
            .map(Value::Analog)
            .ok_or_else(|| "number out of range".to_owned()),
        _ => Err("value is not a boolean or a number".to_owned()),
    }
}

/// The entry of `table` named `s`; `what` names the field in the error.
fn pick<T: Copy>(what: &str, s: &str, table: &[(&str, T)]) -> Result<T, String> {
    let hit = table.iter().find(|(n, _)| *n == s);
    hit.map(|(_, v)| *v)
        .ok_or_else(|| format!("unknown {what} `{s}`"))
}

fn used(j: &J) -> Result<Use, String> {
    let o = j.as_object().ok_or("`uses` entry is not an object")?;
    Ok(Use {
        tag: text(o, "tag")?,
        kind: pick(
            "kind",
            &text(o, "kind")?,
            &[("binary", Kind::Binary), ("analog", Kind::Analog)],
        )?,
        dir: pick(
            "dir",
            &text(o, "dir")?,
            &[("in", Dir::In), ("out", Dir::Out)],
        )?,
    })
}

fn ready(o: &Map<String, J>) -> Result<FromModule, String> {
    let uses = field(o, "uses")?
        .as_array()
        .ok_or("field `uses` is not an array")?;
    Ok(FromModule::Ready {
        role: pick(
            "role",
            &text(o, "role")?,
            &[("server", Role::Server), ("client", Role::Client)],
        )?,
        uses: uses.iter().map(used).collect::<Result<_, _>>()?,
    })
}

fn write(o: &Map<String, J>) -> Result<FromModule, String> {
    Ok(FromModule::Write {
        id: id(o)?,
        tag: text(o, "tag")?,
        value: value(field(o, "value")?)?,
    })
}

fn sent(o: &Map<String, J>) -> Result<FromModule, String> {
    let ok = field(o, "ok")?.as_bool();
    Ok(FromModule::Sent {
        tag: text(o, "tag")?,
        value: value(field(o, "value")?)?,
        ok: ok.ok_or("field `ok` is not a boolean")?,
    })
}

fn done(o: &Map<String, J>) -> Result<FromModule, String> {
    Ok(FromModule::Done {
        id: id(o)?,
        ok: field(o, "ok")?
            .as_bool()
            .ok_or("field `ok` is not a boolean")?,
        err: o.get("err").and_then(J::as_str).unwrap_or("").to_owned(),
    })
}

fn log(o: &Map<String, J>) -> Result<FromModule, String> {
    Ok(FromModule::Log {
        level: text(o, "level")?,
        msg: text(o, "msg")?,
    })
}

/// Parse one line from a module. An unknown `t` or a malformed field is an `Err` naming it.
pub fn decode(line: &str) -> Result<FromModule, String> {
    let j: J = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let o = j.as_object().ok_or("message is not an object")?;
    let parse = pick("message type", &text(o, "t")?, &PARSERS)?;
    parse(o)
}

type Parse = fn(&Map<String, J>) -> Result<FromModule, String>;

/// Each module message type and its parser.
const PARSERS: [(&str, Parse); 6] = [
    ("ready", ready),
    ("write", write),
    ("sent", sent),
    ("done", done),
    ("log", log),
    ("error", error),
];

fn error(o: &Map<String, J>) -> Result<FromModule, String> {
    text(o, "msg").map(FromModule::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(m: &ToModule) -> J {
        let s = encode(m);
        assert!(s.ends_with('\n') && s.matches('\n').count() == 1);
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn hello_matches_the_spec() {
        let m = ToModule::Hello {
            device: "rtu-1".into(),
            strict: false,
            config: "<x/>".into(),
            points: vec![
                ("a".into(), Value::Binary(true)),
                ("b".into(), Value::Analog(2.5)),
            ],
        };
        assert_eq!(
            parse(&m),
            json!({"t":"hello","v":1,"device":"rtu-1","opts":{"strict":false},"config":"<x/>",
                "points":[{"tag":"a","kind":"binary","value":true},{"tag":"b","kind":"analog","value":2.5}]})
        );
    }

    #[test]
    fn values_ack_reject_stop() {
        assert_eq!(
            parse(&ToModule::Values(vec![("b".into(), Value::Analog(2.6))])),
            json!({"t":"values","changes":[["b",2.6]]})
        );
        assert_eq!(parse(&ToModule::Ack(7)), json!({"t":"ack","id":7}));
        assert_eq!(
            parse(&ToModule::Reject(7, "unknown tag".into())),
            json!({"t":"reject","id":7,"err":"unknown tag"})
        );
        assert_eq!(parse(&ToModule::Stop), json!({"t":"stop"}));
    }

    #[test]
    fn decode_ready_write_log_error() {
        let r = decode(
            r#"{"t":"ready","role":"client","uses":[{"tag":"x","kind":"analog","dir":"in"},{"tag":"y","kind":"binary","dir":"out"}]}"#,
        );
        assert_eq!(
            r,
            Ok(FromModule::Ready {
                role: Role::Client,
                uses: vec![
                    Use {
                        tag: "x".into(),
                        kind: Kind::Analog,
                        dir: Dir::In
                    },
                    Use {
                        tag: "y".into(),
                        kind: Kind::Binary,
                        dir: Dir::Out
                    },
                ]
            })
        );
        assert_eq!(
            decode(r#"{"t":"ready","role":"server","uses":[]}"#),
            Ok(FromModule::Ready {
                role: Role::Server,
                uses: vec![]
            })
        );
        assert_eq!(
            decode(r#"{"t":"write","id":7,"tag":"x","value":true}"#),
            Ok(FromModule::Write {
                id: 7,
                tag: "x".into(),
                value: Value::Binary(true)
            })
        );
        assert_eq!(
            decode(r#"{"t":"write","id":8,"tag":"x","value":1.5}"#),
            Ok(FromModule::Write {
                id: 8,
                tag: "x".into(),
                value: Value::Analog(1.5)
            })
        );
        assert_eq!(
            decode(r#"{"t":"log","level":"info","msg":"hi"}"#),
            Ok(FromModule::Log {
                level: "info".into(),
                msg: "hi".into()
            })
        );
        assert_eq!(
            decode(r#"{"t":"error","msg":"bind"}"#),
            Ok(FromModule::Error("bind".into()))
        );
    }

    #[test]
    fn decode_sent() {
        assert_eq!(
            decode(r#"{"t":"sent","tag":"x","value":3.0,"ok":true}"#),
            Ok(FromModule::Sent {
                tag: "x".into(),
                value: Value::Analog(3.0),
                ok: true
            })
        );
        assert_eq!(
            decode(r#"{"t":"sent","tag":"y","value":false,"ok":false}"#),
            Ok(FromModule::Sent {
                tag: "y".into(),
                value: Value::Binary(false),
                ok: false
            })
        );
        for bad in [
            r#"{"t":"sent","tag":"x","value":3.0}"#,
            r#"{"t":"sent","tag":"x","value":3.0,"ok":1}"#,
            r#"{"t":"sent","tag":"x","value":null,"ok":true}"#,
        ] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn put_and_done() {
        let put = ToModule::Put {
            id: 3,
            tag: "x".into(),
            value: Value::Binary(true),
        };
        assert_eq!(
            parse(&put),
            json!({"t":"put","id":3,"tag":"x","value":true})
        );
        assert_eq!(
            decode(r#"{"t":"done","id":3,"ok":false,"err":"denied"}"#),
            Ok(FromModule::Done {
                id: 3,
                ok: false,
                err: "denied".into()
            })
        );
        assert_eq!(
            decode(r#"{"t":"done","id":4,"ok":true}"#),
            Ok(FromModule::Done {
                id: 4,
                ok: true,
                err: "".into()
            })
        );
        assert!(decode(r#"{"t":"done","id":4}"#).is_err());
    }

    #[test]
    fn non_finite_values_are_null() {
        let m = ToModule::Values(vec![("b".into(), Value::Analog(f64::NAN))]);
        assert_eq!(
            encode(&m),
            "{\"changes\":[[\"b\",null]],\"t\":\"values\"}\n"
        );
        assert!(decode(r#"{"t":"write","id":1,"tag":"x","value":null}"#).is_err());
        assert!(decode(r#"{"t":"write","id":1,"tag":"x","value":"x"}"#).is_err());
    }

    #[test]
    fn malformed_ready_and_garbage_are_err() {
        for bad in [
            "not json",
            "[]",
            r#"{"t":"ready","role":"peer","uses":[]}"#,
            r#"{"t":"ready","role":"server","uses":[{"tag":"x","kind":"text","dir":"in"}]}"#,
            r#"{"t":"ready","role":"server","uses":[{"tag":"x","kind":"analog","dir":"up"}]}"#,
            r#"{"t":"ready","role":"server","uses":[1]}"#,
            r#"{"t":"ready","role":"server"}"#,
            r#"{"t":"write","id":-1,"tag":"x","value":true}"#,
        ] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn unknown_fields_are_ignored_unknown_type_is_err() {
        assert_eq!(
            decode(r#"{"t":"error","msg":"m","extra":[1]}"#),
            Ok(FromModule::Error("m".into()))
        );
        assert!(decode(r#"{"t":"nope"}"#).unwrap_err().contains("nope"));
    }
}
