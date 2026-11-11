//! A tag a protocol exposes by name and the one rule every such protocol applies to a write (MQTT commands, SCPI settings): the tag
//! is writable, the value has its type and, for an analog, is finite and within `min`/`max`. Modbus and DNP3 map by address and
//! range-scale instead, so they do not use this.

use super::super::config::{flag, number, require, ConfigError, Opts, Tags};
use crate::hint::show;
use crate::model::{Kind, Value};
use roxmltree::Node;
use std::fmt;

/// One exposed tag. `min`/`max` bound the analog writes it accepts; a tag that is not `writable` accepts none.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub tag: String,
    pub kind: Kind,
    pub writable: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// A value as a protocol decoded it, before it is checked against a tag's type.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Num {
    Bool(bool),
    Number(f64),
}

/// Why `Point::check` refused a write.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    NotWritable,
    WrongType,
    NotFinite,
    OutOfRange(f64),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Refusal::NotWritable => f.write_str("not writable"),
            Refusal::WrongType => f.write_str("wrong type for the tag"),
            Refusal::NotFinite => f.write_str("not a finite number"),
            Refusal::OutOfRange(x) => write!(f, "{x} is outside the limits"),
        }
    }
}

impl Point {
    /// A write value as this point's own type, within its limits.
    pub fn check(&self, n: Num) -> Result<Value, Refusal> {
        if !self.writable {
            return Err(Refusal::NotWritable);
        }
        match (self.kind, n) {
            (Kind::Binary, Num::Bool(b)) => Ok(Value::Binary(b)),
            (Kind::Binary, Num::Number(x)) if x == 0.0 || x == 1.0 => Ok(Value::Binary(x == 1.0)),
            (Kind::Analog, Num::Number(x)) => self.analog(x),
            _ => Err(Refusal::WrongType),
        }
    }

    fn analog(&self, x: f64) -> Result<Value, Refusal> {
        match (
            x.is_finite(),
            self.min.is_some_and(|m| x < m) || self.max.is_some_and(|m| x > m),
        ) {
            (false, _) => Err(Refusal::NotFinite),
            (true, true) => Err(Refusal::OutOfRange(x)),
            (true, false) => Ok(Value::Analog(x)),
        }
    }
}

/// The `<point>` children of a module element whose tag exists, each with its XML node (a module reads its own extra keys there).
/// An unknown tag is warned about and skipped, a tag listed twice or `<min-value>` above `<max-value>` is an error.
pub fn points<'a, 'i>(
    node: Node<'a, 'i>,
    tags: &Tags,
    opts: &Opts,
) -> Result<Vec<(Node<'a, 'i>, Point)>, ConfigError> {
    let mut out: Vec<(Node, Point)> = vec![];
    for n in node.children().filter(|c| c.has_tag_name("point")) {
        let tag = require(n, "tag")?;
        let Some(kind) = tags.kind(&tag) else {
            opts.warn(format!("mapping for unknown tag `{}` skipped", show(&tag)))?;
            continue;
        };
        if out.iter().any(|(_, p)| p.tag == tag) {
            return Err(ConfigError(format!(
                "<point> tag `{}` is listed twice",
                show(&tag)
            )));
        }
        let (min, max) = (
            number::<f64>(n, "min-value")?,
            number::<f64>(n, "max-value")?,
        );
        if min.zip(max).is_some_and(|(a, b)| a > b) {
            return Err(ConfigError(format!(
                "<point> `{}`: <min-value> is above <max-value>",
                show(&tag)
            )));
        }
        let writable = flag(n, "writable", false)?;
        let p = Point {
            tag,
            kind,
            writable,
            min,
            max,
        };
        out.push((n, p));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(kind: Kind, writable: bool, min: Option<f64>, max: Option<f64>) -> Point {
        Point {
            tag: "t".into(),
            kind,
            writable,
            min,
            max,
        }
    }

    #[test]
    fn a_write_must_be_allowed_typed_finite_and_within_limits() {
        let a = point(Kind::Analog, true, Some(0.0), Some(10.0));
        assert_eq!(a.check(Num::Number(10.0)), Ok(Value::Analog(10.0)));
        assert_eq!(a.check(Num::Number(0.0)), Ok(Value::Analog(0.0)));
        assert_eq!(a.check(Num::Number(10.5)), Err(Refusal::OutOfRange(10.5)));
        assert_eq!(a.check(Num::Number(-0.5)), Err(Refusal::OutOfRange(-0.5)));
        assert_eq!(a.check(Num::Number(f64::NAN)), Err(Refusal::NotFinite));
        assert_eq!(a.check(Num::Bool(true)), Err(Refusal::WrongType));
        let open = point(Kind::Analog, true, None, None);
        assert_eq!(open.check(Num::Number(1e9)), Ok(Value::Analog(1e9)));
        let b = point(Kind::Binary, true, None, None);
        assert_eq!(b.check(Num::Bool(false)), Ok(Value::Binary(false)));
        assert_eq!(b.check(Num::Number(1.0)), Ok(Value::Binary(true)));
        assert_eq!(b.check(Num::Number(0.0)), Ok(Value::Binary(false)));
        assert_eq!(b.check(Num::Number(2.0)), Err(Refusal::WrongType));
        let ro = point(Kind::Binary, false, None, None);
        assert_eq!(ro.check(Num::Bool(true)), Err(Refusal::NotWritable));
    }

    #[test]
    fn refusals_read_as_the_log_lines_mqtt_always_wrote() {
        assert_eq!(Refusal::NotWritable.to_string(), "not writable");
        assert_eq!(Refusal::WrongType.to_string(), "wrong type for the tag");
        assert_eq!(Refusal::NotFinite.to_string(), "not a finite number");
        assert_eq!(
            Refusal::OutOfRange(2.5).to_string(),
            "2.5 is outside the limits"
        );
    }
}
