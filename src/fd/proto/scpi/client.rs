//! SCPI controller (`<scpi-client>`, docs/field-device-scpi.md): the other side of `<scpi-server>`. One task per element polls one
//! instrument over the raw socket, HiSLIP or VXI-11 (`crate::scpi::transport`, blocking sockets, so the task is a blocking thread):
//! each poll sends the `<set>` commands whose tag changed since it was last sent, then asks every `<query>` and queues an answer
//! that differs from its tag as a write. `<logic>` computes what is set; nothing here decides anything. It reads the store and
//! queues writes, never touching the scan cycle, and reconnects with the backoff `<mqtt-client>` uses.

use super::tag_mnemonic;
use crate::fd::config::{bounded, flag, require, text, ConfigError, Opts, Tags};
use crate::fd::eventlog::EventLog;
use crate::fd::proto::{backoff, Module, BACKOFF_FIRST};
use crate::fd::scan::pause;
use crate::fd::store::{same, Store};
use crate::hint::show;
use crate::model::{Kind, Value};
use crate::scpi::client::{Link, Outcome};
use crate::scpi::syntax::is_query;
use crate::scpi::transport::{open, parse_target, Target, Wire};
use roxmltree::Node;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// One `<query>` or `<set>`: the tag, its kind, and the command (a set's has `{}` where the value goes).
#[derive(Debug, PartialEq)]
pub struct Item {
    pub tag: String,
    pub kind: Kind,
    pub command: String,
}

pub struct Client {
    target: Target,
    portmapper: u16,
    /// From the start of one poll to the start of the next.
    interval: Duration,
    /// To connect, and for each response.
    wait: Duration,
    /// Read `SYST:ERR?` after every command.
    check: bool,
    queries: Vec<Item>,
    sets: Vec<Item>,
    log: EventLog,
}

fn kind_of(n: Node) -> Result<Option<Kind>, ConfigError> {
    match text(n, "type").as_deref() {
        None => Ok(None),
        Some("analog") => Ok(Some(Kind::Analog)),
        Some("binary") => Ok(Some(Kind::Binary)),
        Some(t) => Err(ConfigError(format!(
            "<type> `{}` is not analog or binary",
            show(t)
        ))),
    }
}

/// The tag's kind as `<tags>` or an earlier element declared it, which a `<type>` must match; a new tag is declared here as a
/// device-local tag of `want` (analog when not given).
fn declare(tags: &mut Tags, tag: &str, want: Option<Kind>) -> Result<Kind, ConfigError> {
    match (tags.kind(tag), want) {
        (Some(k), Some(w)) if k != w => Err(ConfigError(format!(
            "tag `{}` is {k:?}, not {w:?}",
            show(tag)
        ))),
        (Some(k), _) => Ok(k),
        (None, w) => {
            let k = w.unwrap_or(Kind::Analog);
            tags.add_internal(tag, k)?;
            Ok(k)
        }
    }
}

/// The path a ferrofire `<scpi-server>` gives the same tag, so two ferrofire devices pair up by tag name alone.
fn default_command(key: &str, tag: &str, kind: Kind) -> String {
    let m = tag_mnemonic(tag);
    match (key, kind) {
        ("set", _) => format!("SOUR:{m} {{}}"),
        (_, Kind::Analog) => format!("MEAS:SCAL:{m}?"),
        (_, Kind::Binary) => format!("MEAS:BIN:{m}?"),
    }
}

/// A `<query>` or `<set>` element (`key` is which).
fn item(n: Node, key: &str, tags: &mut Tags) -> Result<Item, ConfigError> {
    let tag = require(n, "tag")?;
    let kind = declare(tags, &tag, kind_of(n)?)?;
    let command = text(n, "command").unwrap_or_else(|| default_command(key, &tag, kind));
    let wrong = match key {
        "set" => (!command.contains("{}")).then_some("has no `{}` for the value"),
        _ => (!is_query(&command)).then_some("is not a query (no header ends in `?`)"),
    };
    match wrong {
        Some(why) => Err(ConfigError(format!(
            "<{key}> of tag `{}`: <command> `{}` {why}",
            show(&tag),
            show(&command)
        ))),
        None => Ok(Item { tag, kind, command }),
    }
}

fn items(node: Node, key: &str, tags: &mut Tags) -> Result<Vec<Item>, ConfigError> {
    node.children()
        .filter(|c| c.has_tag_name(key))
        .map(|n| item(n, key, tags))
        .collect()
}

