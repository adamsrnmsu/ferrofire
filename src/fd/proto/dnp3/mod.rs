//! DNP3 outstation (SPEC 4.2, `<dnp3-server>`): our own implementation of the stock opendnp3 behaviour, differential-tested
//! against bennu (`conformance/dnp3/`). Layers, bottom up, are shared with the later master: `link` (FT3 frames and CRCs),
//! `transport` (segments), `app` (headers and indications). The outstation itself is a state machine without I/O
//! (`outstation`, with `read` and `command`); this file is the config, the sockets and the 1 Hz refresh around it.

pub mod app;
pub mod command;
pub mod db;
pub mod link;
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
use db::{AnaPoint, BinPoint};
use outstation::{Close, Conn, Device, Sink, Source};
use roxmltree::Node;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
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
    bin: BTreeMap<u16, BinPoint>,
    ana: BTreeMap<u16, AnaPoint>,
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

/// The generator never emits DNP3 variations, classes or deadbands; v1 supports the defaults and says so.
fn reject_options(n: Node) -> Result<(), ConfigError> {
    for key in ["sgvar", "egvar", "class", "deadband"] {
        if text(n, key).is_some() {
            return Err(ConfigError(format!(
                "<{}> <{key}> is not supported (only the stock defaults are)",
                n.tag_name().name()
            )));
        }
    }
    Ok(())
}

/// One `<binary-input>`-style element: `(address, tag, sbo)`.
fn point(n: Node, name: &str) -> Result<(u16, String, bool), ConfigError> {
    reject_options(n)?;
    let addr = number::<u16>(n, "address")?
        .ok_or_else(|| ConfigError(format!("<{name}> has no <address>")))?;
    let sbo = name.ends_with("output") && flag(n, "sbo")?;
    Ok((addr, require(n, "tag")?, sbo))
}

/// The `<name>` children of a server whose tag exists with the right kind, as `(address, tag, sbo)`.
fn points(
    node: Node,
    name: &str,
    kind: Kind,
    tags: &Tags,
    opts: &Opts,
) -> Result<Vec<(u16, String, bool)>, ConfigError> {
    let mut out = vec![];
    for n in node.children().filter(|c| c.has_tag_name(name)) {
        let p = point(n, name)?;
        if tags.mapped(&p.1, kind, opts)? {
            out.push(p);
        }
    }
    Ok(out)
}

type Maps = (BTreeMap<u16, BinPoint>, BTreeMap<u16, AnaPoint>);

/// Stock order and override rule: one map per kind, inputs first, a later element at the same address replaces it.
fn collect(node: Node, tags: &Tags, opts: &Opts) -> Result<Maps, ConfigError> {
    let (mut bin, mut ana) = (BTreeMap::new(), BTreeMap::new());
    for (name, input) in [("binary-input", true), ("binary-output", false)] {
        for (a, t, sbo) in points(node, name, Kind::Binary, tags, opts)? {
            bin.insert(a, BinPoint::new(&t, sbo, input));
        }
    }
    for (name, input) in [("analog-input", true), ("analog-output", false)] {
        for (a, t, sbo) in points(node, name, Kind::Analog, tags, opts)? {
            ana.insert(a, AnaPoint::new(&t, sbo, input));
        }
    }
    Ok((bin, ana))
}

