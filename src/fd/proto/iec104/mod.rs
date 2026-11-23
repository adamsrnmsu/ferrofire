//! IEC 60870-5-104 (SPEC 4.3): our own implementation, differential-tested against the stock lib60870 behaviour recorded in
//! `conformance/iec104/STOCK.md`. Layers, bottom up, without I/O: `apci` (framing, windows and timers), `asdu` (the codec),
//! `server` (the slave). This file is `<iec60870-5-104-server>`: the config, and per connection a `Session` (no I/O either)
//! inside a small socket loop; and `<iec60870-5-104-client>`: the config (its run time is `client_shell`).

pub mod apci;
pub mod asdu;
pub mod client;
mod client_shell;
pub mod server;

use super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::net;
use super::super::store::Store;
use super::client::{self as shared, Mode};
use super::Module;
use crate::hint::{lock, show};
use crate::model::Kind;
use apci::{Action, Link, Params, Role};
use client::Conn;
use roxmltree::Node;
use server::{Point, Slave, Subtype};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

const MODULE: &str = "iec60870-5-104-server";
const START: &str = "iec60870-5-104 server start";
const TICK: Duration = Duration::from_millis(100);
/// STARTDT con: the link took a STARTDT act.
const START_CON: [u8; 6] = [0x68, 4, 0x0B, 0, 0, 0];
/// A reply that cannot be written within this long ends the connection (as `<dnp3-server>`).
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// lib60870 `CONFIG_CS104_MAX_CLIENT_CONNECTIONS` (`protocol/config/lib60870_config.h:66`): the 6th master is closed at once.
const MAX_MASTERS: usize = 5;
/// ASDUs waiting for the k window, per connection; stock's queue is as deep (`CS104_Slave_create(1000, 1000)`, Server.cpp:45).
const QUEUE: usize = 1000;

pub struct Server {
    endpoint: String,
    log: Option<String>,
    rpoll: Duration,
    subtype: Subtype,
    points: Vec<Point>,
}

impl Server {
    /// The slave this server answers with.
    pub fn slave(&self) -> Slave {
        Slave::new(self.points.clone(), self.subtype)
    }
}

/// The points in the order stock adds them (`MOD/DataHandler.cpp:60-91`, `:120-159` for a client connection): every binary
/// input, then binary output, analog input, analog output, each in document order. The later of two at one address of one kind
/// wins, as in stock's maps. `keep` says whether a point's tag is served (the server: mapped; a client: declared).
fn points(
    node: Node,
    mut keep: impl FnMut(&str, Kind) -> Result<bool, ConfigError>,
) -> Result<Vec<Point>, ConfigError> {
    let mut out = vec![];
    for (name, kind, output) in [
        ("binary-input", Kind::Binary, false),
        ("binary-output", Kind::Binary, true),
        ("analog-input", Kind::Analog, false),
        ("analog-output", Kind::Analog, true),
    ] {
        for n in node.children().filter(|c| c.has_tag_name(name)) {
            let addr = number::<u16>(n, "address")?
                .ok_or_else(|| ConfigError(format!("<{name}> has no <address>")))?;
            let tag = require(n, "tag")?;
            if keep(&tag, kind)? {
                out.push(Point {
                    addr,
                    tag,
                    kind,
                    output,
                });
            }
        }
    }
    Ok(out)
}

/// `<sbo-enabled>` (a boost bool: `true`, `false`, `1`, `0`) and `<sbo-timeout-secs>`: only select-before-operate off is served.
fn sbo(node: Node) -> Result<(), ConfigError> {
    match text(node, "sbo-enabled").as_deref() {
        None | Some("false") | Some("0") => {}
        Some("true") | Some("1") => {
            return Err(ConfigError(
                "<sbo-enabled> is true: select-before-operate is not supported yet (bead ww-3a4)"
                    .into(),
            ))
        }
        Some(v) => {
            return Err(ConfigError(format!(
                "<sbo-enabled> `{}` is not a boolean",
                show(v)
            )))
        }
    }
    number::<u32>(node, "sbo-timeout-secs").map(|_| ())
}

