//! The SCPI instrument without a socket: what one connection's bytes do to the status registers, the error queue and the store, and
//! what is answered. `Instrument` is the shared, read-only part (identity, tag routes), `Session` one connection (line buffer,
//! SESR / SESE / SRE, error queue). The text of messages is `crate::scpi::syntax`; the mapping of tags to paths and the status
//! model are documented in docs/field-device-scpi.md.

use crate::fd::proto::point::{Num, Point, Refusal};
use crate::fd::store::Store;
use crate::hint::show;
use crate::model::{Kind, Value};
use crate::scpi::syntax::{self, Param};
use std::collections::VecDeque;

/// The error queue holds this many entries; when full the last one becomes -350 and later errors are dropped (SCPI-99 21.8).
pub const ERR_CAP: usize = 16;
/// Response bytes one `feed` may hold; more than that and the unsent ones are discarded with -410.
pub const OUT_CAP: usize = 256 * 1024;

// Standard Event Status Register bits (IEEE 488.2 11.5.1) and the status byte bits this instrument has.
const QYE: u8 = 4;
const DDE: u8 = 8;
const EXE: u8 = 16;
const CME: u8 = 32;
const OPC: u8 = 1;
const EAV: u8 = 4;
const MAV: u8 = 16;
const ESB: u8 = 32;
const MSS: u8 = 64;

/// One tag the instrument serves: read at `read` (`MEASure:...?`), set and read back at `write` when the tag is writable.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub point: Point,
    pub read: Vec<String>,
    pub write: Option<Vec<String>>,
    /// What `DEF` means for a write.
    pub default: Option<f64>,
}

pub struct Instrument {
    pub idn: String,
    pub routes: Vec<Route>,
    /// Longest line (without its terminator) a client may send.
    pub max_line: usize,
}

/// An SCPI error: its number and what to add to the standard text.
#[derive(Debug, PartialEq)]
pub struct Fault(pub i32, pub String);

fn fault(code: i32) -> Fault {
    Fault(code, String::new())
}

type Reply = Result<Option<String>, Fault>;

/// The standard text of an error number.
pub fn text(code: i32) -> &'static str {
    match code {
        -100 => "Command error",
        -102 => "Syntax error",
        -104 => "Data type error",
        -108 => "Parameter not allowed",
        -109 => "Missing parameter",
        -113 => "Undefined header",
        -114 => "Header suffix out of range",
        -121 => "Invalid character in number",
        -123 => "Exponent too large",
        -131 => "Invalid suffix",
        -200 => "Execution error",
        -221 => "Settings conflict",
        -222 => "Data out of range",
        -224 => "Illegal parameter value",
        -350 => "Queue overflow",
        -363 => "Input buffer overrun",
        -410 => "Query INTERRUPTED",
        -420 => "Query UNTERMINATED",
        _ => "Unknown error",
    }
}

/// The SESR bit an error sets: command (-1xx), execution (-2xx), device-specific (-3xx) or query (-4xx).
fn class_bit(code: i32) -> u8 {
    match code {
        -199..=-100 => CME,
        -299..=-200 => EXE,
        -399..=-300 => DDE,
        -499..=-400 => QYE,
        _ => 0,
    }
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A value as a response: NR1 / NR2 for numbers, `1` / `0` for a binary, the SCPI sentinels 9.91E37 (not a number) and
/// +-9.9E37 (infinity) where there is no number. ponytail: never an exponent, so a huge value prints all its digits.
fn value_text(v: Value) -> String {
    match v {
        Value::Binary(b) => u8::from(b).to_string(),
        Value::Analog(x) if x.is_nan() => "9.91E+37".into(),
        Value::Analog(x) if x.is_infinite() => format!("{}9.9E+37", if x < 0.0 { "-" } else { "" }),
        Value::Analog(x) => x.to_string(),
    }
}

// ---- matching headers against command patterns ----------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone, Copy)]
enum Match {
    No,
    BadSuffix,
    Yes,
}

/// Does the upper-cased mnemonic `t` name `pattern`? The pattern's leading upper-case letters are its short form, all of it the
/// long form (`MEASure`: `MEAS` or `MEASURE`, never `MEASU`); a numeric suffix is allowed but only 1 exists.
fn keyword(pattern: &str, t: &str) -> Match {
    let long = pattern;
    let short = match pattern.find(|c: char| c.is_ascii_lowercase()) {
        Some(i) if i > 0 => &pattern[..i],
        _ => pattern,
    };
    let is = |m: &str| m.eq_ignore_ascii_case(short) || m.eq_ignore_ascii_case(long);
    let base = t.trim_end_matches(|c: char| c.is_ascii_digit());
    match (is(t), is(base), &t[base.len()..]) {
        (true, _, _) => Match::Yes,
        (_, true, suffix) if suffix.parse::<u32>() == Ok(1) => Match::Yes,
        (_, true, _) => Match::BadSuffix,
        _ => Match::No,
    }
}

/// Whether `path` names `pattern`, suffixes and all.
pub fn path_matches<P: AsRef<str>>(pattern: &[P], path: &[String]) -> bool {
    path_match(pattern, path) != Match::No
}

/// All of `path` against `pattern`: `No` if any mnemonic or the length differs, else `BadSuffix` if any suffix is not 1.
fn path_match<P: AsRef<str>>(pattern: &[P], path: &[String]) -> Match {
    if pattern.len() != path.len() {
        return Match::No;
    }
    let ms = pattern
        .iter()
        .zip(path)
        .map(|(p, t)| keyword(p.as_ref(), t));
    ms.fold(Match::Yes, |acc, m| match (acc, m) {
        (Match::No, _) | (_, Match::No) => Match::No,
        (Match::BadSuffix, _) | (_, Match::BadSuffix) => Match::BadSuffix,
        _ => Match::Yes,
    })
}

// ---- the fixed commands --------------------------------------------------------------------------------------------------

struct Env<'a> {
    ins: &'a Instrument,
    store: &'a Store,
    /// Some earlier unit of this message has produced a response.
    mav: bool,
}