/// A value as a set command's parameter: a binary `1` or `0`, an analog in Rust's shortest round-trip form with `E` for its
/// exponent (NR2 `12.0`, NR3 `1E20`). `None` for NaN and infinity, which SCPI has no number for.
fn parameter(v: Value) -> Option<String> {
    match v {
        Value::Binary(b) => Some(u8::from(b).to_string()),
        Value::Analog(x) if x.is_finite() => Some(format!("{x:?}").to_uppercase()),
        Value::Analog(_) => None,
    }
}

/// The set command for `v`: every `{}` of the template replaced by its value; `None` when the value cannot be sent.
pub fn fill(template: &str, v: Value) -> Option<String> {
    parameter(v).map(|p| template.replace("{}", &p))
}

/// A set is due when its tag holds anything but the value last sent on this connection (and always on a new connection); an answer
/// is written when its tag holds anything but the answer.
pub fn due(last: Option<Value>, now: Value) -> bool {
    !last.is_some_and(|l| same(&l, &now))
}

/// A number, the SCPI sentinels 9.91E37 (not a number) and +-9.9E37 (infinity) becoming NaN and infinity. Rust also reads `inf` and
/// `nan` as numbers; SCPI has no such text, so a letter other than the exponent's refuses the field.
fn analog(s: &str) -> Option<f64> {
    if s.chars()
        .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return None;
    }
    let x: f64 = s.parse().ok()?;
    Some(match x {
        9.91e37 => f64::NAN,
        x if x.abs() == 9.9e37 => f64::INFINITY.copysign(x),
        x => x,
    })
}

fn binary(s: &str) -> Option<bool> {
    match s.to_ascii_uppercase().as_str() {
        "1" | "ON" => Some(true),
        "0" | "OFF" => Some(false),
        _ => None,
    }
}

/// The value of an answer for a tag of `kind`: its first `,`- or `;`-separated field. `None` when that is not one.
pub fn answer(line: &str, kind: Kind) -> Option<Value> {
    let field = line.split([',', ';']).next().unwrap_or_default().trim();
    match kind {
        Kind::Analog => analog(field).map(Value::Analog),
        Kind::Binary => binary(field).map(Value::Binary),
    }
}

