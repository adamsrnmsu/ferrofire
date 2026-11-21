//! DNP3 outstation (SPEC 4.2, `<dnp3-server>`): our own implementation of the stock opendnp3 behaviour, differential-tested
//! against bennu (`conformance/dnp3/`). Layers, bottom up, are shared with the later master: `link` (FT3 frames and CRCs),
//! `transport` (segments), `app` (headers and indications). The outstation itself is a state machine without I/O
//! (`outstation`, with `read` and `command`); this file is the config, the sockets and the 1 Hz refresh around it.

pub mod app;
pub mod command;
pub mod db;
pub mod link;
pub mod master;
pub mod outstation;
pub mod read;
pub mod transport;

use super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::net;
use super::super::store::Store;
use super::Module;
use crate::hint::{lock, show};
use crate::model::{Kind, Value};
use outstation::{Close, Conn, Device, Sink, Slot, Source};
use roxmltree::Node;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Master link address the stock outstation answers (opendnp3 `LinkConfig` default for an outstation).
const MASTER: u16 = 1;
/// A reply that cannot be written within this long ends the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(100);

pub struct Outstation {
    endpoint: String,
    local: u16,
    log: Arc<EventLog>,
    bin: Table<bool>,
    ana: Table<f64>,
    /// `<event-buffer-size>`: events kept per type.
    cap: u16,
}

/// opendnp3 parsing of `<sbo>`: a boost `bool` is `0`, `1`, `true` or `false`.
fn flag(n: Node, name: &str) -> Result<bool, ConfigError> {
    match text(n, name).as_deref() {
        None | Some("0") | Some("false") => Ok(false),
        Some("1") | Some("true") => Ok(true),
        Some(v) => Err(ConfigError(format!(
            "<{name}> `{}` is not a boolean",
            show(v)
        ))),
    }
}

/// The text of the child `name` as bennu reads it: untrimmed, `None` when the element is missing.
fn raw<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.children()
        .find(|c| c.has_tag_name(name))
        .map(|c| c.text().unwrap_or(""))
}

/// A boost `lexical_cast` to `u16` as bennu's `get<uint16_t>(key, default)` does it: leading blanks and a sign are read, a
/// negative number wraps, anything else (trailing text, more than 16 bits, empty) gives `default`.
fn u16_or(raw: Option<&str>, default: u16) -> u16 {
    let t = raw
        .unwrap_or("")
        .trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    match digits
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| digits.parse::<u16>())
    {
        Some(Ok(v)) if neg => v.wrapping_neg(),
        Some(Ok(v)) => v,
        _ => default,
    }
}

/// opendnp3 `PointClassSpec::from_string`; an empty `<class>` is the default.
fn class_of(raw: Option<&str>) -> Option<u8> {
    match raw.unwrap_or("") {
        "" | "Class0" => Some(0),
        "Class1" => Some(1),
        "Class2" => Some(2),
        "Class3" => Some(3),
        _ => None,
    }
}

/// opendnp3 `*VariationSpec::from_string` for `Group{group}Var{n}` with n in `vars`; an empty value is the point's default.
fn variation(
    raw: Option<&str>,
    group: u8,
    vars: std::ops::RangeInclusive<u8>,
    default: u8,
) -> Option<u8> {
    match raw.unwrap_or("") {
        "" => Some(default),
        t => vars
            .into_iter()
            .find(|v| t == format!("Group{group}Var{v}")),
    }
}

type Table<V> = BTreeMap<Slot, db::Point<V>>;
type Maps = (Table<bool>, Table<f64>);
/// The `(group, versions)` of a point's static and of its event variations.
type Vars = [(u8, std::ops::RangeInclusive<u8>); 2];

