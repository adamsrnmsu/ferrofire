//! The bennu command interface (protocol plan row 7): a ZMTP REP socket at a client's `<command-interface>` that `bennu-probe` and
//! the SCEPTRE apps query, read and write tags through. `reply` is the stock `CommandInterface::messageHandler`, reply for reply
//! (conformance/stock/client-*.golden), except where stock dies; `serve` is the socket around it.

use crate::model::Value;
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeromq::{Socket, SocketRecv, SocketSend};

/// What a client module shows the command interface: its tags, and a read and a write of one of them.
pub trait Points: Send + Sync {
    /// Every mapped tag, in name order (stock keeps a `std::set`).
    fn names(&self) -> Vec<String>;
    /// The value of a mapped tag, or the text after `ERR=Failed reading tag 't': `.
    fn read(&self, tag: &str) -> impl Future<Output = Result<Value, String>> + Send;
    /// A write as the request worded it: `Binary` for `true`/`false`, `Analog` for a number, whatever the point is.
    /// `Err` is the text after `ERR=Failed writing tag 't': `.
    fn write(&self, tag: &str, v: Value) -> impl Future<Output = Result<(), String>> + Send;
}

/// `std::to_string(float)`: the value as a float32, `%f`.
pub fn float(x: f64) -> String {
    let x = f64::from(x as f32);
    match (x.is_nan(), x.is_infinite()) {
        (true, _) => "nan".into(),
        (_, true) if x < 0.0 => "-inf".into(),
        (_, true) => "inf".into(),
        _ => format!("{x:.6}"),
    }
}

/// How many bytes at the front of `s` are ASCII digits of `radix`.
fn digits(s: &str, radix: u32) -> usize {
    s.bytes()
        .take_while(|b| char::from(*b).is_digit(radix))
        .count()
}

/// `inf`, `nan` (any case, whatever follows) or a hexadecimal integer at the front of `s`.
/// ponytail: a hexadecimal number is its integer part only (no `.` fraction, no `p` exponent); add them if a tool ever sends one.
fn special(s: &str) -> Option<f64> {
    match s.get(..3).map(str::to_ascii_lowercase).as_deref() {
        Some("inf") => Some(f64::INFINITY),
        Some("nan") => Some(f64::NAN),
        _ => {
            let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
            let n = digits(hex, 16);
            Some(u64::from_str_radix(&hex[..n], 16).unwrap_or(0) as f64)
        }
    }
}

/// How many bytes the decimal number at the front of `s` takes: digits, a fraction, and an exponent when it has digits.
/// 0 when there is no digit.
fn decimal(s: &str) -> usize {
    // on bytes: what follows the number may be any text, and a slice must not cut a character
    let b = s.as_bytes();
    let digits = |from: usize| {
        let rest = &b[from.min(b.len())..];
        rest.iter().take_while(|c| c.is_ascii_digit()).count()
    };
    let int = digits(0);
    let frac = match b.get(int) {
        Some(b'.') => 1 + digits(int + 1),
        _ => 0,
    };
    let end = int + frac;
    if int + frac.saturating_sub(1) == 0 || !matches!(b.get(end), Some(b'e' | b'E')) {
        return if int + frac.saturating_sub(1) == 0 {
            0
        } else {
            end
        };
    }
    let signed = usize::from(matches!(b.get(end + 1), Some(b'+' | b'-')));
    match digits(end + 1 + signed) {
        0 => end,
        n => end + 1 + signed + n,
    }
}