impl Module for Server {
    /// `MOD/DataHandler.cpp:47-92`: `<rpoll-rate>`, `<endpoint>` and `<subtype>` are required; any `<subtype>` naming `double` is
    /// double-point mode (`MOD/Server.cpp:55`).
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let rpoll = number::<u32>(node, "rpoll-rate")?
            .ok_or_else(|| ConfigError(format!("<{MODULE}> has no <rpoll-rate>")))?;
        let endpoint = require(node, "endpoint")?;
        let subtype = match text(node, "subtype") {
            None => return Err(ConfigError(format!("<{MODULE}> needs <subtype>"))),
            Some(s) if s.contains("double") => Subtype::Double,
            Some(_) => Subtype::Single,
        };
        sbo(node)?;
        Ok(Server {
            endpoint,
            log: text(node, "event-logging"),
            // ponytail: stock sleeps `rpoll-rate` s between reports, so 0 floods; we report every tick instead
            rpoll: Duration::from_secs(rpoll.into()).max(TICK),
            subtype,
            points: points(node, |t, k| tags.mapped(t, k, opts))?,
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let log = EventLog::open(self.log.as_deref(), MODULE);
        // stock looks for `tcp://` anywhere in the endpoint (MOD/Server.cpp:33-95)
        if !self.endpoint.contains("tcp://") {
            let msg = format!("Unknown endpoint protocol ({})", self.endpoint);
            return log.say("error", START, &msg);
        }
        let listener = net::listen(&self.endpoint).await;
        if let Err(e) = &listener {
            log.log("error", START, "Starting server failed");
            tracing::error!("{MODULE}: {e}");
        }
        let init = format!(
            "Initialized IEC60870-5-104 server: {} (select-before-operate disabled)",
            self.endpoint
        );
        log.log("info", START, &init);
        let Ok(listener) = listener else { return };
        let ctx = Arc::new(Ctx {
            slave: self.slave(),
            store,
            log,
            rpoll: self.rpoll,
            group: Mutex::default(),
        });
        let on_conn = |s: TcpStream| {
            if let Some(session) = Session::open(&ctx) {
                net::keepalive(&s);
                tokio::spawn(serve(s, session, stop.clone()));
            }
        };
        let report = |e: &std::io::Error| {
            ctx.log.log(
                "error",
                "iec60870-5-104 accept",
                format!("accept failed: {e}; retrying"),
            )
        };
        net::accept_loop(&listener, &stop, on_conn, report).await;
    }
}

/// The connections of the one redundancy group (`CS104_MODE_SINGLE_REDUNDANCY_GROUP`, `MOD/Server.cpp:52`).
#[derive(Default)]
struct Group {
    next: u64,
    open: usize,
    /// The connection opened last (`mConnection`) and whether no connection has closed since (`mConnected`): the report goes
    /// there (`MOD/Server.cpp:713-733`).
    last: Option<u64>,
    connected: bool,
    /// The connection whose STARTDT came last; the others are deactivated (`cs104_slave.c:1410-1435`).
    active: Option<u64>,
}

struct Ctx {
    slave: Slave,
    store: Store,
    log: EventLog,
    rpoll: Duration,
    group: Mutex<Group>,
}

/// What a session event leaves to do: bytes to write, then whether to close.
#[derive(Default)]
struct Out {
    bytes: Vec<u8>,
    close: bool,
}

/// One master's connection without the socket: its link, the ASDUs waiting for the k window, and its place in the group.
struct Session {
    id: u64,
    link: Link,
    queue: VecDeque<Vec<u8>>,
    /// The queue was full and is logged so; cleared once an ASDU fits again.
    full: bool,
    ctx: Arc<Ctx>,
}

impl Session {
    /// A new connection, or `None` when `MAX_MASTERS` are open (stock accepts and closes it, with no open event).
    fn open(ctx: &Arc<Ctx>) -> Option<Session> {
        let mut g = lock(&ctx.group);
        if g.open >= MAX_MASTERS {
            return None;
        }
        g.open += 1;
        g.next += 1;
        (g.last, g.connected) = (Some(g.next), true);
        Some(Session {
            id: g.next,
            link: Link::new(Params::STANDARD, Role::Server),
            queue: VecDeque::new(),
            full: false,
            ctx: ctx.clone(),
        })
    }