type Handler = fn(&mut Session, &Env, &[Param]) -> Reply;

struct Entry {
    common: bool,
    query: bool,
    path: &'static [&'static str],
    /// Fewest and most parameters.
    args: (usize, usize),
    run: Handler,
}

const fn common(
    name: &'static [&'static str],
    query: bool,
    args: (usize, usize),
    run: Handler,
) -> Entry {
    Entry {
        common: true,
        query,
        path: name,
        args,
        run,
    }
}

const fn system(path: &'static [&'static str], run: Handler) -> Entry {
    Entry {
        common: false,
        query: true,
        path,
        args: (0, 0),
        run,
    }
}

static FIXED: &[Entry] = &[
    common(&["IDN"], true, (0, 0), |_, e, _| {
        Ok(Some(e.ins.idn.clone()))
    }),
    common(&["RST"], false, (0, 0), |_, _, _| Ok(None)),
    common(&["CLS"], false, (0, 0), |s, _, _| {
        s.esr = 0;
        s.errors.clear();
        Ok(None)
    }),
    common(&["ESE"], false, (1, 1), |s, _, a| {
        byte(a).map(|b| {
            s.ese = b;
            None
        })
    }),
    common(&["ESE"], true, (0, 0), |s, _, _| {
        Ok(Some(s.ese.to_string()))
    }),
    common(&["ESR"], true, (0, 0), |s, _, _| {
        Ok(Some(std::mem::take(&mut s.esr).to_string()))
    }),
    common(&["OPC"], false, (0, 0), |s, _, _| {
        s.esr |= OPC;
        Ok(None)
    }),
    common(&["OPC"], true, (0, 0), |_, _, _| Ok(Some("1".into()))),
    common(&["SRE"], false, (1, 1), |s, _, a| {
        byte(a).map(|b| {
            s.sre = b & !MSS;
            None
        })
    }),
    common(&["SRE"], true, (0, 0), |s, _, _| {
        Ok(Some(s.sre.to_string()))
    }),
    common(&["STB"], true, (0, 0), |s, e, _| {
        Ok(Some(s.stb(e.mav).to_string()))
    }),
    common(&["TST"], true, (0, 0), |_, _, _| Ok(Some("0".into()))),
    common(&["WAI"], false, (0, 0), |_, _, _| Ok(None)),
    system(&["SYSTem", "ERRor"], |s, _, _| Ok(Some(s.next_error()))),
    system(&["SYSTem", "ERRor", "NEXT"], |s, _, _| {
        Ok(Some(s.next_error()))
    }),
    system(&["SYSTem", "ERRor", "COUNt"], |s, _, _| {
        Ok(Some(s.errors.len().to_string()))
    }),
    system(&["SYSTem", "VERSion"], |_, _, _| Ok(Some("1999.0".into()))),
];

/// `*ESE` / `*SRE` take a whole number 0 to 255 (a fraction is rounded, as SCPI rounds numeric to integer).
fn byte(a: &[Param]) -> Result<u8, Fault> {
    match &a[0] {
        Param::Num(x) if *x > -0.5 && *x < 255.5 => Ok(x.round() as u8),
        Param::Num(_) => Err(fault(-222)),
        _ => Err(fault(-104)),
    }
}

// ---- one connection ------------------------------------------------------------------------------------------------------

