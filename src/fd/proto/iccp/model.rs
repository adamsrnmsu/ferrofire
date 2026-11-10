//! The TASE.2 data value objects (Data_Real, Data_State, Data_Discrete and their Q and QTimeTag forms) as MMS values, mapped from
//! datastore tags. Layouts are from recall of IEC 60870-6-503 (corroborated for the validity and source codes only; see
//! docs/field-device-iccp.md), so they are one place to fix if a real peer disagrees.
//!
//! Data_Flags is an 8-bit string, bit 0 (the first, most significant) first: state (2 bits: 0 between, 1 off, 2 on, 3 invalid),
//! validity (2: 0 valid, 1 held, 2 suspect, 3 notvalid), current source (2: 0 telemetered, 1 calculated, 2 entered, 3 estimated),
//! normal value (1), timestamp quality (1).
//!   Real      plain: float32            Q: {float32, flags}            QTimeTag: {timestamp, float32, flags}
//!   Discrete  plain: integer32          Q: {integer32, flags}          QTimeTag: {timestamp, integer32, flags}
//!   State     plain: bit string(2)      Q: flags                       QTimeTag: {timestamp, flags}
//! The timestamp is an integer of unix seconds (GMTBasedS).

use super::transfer::{structure, INCONSISTENT, INVALID};
use crate::model::{Kind, Value as Datum};
use iec61850_rs::proto::data::Value;
use iec61850_rs::proto::mms::typespec::TypeSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Real,
    State,
    Discrete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Plain,
    Q,
    QTimeTag,
}

impl Class {
    pub fn from_element(e: &str) -> Option<Class> {
        match e {
            "data-real" => Some(Class::Real),
            "data-state" => Some(Class::State),
            "data-discrete" => Some(Class::Discrete),
            _ => None,
        }
    }
    /// The datastore kind a tag must have: a state is a binary, a real or discrete value is an analog.
    pub fn kind(self) -> Kind {
        match self {
            Class::State => Kind::Binary,
            _ => Kind::Analog,
        }
    }
}

impl Variant {
    pub fn from_text(s: &str) -> Option<Variant> {
        match s.to_ascii_lowercase().as_str() {
            "plain" => Some(Variant::Plain),
            "q" => Some(Variant::Q),
            "qtimetag" => Some(Variant::QTimeTag),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub name: String,
    pub tag: String,
    pub class: Class,
    pub variant: Variant,
}

const NOT_VALID: u8 = 3;

fn flags(state: u8, validity: u8) -> Value {
    Value::BitString {
        unused: 0,
        bytes: vec![state << 6 | validity << 4],
    }
}

fn number(c: Class, x: f64) -> Value {
    match c {
        Class::Discrete => Value::Integer(x.round().clamp(-2147483648.0, 2147483647.0) as i64),
        _ => Value::Float32(x as f32),
    }
}

/// Component `i` of a structure value.
fn member(v: &Value, i: usize) -> Result<&Value, i64> {
    v.members().and_then(|m| m.get(i)).ok_or(INCONSISTENT)
}

impl Object {
    fn is_state(&self) -> bool {
        self.class == Class::State
    }

    /// The datum as (number, state bits, validity): a datum of the wrong kind or none at all is notvalid.
    fn reading(&self, datum: Option<Datum>) -> (f64, u8, u8) {
        match (self.class, datum) {
            (Class::State, Some(Datum::Binary(b))) => (0.0, 1 + u8::from(b), 0),
            (Class::State, _) => (0.0, 3, NOT_VALID),
            (_, Some(Datum::Analog(x))) => (x, 0, 0),
            _ => (0.0, 0, NOT_VALID),
        }
    }

    /// What follows the timestamp: the flags alone for a state, else the number and the flags.
    fn parts(&self, (x, state, valid): (f64, u8, u8)) -> Vec<Value> {
        let f = flags(state, valid);
        if self.is_state() {
            vec![f]
        } else {
            vec![number(self.class, x), f]
        }
    }

    /// The value a read returns: `datum` is the tag's current value (`None`: unknown, reported notvalid), `ts` unix seconds.
    pub fn encode(&self, datum: Option<Datum>, ts: i64) -> Value {
        let r = self.reading(datum);
        match self.variant {
            Variant::Plain if self.is_state() => Value::BitString {
                unused: 6,
                bytes: vec![r.1 << 6],
            },
            Variant::Plain => number(self.class, r.0),
            Variant::Q if self.is_state() => flags(r.1, r.2),
            Variant::Q => Value::Structure(self.parts(r)),
            Variant::QTimeTag => {
                let mut v = vec![Value::Integer(ts)];
                v.extend(self.parts(r));
                Value::Structure(v)
            }
        }
    }

    /// The component that holds the value (for a state: the flags, which hold the state).
    fn core<'a>(&self, v: &'a Value) -> Result<&'a Value, i64> {
        match self.variant {
            Variant::Plain => Ok(v),
            Variant::Q if self.is_state() => Ok(v),
            Variant::Q => member(v, 0),
            Variant::QTimeTag => member(v, 1),
        }
    }