    /// Another connection's STARTDT came after ours: stop without a word. Done at the tick and before a report, not before bytes
    /// read: bytes on two sockets have no order, so ones that arrive with the other's STARTDT still find this one started.
    // ponytail: up to one tick (100 ms) late; stock deactivates at once, which no master can time against its other socket
    fn sync(&mut self) {
        if lock(&self.ctx.group).active != Some(self.id) {
            self.link.deactivate();
        }
    }

    /// A STARTDT drops what waited while stopped, as stock resets the queue (`cs104_slave.c:2293`); before the frame's actions,
    /// so the answer to a GI in the same segment is kept.
    fn on_bytes(&mut self, b: &[u8], now: Instant) -> Out {
        let acts = self.link.on_bytes(b, now);
        if acts.contains(&Action::Send(START_CON.to_vec())) {
            lock(&self.ctx.group).active = Some(self.id);
            self.queue.clear();
        }
        self.apply(acts, now)
    }

    fn tick(&mut self, now: Instant) -> Out {
        self.sync();
        let acts = self.link.tick(now);
        self.apply(acts, now)
    }

    /// The periodic report, when this is the connection opened last, none has closed since, and it is started.
    fn report(&mut self, now: Instant, unix_ms: i64) -> Out {
        self.sync();
        let mine = {
            let g = lock(&self.ctx.group);
            g.connected && g.last == Some(self.id)
        };
        if mine && self.link.started() {
            let read = |t: &str| self.ctx.store.get(t);
            for a in self.ctx.slave.periodic(&read, unix_ms) {
                self.push(a.encode());
            }
        }
        self.apply(vec![], now)
    }

    /// The link's actions in order; each ASDU delivered is answered and its replies sent (or queued) before the next action.
    fn apply(&mut self, acts: Vec<Action>, now: Instant) -> Out {
        let mut out = Out::default();
        for a in acts {
            match a {
                Action::Send(b) => out.bytes.extend(b),
                Action::Deliver(asdu) if server::closes(&asdu) => out.close = true,
                Action::Deliver(asdu) => self.answer(&asdu),
                Action::Close(why) => {
                    // not in the event log: stock only prints connection events (`MOD/Server.cpp:709-741`)
                    tracing::warn!("{MODULE}: closing the connection: {why}");
                    out.close = true;
                }
            }
            if out.close {
                return out;
            }
            self.flush(now, &mut out);
        }
        self.flush(now, &mut out);
        out
    }

    fn answer(&mut self, asdu: &[u8]) {
        let read = |t: &str| self.ctx.store.get(t);
        let r = self.ctx.slave.on_asdu(asdu::decode(asdu), &read);
        for (tag, v) in r.writes {
            let _ = self.ctx.store.write(&tag, v);
        }
        for (level, event, msg) in r.log {
            self.ctx.log.log(level, event, msg);
        }
        r.send.into_iter().for_each(|a| self.push(a));
    }

    /// Queue an ASDU for sending. One longer than an APDU can carry is dropped and logged; one more than `QUEUE` waiting is
    /// dropped, and logged once until one fits again.
    fn push(&mut self, a: Vec<u8>) {
        let why = if a.len() > asdu::MAX_ASDU {
            format!(
                "an ASDU of {} bytes is longer than {}",
                a.len(),
                asdu::MAX_ASDU
            )
        } else if self.queue.len() < QUEUE {
            self.full = false;
            return self.queue.push_back(a);
        } else if std::mem::replace(&mut self.full, true) {
            return;
        } else {
            format!("{QUEUE} ASDUs are waiting for the master's acknowledgement")
        };
        self.ctx.log.log(
            "error",
            "iec60870-5-104 send",
            format!("{why}; ASDU dropped"),
        );
    }