pub struct Session {
    buf: Vec<u8>,
    /// Throwing away the rest of an overlong line.
    skipping: bool,
    esr: u8,
    ese: u8,
    sre: u8,
    errors: VecDeque<(i32, String)>,
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session {
            buf: vec![],
            skipping: false,
            esr: 0,
            ese: 0,
            sre: 0,
            errors: VecDeque::new(),
        }
    }

    /// The status byte. `mav`: a response of this message is waiting. Bit 6 is MSS; bits 3 and 7 (questionable, operation) have no
    /// register behind them here.
    fn stb(&self, mav: bool) -> u8 {
        let mut b = 0;
        if !self.errors.is_empty() {
            b |= EAV;
        }
        if mav {
            b |= MAV;
        }
        if self.esr & self.ese != 0 {
            b |= ESB;
        }
        if b & self.sre != 0 {
            b |= MSS;
        }
        b
    }

    /// Queue an error and set its SESR class bit (also when the queue is full: the entry is lost, the bit is not).
    pub fn push_error(&mut self, code: i32, detail: &str) {
        self.esr |= class_bit(code);
        let msg = match detail.is_empty() {
            true => text(code).to_string(),
            false => format!("{};{detail}", text(code)),
        };
        if self.errors.len() < ERR_CAP {
            self.errors.push_back((code, msg));
        } else if let Some(last) = self.errors.back_mut().filter(|l| l.0 != -350) {
            *last = (-350, text(-350).into());
            self.esr |= class_bit(-350);
        }
    }

    fn next_error(&mut self) -> String {
        match self.errors.pop_front() {
            Some((code, msg)) => format!("{code},{}", quoted(&msg)),
            None => "0,\"No error\"".into(),
        }
    }

    /// Bytes from the client; the responses to every complete line in them.
    pub fn feed(&mut self, ins: &Instrument, store: &Store, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        for chunk in bytes.split_inclusive(|&b| b == b'\n') {
            if let Some(line) = self.take_line(ins.max_line, chunk) {
                self.message(ins, store, &line, &mut out);
            }
        }
        out
    }

    /// The client closed its sending side: a last line without a terminator is a message too, and answered (a query in it is
    /// -420, the response having no terminator to answer).
    pub fn eof(&mut self, ins: &Instrument, store: &Store) -> Vec<u8> {
        let mut out = vec![];
        if !std::mem::take(&mut self.skipping) && !self.buf.is_empty() {
            let line = line_text(&std::mem::take(&mut self.buf));
            self.message(ins, store, &line, &mut out);
            if syntax::is_query(&line) {
                self.push_error(-420, "");
            }
        }
        out
    }

    /// Add `chunk` (ending in a newline or not) to the line being collected. An overlong line is -363 and thrown away up to its
    /// newline; the buffer never holds more than `max` bytes.
    fn take_line(&mut self, max: usize, chunk: &[u8]) -> Option<String> {
        let (body, ended) = match chunk.strip_suffix(b"\n") {
            Some(b) => (b, true),
            None => (chunk, false),
        };
        if self.skipping {
            self.skipping = !ended;
            return None;
        }
        if self.buf.len() + body.len() > max {
            self.buf.clear();
            self.skipping = !ended;
            self.push_error(-363, "");
            return None;
        }
        self.buf.extend_from_slice(body);
        ended.then(|| line_text(&std::mem::take(&mut self.buf)))
    }

    /// One program message: its units run in order, the responses of its queries join with `;`.
    fn message(&mut self, ins: &Instrument, store: &Store, line: &str, out: &mut Vec<u8>) {
        if line.trim().is_empty() {
            return;
        }
        let (mut replies, mut parent): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for unit in syntax::split(line, ';') {
            let env = Env {
                ins,
                store,
                mav: !replies.is_empty(),
            };
            match self.unit(&env, unit, &mut parent) {
                Ok(Some(r)) => replies.push(r),
                Ok(None) => {}
                Err(Fault(code, detail)) => self.push_error(code, &detail),
            }
        }
        if !replies.is_empty() {
            let reply = replies.join(";") + "\n";
            if out.len() + reply.len() > OUT_CAP {
                out.clear();
                self.push_error(-410, "");
            }
            out.extend_from_slice(reply.as_bytes());
        }
    }

    /// One program message unit. `parent` is the path the previous unit's header sat under: a header without a leading `:` is
    /// relative to it, a common command (`*...`) neither uses nor changes it.
    fn unit(&mut self, env: &Env, unit: &str, parent: &mut Vec<String>) -> Reply {
        let (h, rest) = syntax::header_rest(unit);
        let header = syntax::header(h).ok_or_else(|| Fault(-102, show(h)))?;
        let params = syntax::params(rest).map_err(fault)?;
        let mut path = header.path.clone();
        if !header.common && !header.rooted {
            path.splice(0..0, parent.iter().cloned());
        }
        if !header.common {
            *parent = path[..path.len() - 1].to_vec();
        }
        let found = FIXED
            .iter()
            .map(|e| (e, entry_match(e, &header, &path)))
            .find(|(_, m)| *m != Match::No);
        match found {
            Some((_, Match::BadSuffix)) => Err(Fault(-114, show(h))),
            Some((e, _)) => run_fixed(e, self, env, &params),
            None => self.point(env, &header, &path, &params, h),
        }
    }

    /// A tag's own path: `MEASure:...?` reads it, `SOURce:...` sets it, `SOURce:...?` reads the setting back.
    fn point(
        &mut self,
        env: &Env,
        header: &syntax::Header,
        path: &[String],
        params: &[Param],
        h: &str,
    ) -> Reply {
        let mut suffix = false;
        for r in &env.ins.routes {
            let on_read = header.query.then(|| path_match(&r.read, path));
            let on_write = r.write.as_ref().map(|w| path_match(w, path));
            match (on_read, on_write) {
                (Some(Match::Yes), _) => return read(env, r, params),
                (_, Some(Match::Yes)) => return setting(env, r, header.query, params),
                (Some(Match::BadSuffix), _) | (_, Some(Match::BadSuffix)) => suffix = true,
                _ => {}
            }
        }
        Err(Fault(if suffix { -114 } else { -113 }, show(h)))
    }
}

fn line_text(bytes: &[u8]) -> String {
    let line = String::from_utf8_lossy(bytes);
    line.strip_suffix('\r').unwrap_or(&line).to_string()
}

/// A common command has no short form and no numeric suffix: `*IDN1?` is simply not a command.
fn entry_match(e: &Entry, h: &syntax::Header, path: &[String]) -> Match {
    let exact = e.path.len() == path.len() && e.path.iter().zip(path).all(|(a, b)| a == b);
    match (e.common == h.common && e.query == h.query, e.common) {
        (false, _) => Match::No,
        (true, true) if exact => Match::Yes,
        (true, true) => Match::No,
        (true, false) => path_match(e.path, path),
    }
}

fn run_fixed(e: &Entry, s: &mut Session, env: &Env, params: &[Param]) -> Reply {
    match params.len() {
        n if n < e.args.0 => Err(fault(-109)),
        n if n > e.args.1 => Err(fault(-108)),
        _ => (e.run)(s, env, params),
    }
}

// ---- tags ----------------------------------------------------------------------------------------------------------------

fn current(env: &Env, r: &Route) -> Result<String, Fault> {
    match env.store.get(&r.point.tag) {
        Some(v) => Ok(value_text(v)),
        None => Err(Fault(-200, show(&r.point.tag))),
    }
}

/// `MEASure:...?`: the live value, no parameters.
fn read(env: &Env, r: &Route, params: &[Param]) -> Reply {
    match params.is_empty() {
        true => current(env, r).map(Some),
        false => Err(fault(-108)),
    }
}