    /// What a client wrote, as the datum to queue. `INCONSISTENT` for the wrong type, `INVALID` for a state that is not on or off.
    /// Quality and time a client sends are ignored: the device is the source of both.
    pub fn decode(&self, v: &Value) -> Result<Datum, i64> {
        match (self.class, self.core(v)?) {
            (Class::Real, Value::Float32(f)) => Ok(Datum::Analog(f64::from(*f))),
            (Class::Real, Value::Float64(f)) => Ok(Datum::Analog(*f)),
            (Class::Discrete, Value::Integer(i)) => Ok(Datum::Analog(*i as f64)),
            (Class::State, Value::BitString { bytes, .. }) => state_datum(bytes),
            _ => Err(INCONSISTENT),
        }
    }

    fn value_type(&self) -> TypeSpec {
        match self.class {
            Class::Real => TypeSpec::FloatingPoint {
                format_width: 32,
                exponent_width: 8,
            },
            Class::Discrete => TypeSpec::Integer(32),
            Class::State => TypeSpec::BitString(8),
        }
    }

    pub fn type_spec(&self) -> TypeSpec {
        let (ts, f, value) = (
            TypeSpec::Integer(32),
            TypeSpec::BitString(8),
            self.value_type(),
        );
        match (self.variant, self.is_state()) {
            (Variant::Plain, true) => TypeSpec::BitString(2),
            (Variant::Plain, false) => value,
            (Variant::Q, true) => f,
            (Variant::Q, false) => structure(vec![value, f]),
            (Variant::QTimeTag, true) => structure(vec![ts, f]),
            (Variant::QTimeTag, false) => structure(vec![ts, value, f]),
        }
    }
}

