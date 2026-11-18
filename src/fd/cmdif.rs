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

/// C++ `std::stod`: blanks, a sign, then the longest decimal, hexadecimal, `inf` or `nan` at the front; the rest is ignored.
/// `None` where `stod` throws (no number, or one out of a double's range), which kills stock.
/// ponytail: a hexadecimal number is its integer part only (no `.` fraction, no `p` exponent); add them if a tool ever sends one.
pub fn stod(s: &str) -> Option<f64> {
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (sign, s) = match s.as_bytes().first() {
        Some(b'-') => (-1.0, &s[1..]),
        Some(b'+') => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    let lower = s.get(..3).map(str::to_ascii_lowercase);
    if lower.as_deref() == Some("inf") {
        return Some(sign * f64::INFINITY);
    }
    if lower.as_deref() == Some("nan") {
        return Some(f64::NAN);
    }
    if s.starts_with("0x") || s.starts_with("0X") {
        let n = digits(&s[2..], 16);
        return Some(sign * u64::from_str_radix(&s[2..2 + n], 16).unwrap_or(0) as f64);
    }
    let int = digits(s, 10);
    let frac = match s[int..].starts_with('.') {
        true => 1 + digits(&s[int + 1..], 10),
        false => 0,
    };
    if int + frac.saturating_sub(1) == 0 {
        return None;
    }
    let mut end = int + frac;
    if let Some(e) = s[end..].strip_prefix(['e', 'E']) {
        let signed = usize::from(e.starts_with(['+', '-']));
        let n = digits(&e[signed..], 10);
        if n > 0 {
            end += 1 + signed + n;
        }
    }
    let x: f64 = s[..end].parse().ok()?;
    x.is_finite().then_some(sign * x)
}

fn done(text: String) -> Vec<u8> {
    crate::bennu::with_nul(text)
}

/// The whole reply to one request, NUL included. `line` is the request up to its NUL.
pub async fn reply(line: &str, p: &impl Points) -> Vec<u8> {
    let malformed =
        || done("ERR=Malformed request (must be QUERY=|READ=tag|WRITE=tag:value)".into());
    let mut parts = line.split('=');
    let (op, Some(tag)) = (parts.next().unwrap_or(""), parts.next()) else {
        return malformed(); // stock indexes past the end of the split and dies
    };
    let unmapped = |tag: &str| {
        done(format!(
            "ERR=Client does not have a mapping for tag '{tag}'"
        ))
    };
    let names = p.names();
    match op {
        "QUERY" | "query" if names.is_empty() => {
            done("ERR=Client does not have any tag mappings".into())
        }
        "QUERY" | "query" => done(format!("ACK={},", names.join(","))),
        "READ" | "read" if !names.iter().any(|n| n == tag) => unmapped(tag),
        "READ" | "read" => done(match p.read(tag).await {
            Ok(Value::Analog(x)) => format!("ACK={tag}:{}", float(x)),
            Ok(Value::Binary(b)) => format!("ACK={tag}:{b}"),
            Err(why) => format!("ERR=Failed reading tag '{tag}': {why}"),
        }),
        "WRITE" | "write" => {
            let mut parts = tag.split(':');
            let (tag, Some(val)) = (parts.next().unwrap_or(""), parts.next()) else {
                return malformed(); // the same death
            };
            if !names.iter().any(|n| n == tag) {
                return unmapped(tag);
            }
            let v = match val {
                "true" => Value::Binary(true),
                "false" => Value::Binary(false),
                _ => match stod(val) {
                    Some(x) => Value::Analog(x),
                    // stock: `terminate called after throwing an instance of 'std::invalid_argument'`
                    None => {
                        return done(format!(
                            "ERR=Failed writing tag '{tag}': value '{val}' is not true, false or a number"
                        ))
                    }
                },
            };
            done(match p.write(tag, v).await {
                Ok(()) => format!("ACK=Wrote tag {tag} -- {val}"),
                Err(why) => format!("ERR=Failed writing tag '{tag}': {why}"),
            })
        }
        _ => done("ERR=Unknown command type (must be QUERY|READ|WRITE)".into()),
    }
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
            m = rep.recv() => match m { Ok(m) => m, Err(_) => continue },
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
}