/// The path of a writable tag: a command sets it, a query reads it back (or its `MIN` / `MAX` / `DEF`).
fn setting(env: &Env, r: &Route, query: bool, params: &[Param]) -> Reply {
    match (query, params) {
        (true, []) => current(env, r).map(Some),
        (true, [p]) => limit(r, p).map(|x| Some(value_text(Value::Analog(x)))),
        (_, []) => Err(fault(-109)),
        (false, [p]) => set(env, r, p).map(|()| None),
        _ => Err(fault(-108)),
    }
}

/// `MIN`, `MAX` or `DEF` of a tag: its configured limit or default. -221 when it has none, -224 for any other word.
fn limit(r: &Route, p: &Param) -> Result<f64, Fault> {
    let Param::Word(w) = p else {
        return Err(fault(-104));
    };
    let x = match w.as_str() {
        "MIN" => r.point.min,
        "MAX" => r.point.max,
        "DEF" => r.default,
        _ => return Err(Fault(-224, show(w))),
    };
    x.ok_or_else(|| {
        Fault(
            -221,
            format!("{w} of {} is not configured", show(&r.point.tag)),
        )
    })
}

fn num(r: &Route, p: &Param) -> Result<Num, Fault> {
    match p {
        Param::Num(x) => Ok(Num::Number(*x)),
        Param::Word(w) if w == "ON" => Ok(Num::Bool(true)),
        Param::Word(w) if w == "OFF" => Ok(Num::Bool(false)),
        Param::Word(_) => limit(r, p).map(Num::Number),
        Param::Str(_) => Err(fault(-104)),
    }
}

/// A set command: the value is decoded, checked the way every protocol checks a write (`Point::check`) and queued for the
/// provider; the store changes when the provider acknowledges, as for Modbus.
fn set(env: &Env, r: &Route, p: &Param) -> Result<(), Fault> {
    let value = r.point.check(num(r, p)?).map_err(|e| refusal(r, e))?;
    env.store
        .write(&r.point.tag, value)
        .map_err(|e| Fault(-200, format!("{} {e:?}", show(&r.point.tag))))
}