/// C++ `std::stod`: blanks, a sign, then the longest decimal, hexadecimal, `inf` or `nan` at the front; the rest is ignored.
/// `None` where `stod` throws (no number, or one out of a double's range), which kills stock.
pub fn stod(s: &str) -> Option<f64> {
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (sign, s) = match s.as_bytes().first() {
        Some(b'-') => (-1.0, &s[1..]),
        Some(b'+') => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    if let Some(x) = special(s) {
        return Some(sign * x);
    }
    let x: f64 = s[..decimal(s)].parse().ok()?;
    x.is_finite().then_some(sign * x)
}

fn done(text: String) -> Vec<u8> {
    crate::bennu::with_nul(text)
}

/// What a request that kills stock (it indexes past the end of a split) is answered.
const MALFORMED: &str = "ERR=Malformed request (must be QUERY=|READ=tag|WRITE=tag:value)";

fn unmapped(tag: &str) -> String {
    format!("ERR=Client does not have a mapping for tag '{tag}'")
}

fn query(names: &[String]) -> String {
    match names.is_empty() {
        true => "ERR=Client does not have any tag mappings".into(),
        false => format!("ACK={},", names.join(",")),
    }
}

async fn read(tag: &str, p: &impl Points) -> String {
    if !p.names().iter().any(|n| n == tag) {
        return unmapped(tag);
    }
    match p.read(tag).await {
        Ok(Value::Analog(x)) => format!("ACK={tag}:{}", float(x)),
        Ok(Value::Binary(b)) => format!("ACK={tag}:{b}"),
        Err(why) => format!("ERR=Failed reading tag '{tag}': {why}"),
    }
}

/// The value a WRITE names: `true`/`false`, or whatever `stod` makes of the text.
fn value(text: &str) -> Option<Value> {
    match text {
        "true" => Some(Value::Binary(true)),
        "false" => Some(Value::Binary(false)),
        _ => stod(text).map(Value::Analog),
    }
}

/// `rest` is `tag:value`; what follows a second `:` is ignored, as stock ignores it.
async fn write(rest: &str, p: &impl Points) -> String {
    let mut parts = rest.split(':');
    let (tag, Some(val)) = (parts.next().unwrap_or(""), parts.next()) else {
        return MALFORMED.into();
    };
    if !p.names().iter().any(|n| n == tag) {
        return unmapped(tag);
    }
    // stock: `terminate called after throwing an instance of 'std::invalid_argument'`
    let Some(v) = value(val) else {
        return format!(
            "ERR=Failed writing tag '{tag}': value '{val}' is not true, false or a number"
        );
    };
    match p.write(tag, v).await {
        Ok(()) => format!("ACK=Wrote tag {tag} -- {val}"),
        Err(why) => format!("ERR=Failed writing tag '{tag}': {why}"),
    }
}

/// The whole reply to one request, NUL included. `line` is the request up to its NUL: `op=rest`, and what follows a second
/// `=` is ignored, as stock ignores it.
pub async fn reply(line: &str, p: &impl Points) -> Vec<u8> {
    let mut parts = line.split('=');
    let (op, Some(rest)) = (parts.next().unwrap_or(""), parts.next()) else {
        return done(MALFORMED.into());
    };
    done(match op {
        "QUERY" | "query" => query(&p.names()),
        "READ" | "read" => read(rest, p).await,
        "WRITE" | "write" => write(rest, p).await,
        _ => "ERR=Unknown command type (must be QUERY|READ|WRITE)".into(),
    })
}

/// A reply that cannot be sent within this long is given up; the socket goes on.
const SEND_TIMEOUT: Duration = Duration::from_secs(1);

/// Serve `p` on a ZMTP REP socket at `endpoint` until `stop`. A bind failure logs and returns (stock exits the whole device).
pub async fn serve(endpoint: &str, p: impl Points + 'static, stop: CancellationToken) {
    let mut rep = zeromq::RepSocket::new();
    if let Err(e) = rep
        .bind(&endpoint.replace("tcp://*:", "tcp://0.0.0.0:"))
        .await
    {
        tracing::error!("command-interface: bind {endpoint}: {e}");
        return;
    }
    loop {
        let msg = tokio::select! {
            _ = stop.cancelled() => return,
            m = rep.recv() => match m {
                Ok(m) => m,
                // a client that went away mid-request: wait a moment so that a socket that keeps failing cannot spin
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            },
        };
        // stock reads the request as a C string
        let req = msg.get(0).map(|f| f.to_vec()).unwrap_or_default();
        let line =
            String::from_utf8_lossy(req.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
        let out = reply(&line, &p).await;
        let _ = tokio::time::timeout(SEND_TIMEOUT, rep.send(out.into())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        wrote: Mutex<Vec<(String, Value)>>,
        empty: bool,
    }

    impl Points for Fake {
        fn names(&self) -> Vec<String> {
            match self.empty {
                true => vec![],
                false => vec!["a".into(), "b".into(), "bad".into()],
            }
        }
        async fn read(&self, tag: &str) -> Result<Value, String> {
            match tag {
                "a" => Ok(Value::Analog(1.5)),
                "b" => Ok(Value::Binary(true)),
                _ => Err("readRegisterByTag(): Failure".into()),
            }
        }
        async fn write(&self, tag: &str, v: Value) -> Result<(), String> {
            self.wrote.lock().unwrap().push((tag.to_string(), v));
            match tag {
                "bad" => Err("writeCoil(): Failed writing coil".into()),
                _ => Ok(()),
            }
        }
    }

    async fn say(line: &str) -> String {
        String::from_utf8(reply(line, &Fake::default()).await).unwrap()
    }

    const UNKNOWN: &str = "ERR=Unknown command type (must be QUERY|READ|WRITE)\0";

    #[tokio::test]
    async fn a_query_lists_the_tags_and_the_op_is_upper_or_lower_case_only() {
        assert_eq!(say("QUERY=").await, "ACK=a,b,bad,\0");
        assert_eq!(say("query=").await, "ACK=a,b,bad,\0");
        assert_eq!(say("QUERY=zzz").await, "ACK=a,b,bad,\0");
        assert_eq!(say("Query=").await, UNKNOWN);
        assert_eq!(say("FOO=x").await, UNKNOWN);
        assert_eq!(say("=").await, UNKNOWN);
        assert_eq!(say(" READ=a").await, UNKNOWN);
        let none = Fake {
            empty: true,
            ..Fake::default()
        };
        assert_eq!(
            reply("QUERY=", &none).await,
            b"ERR=Client does not have any tag mappings\0"
        );
    }

    #[tokio::test]
    async fn a_read_prints_a_float32_with_six_decimals_or_true_false() {
        assert_eq!(say("read=a").await, "ACK=a:1.500000\0");
        assert_eq!(say("READ=b").await, "ACK=b:true\0");
        assert_eq!(say("READ=a=x").await, "ACK=a:1.500000\0");
        assert_eq!(
            say("READ=bad").await,
            "ERR=Failed reading tag 'bad': readRegisterByTag(): Failure\0"
        );
        for (tag, shown) in [("nope", "nope"), ("a ", "a "), ("", ""), ("a:3", "a:3")] {
            assert_eq!(
                say(&format!("READ={tag}")).await,
                format!("ERR=Client does not have a mapping for tag '{shown}'\0")
            );
        }
    }

    #[test]
    fn a_number_prints_as_c_prints_a_float() {
        assert_eq!(float(16416.0 / 65.535), "250.492096");
        assert_eq!(float(0.1), "0.100000");
        assert_eq!(float(-2.5), "-2.500000");
        assert_eq!(float(1e39), "inf");
        assert_eq!(float(-1e39), "-inf");
        assert_eq!(float(f64::NAN), "nan");
    }

    #[tokio::test]
    async fn a_write_goes_by_the_text_of_its_value_and_echoes_it() {
        let f = Fake::default();
        let say = |line: &'static str| async { String::from_utf8(reply(line, &f).await).unwrap() };
        assert_eq!(say("WRITE=a:2").await, "ACK=Wrote tag a -- 2\0");
        assert_eq!(say("write=b:true").await, "ACK=Wrote tag b -- true\0");
        assert_eq!(say("WRITE=a:false").await, "ACK=Wrote tag a -- false\0");
        assert_eq!(say("WRITE=b: 7x").await, "ACK=Wrote tag b --  7x\0");
        assert_eq!(say("WRITE=a:5:6").await, "ACK=Wrote tag a -- 5\0");
        assert_eq!(say("WRITE=a:true:x").await, "ACK=Wrote tag a -- true\0");
        assert_eq!(
            say("WRITE=bad:1").await,
            "ERR=Failed writing tag 'bad': writeCoil(): Failed writing coil\0"
        );
        assert_eq!(
            say("WRITE=nope:1").await,
            "ERR=Client does not have a mapping for tag 'nope'\0"
        );
        assert_eq!(
            say("WRITE=:").await,
            "ERR=Client does not have a mapping for tag ''\0"
        );
        use Value::{Analog, Binary};
        let want = [
            ("a", Analog(2.0)),
            ("b", Binary(true)),
            ("a", Binary(false)),
            ("b", Analog(7.0)),
            ("a", Analog(5.0)),
            ("a", Binary(true)),
            ("bad", Analog(1.0)),
        ];
        let got = f.wrote.lock().unwrap().clone();
        assert_eq!(got.len(), want.len());
        for ((gt, gv), (wt, wv)) in got.iter().zip(want) {
            assert_eq!((gt.as_str(), *gv), (wt, wv));
        }
    }

    #[tokio::test]
    async fn requests_that_kill_stock_get_an_error_and_write_nothing() {
        let f = Fake::default();
        for line in ["READ", "", "QUERY", "WRITE=a", "WRITE="] {
            assert_eq!(
                reply(line, &f).await,
                b"ERR=Malformed request (must be QUERY=|READ=tag|WRITE=tag:value)\0",
                "{line:?}"
            );
        }
        for v in ["TRUE", "", "-", "abc", "1e999"] {
            assert_eq!(
                String::from_utf8(reply(&format!("WRITE=a:{v}"), &f).await).unwrap(),
                format!("ERR=Failed writing tag 'a': value '{v}' is not true, false or a number\0")
            );
        }
        assert!(f.wrote.lock().unwrap().is_empty());
    }

    #[test]
    fn stod_takes_the_longest_number_at_the_front() {
        for (text, want) in [
            ("2", 2.0),
            ("250.5", 250.5),
            ("-5", -5.0),
            ("+5", 5.0),
            ("1e3", 1000.0),
            ("1E-2", 0.01),
            (".5", 0.5),
            ("5.", 5.0),
            (" 7", 7.0),
            ("\t\n7", 7.0),
            ("7x", 7.0),
            ("7e", 7.0),
            ("7e+", 7.0),
            ("0x10", 16.0),
            ("0X1f", 31.0),
            ("0x", 0.0),
            ("inf", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("INFx", f64::INFINITY),
            ("1e39", 1e39),
        ] {
            assert_eq!(stod(text), Some(want), "{text:?}");
        }
        assert!(stod("nan").unwrap().is_nan() && stod("-NaN").unwrap().is_nan());
        for text in [
            "", " ", "-", "+", ".", "e5", "abc", "TRUE", "x7", "1e999", "-1e999",
        ] {
            assert_eq!(stod(text), None, "{text:?}");
        }
    }

    /// Final review, finding 1: what follows the number may be any text, a multi-byte character included.
    #[tokio::test]
    async fn text_that_is_not_ascii_after_a_number_is_ignored_like_any_other() {
        for (text, want) in [
            ("1é", 1.0),
            ("1eé", 1.0),
            ("2.5\u{fffd}", 2.5),
            ("7e+é", 7.0),
            ("3é5", 3.0),
        ] {
            assert_eq!(stod(text), Some(want), "{text:?}");
        }
        assert_eq!(stod("é"), None);
        assert_eq!(stod("-é1"), None);
        let f = Fake::default();
        let got = String::from_utf8(reply("WRITE=a:1é", &f).await).unwrap();
        assert_eq!(got, "ACK=Wrote tag a -- 1é\0");
        assert_eq!(
            reply("READ=é", &f).await,
            "ERR=Client does not have a mapping for tag 'é'\0".as_bytes()
        );
    }
}