    /// Send what the k window takes, oldest first.
    fn flush(&mut self, now: Instant, out: &mut Out) {
        while let Some(a) = self.queue.front() {
            let Some(acts) = self.link.send(a.clone(), now) else {
                return;
            };
            self.queue.pop_front();
            for act in acts {
                if let Action::Send(b) = act {
                    out.bytes.extend(b);
                }
            }
        }
    }
}

impl Drop for Session {
    /// Any close stops the report until a connection opens (`MOD/Server.cpp:726-733`).
    fn drop(&mut self) {
        let mut g = lock(&self.ctx.group);
        g.open -= 1;
        g.connected = false;
        if g.active == Some(self.id) {
            g.active = None;
        }
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Serve one master until it goes, the session closes it, a write stalls or the device stops.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    mut session: Session,
    stop: CancellationToken,
) {
    let mut buf = [0u8; 4096];
    let mut tick = tokio::time::interval(TICK);
    // the first report `rpoll-rate` s after the open: a connection is never started at once
    let rpoll = session.ctx.rpoll;
    let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + rpoll, rpoll);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let out = tokio::select! {
            _ = stop.cancelled() => return,
            _ = tick.tick() => session.tick(Instant::now()),
            _ = poll.tick() => session.report(Instant::now(), unix_ms()),
            r = s.read(&mut buf) => match r {
                Ok(0) | Err(_) => return,
                Ok(n) => session.on_bytes(&buf[..n], Instant::now()),
            },
        };
        if !out.bytes.is_empty()
            && !matches!(
                tokio::time::timeout(WRITE_TIMEOUT, s.write_all(&out.bytes)).await,
                Ok(Ok(()))
            )
        {
            return;
        }
        if out.close {
            return;
        }
    }
}

pub struct Client {
    pub mode: Mode,
    /// `--strict`: Q12 off, single points decoded.
    pub strict: bool,
    pub conns: Vec<Conn>,
    pub cmdif: Option<String>,
    /// `fep` mode: how often the output tags are looked at (the field device's `<cycle-time>`).
    pub cycle: Duration,
}

/// One `<iec60870-5-104-connection>` (`MOD/DataHandler.cpp:112-165`).
fn connection(c: Node, mode: Mode, tags: &mut Tags, opts: &Opts) -> Result<Conn, ConfigError> {
    let endpoint = require(c, "endpoint")?;
    // stock looks for `tcp://` anywhere in the endpoint, and exits the device when it is not there (MOD/ClientConnection.cpp:23-62)
    if !endpoint.contains("tcp://") {
        let msg = format!("Unknown endpoint protocol: {}", show(&endpoint));
        return Err(ConfigError(msg));
    }
    sbo(c)?;
    let fep = shared::fep_keys(c, mode, tags, opts)?;
    let points = points(c, |t, k| shared::point_tag(tags, t, k, opts))?;
    Ok(Conn {
        endpoint,
        points,
        fep,
    })
}