fn refusal(r: &Route, why: Refusal) -> Fault {
    let detail = format!("{}: {why}", show(&r.point.tag));
    match (why, r.point.kind) {
        (Refusal::OutOfRange(_), _) => Fault(-222, detail),
        (Refusal::WrongType, Kind::Analog) => Fault(-104, detail),
        (Refusal::WrongType | Refusal::NotFinite, _) => Fault(-224, detail),
        (Refusal::NotWritable, _) => Fault(-200, detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::store::Layout;

    fn route(tag: &str, kind: Kind, writable: bool, min: Option<f64>, max: Option<f64>) -> Route {
        let name = tag.to_ascii_uppercase();
        let k = if kind == Kind::Analog {
            "ANALog"
        } else {
            "BINary"
        };
        Route {
            point: Point {
                tag: tag.into(),
                kind,
                writable,
                min,
                max,
            },
            read: vec!["MEASure".into(), k.into(), name.clone()],
            write: writable.then(|| vec!["SOURce".into(), name]),
            default: None,
        }
    }

    struct Rig {
        ins: Instrument,
        store: Store,
        s: Session,
    }

    fn rig() -> Rig {
        let mut lvl = route("lvl", Kind::Analog, true, Some(0.0), Some(10.0));
        lvl.default = Some(5.0);
        let routes = vec![
            lvl,
            route("ro", Kind::Analog, false, None, None),
            route("sw", Kind::Binary, true, None, None),
            route("free", Kind::Analog, true, None, None),
            route("bus1", Kind::Analog, false, None, None),
        ];
        let store = Store::new(Layout {
            points: vec![],
            external: vec![],
            internal: vec![
                ("lvl".into(), Value::Analog(2.5)),
                ("ro".into(), Value::Analog(f64::NAN)),
                ("sw".into(), Value::Binary(true)),
                ("free".into(), Value::Analog(-1e3)),
                ("bus1".into(), Value::Analog(f64::INFINITY)),
            ],
        });
        let ins = Instrument {
            idn: "ferrofire,rtu,0,1".into(),
            routes,
            max_line: 64,
        };
        Rig {
            ins,
            store,
            s: Session::new(),
        }
    }

    impl Rig {
        fn raw(&mut self, bytes: &[u8]) -> String {
            String::from_utf8(self.s.feed(&self.ins, &self.store, bytes)).unwrap()
        }
        /// The response to one line (with its newline), "" when there is none.
        fn ask(&mut self, line: &str) -> String {
            self.raw(format!("{line}\n").as_bytes())
        }
        fn errors(&mut self) -> Vec<String> {
            let mut out = vec![];
            loop {
                let e = self.ask("SYST:ERR?");
                if e == "0,\"No error\"\n" {
                    return out;
                }
                out.push(e.trim_end().to_string());
            }
        }
    }

    #[test]
    fn identity_and_the_trivial_common_commands() {
        let mut r = rig();
        assert_eq!(r.ask("*IDN?"), "ferrofire,rtu,0,1\n");
        assert_eq!(r.ask("*idn?"), "ferrofire,rtu,0,1\n");
        assert_eq!(r.ask("*TST?"), "0\n");
        assert_eq!(r.ask("*OPC?"), "1\n");
        assert_eq!(r.ask("SYST:VERS?"), "1999.0\n");
        assert_eq!(r.ask("*RST;*WAI;*CLS"), "");
        assert_eq!(r.ask(""), "");
        assert_eq!(r.ask("  \r"), "");
        assert!(r.errors().is_empty());
    }

    #[test]
    fn a_common_command_in_the_wrong_form_or_with_the_wrong_arguments_is_an_error() {
        let mut r = rig();
        for (line, code) in [
            ("*IDN", -113),
            ("*CLS?", -113),
            ("*IDN1?", -113),
            ("*IDN? 1", -108),
            ("*ESE", -109),
            ("*ESE 1,2", -108),
            ("*ESE?  5", -108),
            ("*ESE 256", -222),
            ("*ESE -1", -222),
            ("*ESE abc", -104),
            ("*SRE \"1\"", -104),
            ("*FOO?", -113),
        ] {
            r.ask(line);
            assert_eq!(r.errors().len(), 1, "{line}");
            r.ask(line);
            assert!(r.errors()[0].starts_with(&format!("{code},")), "{line}");
        }
    }

    #[test]
    fn the_standard_event_status_register_collects_error_classes_and_clears_on_read() {
        let mut r = rig();
        for (line, bit) in [
            ("*FOO?", 32),
            ("*ESE 256", 16),
            ("SYST:ERR? 1", 32),
            ("*OPC", 1),
        ] {
            r.ask("*CLS");
            r.ask(line);
            assert_eq!(r.ask("*ESR?"), format!("{bit}\n"), "{line}");
            assert_eq!(r.ask("*ESR?"), "0\n", "{line}: cleared by the read");
        }
        let mut s = Session::new();
        for (code, bit) in [
            (-100, 32),
            (-199, 32),
            (-200, 16),
            (-299, 16),
            (-300, 8),
            (-350, 8),
            (-399, 8),
            (-400, 4),
            (-499, 4),
            (0, 0),
            (-99, 0),
            (-500, 0),
            (1, 0),
        ] {
            s.esr = 0;
            s.push_error(code, "");
            assert_eq!(s.esr, bit, "{code}");
        }
    }

    #[test]
    fn ese_and_sre_are_settable_readable_and_rounded() {
        let mut r = rig();
        r.ask("*ESE 36;*SRE 255");
        assert_eq!(r.ask("*ESE?"), "36\n");
        assert_eq!(
            r.ask("*SRE?"),
            "191\n",
            "bit 6 of the service request enable is never set"
        );
        r.ask("*ESE 2.6");
        assert_eq!(r.ask("*ESE?"), "3\n");
        r.ask("*ESE 255.4");
        assert_eq!(r.ask("*ESE?"), "255\n");
        r.ask("*ESE -0.4");
        assert_eq!(r.ask("*ESE?"), "0\n");
        assert!(r.errors().is_empty());
        r.ask("*ESE 255.5");
        r.ask("*ESE -0.5");
        assert_eq!(r.errors().len(), 2);
        r.ask("*CLS");
        r.ask("*ESE 7");
        r.ask("*CLS");
        assert_eq!(r.ask("*ESE?"), "7\n", "*CLS leaves the enable registers");
    }

    #[test]
    fn the_status_byte_has_the_queue_message_event_and_summary_bits() {
        let mut r = rig();
        assert_eq!(r.ask("*STB?"), "0\n");
        r.ask("*FOO");
        assert_eq!(r.ask("*STB?"), "4\n", "error queue not empty");
        assert_eq!(
            r.ask("*IDN?;*STB?"),
            "ferrofire,rtu,0,1;20\n",
            "a response is waiting: MAV"
        );
        assert_eq!(r.ask("*CLS;*STB?"), "0\n");
        r.ask("*ESE 32");
        r.ask("*FOO");
        r.ask("SYST:ERR?");
        assert_eq!(
            r.ask("*STB?"),
            "32\n",
            "ESB: an enabled event bit is set, the queue is empty"
        );
        r.ask("*SRE 32");
        assert_eq!(r.ask("*STB?"), "96\n", "MSS: a summary bit is enabled");
        r.ask("*SRE 16");
        assert_eq!(r.ask("*STB?"), "32\n", "MAV not enabled, no MSS");
        assert_eq!(r.ask("*IDN?;*STB?"), "ferrofire,rtu,0,1;112\n");
        r.ask("*SRE 4");
        r.ask("*FOO");
        assert_eq!(r.ask("*STB?"), "100\n", "EAV enabled: MSS");
    }

    #[test]
    fn the_error_queue_is_first_in_first_out_with_text_and_detail() {
        let mut r = rig();
        r.ask("*FOO?;*ESE;A");
        assert_eq!(r.ask("SYST:ERR:COUN?"), "3\n");
        assert_eq!(r.ask("SYST:ERR?"), "-113,\"Undefined header;*FOO?\"\n");
        assert_eq!(r.ask("SYSTem:ERRor:NEXT?"), "-109,\"Missing parameter\"\n");
        assert_eq!(r.ask("SYST:ERR:COUN?"), "1\n");
        assert_eq!(r.ask(":SYST:ERR?"), "-113,\"Undefined header;A\"\n");
        assert_eq!(r.ask("SYST:ERR?"), "0,\"No error\"\n");
        assert_eq!(r.ask("SYST:ERR:COUN?"), "0\n");
        r.ask("\"");
        assert_eq!(r.ask("SYST:ERR?"), "-102,\"Syntax error;\"\"\"\n");
        r.ask("A\"B?");
        assert_eq!(r.ask("SYST:ERR?"), "-102,\"Syntax error;A\"\"B?\"\n");
    }

    #[test]
    fn a_full_queue_keeps_the_oldest_and_ends_with_queue_overflow() {
        let mut r = rig();
        for i in 0..ERR_CAP + 5 {
            r.ask(&format!("*X{i}"));
        }
        assert_eq!(r.ask("SYST:ERR:COUN?"), "16\n");
        let e = r.errors();
        assert_eq!(e.len(), 16);
        assert!(e[0].contains("*X0"), "{}", e[0]);
        assert!(e[14].contains("*X14"), "{}", e[14]);
        assert_eq!(e[15], "-350,\"Queue overflow\"");
        r.ask("*FOO");
        assert_eq!(
            r.ask("SYST:ERR:COUN?"),
            "1\n",
            "room again after the queue was drained"
        );
        let mut s = Session::new();
        for _ in 0..ERR_CAP {
            s.push_error(-350, "");
        }
        s.push_error(-102, "");
        assert_eq!(s.errors.len(), ERR_CAP);
        assert_eq!(s.errors.back().map(|e| e.0), Some(-350));
    }

    #[test]
    fn mnemonics_have_a_short_and_a_long_form_in_any_case_and_a_suffix_of_one() {
        let mut r = rig();
        for h in [
            "MEAS:ANAL:LVL?",
            "measure:analog:lvl?",
            "MEASURE:ANAL:Lvl?",
            ":MEAS1:ANAL1:LVL1?",
            "Meas:Anal:lvl?",
        ] {
            assert_eq!(r.ask(h), "2.5\n", "{h}");
        }
        assert!(r.errors().is_empty());
        for h in [
            "MEASU:ANAL:LVL?",
            "MEA:ANAL:LVL?",
            "MEAS:ANALOGUE:LVL?",
            "MEAS:LVL?",
            "MEAS:ANAL:LVL:X?",
            "MEAS:ANAL:BIN:LVL?",
            "LVL?",
        ] {
            assert_eq!(r.ask(h), "", "{h}");
            assert!(r.errors()[0].starts_with("-113,"), "{h}");
        }
        for h in [
            "MEAS2:ANAL:LVL?",
            "MEAS:ANAL2:LVL?",
            "MEAS:ANAL:LVL0?",
            "SOUR2:LVL 1",
            "SYST2:ERR?",
            "SYST:ERR2?",
            "SYST:ERR:NEXT3?",
        ] {
            r.ask(h);
            let e = r.errors();
            assert!(e[0].starts_with("-114,"), "{h}: {e:?}");
        }
        assert_eq!(
            r.ask("MEAS:ANAL:BUS1?"),
            "9.9E+37\n",
            "a tag that ends in a digit is its own mnemonic"
        );
        r.ask("MEAS:ANAL:BUS2?");
        assert!(
            r.errors()[0].starts_with("-113,"),
            "BUS2 is not BUS1 with a suffix"
        );
    }

    #[test]
    fn a_header_without_a_colon_is_relative_to_the_previous_one() {
        let mut r = rig();
        assert_eq!(r.ask("MEAS:ANAL:LVL?;FREE?;RO?"), "2.5;-1000;9.91E+37\n");
        assert_eq!(
            r.ask("MEAS:ANAL:LVL?;*IDN?;FREE?"),
            "2.5;ferrofire,rtu,0,1;-1000\n",
            "a common command leaves the path alone"
        );
        assert_eq!(
            r.ask("MEAS:ANAL:LVL?;:SYST:VERS?;ERR:COUN?"),
            "2.5;1999.0;0\n"
        );
        assert_eq!(r.ask("SYST:ERR?;VERS?"), "0,\"No error\";1999.0\n");
        assert_eq!(r.ask("VERS?"), "", "the first unit has no previous path");
        assert!(r.errors()[0].starts_with("-113,"));
        assert_eq!(r.ask("MEAS:ANAL:LVL?;:FREE?"), "2.5\n");
        assert!(
            r.errors()[0].starts_with("-113,"),
            "a leading colon restarts at the root"
        );
    }

    #[test]
    fn queries_answer_in_order_and_join_with_semicolons_a_failed_one_has_no_answer() {
        let mut r = rig();
        assert_eq!(r.ask("*IDN?;*OPC?;*TST?"), "ferrofire,rtu,0,1;1;0\n");
        assert_eq!(r.ask("*IDN?;*FOO?;*OPC?"), "ferrofire,rtu,0,1;1\n");
        assert_eq!(r.ask("*CLS;*FOO?"), "");
        assert_eq!(
            r.ask("*IDN?;;*OPC?"),
            "ferrofire,rtu,0,1;1\n",
            "an empty unit is a syntax error, the rest still runs"
        );
        assert_eq!(r.errors().len(), 2);
        assert_eq!(r.ask("*IDN? ; *OPC?"), "ferrofire,rtu,0,1;1\n");
        assert_eq!(r.ask("*ESE 3; *ESE?"), "3\n");
    }

    #[test]
    fn values_are_numbers_flags_and_the_scpi_sentinels() {
        let mut r = rig();
        assert_eq!(r.ask("MEAS:ANAL:LVL?"), "2.5\n");
        assert_eq!(r.ask("MEAS:BIN:SW?"), "1\n");
        assert_eq!(r.ask("MEAS:ANAL:FREE?"), "-1000\n");
        assert_eq!(r.ask("MEAS:ANAL:RO?"), "9.91E+37\n", "not a number");
        assert_eq!(r.ask("MEAS:ANAL:BUS1?"), "9.9E+37\n", "infinity");
        assert_eq!(value_text(Value::Analog(f64::NEG_INFINITY)), "-9.9E+37");
        assert_eq!(value_text(Value::Binary(false)), "0");
        assert_eq!(value_text(Value::Analog(-0.125)), "-0.125");
        r.ask("MEAS:ANAL:LVL? 5");
        r.ask("MEAS:ANAL:LVL 5");
        r.ask("MEAS:ANAL:LVL");
        let e = r.errors();
        assert!(
            e[0].starts_with("-108,") && e[1].starts_with("-113,") && e[2].starts_with("-113,"),
            "{e:?}"
        );
    }

    #[test]
    fn a_set_command_goes_to_the_write_queue_and_not_into_the_store() {
        let mut r = rig();
        assert_eq!(r.ask("SOUR:LVL 7.5"), "");
        assert!(r.store.is_queued("lvl"));
        assert_eq!(r.store.get_f64("lvl"), Some(2.5));
        r.ask("SOURce:SW ON");
        r.ask("sour:free -5e2");
        assert!(r.store.is_queued("sw") && r.store.is_queued("free"));
        assert!(r.errors().is_empty());
        assert_eq!(
            r.ask("SOUR:LVL?"),
            "2.5\n",
            "the setting reads back what the store holds"
        );
        r.store.apply_internal();
        assert_eq!(r.store.get_f64("lvl"), Some(7.5));
        assert_eq!(r.store.get_bool("sw"), Some(true));
        assert_eq!(r.store.get_f64("free"), Some(-500.0));
        r.ask("SOUR:SW 0;:SOUR:SW 1;SW OFF");
        r.store.apply_internal();
        assert_eq!(
            r.store.get_bool("sw"),
            Some(false),
            "the last of several sets wins"
        );
    }

    #[test]
    fn a_set_command_that_breaks_a_rule_is_an_error_and_changes_nothing() {
        let mut r = rig();
        for (line, code) in [
            ("SOUR:LVL 10.5", -222),
            ("SOUR:LVL -0.1", -222),
            ("SOUR:LVL 1E9", -222),
            ("SOUR:SW 2", -224),
            ("SOUR:SW 0.5", -224),
            ("SOUR:SW MAX", -221),
            ("SOUR:SW foo", -224),
            ("SOUR:LVL ON", -104),
            ("SOUR:LVL \"5\"", -104),
            ("SOUR:SW \"1\"", -104),
            ("SOUR:LVL foo", -224),
            ("SOUR:LVL", -109),
            ("SOUR:LVL 1,2", -108),
            ("SOUR:LVL 5V", -131),
            ("SOUR:LVL 1.2.3", -121),
            ("SOUR:LVL 1E999", -123),
            ("SOUR:LVL #H10", -104),
            ("SOUR:RO 1", -113),
            ("SOUR:NOPE 1", -113),
            ("SOUR:LVL 1,", -102),
            ("SOUR:LVL \"x", -102),
            ("SOUR:FREE MIN", -221),
            ("SOUR:FREE DEF", -221),
        ] {
            assert_eq!(r.ask(line), "", "{line}");
            let e = r.errors();
            assert_eq!(e.len(), 1, "{line}: {e:?}");
            assert!(e[0].starts_with(&format!("{code},")), "{line}: {e:?}");
        }
        assert!(
            !r.store.is_queued("lvl") && !r.store.is_queued("sw") && !r.store.is_queued("free")
        );
        r.ask("SOUR:LVL 11");
        assert_eq!(
            r.errors(),
            ["-222,\"Data out of range;lvl: 11 is outside the limits\""]
        );
    }

    #[test]
    fn min_max_and_def_name_the_configured_limits() {
        let mut r = rig();
        assert_eq!(r.ask("SOUR:LVL? MIN"), "0\n");
        assert_eq!(r.ask("SOUR:LVL? max"), "10\n");
        assert_eq!(r.ask("SOUR:LVL? DEF"), "5\n");
        r.ask("SOUR:LVL MAX");
        r.ask("SOUR:LVL MIN");
        assert!(r.store.is_queued("lvl"));
        r.store.apply_internal();
        assert_eq!(r.store.get_f64("lvl"), Some(0.0), "the last set was MIN");
        r.ask("SOUR:LVL DEF");
        r.store.apply_internal();
        assert_eq!(r.store.get_f64("lvl"), Some(5.0));
        assert!(r.errors().is_empty());
        for (line, code) in [
            ("SOUR:LVL? FOO", -224),
            ("SOUR:LVL? 1", -104),
            ("SOUR:LVL? MIN,MAX", -108),
            ("SOUR:FREE? MAX", -221),
        ] {
            assert_eq!(r.ask(line), "", "{line}");
            assert!(r.errors()[0].starts_with(&format!("{code},")), "{line}");
        }
    }

    #[test]
    fn refusals_have_their_scpi_numbers_by_the_kind_of_tag() {
        let r = rig();
        let (a, b) = (&r.ins.routes[0], &r.ins.routes[2]);
        let code = |r: &Route, w: Refusal| refusal(r, w).0;
        assert_eq!(code(a, Refusal::OutOfRange(1.0)), -222);
        assert_eq!(code(b, Refusal::OutOfRange(1.0)), -222);
        assert_eq!(code(a, Refusal::WrongType), -104);
        assert_eq!(code(b, Refusal::WrongType), -224);
        assert_eq!(code(a, Refusal::NotFinite), -224);
        assert_eq!(code(a, Refusal::NotWritable), -200);
    }

    #[test]
    fn a_write_the_store_refuses_is_an_execution_error() {
        let mut r = rig();
        r.ins
            .routes
            .push(route("gone", Kind::Analog, true, None, None));
        r.ask("SOUR:GONE 1");
        r.ask("MEAS:ANAL:GONE?");
        let e = r.errors();
        assert!(
            e[0].starts_with("-200,") && e[1].starts_with("-200,"),
            "{e:?}"
        );
        r.ins
            .routes
            .push(route("sw", Kind::Binary, true, None, None));
    }

    #[test]
    fn lines_end_at_a_newline_crlf_is_tolerated_and_split_reads_are_joined() {
        let mut r = rig();
        assert_eq!(r.raw(b"*IDN?\r\n"), "ferrofire,rtu,0,1\n");
        assert_eq!(r.raw(b"*ID"), "");
        assert_eq!(r.raw(b"N?"), "");
        assert_eq!(r.raw(b"\n*OPC?\n*TST?"), "ferrofire,rtu,0,1\n1\n");
        assert_eq!(r.raw(b"\n"), "0\n");
        assert_eq!(r.raw(b"*OPC?\r"), "");
        assert_eq!(r.raw(b"\n\n\r\n"), "1\n");
        assert_eq!(
            r.raw(b"*OP\rC?\n"),
            "",
            "a carriage return inside a line is not a terminator"
        );
        assert!(r.errors()[0].starts_with("-102,"));
        assert_eq!(r.raw(&[0xFF, 0xFE, b'\n']), "");
        assert!(
            r.errors()[0].starts_with("-102,"),
            "bytes that are not text are a syntax error"
        );
    }

    #[test]
    fn a_line_over_the_limit_is_an_error_and_thrown_away_to_its_newline() {
        let mut r = rig();
        let ok = [b'A'; 64];
        let long = vec![b'A'; 65];
        assert_eq!(r.raw(&[&ok[..], b"\n"].concat()), "");
        assert!(r.errors()[0].starts_with("-113,"), "64 bytes is allowed");
        r.raw(&[&long[..], b"\n*IDN?\n"].concat());
        assert_eq!(
            r.errors(),
            ["-363,\"Input buffer overrun\""],
            "one error, then the next line is read"
        );
        assert_eq!(
            r.raw(&[&long[..], b"\n*IDN?\n"].concat()),
            "ferrofire,rtu,0,1\n"
        );
        r.errors();
        assert_eq!(r.raw(&long), "");
        assert_eq!(r.raw(b"*IDN?"), "", "still inside the overlong line");
        assert_eq!(r.raw(&long), "");
        assert_eq!(r.raw(b"\n*OPC?\n"), "1\n");
        assert_eq!(r.errors().len(), 1);
        r.raw(&[b'B'; 40]);
        r.raw(&[b'B'; 40]);
        assert!(r.s.buf.is_empty(), "the buffer was given up");
        assert!(r.s.skipping);
        assert_eq!(r.raw(b"x\n"), "");
        assert!(!r.s.skipping);
        assert_eq!(r.errors().len(), 1);
    }

    #[test]
    fn a_last_line_without_a_terminator_is_answered_at_eof_and_a_query_in_it_is_unterminated() {
        let mut r = rig();
        r.raw(b"*IDN?");
        let out = r.s.eof(&r.ins, &r.store);
        assert_eq!(out, b"ferrofire,rtu,0,1\n");
        assert_eq!(r.errors(), ["-420,\"Query UNTERMINATED\""]);
        r.raw(b"*ESE 3");
        assert!(r.s.eof(&r.ins, &r.store).is_empty());
        assert!(r.errors().is_empty());
        assert_eq!(r.ask("*ESE?"), "3\n");
        assert!(r.s.eof(&r.ins, &r.store).is_empty(), "nothing left");
        r.raw(&[b'A'; 100]);
        r.raw(b"*IDN?");
        assert!(
            r.s.eof(&r.ins, &r.store).is_empty(),
            "the tail of an overlong line is not a message"
        );
        assert_eq!(r.errors().len(), 1);
    }

    #[test]
    fn responses_beyond_the_output_cap_discard_what_was_unsent() {
        let mut r = rig();
        let line = b"*IDN?\n";
        let reply = "ferrofire,rtu,0,1\n".len();
        let n = OUT_CAP / reply + 10;
        let out = r.raw(&line.repeat(n));
        assert!(out.len() <= OUT_CAP && !out.is_empty(), "{}", out.len());
        assert!(out.len().is_multiple_of(reply));
        assert_eq!(r.errors(), ["-410,\"Query INTERRUPTED\""]);
        assert_eq!(
            r.raw(&line.repeat(n / 2)).len(),
            n / 2 * reply,
            "under the cap everything is sent"
        );
        assert!(r.errors().is_empty());
    }

    #[test]
    fn keywords_match_by_short_or_long_form_and_suffix() {
        assert_eq!(keyword("MEASure", "MEAS"), Match::Yes);
        assert_eq!(keyword("MEASure", "MEASURE"), Match::Yes);
        assert_eq!(keyword("MEASure", "MEASURE1"), Match::Yes);
        assert_eq!(keyword("MEASure", "MEAS01"), Match::Yes);
        assert_eq!(keyword("MEASure", "MEAS2"), Match::BadSuffix);
        assert_eq!(keyword("MEASure", "MEAS0"), Match::BadSuffix);
        assert_eq!(keyword("MEASure", "MEASU"), Match::No);
        assert_eq!(keyword("MEASure", "ME"), Match::No);
        assert_eq!(keyword("MEASure", "1"), Match::No);
        assert_eq!(keyword("MEASure", "MEASURES"), Match::No);
        assert_eq!(
            keyword("tag", "TAG"),
            Match::Yes,
            "no upper-case prefix: only the long form"
        );
        assert_eq!(keyword("tag", "T"), Match::No);
        assert_eq!(keyword("BUS1", "BUS1"), Match::Yes);
        assert_eq!(keyword("BUS1", "BUS"), Match::No);
        assert_eq!(keyword("BUS", "BUS1"), Match::Yes);
        assert_eq!(keyword("BUS", "BUS99999999999999"), Match::BadSuffix);
        assert_eq!(path_match(&["A", "B"], &["A".into()]), Match::No);
        assert_eq!(
            path_match(&["A", "B"], &["A".into(), "B2".into()]),
            Match::BadSuffix
        );
        assert_eq!(
            path_match(&["A", "B"], &["A2".into(), "C".into()]),
            Match::No
        );
        assert_eq!(
            path_match(&["A", "B"], &["A".into(), "B".into()]),
            Match::Yes
        );
    }
}