/// On (2) and off (1) in the top two bits are the only states a client may command.
fn state_datum(bytes: &[u8]) -> Result<Datum, i64> {
    match bytes.first().map(|b| b >> 6) {
        Some(2) => Ok(Datum::Binary(true)),
        Some(1) => Ok(Datum::Binary(false)),
        _ => Err(INVALID),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(class: Class, variant: Variant) -> Object {
        Object {
            name: "X".into(),
            tag: "x".into(),
            class,
            variant,
        }
    }
    fn bits(v: &Value) -> u8 {
        match v {
            Value::BitString { bytes, .. } => bytes[0],
            _ => panic!("not a bit string: {v:?}"),
        }
    }

    #[test]
    fn real_in_its_three_forms() {
        let a = Some(Datum::Analog(400.5));
        assert_eq!(
            obj(Class::Real, Variant::Plain).encode(a, 9),
            Value::Float32(400.5)
        );
        let q = obj(Class::Real, Variant::Q).encode(a, 9);
        assert_eq!(q.members().unwrap()[0], Value::Float32(400.5));
        assert_eq!(
            bits(&q.members().unwrap()[1]),
            0,
            "valid, telemetered, normal"
        );
        let t = obj(Class::Real, Variant::QTimeTag).encode(a, 1_700_000_000);
        let m = t.members().unwrap();
        assert_eq!(
            (&m[0], &m[1]),
            (&Value::Integer(1_700_000_000), &Value::Float32(400.5))
        );
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn discrete_rounds_and_clamps_to_32_bits() {
        let d = obj(Class::Discrete, Variant::Plain);
        assert_eq!(d.encode(Some(Datum::Analog(41.6)), 0), Value::Integer(42));
        assert_eq!(
            d.encode(Some(Datum::Analog(1e12)), 0),
            Value::Integer(2147483647)
        );
        assert_eq!(
            d.encode(Some(Datum::Analog(-1e12)), 0),
            Value::Integer(-2147483648)
        );
    }

    #[test]
    fn state_carries_on_off_in_the_top_two_bits() {
        let s = |v: Variant, b| obj(Class::State, v).encode(Some(Datum::Binary(b)), 5);
        assert_eq!(
            s(Variant::Plain, true),
            Value::BitString {
                unused: 6,
                bytes: vec![0x80]
            }
        );
        assert_eq!(
            s(Variant::Plain, false),
            Value::BitString {
                unused: 6,
                bytes: vec![0x40]
            }
        );
        assert_eq!(bits(&s(Variant::Q, true)), 0x80);
        let t = s(Variant::QTimeTag, false);
        assert_eq!(t.members().unwrap()[0], Value::Integer(5));
        assert_eq!(bits(&t.members().unwrap()[1]), 0x40);
    }

    #[test]
    fn an_unknown_value_is_notvalid_and_a_state_is_invalid() {
        let r = obj(Class::Real, Variant::Q).encode(None, 0);
        assert_eq!(
            bits(&r.members().unwrap()[1]),
            0x30,
            "validity 3 at bits 2-3"
        );
        let s = obj(Class::State, Variant::Q).encode(None, 0);
        assert_eq!(bits(&s), 0xC0 | 0x30, "state invalid, validity notvalid");
        let wrong = obj(Class::Real, Variant::Q).encode(Some(Datum::Binary(true)), 0);
        assert_eq!(
            bits(&wrong.members().unwrap()[1]),
            0x30,
            "a binary tag under a real object is not valid data"
        );
    }

    #[test]
    fn decode_reads_the_value_out_of_every_form_and_refuses_the_rest() {
        for class in [Class::Real, Class::Discrete, Class::State] {
            for variant in [Variant::Plain, Variant::Q, Variant::QTimeTag] {
                let o = obj(class, variant);
                let d = match class {
                    Class::State => Datum::Binary(true),
                    _ => Datum::Analog(7.0),
                };
                assert_eq!(
                    o.decode(&o.encode(Some(d), 1)),
                    Ok(d),
                    "{class:?} {variant:?}"
                );
            }
        }
        let st = obj(Class::State, Variant::Plain);
        assert_eq!(
            st.decode(&Value::BitString {
                unused: 6,
                bytes: vec![0x40]
            }),
            Ok(Datum::Binary(false))
        );
        assert_eq!(
            st.decode(&Value::BitString {
                unused: 6,
                bytes: vec![0xC0]
            }),
            Err(INVALID),
            "invalid is not a command"
        );
        assert_eq!(
            st.decode(&Value::BitString {
                unused: 6,
                bytes: vec![]
            }),
            Err(INVALID)
        );
        let r = obj(Class::Real, Variant::Plain);
        assert_eq!(r.decode(&Value::Float64(2.5)), Ok(Datum::Analog(2.5)));
        assert_eq!(r.decode(&Value::Boolean(true)), Err(INCONSISTENT));
        assert_eq!(
            obj(Class::Real, Variant::Q).decode(&Value::Float32(1.0)),
            Err(INCONSISTENT)
        );
        assert_eq!(
            obj(Class::Real, Variant::Q).decode(&Value::Structure(vec![])),
            Err(INCONSISTENT)
        );
        assert_eq!(
            obj(Class::Real, Variant::QTimeTag).decode(&Value::Structure(vec![Value::Integer(1)])),
            Err(INCONSISTENT)
        );
        assert_eq!(
            obj(Class::State, Variant::QTimeTag).decode(&Value::Structure(vec![Value::Integer(1)])),
            Err(INCONSISTENT)
        );
        assert_eq!(
            obj(Class::Real, Variant::QTimeTag).decode(&Value::Float32(1.0)),
            Err(INCONSISTENT)
        );
    }

    #[test]
    fn the_type_matches_the_value_shape() {
        let a = Some(Datum::Analog(1.0));
        let b = Some(Datum::Binary(true));
        for class in [Class::Real, Class::Discrete, Class::State] {
            for variant in [Variant::Plain, Variant::Q, Variant::QTimeTag] {
                let o = obj(class, variant);
                let v = o.encode(if class == Class::State { b } else { a }, 1);
                let n = match o.type_spec() {
                    TypeSpec::Structure { components, .. } => components.len(),
                    _ => 0,
                };
                assert_eq!(
                    n,
                    v.members().map_or(0, <[Value]>::len),
                    "{class:?} {variant:?}"
                );
            }
        }
    }

    #[test]
    fn names_of_classes_variants_and_kinds() {
        assert_eq!(Class::from_element("data-state"), Some(Class::State));
        assert_eq!(Class::from_element("data-real"), Some(Class::Real));
        assert_eq!(Class::from_element("data-discrete"), Some(Class::Discrete));
        assert_eq!(Class::from_element("peer"), None);
        assert_eq!(Class::State.kind(), Kind::Binary);
        assert_eq!(Class::Discrete.kind(), Kind::Analog);
        assert_eq!(Variant::from_text("QTimeTag"), Some(Variant::QTimeTag));
        assert_eq!(Variant::from_text("q"), Some(Variant::Q));
        assert_eq!(Variant::from_text("plain"), Some(Variant::Plain));
        assert_eq!(Variant::from_text("x"), None);
    }
}