impl Module for Client {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let mode = shared::mode(node)?;
        let conns = node
            .children()
            .filter(|c| c.has_tag_name("iec60870-5-104-connection"))
            .map(|c| connection(c, mode, tags, opts))
            .collect::<Result<_, _>>()?;
        Ok(Client {
            mode,
            strict: opts.strict,
            conns,
            cmdif: text(node, "command-interface"),
            cycle: shared::cycle(node),
        })
    }

    /// One task per connection, so a dead server stalls nothing else.
    async fn run(self, store: Store, stop: CancellationToken) {
        client_shell::run(self, store, stop).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::config::Config;
    use crate::model::{Kind, Value};

    fn tags() -> Tags {
        let mut t = Tags::default();
        for (n, k) in [
            ("bi", Kind::Binary),
            ("bo", Kind::Binary),
            ("bo2", Kind::Binary),
            ("ai", Kind::Analog),
        ] {
            t.add_internal(n, k).unwrap();
        }
        t
    }

    const HEAD: &str =
        "<rpoll-rate>1</rpoll-rate><subtype>single</subtype><endpoint>tcp://127.0.0.1:1</endpoint>";

    fn parse(inner: &str) -> Result<Server, ConfigError> {
        let xml = format!("<iec60870-5-104-server>{inner}</iec60870-5-104-server>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        Server::parse(doc.root_element(), &mut tags(), &Opts::default())
    }

    fn err(inner: &str) -> String {
        parse(inner).err().expect("an error").0
    }

    #[test]
    fn stock_sample_loads() {
        let strict = Opts {
            strict: true,
            ..Opts::default()
        };
        for xml in [
            include_str!("../../../../conformance/stock/iec104-server.xml"),
            include_str!("../../../../conformance/stock/server-iec104.xml"),
        ] {
            let c = Config::parse(xml, crate::fd::proto::REGISTRY, &strict).unwrap();
            let names: Vec<_> = c.modules.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, ["iec60870-5-104-server"]);
        }
        let s = parse(&format!("{HEAD}<event-logging>x.log</event-logging>")).unwrap();
        assert_eq!(
            (s.subtype, s.rpoll.as_secs(), s.endpoint.as_str()),
            (Subtype::Single, 1, "tcp://127.0.0.1:1")
        );
        assert_eq!(s.log.as_deref(), Some("x.log"));
    }

    #[test]
    fn missing_subtype_is_an_error_naming_it() {
        let e = err("<rpoll-rate>1</rpoll-rate><endpoint>tcp://127.0.0.1:1</endpoint>");
        assert!(e.contains("<iec60870-5-104-server> needs <subtype>"), "{e}");
        assert!(err("<subtype>single</subtype><endpoint>x</endpoint>")
            .contains("<iec60870-5-104-server> has no <rpoll-rate>"));
        assert!(err("<rpoll-rate>1</rpoll-rate><subtype>single</subtype>")
            .contains("<iec60870-5-104-server> has no <endpoint>"));
        assert!(
            err("<rpoll-rate>-1</rpoll-rate><subtype>single</subtype><endpoint>x</endpoint>")
                .contains("<rpoll-rate> `-1` is not a valid number")
        );
        let pt = |inner: &str| err(&format!("{HEAD}<binary-input>{inner}</binary-input>"));
        assert!(pt("<tag>bi</tag>").contains("<binary-input> has no <address>"));
        assert!(pt("<address>2</address>").contains("<binary-input> has no <tag>"));
        assert!(pt("<address>70000</address><tag>bi</tag>").contains("not a valid number"));
        assert!(err(&format!(
            "{HEAD}<analog-input><address>1</address><tag>bi</tag></analog-input>"
        ))
        .contains("is Binary but its mapping needs Analog"));
    }

    /// Stock: `subtype.find("double")` picks double-point mode, anything else single (`MOD/Server.cpp:55`).
    #[test]
    fn a_subtype_naming_double_is_double_point_mode() {
        let sub = |v: &str| {
            parse(&format!(
                "<rpoll-rate>1</rpoll-rate><subtype>{v}</subtype><endpoint>x</endpoint>"
            ))
            .unwrap()
            .subtype
        };
        assert_eq!(sub("double"), Subtype::Double);
        assert_eq!(sub("doublepoint"), Subtype::Double);
        assert_eq!(sub("single"), Subtype::Single);
        assert_eq!(sub(""), Subtype::Single);
    }

    #[test]
    fn select_before_operate_on_is_refused_off_is_accepted() {
        for v in ["true", "1"] {
            let e = err(&format!("{HEAD}<sbo-enabled>{v}</sbo-enabled>"));
            assert!(
                e.contains("<sbo-enabled>")
                    && e.contains("select-before-operate is not supported yet"),
                "{v}: {e}"
            );
        }
        assert!(err(&format!("{HEAD}<sbo-enabled>yes</sbo-enabled>")).contains("is not a boolean"));
        for ok in [
            "<sbo-enabled>false</sbo-enabled>",
            "<sbo-enabled>0</sbo-enabled><sbo-timeout-secs>5</sbo-timeout-secs>",
            "",
        ] {
            assert!(parse(&format!("{HEAD}{ok}")).is_ok(), "{ok}");
        }
        assert!(
            err(&format!("{HEAD}<sbo-timeout-secs>x</sbo-timeout-secs>"))
                .contains("not a valid number")
        );
    }

    /// A zero rate would spin (stock sleeps 0 s between reports); it is the shell's tick instead.
    #[test]
    fn a_zero_rpoll_rate_is_the_tick() {
        let s = parse("<rpoll-rate>0</rpoll-rate><subtype>single</subtype><endpoint>x</endpoint>")
            .unwrap();
        assert_eq!(s.rpoll, TICK);
    }

    fn command(s: &Server, ioa: u8) -> Vec<(String, Value)> {
        s.slave()
            .on_asdu(asdu::decode(&[45, 1, 6, 0, 1, 0, ioa, 0, 0, 1]), &|_| None)
            .writes
    }

    /// Stock adds all binary inputs, then binary outputs, analog inputs, analog outputs (`MOD/DataHandler.cpp:60-91`), each into
    /// a map by address: a later point of one kind at one address replaces an earlier one, and an output beats an input whatever
    /// the document order.
    #[test]
    fn a_later_point_at_an_address_wins_in_stock_order() {
        let s = parse(&format!(
            "{HEAD}<binary-output><address>5</address><tag>bo</tag></binary-output>\
             <binary-input><address>5</address><tag>bi</tag></binary-input>\
             <binary-output><address>6</address><tag>bo</tag></binary-output>\
             <binary-output><address>6</address><tag>bo2</tag></binary-output>"
        ))
        .unwrap();
        assert_eq!(command(&s, 5), [("bo".to_string(), Value::Binary(true))]);
        assert_eq!(command(&s, 6), [("bo2".to_string(), Value::Binary(true))]);
        let unknown = parse(&format!(
            "{HEAD}<binary-output><address>5</address><tag>nope</tag></binary-output>"
        ))
        .unwrap();
        assert!(command(&unknown, 5).is_empty(), "an unknown tag is skipped");
    }

    fn log_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("iec104-{name}-{}.log", std::process::id()))
            .display()
            .to_string()
    }

    #[tokio::test]
    async fn non_tcp_endpoint_starts_no_server() {
        let log = log_path("udp");
        let s = parse(&format!(
            "<rpoll-rate>1</rpoll-rate><subtype>single</subtype><endpoint>udp://127.0.0.1:2404</endpoint><event-logging>{log}</event-logging>"
        ))
        .unwrap();
        let store = Store::new(Default::default());
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            s.run(store, CancellationToken::new()),
        )
        .await
        .expect("run returns at once");
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains(
                "error,iec60870-5-104-server,iec60870-5-104 server start,Unknown endpoint protocol (udp://127.0.0.1:2404)"
            ),
            "{text}"
        );
        assert!(!text.contains("Initialized"), "{text}");
    }

    fn ctx_with(log: EventLog) -> Arc<Ctx> {
        Arc::new(Ctx {
            slave: parse(HEAD).unwrap().slave(),
            store: Store::new(Default::default()),
            log,
            rpoll: TICK,
            group: Mutex::default(),
        })
    }

    fn ctx() -> Arc<Ctx> {
        ctx_with(EventLog::to(MODULE, Box::new(std::io::sink())))
    }

    /// A full queue is logged once until it has drained below `QUEUE`, so a master that never acks cannot flood the event log.
    #[test]
    fn an_oversized_asdu_or_a_full_queue_is_dropped() {
        let path = log_path("full");
        let _ = std::fs::remove_file(&path);
        let ctx = ctx_with(EventLog::open(Some(&path), MODULE));
        let mut s = Session::open(&ctx).unwrap();
        s.push(vec![0; asdu::MAX_ASDU + 1]);
        assert!(s.queue.is_empty());
        for _ in 0..QUEUE + 3 {
            s.push(vec![0; asdu::MAX_ASDU]);
        }
        assert_eq!(s.queue.len(), QUEUE);
        let lines = |what: &str| {
            let text = std::fs::read_to_string(&path).unwrap();
            text.lines().filter(|l| l.contains(what)).count()
        };
        assert_eq!((lines("is longer than"), lines("are waiting")), (1, 1));
        s.queue.pop_front();
        s.push(vec![0; asdu::MAX_ASDU]);
        s.push(vec![0; asdu::MAX_ASDU]);
        assert_eq!(lines("are waiting"), 2, "full again after a drain");
    }

    /// A master that stopped reading ends its connection after `WRITE_TIMEOUT` (T4-M4). Paused time: no real 10 s.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_write_ends_the_server_connection() {
        let ctx = ctx();
        let session = Session::open(&ctx).unwrap();
        let (ours, mut peer) = tokio::io::duplex(1);
        // the pipe is 1 byte both ways: the master writes as the server reads, and never reads
        let talk = tokio::spawn(async move {
            peer.write_all(&STARTDT).await.unwrap();
            std::future::pending::<()>().await;
        });
        let t = tokio::time::Instant::now();
        serve(ours, session, CancellationToken::new()).await;
        let took = t.elapsed();
        assert!(
            took >= WRITE_TIMEOUT && took < WRITE_TIMEOUT * 3 / 2,
            "{took:?}"
        );
        talk.abort();
    }

    const STARTDT: [u8; 6] = [0x68, 4, 7, 0, 0, 0];

    /// The group's rules without sockets: a STARTDT deactivates the others; the report is the last opened's while it is started;
    /// a close stops it for all; the 6th connection is refused.
    #[test]
    fn a_session_follows_the_redundancy_group() {
        let (ctx, t) = (ctx(), Instant::now());
        let mut a = Session::open(&ctx).unwrap();
        a.on_bytes(&STARTDT, t);
        assert!(!a.report(t, 0).bytes.is_empty());
        let mut b = Session::open(&ctx).unwrap();
        assert!(a.report(t, 0).bytes.is_empty() && b.report(t, 0).bytes.is_empty());
        b.on_bytes(&STARTDT, t);
        // an S-frame that came with B's STARTDT finds A still started, and does not take the group back
        assert!(!a.on_bytes(&[0x68, 4, 1, 0, 0, 0], t).close);
        assert_eq!(lock(&ctx.group).active, Some(b.id));
        assert!(!b.report(t, 0).bytes.is_empty());
        a.tick(t);
        assert!(!a.link.started(), "B's STARTDT deactivated A");
        let i_frame = [0x68, 4, 0, 0, 0, 0];
        assert!(a.on_bytes(&i_frame, t).close);
        let more: Vec<_> = (0..3).map(|_| Session::open(&ctx).unwrap()).collect();
        assert!(Session::open(&ctx).is_none(), "five are open");
        drop(a);
        assert!(
            b.report(t, 0).bytes.is_empty(),
            "A's close stopped the report"
        );
        drop(more);
        let mut c = Session::open(&ctx).unwrap();
        c.on_bytes(&STARTDT, t);
        assert!(!c.report(t, 0).bytes.is_empty());
        let gi = |ns: u8| [0x68, 14, ns << 1, 0, 0, 0, 100, 1, 6, 0, 1, 0, 0, 0, 0, 20];
        assert!(!c.on_bytes(&gi(0), t).close);
        // stock resets the queue on STARTDT (`cs104_slave.c:2293`): what waited while stopped is not sent
        let (stopdt, stale) = ([0x68, 4, 0x13, 0, 0, 0], vec![0xAA; 10]);
        c.on_bytes(&stopdt, t);
        c.push(stale.clone());
        assert_eq!(c.on_bytes(&STARTDT, t).bytes, START_CON, "only the con");
        // the GI in the STARTDT's segment is answered: the reset comes before it
        c.on_bytes(&stopdt, t);
        c.push(stale.clone());
        let b = c.on_bytes(&[&STARTDT[..], &gi(1)].concat(), t).bytes;
        assert!(b.starts_with(&START_CON) && b.len() > 6, "{b:02x?}");
        assert!(!b.windows(10).any(|w| w == stale), "{b:02x?}");
        let short = [0x68, 7, 4, 0, 0, 0, 45, 1, 6];
        assert!(c.on_bytes(&short, t).close, "an ASDU under 6 bytes closes");
    }
}