impl Module for Client {
    fn parse(node: Node, tags: &mut Tags, _opts: &Opts) -> Result<Self, ConfigError> {
        let endpoint = require(node, "endpoint")?;
        let target = parse_target(&endpoint).map_err(|e| ConfigError(format!("<endpoint> {e}")))?;
        let ms =
            |key, default, range| bounded(node, key, default, range).map(Duration::from_millis);
        Ok(Client {
            target,
            portmapper: bounded(node, "portmapper", 111u16, 1..=65535)?,
            interval: ms("interval", 1000, 50..=3_600_000)?,
            wait: ms("timeout", 5000, 100..=600_000)?,
            check: flag(node, "check", true)?,
            queries: items(node, "query", tags)?,
            sets: items(node, "set", tags)?,
            log: EventLog::open(text(node, "event-logging").as_deref(), "scpi-client"),
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let _ = tokio::task::spawn_blocking(move || self.serve(&store, &stop)).await;
    }
}

impl Client {
    /// Connect, poll until the connection fails, pause, again, until `stop`. A failed connect is logged once until one works.
    fn serve(&self, store: &Store, stop: &CancellationToken) {
        self.reconnect(store, stop, |wait| pause(wait, stop));
    }

    /// `serve` with the reconnect pause passed in as `rest`.
    fn reconnect(&self, store: &Store, stop: &CancellationToken, mut rest: impl FnMut(Duration)) {
        let (mut wait, mut quiet) = (BACKOFF_FIRST, false);
        while !stop.is_cancelled() {
            let up = match open(&self.target, self.wait, self.portmapper) {
                Ok(wire) => {
                    self.log.log("info", "scpi connect", "connected");
                    quiet = false;
                    self.session(Link::wired(wire, self.wait), store, stop);
                    true
                }
                Err(e) if !quiet => {
                    self.log
                        .say("error", "scpi connect", &format!("{e}; retrying"));
                    quiet = true;
                    false
                }
                Err(_) => false,
            };
            wait = backoff(wait, up);
            rest(wait);
        }
    }

    /// Poll every `interval` until a command fails or `stop`. Nothing has been sent on a new connection, so every set is due.
    fn session(&self, mut link: Link<Box<dyn Wire>>, store: &Store, stop: &CancellationToken) {
        let mut sent = vec![None; self.sets.len()];
        while !stop.is_cancelled() {
            let start = Instant::now();
            if let Err(e) = self.poll(&mut link, &mut sent, store, stop) {
                self.log
                    .say("error", "scpi connection", &format!("{e}; reconnecting"));
                return;
            }
            pause(self.interval.saturating_sub(start.elapsed()), stop);
        }
    }

    /// One poll: the sets that are due, then every query. `sent` is the value last sent per set. `Err`: the connection is gone.
    fn poll<W: Wire>(
        &self,
        link: &mut Link<W>,
        sent: &mut [Option<Value>],
        store: &Store,
        stop: &CancellationToken,
    ) -> Result<(), String> {
        for (set, last) in self.sets.iter().zip(sent.iter_mut()) {
            let Some(now) = store.get(&set.tag).filter(|v| due(*last, *v)) else {
                continue;
            };
            if stop.is_cancelled() {
                return Ok(());
            }
            *last = Some(now);
            match fill(&set.command, now) {
                Some(cmd) => self.errors(link.run(&cmd, self.check)?),
                None => self.log.say(
                    "warn",
                    "scpi set",
                    &format!(
                        "`{}` holds {now:?}, which SCPI has no number for; not sent",
                        show(&set.tag)
                    ),
                ),
            }
        }
        for q in &self.queries {
            if stop.is_cancelled() {
                return Ok(());
            }
            let o = link.run(&q.command, self.check)?;
            self.take(q, o.response.as_deref(), store);
            self.errors(o);
        }
        Ok(())
    }

    /// Queue a query's answer as a write to its tag when the tag holds something else, or when a write to it is still queued (the
    /// newest answer replaces it). An unchanged answer is not written, so an external tag costs a provider write only when they differ;
    /// one the provider refused or overwrote differs again and is written again. A rejected query (no response) leaves the tag alone,
    /// as does an answer that is not a value of its kind.
    fn take(&self, q: &Item, response: Option<&str>, store: &Store) {
        let Some(line) = response else { return };
        match answer(line, q.kind) {
            // the queue before the value: checked the other way round, a queued write applied in between would be missed
            Some(v) if store.is_queued(&q.tag) || due(store.get(&q.tag), v) => {
                let _ = store.write(&q.tag, v);
            }
            Some(_) => {}
            None => self.log.say(
                "warn",
                "scpi answer",
                &format!(
                    "`{}` answered `{}`, not {}; `{}` keeps its value",
                    show(&q.command),
                    show(line),
                    match q.kind {
                        Kind::Analog => "a number",
                        Kind::Binary => "1, 0, ON or OFF",
                    },
                    show(&q.tag)
                ),
            ),
        }
    }

    /// One event-log line per error the instrument's queue held after a command.
    fn errors(&self, o: Outcome) {
        for p in o.problems() {
            self.log.say("warn", "scpi error", &p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::store::Layout;
    use crate::scpi::syntax::{params, Param};
    use crate::scpi::transport::Target;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    fn parse_with(inner: &str, tags: &mut Tags) -> Result<Client, ConfigError> {
        let quiet = match inner.contains("<event-logging>") {
            true => "",
            false => "<event-logging>/dev/null</event-logging>",
        };
        let xml = format!("<scpi-client>{quiet}{inner}</scpi-client>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        Client::parse(doc.root_element(), tags, &Opts::default())
    }

    fn parse(inner: &str) -> Result<Client, ConfigError> {
        parse_with(
            &format!("<endpoint>lab</endpoint>{inner}"),
            &mut Tags::default(),
        )
    }

    fn err(inner: &str) -> String {
        parse(inner).err().expect("an error").0
    }

    fn item(tag: &str, kind: Kind, command: &str) -> Item {
        Item {
            tag: tag.into(),
            kind,
            command: command.into(),
        }
    }

    #[test]
    fn defaults_and_every_key() {
        let c = parse("").unwrap();
        assert_eq!(c.target, Target::Socket("lab".into(), 5025));
        assert_eq!(
            (c.portmapper, c.interval, c.wait, c.check),
            (111, Duration::from_secs(1), Duration::from_secs(5), true)
        );
        assert!(c.queries.is_empty() && c.sets.is_empty());
        let c = parse_with(
            "<endpoint>TCPIP::h::hislip0::INSTR</endpoint><portmapper>1111</portmapper><interval>50</interval>
             <timeout>100</timeout><check>false</check>",
            &mut Tags::default(),
        )
        .unwrap();
        assert_eq!(c.target, Target::Hislip("h".into(), 4880, "hislip0".into()));
        assert_eq!(
            (c.portmapper, c.interval, c.wait, c.check),
            (
                1111,
                Duration::from_millis(50),
                Duration::from_millis(100),
                false
            )
        );
        let top = parse("<interval>3600000</interval><timeout>600000</timeout>").unwrap();
        assert_eq!(
            (top.interval, top.wait),
            (Duration::from_secs(3600), Duration::from_secs(600))
        );
    }

    #[test]
    fn bad_values_are_named() {
        let mut t = Tags::default();
        let e = parse_with("", &mut t).err().unwrap().0;
        assert!(e.contains("<scpi-client> has no <endpoint>"), "{e}");
        let e = parse_with("<endpoint>tcp://h:5025</endpoint>", &mut t)
            .err()
            .unwrap()
            .0;
        assert!(e.starts_with("<endpoint> `tcp://h:5025`"), "{e}");
        assert!(
            err("<interval>49</interval>").contains("<interval> 49 is not between 50 and 3600000")
        );
        assert!(err("<interval>3600001</interval>").contains("<interval> 3600001"));
        assert!(err("<timeout>99</timeout>").contains("<timeout> 99 is not between 100 and 600000"));
        assert!(err("<timeout>600001</timeout>").contains("<timeout> 600001"));
        assert!(
            err("<portmapper>0</portmapper>").contains("<portmapper> 0 is not between 1 and 65535")
        );
        assert!(err("<portmapper>x</portmapper>").contains("not a valid number"));
        assert!(err("<check>yes</check>").contains("<check> `yes` is not true or false"));
        assert!(err("<query/>").contains("<query> has no <tag>"));
        assert!(err("<set><command>X {}</command></set>").contains("<set> has no <tag>"));
        assert!(err("<query><tag>a</tag><type>string</type></query>")
            .contains("<type> `string` is not analog or binary"));
        assert_eq!(
            err("<set><tag>a</tag><command>OUTP ON</command></set>"),
            "<set> of tag `a`: <command> `OUTP ON` has no `{}` for the value"
        );
        assert_eq!(
            err("<query><tag>a</tag><command>MEAS:VOLT</command></query>"),
            "<query> of tag `a`: <command> `MEAS:VOLT` is not a query (no header ends in `?`)"
        );
    }

    #[test]
    fn a_known_tag_keeps_its_kind_and_a_new_one_is_declared() {
        let mut t = Tags::default();
        t.add_internal("lvl", Kind::Analog).unwrap();
        t.add_internal("pump", Kind::Binary).unwrap();
        let c = parse_with(
            "<endpoint>lab</endpoint>
             <query><tag>lvl</tag></query><query><tag>pump</tag></query><query><tag>new.b</tag><type>binary</type></query>
             <query><tag>both</tag><type>binary</type></query><set><tag>both</tag></set><set><tag>new.a</tag></set>
             <set><tag>pump</tag><type>binary</type><command>OUTP {}</command></set>",
            &mut t,
        )
        .unwrap();
        assert_eq!(
            c.queries,
            [
                item("lvl", Kind::Analog, "MEAS:SCAL:LVL?"),
                item("pump", Kind::Binary, "MEAS:BIN:PUMP?"),
                item("new.b", Kind::Binary, "MEAS:BIN:NEW_B?"),
                item("both", Kind::Binary, "MEAS:BIN:BOTH?"),
            ]
        );
        assert_eq!(
            c.sets,
            [
                item("both", Kind::Binary, "SOUR:BOTH {}"),
                item("new.a", Kind::Analog, "SOUR:NEW_A {}"),
                item("pump", Kind::Binary, "OUTP {}"),
            ]
        );
        assert_eq!(t.kind("new.b"), Some(Kind::Binary));
        assert_eq!(t.kind("new.a"), Some(Kind::Analog));
        assert_eq!(t.kind("both"), Some(Kind::Binary));
        let mut t = Tags::default();
        t.add_internal("pump", Kind::Binary).unwrap();
        let e = parse_with(
            "<endpoint>lab</endpoint><query><tag>pump</tag><type>analog</type></query>",
            &mut t,
        )
        .err()
        .unwrap();
        assert_eq!(e.0, "tag `pump` is Binary, not Analog");
        let c = parse(
            "<query><tag>1st</tag><command>MEAS:VOLT? (@1)</command></query><set><tag>1st</tag><command>VOLT {};*WAI</command></set>",
        )
        .unwrap();
        assert_eq!(c.queries, [item("1st", Kind::Analog, "MEAS:VOLT? (@1)")]);
        assert_eq!(c.sets[0].command, "VOLT {};*WAI");
        assert_eq!(
            default_command("query", "1st", Kind::Analog),
            "MEAS:SCAL:T1ST?"
        );
    }

    #[test]
    fn a_set_is_due_unless_its_value_was_the_last_sent() {
        let (a, b) = (Value::Analog, Value::Binary);
        assert!(due(None, a(5.0)), "a new connection sends every set");
        assert!(due(None, b(false)));
        assert!(!due(Some(a(5.0)), a(5.0)));
        assert!(due(Some(a(5.0)), a(6.0)));
        assert!(
            !due(Some(a(f64::NAN)), a(f64::NAN)),
            "NaN is the same as NaN"
        );
        assert!(due(Some(a(f64::NAN)), a(1.0)));
        assert!(!due(Some(b(true)), b(true)));
        assert!(due(Some(b(true)), b(false)));
    }

    #[test]
    fn a_template_takes_the_value_in_nr2_or_nr3() {
        let a = Value::Analog;
        assert_eq!(fill("SOUR:X {}", a(12.0)).as_deref(), Some("SOUR:X 12.0"));
        assert_eq!(fill("SOUR:X {}", a(-0.25)).as_deref(), Some("SOUR:X -0.25"));
        assert_eq!(fill("V {}", a(1e20)).as_deref(), Some("V 1E20"));
        assert_eq!(fill("V {}", a(1.5e-7)).as_deref(), Some("V 1.5E-7"));
        assert_eq!(
            fill("OUTP {}", Value::Binary(true)).as_deref(),
            Some("OUTP 1")
        );
        assert_eq!(
            fill("OUTP {}", Value::Binary(false)).as_deref(),
            Some("OUTP 0")
        );
        assert_eq!(fill("A {};B {}", a(2.5)).as_deref(), Some("A 2.5;B 2.5"));
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(fill("V {}", a(x)), None, "{x}");
        }
        // the server's own number parser reads every form back as the same double
        for x in [
            0.0,
            -0.0,
            12.0,
            0.1,
            -3.75,
            1e20,
            1.5e-7,
            f64::MAX,
            f64::MIN_POSITIVE,
            123456.789,
        ] {
            let p = parameter(a(x)).unwrap();
            assert_eq!(params(&p), Ok(vec![Param::Num(x)]), "{p}");
        }
    }

    #[test]
    fn an_answer_is_the_first_field_as_the_tags_kind() {
        let an = |s| answer(s, Kind::Analog);
        let bin = |s| answer(s, Kind::Binary);
        assert_eq!(an("12.5"), Some(Value::Analog(12.5)));
        assert_eq!(an("+1.25E+02,V"), Some(Value::Analog(125.0)));
        assert_eq!(an(" 3 ;1"), Some(Value::Analog(3.0)));
        assert_eq!(an("-4"), Some(Value::Analog(-4.0)));
        assert!(matches!(an("9.91E+37"), Some(Value::Analog(x)) if x.is_nan()));
        assert!(matches!(an("+9.91000E37"), Some(Value::Analog(x)) if x.is_nan()));
        assert_eq!(an("9.9E+37"), Some(Value::Analog(f64::INFINITY)));
        assert_eq!(an("+9.9e37"), Some(Value::Analog(f64::INFINITY)));
        assert_eq!(an("-9.9E+37"), Some(Value::Analog(f64::NEG_INFINITY)));
        assert_eq!(an("9.8E+37"), Some(Value::Analog(9.8e37)));
        for bad in [
            "",
            "nan",
            "inf",
            "-infinity",
            "NaN",
            "abc",
            "1V",
            "ON",
            ",5",
        ] {
            assert_eq!(an(bad), None, "{bad:?}");
        }
        assert_eq!(bin("1"), Some(Value::Binary(true)));
        assert_eq!(bin("0"), Some(Value::Binary(false)));
        assert_eq!(bin("ON"), Some(Value::Binary(true)));
        assert_eq!(bin("off"), Some(Value::Binary(false)));
        assert_eq!(bin("1;1"), Some(Value::Binary(true)));
        assert_eq!(bin(" 0 ,x"), Some(Value::Binary(false)));
        for bad in ["", "2", "1.0", "+1", "TRUE", "-1"] {
            assert_eq!(bin(bad), None, "{bad:?}");
        }
    }

    /// A wire that answers from a script and records what was sent.
    #[derive(Clone, Default)]
    struct Script {
        replies: Rc<RefCell<VecDeque<String>>>,
        sent: Rc<RefCell<Vec<String>>>,
    }

    impl Wire for Script {
        fn send(&mut self, m: &str) -> std::io::Result<()> {
            self.sent.borrow_mut().push(m.into());
            Ok(())
        }
        fn recv(&mut self) -> std::io::Result<Option<String>> {
            Ok(self.replies.borrow_mut().pop_front())
        }
    }

    impl Script {
        fn answer(&self, lines: &[&str]) {
            self.replies
                .borrow_mut()
                .extend(lines.iter().map(|l| l.to_string()));
        }
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.sent.borrow_mut())
        }
    }

    const OK: &str = "0,\"No error\"";

    struct Bench {
        c: Client,
        store: Store,
        wire: Script,
        link: Link<Script>,
        sent: Vec<Option<Value>>,
        stop: CancellationToken,
        log: std::path::PathBuf,
    }

    /// `name` keeps the event log of each bench apart, as tests run at the same time.
    fn bench(name: &str, keys: &str) -> Bench {
        let log =
            std::env::temp_dir().join(format!("scpi-client-{}-{name}.log", std::process::id()));
        let mut t = Tags::default();
        t.add_internal("v", Kind::Analog).unwrap();
        let c = parse_with(
            &format!(
                "<endpoint>lab</endpoint>{keys}<event-logging>{}</event-logging>
                 <set><tag>v</tag></set><set><tag>out</tag><type>binary</type></set><set><tag>gone</tag></set>
                 <query><tag>lvl</tag></query>",
                log.display()
            ),
            &mut t,
        )
        .unwrap();
        let store = Store::new(Layout {
            internal: vec![
                ("v".into(), Value::Analog(1.0)),
                ("out".into(), Value::Binary(true)),
                ("lvl".into(), Value::Analog(0.0)),
            ],
            ..Layout::default()
        });
        let wire = Script::default();
        Bench {
            sent: vec![None; c.sets.len()],
            c,
            store,
            link: Link::wired(wire.clone(), Duration::from_secs(1)),
            wire,
            stop: CancellationToken::new(),
            log,
        }
    }

    impl Bench {
        fn poll(&mut self) -> Result<(), String> {
            let r = self
                .c
                .poll(&mut self.link, &mut self.sent, &self.store, &self.stop);
            self.store.apply_internal();
            r
        }
        fn log(&self) -> String {
            std::fs::read_to_string(&self.log).unwrap()
        }
    }

    #[test]
    fn a_poll_sends_what_changed_then_asks_and_reads_the_error_queue() {
        let mut b = bench("poll", "");
        b.wire
            .answer(&[OK, "-222,\"Data out of range\"", OK, "12.5;1", OK]);
        b.poll().unwrap();
        assert_eq!(
            b.wire.take(),
            [
                "SOUR:V 1.0",
                "SYST:ERR?",
                "SOUR:OUT 1",
                "SYST:ERR?",
                "SYST:ERR?",
                "MEAS:SCAL:LVL?;*OPC?",
                "SYST:ERR?"
            ],
            "`gone` is in no store and is skipped"
        );
        assert_eq!(b.store.get_f64("lvl"), Some(12.5));
        assert!(b
            .log()
            .contains("error: SOUR:OUT 1: -222,Data out of range"));

        // nothing changed: no set; the query is rejected and the tag keeps its value
        b.wire.answer(&["1", "-113,\"Undefined header\"", OK]);
        b.poll().unwrap();
        assert_eq!(
            b.wire.take(),
            ["MEAS:SCAL:LVL?;*OPC?", "SYST:ERR?", "SYST:ERR?"]
        );
        assert!(!b.store.is_queued("lvl"));
        assert_eq!(b.store.get_f64("lvl"), Some(12.5));
        assert!(b.log().contains("-113,Undefined header"));

        // a NaN is not sent, and logged once; an answer that is not a number leaves the tag alone
        b.store.write("v", Value::Analog(f64::NAN)).unwrap();
        b.store.write("out", Value::Binary(false)).unwrap();
        b.store.apply_internal();
        b.wire.answer(&[OK, "abc;1", OK, "2;1", OK]);
        b.poll().unwrap();
        b.poll().unwrap();
        assert_eq!(
            b.wire.take(),
            [
                "SOUR:OUT 0",
                "SYST:ERR?",
                "MEAS:SCAL:LVL?;*OPC?",
                "SYST:ERR?",
                "MEAS:SCAL:LVL?;*OPC?",
                "SYST:ERR?"
            ]
        );
        assert_eq!(b.store.get_f64("lvl"), Some(2.0));
        let log = b.log();
        assert_eq!(
            log.matches("which SCPI has no number for").count(),
            1,
            "{log}"
        );
        assert!(
            log.contains("`MEAS:SCAL:LVL?` answered `abc`, not a number; `lvl` keeps its value")
        );

        // after a reconnect every set is due again
        b.sent = vec![None; 3];
        b.store.write("v", Value::Analog(3.0)).unwrap();
        b.store.apply_internal();
        b.wire.answer(&[OK, OK, "4;1", OK]);
        b.poll().unwrap();
        assert_eq!(
            b.wire.take(),
            [
                "SOUR:V 3.0",
                "SYST:ERR?",
                "SOUR:OUT 0",
                "SYST:ERR?",
                "MEAS:SCAL:LVL?;*OPC?",
                "SYST:ERR?"
            ]
        );
        std::fs::remove_file(&b.log).unwrap();
    }

    /// A client with one `<query>` of `lvl` (no check) over a scripted wire.
    fn asker(store: &Store) -> impl FnMut(&str) -> bool + '_ {
        let c = parse("<check>false</check><query><tag>lvl</tag></query>").unwrap();
        let wire = Script::default();
        let mut link = Link::wired(wire.clone(), Duration::from_secs(1));
        let stop = CancellationToken::new();
        // one poll answering `line`, without the end of the scan cycle: is a write to `lvl` queued?
        move |line| {
            wire.answer(&[line]);
            c.poll(&mut link, &mut [], store, &stop).unwrap();
            store.is_queued("lvl")
        }
    }

    #[test]
    fn an_answer_a_device_local_tag_already_holds_is_not_written_again() {
        let store = Store::new(Layout {
            internal: vec![("lvl".into(), Value::Analog(0.0))],
            ..Layout::default()
        });
        let mut ask = asker(&store);
        assert!(!ask("0"), "the tag holds 0 already");
        assert!(ask("5"));
        // the 5 is not applied yet when the instrument goes back to 0: the 0 must replace it
        assert!(ask("0"));
        store.apply_internal();
        assert_eq!(store.get_f64("lvl"), Some(0.0));
        assert!(ask("5"));
        store.apply_internal();
        assert!(!ask("5"));
        assert!(ask("9.91E37"));
        store.apply_internal();
        assert!(!ask("9.91E37"), "NaN is the same as NaN");
    }

    #[test]
    fn an_external_tag_is_written_when_it_differs_from_the_answer_and_only_then() {
        let store = Store::new(Layout {
            points: vec![("p.lvl".into(), Kind::Analog, None)],
            external: vec![("lvl".into(), "p.lvl".into())],
            ..Layout::default()
        });
        store.publish("p.lvl", 3.0, 1);
        let mut ask = asker(&store);
        assert!(!ask("3"), "the provider has the instrument's value");
        assert!(ask("5"));
        store.ack("lvl", Value::Analog(5.0));
        assert!(
            !ask("5"),
            "no provider write per poll once it holds the answer"
        );
        // the provider publishes its own value once the ACK hold is over: the answer is written again
        store.publish_at("p.lvl", 3.0, 2, Instant::now() + Duration::from_secs(3));
        assert_eq!(store.get_f64("lvl"), Some(3.0));
        assert!(ask("5"));
        // the provider refuses the write: the next poll writes it again
        store.reject("lvl", Value::Analog(5.0));
        assert!(!store.is_queued("lvl"));
        assert!(ask("5"));
    }

    #[test]
    fn without_check_commands_go_out_alone() {
        let mut b = bench("nocheck", "<check>false</check>");
        b.wire.answer(&["7"]);
        b.poll().unwrap();
        assert_eq!(
            b.wire.take(),
            ["SOUR:V 1.0", "SOUR:OUT 1", "MEAS:SCAL:LVL?"]
        );
        assert_eq!(b.store.get_f64("lvl"), Some(7.0));
        b.wire.answer(&["OFF"]);
        assert!(b.poll().is_ok());
        assert!(b
            .log()
            .contains("`MEAS:SCAL:LVL?` answered `OFF`, not a number"));
        std::fs::remove_file(&b.log).unwrap();
    }

    #[test]
    fn a_binary_answer_that_is_not_one_is_named() {
        let mut t = Tags::default();
        let log = std::env::temp_dir().join(format!("scpi-client-bin-{}.log", std::process::id()));
        let c = parse_with(
            &format!(
                "<endpoint>lab</endpoint><check>false</check><event-logging>{}</event-logging><query><tag>p</tag><type>binary</type></query>",
                log.display()
            ),
            &mut t,
        )
        .unwrap();
        let store = Store::new(Layout {
            internal: vec![("p".into(), Value::Binary(false))],
            ..Layout::default()
        });
        let wire = Script::default();
        wire.answer(&["2", "ON"]);
        let mut link = Link::wired(wire.clone(), Duration::from_secs(1));
        let stop = CancellationToken::new();
        c.poll(&mut link, &mut [], &store, &stop).unwrap();
        c.poll(&mut link, &mut [], &store, &stop).unwrap();
        store.apply_internal();
        assert_eq!(store.get_bool("p"), Some(true));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("`MEAS:BIN:P?` answered `2`, not 1, 0, ON or OFF; `p` keeps its value"),
            "{text}"
        );
        std::fs::remove_file(&log).unwrap();
    }

    #[test]
    fn a_stopped_poll_sends_nothing_more_and_a_lost_connection_is_an_error() {
        let mut b = bench("stop1", "");
        b.stop.cancel();
        b.poll().unwrap();
        assert!(b.wire.take().is_empty());
        assert_eq!(b.sent, [None, None, None], "nothing counts as sent");
        // stopped between the sets and the queries
        let mut b = bench("stop2", "<check>false</check>");
        b.sent = vec![Some(Value::Analog(1.0)), Some(Value::Binary(true)), None];
        b.stop.cancel();
        b.poll().unwrap();
        assert!(b.wire.take().is_empty());
        let mut b = bench("stop3", "");
        let e = b.poll().unwrap_err();
        assert!(
            e.contains("closed the connection before answering `SYST:ERR?`"),
            "{e}"
        );
        std::fs::remove_file(&b.log).unwrap();
    }

    #[test]
    fn the_reconnect_pause_doubles_while_refused_and_is_1_s_again_after_a_connection() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port(); // closed again at once: refused until the test listens
        let c = parse_with(
            &format!(
                "<endpoint>127.0.0.1:{port}</endpoint><check>false</check><query><tag>x</tag></query>"
            ),
            &mut Tags::default(),
        )
        .unwrap();
        let (store, stop) = (Store::new(Layout::default()), CancellationToken::new());
        let mut pauses = vec![];
        c.reconnect(&store, &stop, |wait| {
            pauses.push(wait);
            match pauses.len() {
                // refused twice; then one connection, which the instrument closes (the query fails); then refused again
                2 => {
                    let l = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
                    std::thread::spawn(move || {
                        let (conn, _) = l.accept().unwrap();
                        drop(l); // before the connection, so the next connect is refused
                        drop(conn);
                    });
                }
                4 => stop.cancel(),
                _ => {}
            }
        });
        let s = Duration::from_secs;
        assert_eq!(pauses, [s(2), s(4), s(1), s(2)]);
    }

    #[test]
    fn an_instrument_that_stays_away_is_logged_once_and_retried_until_stop() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port(); // closed again at once: every connect is refused
        let log = std::env::temp_dir().join(format!("scpi-client-away-{}.log", std::process::id()));
        let c = parse_with(
            &format!(
                "<endpoint>127.0.0.1:{port}</endpoint><timeout>100</timeout><event-logging>{}</event-logging>",
                log.display()
            ),
            &mut Tags::default(),
        )
        .unwrap();
        let (store, stop) = (Store::new(Layout::default()), CancellationToken::new());
        let s = stop.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2500)); // attempts at 0 s and 2 s (the pause doubles from 1 s)
            s.cancel();
        });
        let t = Instant::now();
        c.serve(&store, &stop);
        assert!(
            t.elapsed() < Duration::from_millis(3500),
            "stop ends the pause"
        );
        stopper.join().unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.matches("; retrying").count(), 1, "{text}");
        assert!(!text.contains(",connected"), "{text}");
        std::fs::remove_file(&log).unwrap();
    }
}