impl Outstation {
    /// The outstation's shared state, fresh: every point at its power-up value, no events.
    pub fn device(&self) -> Device {
        Device::new(self.local, MASTER, self.bin.clone(), self.ana.clone())
    }
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
        log.log(
            "info",
            "dnp3 server init",
            &format!(
                "Binary Size is {} and Analog Size is {}.",
                bin.len(),
                ana.len()
            ),
        );
        Ok(Outstation {
            endpoint,
            local,
            log,
            bin,
            ana,
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let listener = match net::listen(&self.endpoint).await {
            Ok(l) => l,
            Err(e) => {
                self.log.log(
                    "error",
                    "dnp3 TCP server init",
                    &format!("failed to add TCPServer: {e}"),
                );
                tracing::error!("dnp3-server: {e}");
                return;
            }
        };
        let dev = Arc::new(Mutex::new(self.device()));
        tokio::spawn(refresh(dev.clone(), store.clone(), stop.clone()));
        let busy = Arc::new(AtomicBool::new(false));
        let mut failing = false;
        loop {
            let accepted =
                tokio::select! { _ = stop.cancelled() => return, r = listener.accept() => r };
            match accepted {
                Ok((stream, _)) => {
                    failing = false;
                    if busy.swap(true, Ordering::SeqCst) {
                        continue; // ServerAcceptMode::CloseNew: a second master is closed, the first is served on
                    }
                    let ctx = Ctx {
                        dev: dev.clone(),
                        store: store.clone(),
                        log: self.log.clone(),
                        stop: stop.clone(),
                    };
                    let free = Release(busy.clone());
                    tokio::spawn(async move {
                        serve(stream, ctx).await;
                        drop(free);
                    });
                }
                Err(e) => {
                    // out of descriptors or memory: say so once, wait, and keep accepting
                    if !failing {
                        self.log.log(
                            "error",
                            "dnp3 accept",
                            &format!("accept failed: {e}; retrying"),
                        );
                    }
                    failing = true;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
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
}

/// One refresh task for the whole outstation, whatever the connections do.
async fn refresh(dev: Arc<Mutex<Device>>, store: Store, stop: CancellationToken) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! { _ = stop.cancelled() => return, _ = every.tick() => lock(&dev).refresh(&store) }
    }
}

/// What a connection task needs.
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
            &format!("Binary point command at address {index} with value {v}."),
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
            &format!("Analog point command at address {index} with value {v}."),
        );
        let _ = self.store.write(tag, Value::Analog(f64::from(value)));
        self.log
            .log("info", "write analog", "Data successfully written.");
    }
}

fn keepalive(stream: &TcpStream) {
    use socket2::{SockRef, TcpKeepalive};
    let ka = TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    let _ = SockRef::from(stream).set_tcp_keepalive(&ka);
}

/// Serve one master until it goes, misbehaves or the device stops. The socket closes when this returns.
async fn serve(mut stream: TcpStream, ctx: Ctx) {
    keepalive(&stream);
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
                    &format!("closing the connection: {why}"),
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
        assert!(d.bin[&2].input && !d.bin[&2].sbo && !d.ana[&1].input && d.ana[&1].sbo);
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

    #[test]
    fn variations_classes_and_deadbands_are_refused_by_name() {
        for key in ["sgvar", "egvar", "class", "deadband"] {
            let e = err(&format!("{HEAD}<analog-input><address>0</address><tag>an</tag><{key}>x</{key}></analog-input>"));
            assert!(
                e.contains(&format!("<{key}> is not supported")) && e.contains("<analog-input>"),
                "{e}"
            );
        }
    }

    #[test]
    fn sbo_is_a_boolean_and_only_outputs_have_one() {
        let sbo = |v: &str| {
            let inner = format!("{HEAD}<binary-output><address>5</address><tag>bi</tag><sbo>{v}</sbo></binary-output>");
            parse(&inner, &Opts::default()).map(|o| o.device().bin[&5].sbo)
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
            !parse(&input, &Opts::default()).unwrap().device().bin[&5].sbo,
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
        assert!(parse(&skip, &Opts { strict: true }).is_err());
        let kind = format!("{HEAD}<binary-input><address>2</address><tag>an</tag></binary-input>");
        assert!(err(&kind).contains("is Analog but its mapping needs Binary"));
        let both = format!(
            "{HEAD}<binary-output><address>2</address><tag>bi</tag></binary-output><binary-input><address>2</address><tag>bi</tag></binary-input>"
        );
        let d = parse(&both, &Opts::default()).unwrap().device();
        assert_eq!(d.bin.len(), 1);
        assert!(
            !d.bin[&2].input,
            "outputs are read after inputs, so the output replaced the input at that address"
        );
    }
}