/// One `<binary-input>` ... `<analog-output>` element as a point, or `None` when bennu leaves it out. Outputs read `<class>` and
/// `<sbo>` only; an input reads `<class>`, `<sgvar>`, `<egvar>` and (an analog one) `<deadband>`. A name opendnp3 does not know is
/// no error: bennu skips the point silently and the Size lines do not count it. A `<deadband>` that is not a number is: bennu
/// aborts the whole `<dnp3-server>` setup (logs it to stdout and carries on without a DNP3 outstation); we refuse the config.
fn point<V: db::Static>(
    n: Node,
    input: bool,
    tag: &str,
    vars: Vars,
    analog: bool,
) -> Result<Option<db::Point<V>>, ConfigError> {
    let mut p = db::Point::new(tag, !input && flag(n, "sbo")?, input);
    let Some(class) = class_of(raw(n, "class")) else {
        return Ok(None);
    };
    p.class = class;
    if input {
        let [(sg, svars), (eg, evars)] = vars;
        let (Some(s), Some(e)) = (
            variation(raw(n, "sgvar"), sg, svars, p.svar),
            variation(raw(n, "egvar"), eg, evars, p.evar),
        ) else {
            return Ok(None);
        };
        (p.svar, p.evar) = (s, e);
        if let Some(d) = raw(n, "deadband").filter(|_| analog) {
            p.deadband = d
                .trim_start()
                .parse()
                .ok()
                .filter(|x: &f64| x.is_finite())
                .ok_or_else(|| {
                    ConfigError(format!("<deadband> `{}` is not a valid number", show(d)))
                })?;
        }
    }
    Ok(Some(p))
}

/// The `<name>` children of a server whose tag exists with the right kind, as `(index, point)`.
fn points<V: db::Static>(
    node: Node,
    name: &str,
    kind: Kind,
    tags: &Tags,
    opts: &Opts,
    vars: &Vars,
) -> Result<Vec<(u16, db::Point<V>)>, ConfigError> {
    let mut out = vec![];
    for n in node.children().filter(|c| c.has_tag_name(name)) {
        let addr = number::<u16>(n, "address")?
            .ok_or_else(|| ConfigError(format!("<{name}> has no <address>")))?;
        let tag = require(n, "tag")?;
        if tags.mapped(&tag, kind, opts)? {
            let input = name.ends_with("input");
            if let Some(p) = point(n, input, &tag, vars.clone(), kind == Kind::Analog)? {
                out.push((addr, p));
            }
        }
    }
    Ok(out)
}

/// Stock order and override rule: four tables (g1, g10, g30, g40), inputs first; a later element at the same index in the same
/// table replaces it, an input and an output may share an index.
fn collect(node: Node, tags: &Tags, opts: &Opts) -> Result<Maps, ConfigError> {
    let (mut bin, mut ana) = (Table::new(), Table::new());
    let (bv, av) = ([(1, 1..=2), (2, 1..=3)], [(30, 1..=6), (32, 1..=8)]);
    for name in ["binary-input", "binary-output"] {
        for (a, p) in points(node, name, Kind::Binary, tags, opts, &bv)? {
            bin.insert((!p.input, a), p);
        }
    }
    for name in ["analog-input", "analog-output"] {
        for (a, p) in points(node, name, Kind::Analog, tags, opts, &av)? {
            ana.insert((!p.input, a), p);
        }
    }
    Ok((bin, ana))
}

impl Outstation {
    /// `<event-buffer-size>`: the events kept per type.
    pub fn cap(&self) -> u16 {
        self.cap
    }

    /// The outstation's shared state, fresh: every point at its power-up value, no events.
    pub fn device(&self) -> Device {
        Device::new(
            self.local,
            MASTER,
            self.bin.clone(),
            self.ana.clone(),
            self.cap,
        )
    }
}

/// The three lines bennu v2026.11.03 logs at start: input counts, output counts, event buffer size.
pub fn log_init(log: &EventLog, bin: &Table<bool>, ana: &Table<f64>, cap: u16) {
    let (bo, ao) = (
        bin.values().filter(|p| !p.input).count(),
        ana.values().filter(|p| !p.input).count(),
    );
    let say = |m: String| log.log("info", "dnp3 server init", m);
    say(format!(
        "Binary Size is {} and Analog Size is {}.",
        bin.len() - bo,
        ana.len() - ao
    ));
    say(format!(
        "Binary Output Size is {bo} and Analog Output Size is {ao}."
    ));
    say(format!("Event buffer size (per type) is {cap}."));
}

impl Module for Outstation {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let log = Arc::new(EventLog::open(
            text(node, "event-logging").as_deref(),
            "dnp3-server",
        ));
        let (bin, ana) = collect(node, tags, opts)?;
        let endpoint = require(node, "endpoint")?;
        let local = number::<u16>(node, "address")?
            .ok_or_else(|| ConfigError("<dnp3-server> has no <address>".into()))?;
        let cap = u16_or(raw(node, "event-buffer-size"), db::EVENT_CAP);
        log_init(&log, &bin, &ana, cap);
        Ok(Outstation {
            endpoint,
            local,
            log,
            bin,
            ana,
            cap,
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let bound = match net::bind(&self.endpoint).await {
            Ok(b) => b,
            Err(e) => {
                self.log.log(
                    "error",
                    "dnp3 TCP server init",
                    format!("failed to add TCPServer: {e}"),
                );
                tracing::error!("dnp3-server: {e}");
                return;
            }
        };
        let dev = Arc::new(Mutex::new(self.device()));
        tokio::spawn(refresh(dev.clone(), store.clone(), stop.clone()));
        let ctx = Ctx {
            dev,
            store,
            log: self.log,
            stop,
        };
        match bound {
            net::Bound::Tcp(l) => accept(l, ctx).await,
            #[cfg(feature = "proto-serial")]
            net::Bound::Serial(path, port) => serial(&path, port, ctx).await,
        }
    }
}

/// Serve masters from a TCP listener until `stop`, one at a time.
async fn accept(listener: tokio::net::TcpListener, ctx: Ctx) {
    let busy = Arc::new(AtomicBool::new(false));
    let mut failing = false;
    loop {
        let accepted =
            tokio::select! { _ = ctx.stop.cancelled() => return, r = listener.accept() => r };
        match accepted {
            Ok((stream, _)) => {
                failing = false;
                if busy.swap(true, Ordering::SeqCst) {
                    continue; // ServerAcceptMode::CloseNew: a second master is closed, the first is served on
                }
                let (ctx, free) = (ctx.clone(), Release(busy.clone()));
                tokio::spawn(async move {
                    serve(net::Conn::Tcp(stream), ctx).await;
                    drop(free);
                });
            }
            Err(e) => {
                // out of descriptors or memory: say so once, wait, and keep accepting
                if !failing {
                    ctx.log.log(
                        "error",
                        "dnp3 accept",
                        format!("accept failed: {e}; retrying"),
                    );
                }
                failing = true;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Serve the one master on a serial port until `stop` (no CloseNew slot: the port is one connection).
#[cfg(feature = "proto-serial")]
async fn serial(path: &str, port: tokio_serial::SerialStream, ctx: Ctx) {
    let session = |c| serve(c, ctx.clone());
    let report = |e: &str| {
        ctx.log
            .log("error", "dnp3 serial", format!("{e}; retrying"))
    };
    net::serial_loop(path, port, &ctx.stop, session, report).await;
}

/// Frees the one master slot when the connection task ends, however it ends.
struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Source for Store {
    fn binary(&self, tag: &str) -> Option<bool> {
        self.get_bool(tag)
    }
    fn analog(&self, tag: &str) -> Option<(f64, u64)> {
        Some((self.get_f64(tag)?, self.timestamp(tag).unwrap_or(0)))
    }
    fn pending(&self, tag: &str) -> bool {
        self.is_queued(tag)
    }
}

/// One refresh task for the whole outstation, whatever the connections do.
async fn refresh(dev: Arc<Mutex<Device>>, store: Store, stop: CancellationToken) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! { _ = stop.cancelled() => return, _ = every.tick() => lock(&dev).refresh(&store) }
    }
}

/// What a connection task needs.
#[derive(Clone)]
struct Ctx {
    dev: Arc<Mutex<Device>>,
    store: Store,
    log: Arc<EventLog>,
    stop: CancellationToken,
}

/// Commands from a master become writes to the datastore's queue, logged like bennu does.
pub struct Commands<'a> {
    store: &'a Store,
    log: &'a EventLog,
}

impl<'a> Commands<'a> {
    pub fn new(store: &'a Store, log: &'a EventLog) -> Commands<'a> {
        Commands { store, log }
    }
}

/// C's `%g` with six significant digits, which is what `std::ostream << float` prints.
pub fn fmt_g(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".into()
        } else if x > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let sci = format!("{x:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if (-4..6).contains(&exp) {
        trim(&format!("{x:.*}", (5 - exp).max(0) as usize))
    } else {
        format!(
            "{}e{}{:02}",
            trim(mant),
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

impl Sink for Commands<'_> {
    fn binary(&mut self, tag: &str, index: u16, value: bool) {
        let v = u8::from(value);
        self.log.log(
            "info",
            "dnp3 Server writeBinary",
            format!("Binary point command at address {index} with value {v}."),
        );
        let _ = self.store.write(tag, Value::Binary(value));
        self.log
            .log("info", "write binary", "Data successfully written.");
    }
    fn analog(&mut self, tag: &str, index: u16, value: f32) {
        let v = fmt_g(f64::from(value));
        self.log.log(
            "info",
            "dnp3 Server writeAnalog",
            format!("Analog point command at address {index} with value {v}."),
        );
        let _ = self.store.write(tag, Value::Analog(f64::from(value)));
        self.log
            .log("info", "write analog", "Data successfully written.");
    }
}

/// Serve one master until it goes, misbehaves or the device stops. The socket closes when this returns.
async fn serve(mut stream: net::Conn, ctx: Ctx) {
    stream.keepalive();
    let (mut conn, start) = (Conn::new(), Instant::now());
    let mut buf = [0u8; 2048];
    let mut tick = tokio::time::interval(TICK);
    loop {
        let n = tokio::select! {
            _ = ctx.stop.cancelled() => break,
            _ = tick.tick() => { conn.tick(&mut lock(&ctx.dev), start.elapsed()); continue }
            r = stream.read(&mut buf) => match r { Ok(0) | Err(_) => break, Ok(n) => n },
        };
        let mut sink = Commands::new(&ctx.store, &ctx.log);
        let reply = conn.feed(&mut lock(&ctx.dev), &mut sink, &buf[..n], start.elapsed());
        match reply {
            Err(Close(why)) => {
                ctx.log.log(
                    "error",
                    "dnp3 connection",
                    format!("closing the connection: {why}"),
                );
                break;
            }
            Ok(bytes) if bytes.is_empty() => {}
            Ok(bytes) => {
                if !matches!(
                    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(&bytes)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        }
    }
    conn.cancel(&mut lock(&ctx.dev));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_g_prints_like_a_c_stream() {
        for (x, want) in [
            (1.5, "1.5"),
            (55.5, "55.5"),
            (0.0, "0"),
            (-2.5, "-2.5"),
            (100000.0, "100000"),
            (999999.0, "999999"),
            (1e6, "1e+06"),
            (1234567.0, "1.23457e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e-10, "1e-10"),
            (1e100, "1e+100"),
            (123456.0, "123456"),
            (0.1f32 as f64, "0.1"),
            (std::f32::consts::PI as f64, "3.14159"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (3.0, "3"),
            (100.0, "100"),
        ] {
            assert_eq!(fmt_g(x), want, "{x}");
        }
    }

    fn tags() -> Tags {
        let mut t = Tags::default();
        t.add_internal("bi", Kind::Binary).unwrap();
        t.add_internal("an", Kind::Analog).unwrap();
        t
    }

    fn parse(inner: &str, opts: &Opts) -> Result<Outstation, ConfigError> {
        let xml =
            format!("<dnp3-server><event-logging>/dev/null</event-logging>{inner}</dnp3-server>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        Outstation::parse(doc.root_element(), &mut tags(), opts)
    }

    const HEAD: &str = "<endpoint>tcp://127.0.0.1:1</endpoint><address>10</address>";

    fn err(inner: &str) -> String {
        parse(inner, &Opts::default()).err().expect("an error").0
    }

    #[test]
    fn a_server_element_becomes_two_point_maps() {
        let o = parse(
            &format!("{HEAD}<binary-input><address>2</address><tag>bi</tag></binary-input><analog-output><address>1</address><tag>an</tag><sbo>true</sbo></analog-output>"),
            &Opts::default(),
        )
        .unwrap();
        let d = o.device();
        assert_eq!((d.local, d.master, d.bin.len(), d.ana.len()), (10, 1, 1, 1));
        assert!(
            d.bin[&(false, 2)].input
                && !d.bin[&(false, 2)].sbo
                && !d.ana[&(true, 1)].input
                && d.ana[&(true, 1)].sbo
        );
    }

    #[test]
    fn missing_keys_are_named() {
        assert!(err("<address>10</address>").contains("<dnp3-server> has no <endpoint>"));
        assert!(err("<endpoint>tcp://127.0.0.1:1</endpoint>")
            .contains("<dnp3-server> has no <address>"));
        let pt = |inner: &str| err(&format!("{HEAD}<binary-input>{inner}</binary-input>"));
        assert!(pt("<tag>bi</tag>").contains("<binary-input> has no <address>"));
        assert!(pt("<address>2</address>").contains("has no <tag>"));
        assert!(pt("<address>70000</address><tag>bi</tag>").contains("not a valid number"));
        assert!(err("<endpoint>x</endpoint><address>-1</address>").contains("not a valid number"));
    }

    fn analog(inner: &str) -> Device {
        let xml = format!("{HEAD}{inner}");
        parse(&xml, &Opts::default()).unwrap().device()
    }

    /// Stock v2026.11.03 (probed, conformance/dnp3/stock-cfg.golden): an input takes class, sgvar, egvar and (analog) deadband, an
    /// output only class; the defaults are class 0 and g1v2 / g2v2, g30v6 / g32v6, g10v2 / g11v1, g40v1 / g42v1.
    #[test]
    fn class_variations_and_deadband_are_read() {
        let d = analog(
            "<analog-input><address>0</address><tag>an</tag><class>Class2</class><sgvar>Group30Var3</sgvar><egvar>Group32Var8</egvar><deadband>2.5</deadband></analog-input>\
             <analog-output><address>0</address><tag>an</tag><class>Class3</class><sgvar>Group40Var3</sgvar><deadband>9</deadband></analog-output>\
             <binary-input><address>1</address><tag>bi</tag></binary-input>\
             <binary-output><address>1</address><tag>bi</tag></binary-output>",
        );
        let ai = &d.ana[&(false, 0)];
        assert_eq!((ai.class, ai.svar, ai.evar, ai.deadband), (2, 3, 8, 2.5));
        let ao = &d.ana[&(true, 0)];
        assert_eq!((ao.class, ao.svar, ao.evar, ao.deadband), (3, 1, 1, 0.0));
        let (bi, bo) = (&d.bin[&(false, 1)], &d.bin[&(true, 1)]);
        assert_eq!((bi.class, bi.svar, bi.evar), (0, 2, 2));
        assert_eq!((bo.class, bo.svar, bo.evar), (0, 2, 1));
    }

    /// A name opendnp3 does not know leaves the point out without a word (the Size lines do not count it): case matters, so does a
    /// trailing blank; a variation must be one of its type's.
    #[test]
    fn a_name_opendnp3_does_not_know_leaves_the_point_out() {
        for (key, v) in [
            ("class", "class1"),
            ("class", "Class4"),
            ("class", "1"),
            ("sgvar", "Group1Var1 "),
            ("sgvar", "Group1Var0"),
            ("sgvar", "Group2Var1"),
            ("egvar", "Group2Var0"),
            ("egvar", "Group2Var4"),
            ("sgvar", "x"),
        ] {
            let inner = format!(
                "<binary-input><address>0</address><tag>bi</tag><{key}>{v}</{key}></binary-input>"
            );
            assert_eq!(analog(&inner).bin.len(), 0, "{key} {v}");
        }
        for (key, v) in [
            ("sgvar", "Group30Var0"),
            ("sgvar", "Group30Var7"),
            ("egvar", "Group32Var0"),
            ("egvar", "Group32Var9"),
        ] {
            let inner = format!(
                "<analog-input><address>0</address><tag>an</tag><{key}>{v}</{key}></analog-input>"
            );
            assert_eq!(analog(&inner).ana.len(), 0, "{key} {v}");
        }
        let out =
            "<binary-output><address>0</address><tag>bi</tag><class>bogus</class></binary-output>";
        assert_eq!(analog(out).bin.len(), 0);
        let empty = "<binary-input><address>0</address><tag>bi</tag><class></class><sgvar></sgvar></binary-input>";
        assert_eq!(analog(empty).bin.len(), 1, "an empty value is the default");
        for (key, v, var) in [
            ("sgvar", "Group1Var1", 1),
            ("sgvar", "Group1Var2", 2),
            ("egvar", "Group2Var3", 3),
        ] {
            let inner = format!(
                "<binary-input><address>0</address><tag>bi</tag><{key}>{v}</{key}></binary-input>"
            );
            let p = &analog(&inner).bin[&(false, 0)].clone();
            assert_eq!(if key == "sgvar" { p.svar } else { p.evar }, var);
        }
    }

    /// bennu aborts its DNP3 setup on a deadband that `lexical_cast` refuses; we refuse the config. An output has no deadband.
    #[test]
    fn a_deadband_that_is_no_number_is_refused_on_an_analog_input_only() {
        for v in ["abc", "", "1e", "0x10", "inf", "nan", "1e999", "5 "] {
            let e = err(&format!("{HEAD}<analog-input><address>0</address><tag>an</tag><deadband>{v}</deadband></analog-input>"));
            assert!(
                e.contains("<deadband>") && e.contains("not a valid number"),
                "{v}: {e}"
            );
        }
        for (v, want) in [
            ("5", 5.0),
            ("-1", -1.0),
            ("1e1", 10.0),
            (".5", 0.5),
            (" 3", 3.0),
            ("+2", 2.0),
        ] {
            let d = analog(&format!("<analog-input><address>0</address><tag>an</tag><deadband>{v}</deadband></analog-input>"));
            assert_eq!(d.ana[&(false, 0)].deadband, want, "{v}");
        }
        analog("<analog-output><address>0</address><tag>an</tag><deadband>abc</deadband></analog-output><binary-input><address>0</address><tag>bi</tag><deadband>abc</deadband></binary-input>");
    }

    /// `get<uint16_t>("event-buffer-size", 100)`: a number that does not read as a 16-bit integer is the default; a negative one wraps
    /// (probed: -5 is 65531, 70000 and 1e2 and 0x10 and `4 ` are 100, ` 4` is 4).
    #[test]
    fn the_event_buffer_size_is_a_lexical_cast_with_a_default_of_100() {
        for (v, want) in [
            (Some("3"), 3),
            (Some("0"), 0),
            (Some("65535"), 65535),
            (Some("65536"), 100),
            (Some("70000"), 100),
            (Some("-5"), 65531),
            (Some("-65535"), 1),
            (Some("-65536"), 100),
            (Some("+7"), 7),
            (Some(" 4"), 4),
            (Some("4 "), 100),
            (Some("2.5"), 100),
            (Some("1e2"), 100),
            (Some("0x10"), 100),
            (Some("abc"), 100),
            (Some(""), 100),
            (Some("-"), 100),
            (None, 100),
        ] {
            assert_eq!(u16_or(v, 100), want, "{v:?}");
        }
        let o = parse(
            &format!("{HEAD}<event-buffer-size>3</event-buffer-size>"),
            &Opts::default(),
        )
        .unwrap();
        assert_eq!(o.cap, 3);
    }

    #[test]
    fn sbo_is_a_boolean_and_only_outputs_have_one() {
        let sbo = |v: &str| {
            let inner = format!("{HEAD}<binary-output><address>5</address><tag>bi</tag><sbo>{v}</sbo></binary-output>");
            parse(&inner, &Opts::default()).map(|o| o.device().bin[&(true, 5)].sbo)
        };
        assert_eq!(sbo("1"), Ok(true));
        assert_eq!(sbo("true"), Ok(true));
        assert_eq!(sbo("0"), Ok(false));
        assert_eq!(sbo("false"), Ok(false));
        assert!(sbo("2").unwrap_err().0.contains("`2` is not a boolean"));
        let input = format!(
            "{HEAD}<binary-input><address>5</address><tag>bi</tag><sbo>1</sbo></binary-input>"
        );
        assert!(
            !parse(&input, &Opts::default()).unwrap().device().bin[&(false, 5)].sbo,
            "an input ignores <sbo>"
        );
    }

    #[test]
    fn unknown_tags_are_skipped_wrong_kinds_refused_and_a_later_element_replaces_an_earlier() {
        let skip =
            format!("{HEAD}<binary-input><address>2</address><tag>nope</tag></binary-input>");
        assert_eq!(
            parse(&skip, &Opts::default()).unwrap().device().bin.len(),
            0
        );
        assert!(parse(
            &skip,
            &Opts {
                strict: true,
                ..Opts::default()
            }
        )
        .is_err());
        let kind = format!("{HEAD}<binary-input><address>2</address><tag>an</tag></binary-input>");
        assert!(err(&kind).contains("is Analog but its mapping needs Binary"));
        let both = format!(
            "{HEAD}<binary-output><address>2</address><tag>bi</tag></binary-output><binary-input><address>2</address><tag>bi</tag></binary-input><binary-input><address>2</address><tag>bi</tag><class>Class1</class></binary-input>"
        );
        let d = parse(&both, &Opts::default()).unwrap().device();
        assert_eq!(d.bin.len(), 2, "an input and an output share an index");
        assert_eq!(
            d.bin[&(false, 2)].class,
            1,
            "a later element of a table replaces an earlier"
        );
    }
}
