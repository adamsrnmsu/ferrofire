//! Modbus/TCP client (`<modbus-client>`, protocol plan row 8). In `bennu` mode it is the stock fd-client as captured
//! (conformance/stock/client-modbus.golden): nothing is polled, a command-interface READ is one live request and a WRITE one FC5 or
//! FC6. `fep` mode polls every connection into tags and writes local changes down (docs/field-device-clients.md).

use super::super::super::cmdif::{self, Points};
use super::super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::super::eventlog::EventLog;
use super::super::super::net;
use super::super::super::store::Store;
use super::super::client::{self as shared, Fep, Mode, Out};
use super::super::Module;
use super::codec::{self, Parse};
use super::device::{Range, DEFAULT_RANGE};
use crate::model::{Kind, Value};
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// How long a connect or a response may take. Stock has no limit (a silent server blocks it for good); a divergence
/// conformance/README.md lists.
pub const DEADLINE: Duration = Duration::from_millis(1000);
/// The transaction id of a connection's first request, as stock counts.
const FIRST_TID: u16 = 3;
/// What stock meant to say after `ERR=Failed reading tag 't': ` and `ERR=Failed writing tag 't': ` (it sends a dangling pointer).
const READ_FAILED: &str = "readRegisterByTag(): Failure";
const WRITE_FAILED: &str = "writeCoil(): Failed writing coil";

/// A read request PDU (FC 1 to 4).
pub fn read_req(fc: u8, addr: u16, qty: u16) -> [u8; 5] {
    let (a, q) = (addr.to_be_bytes(), qty.to_be_bytes());
    [fc, a[0], a[1], q[0], q[1]]
}

/// A single write request PDU: FC5 (`value` 0xFF00 or 0) or FC6.
pub fn write_req(fc: u8, addr: u16, value: u16) -> [u8; 5] {
    read_req(fc, addr, value)
}

/// Why a response PDU gave no values.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Exception(u8),
    Bad(&'static str),
}

/// The values in the response PDU to a request of function `fc` for `qty` items: one number per register, 0 or 1 per bit, and
/// for a single write the value echoed. Never panics on input.
pub fn response(fc: u8, qty: u16, pdu: &[u8]) -> Result<Vec<u16>, Reply> {
    let n = usize::from(qty);
    match (pdu, fc) {
        ([f, code], _) if *f == fc | 0x80 => Err(Reply::Exception(*code)),
        ([f, ..], _) if *f != fc => Err(Reply::Bad("the response is for another function")),
        ([_, count, data @ ..], 1 | 2)
            if usize::from(*count) == n.div_ceil(8) && data.len() == n.div_ceil(8) =>
        {
            Ok((0..n)
                .map(|i| u16::from(data[i / 8] >> (i % 8) & 1))
                .collect())
        }
        ([_, count, data @ ..], 3 | 4) if usize::from(*count) == 2 * n && data.len() == 2 * n => {
            Ok(data
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect())
        }
        ([_, _, _, hi, lo], 5 | 6) => Ok(vec![u16::from_be_bytes([*hi, *lo])]),
        _ => Err(Reply::Bad("the response does not fit the request")),
    }
}

/// The reply PDU for transaction `tid` at the front of `buf`, once it is whole. Frames with another id are discarded (a late
/// answer to a request given up on); `Err` means the stream is not Modbus/TCP.
pub fn take(buf: &mut Vec<u8>, tid: u16) -> Option<Result<Vec<u8>, &'static str>> {
    loop {
        let (got, pdu, used) = match codec::parse(buf) {
            Parse::Need => return None,
            Parse::Bad(why) => return Some(Err(why)),
            Parse::Frame { tid, pdu, used, .. } => (tid, pdu.to_vec(), used),
        };
        buf.drain(..used);
        if got == tid {
            return Some(Ok(pdu));
        }
        tracing::warn!(
            "modbus-client: discarded a response with transaction id {got}, expected {tid}"
        );
    }
}

/// The register value of a `bennu` mode write: stock's `value * slope + intercept` on a float32 value, converted to an unsigned
/// 32-bit integer and cut to 16 bits, so a value past the range wraps (2000 over 0..1000 is 0xfffe) and a negative one is 0.
pub fn wrap(range: &Range, v: f64) -> u16 {
    let slope = range.slope();
    slope.mul_add(f64::from(v as f32), range.intercept(slope)) as u32 as u16
}

/// The read requests of one poll, as `(function, start, quantity)`: each table's addresses in order, contiguous ones merged up
/// to 2,000 bits or 125 registers. An address in `split` (function, address) is read alone: a merged read there was refused.
pub fn plan_reads(points: &[Point], split: &BTreeSet<(u8, u16)>) -> Vec<(u8, u16, u16)> {
    let addrs: BTreeSet<(u8, u16)> = points.iter().map(|p| (p.table.read_fc(), p.addr)).collect();
    let mut out: Vec<(u8, u16, u16)> = vec![];
    for (fc, addr) in addrs {
        let limit = if fc <= 2 { 2000 } else { 125 };
        match out.last_mut() {
            Some((f, start, qty))
                if *f == fc
                    && u32::from(*start) + u32::from(*qty) == u32::from(addr)
                    && *qty < limit
                    && !split.contains(&(fc, addr))
                    && !split.contains(&(fc, *start)) =>
            {
                *qty += 1
            }
            _ => out.push((fc, addr, 1)),
        }
    }
    out
}

/// What a `fep` poll makes of the raw item `raw` of point `p`: the register unscaled through the point's own range, or, for a
/// binary tag, whether it is non-zero.
pub fn polled(p: &Point, raw: u16) -> Value {
    match p.kind {
        Kind::Binary => Value::Binary(raw != 0),
        Kind::Analog => Value::Analog(p.range.unscale(raw)),
    }
}

/// What `v` is on the wire for point `p`, the form the echo rule compares and `wire` sends: an analog value as the register
/// it scales to (clamped to the range, as the server scales), anything else as it is. Reading a register back gives the
/// same register: the scaling truncates, so a hair is added before it, or `unscale` followed by `scale` could lose a count.
pub fn key(p: &Point, v: Value) -> Value {
    match (p.kind, v) {
        (Kind::Analog, Value::Analog(x)) if !x.is_nan() => {
            let slope = p.range.slope();
            let raw = (slope.mul_add(x, p.range.intercept(slope)) + 1e-6) as u16;
            Value::Analog(f64::from(raw))
        }
        _ => v,
    }
}

/// The single write (function, value) that takes `v`, a value in `key` form, down to output point `p` in `fep` mode: FC5
/// for a coil, FC6 for a holding register; a binary tag on a register writes 1 or 0. `None`: nothing to send (NaN).
pub fn wire(p: &Point, v: Value) -> Option<(u8, u16)> {
    match (p.table, v) {
        (Table::Coil, Value::Binary(on)) => Some((5, if on { 0xFF00 } else { 0 })),
        (Table::Holding, Value::Binary(on)) => Some((6, u16::from(on))),
        (Table::Holding, Value::Analog(raw)) if !raw.is_nan() => Some((6, raw as u16)),
        _ => None,
    }
}

/// The four tables, by what reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Table {
    Coil,
    Discrete,
    Holding,
    Input,
}

impl Table {
    pub fn read_fc(self) -> u8 {
        self as u8 + 1
    }

    fn bits(self) -> bool {
        matches!(self, Table::Coil | Table::Discrete)
    }

    /// A coil or a holding register: what a FEP writes down.
    fn output(self) -> bool {
        matches!(self, Table::Coil | Table::Holding)
    }
}

/// One mapped point. Two tags may name the same address, so the map is by tag, not the server's address table.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub tag: String,
    pub table: Table,
    pub addr: u16,
    pub range: Range,
    /// The tag's kind: binary for a coil or discrete input, analog for a register.
    pub kind: Kind,
}

pub struct Conn {
    pub endpoint: String,
    pub unit: u8,
    pub points: Vec<Point>,
    pub fep: Option<Fep>,
    /// Stock keeps one range per address for the whole connection, the last one declared (holding registers, then input
    /// registers): two registers at one address share it, whatever their tables. `bennu` mode scales with these.
    pub ranges: BTreeMap<u16, Range>,
}

pub struct Client {
    pub mode: Mode,
    pub conns: Vec<Conn>,
    pub cmdif: Option<String>,
    /// The device's `<cycle-time>`: how often a FEP looks for local changes.
    pub cycle: Duration,
    log: Arc<EventLog>,
}

const TABLES: [(&str, Table); 4] = [
    ("coil", Table::Coil),
    ("discrete-input", Table::Discrete),
    ("holding-register", Table::Holding),
    ("input-register", Table::Input),
];

/// `fep` only: a register may feed a binary tag (`<type>`), and a range needs a width to be turned around.
fn fep_kind(n: Node, tag: &str, range: &Range) -> Result<Kind, ConfigError> {
    if range.min == range.max {
        return Err(ConfigError(format!(
            "<{}> of tag `{}`: <min-value> equals <max-value>",
            n.tag_name().name(),
            crate::hint::show(tag)
        )));
    }
    match text(n, "type").as_deref() {
        None | Some("analog") => Ok(Kind::Analog),
        Some("binary") => Ok(Kind::Binary),
        Some(other) => Err(ConfigError(format!(
            "<type> `{}` is not analog or binary",
            crate::hint::show(other)
        ))),
    }
}

/// The range and the tag kind of a point: a bit is binary; a register is analog over its range, unless `fep` mode says more.
fn scaled(n: Node, table: Table, mode: Mode, tag: &str) -> Result<(Range, Kind), ConfigError> {
    if table.bits() {
        return Ok((DEFAULT_RANGE, Kind::Binary));
    }
    let range = super::range(n)?;
    match mode {
        Mode::Bennu => Ok((range, Kind::Analog)),
        Mode::Fep => Ok((range, fep_kind(n, tag, &range)?)),
    }
}

/// One point element of `table`.
fn point(n: Node, table: Table, mode: Mode) -> Result<Point, ConfigError> {
    let name = n.tag_name().name();
    let addr = number::<u16>(n, "address")?;
    let addr = addr.ok_or_else(|| ConfigError(format!("<{name}> has no <address>")))?;
    let tag = require(n, "tag")?;
    let (range, kind) = scaled(n, table, mode, &tag)?;
    Ok(Point {
        tag,
        table,
        addr,
        range,
        kind,
    })
}

/// The points of a connection that got their tag, and stock's range map (which every register enters, tag or not).
fn points(
    c: Node,
    mode: Mode,
    tags: &mut Tags,
    opts: &Opts,
) -> Result<(Vec<Point>, BTreeMap<u16, Range>), ConfigError> {
    let (mut points, mut ranges) = (vec![], BTreeMap::new());
    for (name, table) in TABLES {
        for n in c.children().filter(|n| n.has_tag_name(name)) {
            let p = point(n, table, mode)?;
            if !table.bits() {
                ranges.insert(p.addr, p.range);
            }
            if shared::point_tag(tags, &p.tag, p.kind, opts)? {
                points.push(p);
            }
        }
    }
    Ok((points, ranges))
}

fn connection(c: Node, mode: Mode, tags: &mut Tags, opts: &Opts) -> Result<Conn, ConfigError> {
    let fep = shared::fep_keys(c, mode, tags, opts)?;
    let (points, ranges) = points(c, mode, tags, opts)?;
    Ok(Conn {
        endpoint: require(c, "endpoint")?,
        unit: number::<u8>(c, "unit-id")?.unwrap_or(0),
        points,
        fep,
        ranges,
    })
}

impl Module for Client {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let mode = shared::mode(node)?;
        let conns = node
            .children()
            .filter(|c| c.has_tag_name("modbus-connection"))
            .map(|c| connection(c, mode, tags, opts))
            .collect::<Result<_, _>>()?;
        // stock's client keeps no event log; ours does when the config names a file
        let log = match text(node, "event-logging") {
            Some(path) => EventLog::open(Some(&path), "modbus-client"),
            None => EventLog::to("modbus-client", Box::new(std::io::sink())),
        };
        Ok(Client {
            mode,
            conns,
            cmdif: text(node, "command-interface"),
            cycle: shared::cycle(node),
            log: Arc::new(log),
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let mode = self.mode;
        let (cmdif, cycle) = (self.cmdif.clone(), self.cycle);
        let (live, sessions) = self.live(store);
        match mode {
            // one task per connection: a slow or dead device stalls neither the scan cycle nor the other connections
            Mode::Fep => {
                for (i, points, fep) in sessions {
                    let session = Session::new(live.clone(), i, points, fep);
                    tokio::spawn(session.run(cycle, stop.clone()));
                }
            }
            // stock connects every connection once at start and then only when a request finds it closed
            Mode::Bennu => {
                for link in &live.links {
                    link.lock().await.connect().await;
                }
            }
        }
        let outputs = (live.points.iter()).map(|(t, (_, p))| (t.clone(), p.table.output()));
        match (cmdif, mode) {
            (None, _) => stop.cancelled().await,
            (Some(ep), Mode::Bennu) => cmdif::serve(&ep, Bennu(live), stop).await,
            (Some(ep), Mode::Fep) => {
                let points =
                    shared::TagPoints::new(live.store.clone(), outputs.collect::<Vec<_>>());
                cmdif::serve(&ep, points, stop).await
            }
        }
    }
}

/// A `fep` connection to start: its index, points and keys.
type Polled = (usize, Vec<Point>, Fep);

impl Client {
    /// What the running client shares, and the connections that poll.
    fn live(self, store: Store) -> (Arc<Live>, Vec<Polled>) {
        let (mut links, mut ranges, mut sessions) = (vec![], vec![], vec![]);
        let mut points = BTreeMap::new();
        for (i, c) in self.conns.into_iter().enumerate() {
            sessions.extend(c.fep.map(|fep| (i, c.points.clone(), fep)));
            points.extend(c.points.into_iter().map(|p| (p.tag.clone(), (i, p))));
            links.push(Mutex::new(Link::new(c.endpoint, c.unit)));
            ranges.push(c.ranges);
        }
        let live = Live {
            links,
            ranges,
            points,
            store,
            log: self.log,
        };
        (Arc::new(live), sessions)
    }
}

/// One connection's socket: opened at start, reopened by the request that finds it closed.
struct Link {
    endpoint: String,
    unit: u8,
    stream: Option<TcpStream>,
    tid: u16,
    buf: Vec<u8>,
}

impl Link {
    fn new(endpoint: String, unit: u8) -> Link {
        Link {
            endpoint,
            unit,
            stream: None,
            tid: FIRST_TID,
            buf: vec![],
        }
    }

    async fn connect(&mut self) -> bool {
        self.buf.clear();
        self.stream = match tokio::time::timeout(DEADLINE, net::connect(&self.endpoint)).await {
            Ok(Ok(s)) => {
                let _ = s.set_nodelay(true);
                Some(s)
            }
            Ok(Err(e)) => {
                tracing::debug!("modbus-client: {e}");
                None
            }
            Err(_) => None,
        };
        self.stream.is_some()
    }

    /// One request, one reply PDU. The transaction id counts every request, sent or not (stock). A failure closes the
    /// connection, so the next request opens a new one.
    async fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>, String> {
        let tid = self.tid;
        self.tid = tid.wrapping_add(1);
        if self.stream.is_none() && !self.connect().await {
            return Err(format!("{} is not reachable", self.endpoint));
        }
        let r = match tokio::time::timeout(DEADLINE, self.exchange(tid, pdu)).await {
            Ok(r) => r,
            Err(_) => Err(format!("no response within {} ms", DEADLINE.as_millis())),
        };
        if r.is_err() {
            self.stream = None;
        }
        r
    }

    async fn exchange(&mut self, tid: u16, pdu: &[u8]) -> Result<Vec<u8>, String> {
        let Some(stream) = self.stream.as_mut() else {
            return Err("not connected".into());
        };
        let io = |e: std::io::Error| e.to_string();
        stream
            .write_all(&codec::frame(tid, self.unit, pdu))
            .await
            .map_err(io)?;
        let mut chunk = [0u8; 512];
        loop {
            if let Some(r) = take(&mut self.buf, tid) {
                return r.map_err(str::to_string);
            }
            match stream.read(&mut chunk).await.map_err(io)? {
                0 => return Err("the server closed the connection".into()),
                n => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }
}

/// Why a request gave no values: the device refused it, or the connection gave no usable answer.
enum Fail {
    Exception(u8),
    Link,
}

/// What the running client shares between its command interface and its connections.
struct Live {
    links: Vec<Mutex<Link>>,
    /// Per connection: stock's address -> range map.
    ranges: Vec<BTreeMap<u16, Range>>,
    /// Tag -> (connection, point), in name order.
    points: BTreeMap<String, (usize, Point)>,
    store: Store,
    log: Arc<EventLog>,
}

impl Live {
    /// One request on connection `i`; the values, or why there are none (logged). A response that does not fit the request
    /// closes the connection like a lost one: the stream cannot be trusted after it.
    async fn ask(&self, i: usize, fc: u8, addr: u16, n: u16) -> Result<Vec<u16>, Fail> {
        let mut link = self.links[i].lock().await;
        let qty = if fc > 4 { 1 } else { n };
        let r = match link.request(&read_req(fc, addr, n)).await {
            Err(why) => Err((Fail::Link, why)),
            Ok(pdu) => response(fc, qty, &pdu).map_err(|e| match e {
                Reply::Exception(code) => (Fail::Exception(code), format!("exception {code}")),
                Reply::Bad(why) => {
                    link.stream = None;
                    (Fail::Link, why.to_string())
                }
            }),
        };
        let (level, said) = match &r {
            Ok(_) => ("info", "ok"),
            Err((_, why)) => ("error", why.as_str()),
        };
        self.log.log(
            level,
            format_args!("function {fc}"),
            format_args!("address {addr}, {n}: {said}"),
        );
        r.map_err(|(fail, _)| fail)
    }
}

/// The command interface of a `bennu` mode client: every READ and WRITE is one live request, as stock.
struct Bennu(Arc<Live>);

impl Points for Bennu {
    fn names(&self) -> Vec<String> {
        self.0.points.keys().cloned().collect()
    }

    async fn read(&self, tag: &str) -> Result<Value, String> {
        let (i, p) = self.0.points.get(tag).ok_or(READ_FAILED)?;
        let raw = self
            .0
            .ask(*i, p.table.read_fc(), p.addr, 1)
            .await
            .map_err(|_| READ_FAILED)?[0];
        let v = match p.table.bits() {
            true => Value::Binary(raw != 0),
            // stock keeps the value in a float; the range is the address's
            false => Value::Analog(f64::from(self.0.ranges[*i][&p.addr].unscale(raw) as f32)),
        };
        let _ = self.0.store.write(tag, v);
        Ok(v)
    }

    /// `true`/`false` is FC5 at the point's address and a number FC6, whatever table the point is in; a number for an address
    /// with no range (a coil's, unless a register shares it) is written as 0. All of it stock.
    async fn write(&self, tag: &str, v: Value) -> Result<(), String> {
        let (i, p) = self.0.points.get(tag).ok_or(WRITE_FAILED)?;
        let (fc, raw) = match v {
            Value::Binary(on) => (5, if on { 0xFF00 } else { 0 }),
            Value::Analog(x) => (6, self.0.ranges[*i].get(&p.addr).map_or(0, |r| wrap(r, x))),
        };
        self.0
            .ask(*i, fc, p.addr, raw)
            .await
            .map_err(|_| WRITE_FAILED)?;
        // the tag holds the last value written, when it is of the tag's kind
        let _ = self.0.store.write(tag, v);
        Ok(())
    }
}

/// One connection of a `fep` mode client: poll every `scan`, look for local changes every cycle.
struct Session {
    live: Arc<Live>,
    i: usize,
    points: Vec<Point>,
    fep: Fep,
    outs: BTreeMap<String, Out>,
    /// Addresses a merged read was refused for (exception 2): read alone from then on.
    split: BTreeSet<(u8, u16)>,
    /// Points whose own read the device refuses, so that it is said once.
    refused: BTreeSet<(u8, u16)>,
    up: bool,
}

impl Session {
    fn new(live: Arc<Live>, i: usize, points: Vec<Point>, fep: Fep) -> Session {
        let outs = points
            .iter()
            .filter(|p| p.table.output())
            // the tag's first value counts as sent, in the form the wire carries it
            .map(|p| {
                (
                    p.tag.clone(),
                    Out::new(key(p, crate::fd::store::zero(p.kind))),
                )
            })
            .collect();
        Session {
            live,
            i,
            points,
            fep,
            outs,
            split: BTreeSet::new(),
            refused: BTreeSet::new(),
            up: false,
        }
    }

    async fn run(mut self, cycle: Duration, stop: CancellationToken) {
        use tokio::time::Instant;
        let mut tick = tokio::time::interval(cycle);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let (mut next_poll, mut pause) = (Instant::now(), super::super::BACKOFF_FIRST);
        loop {
            tokio::select! { _ = stop.cancelled() => return, _ = tick.tick() => {} }
            let (was_up, poll) = (self.up, Instant::now() >= next_poll);
            self.up = match (poll, was_up) {
                (true, _) => self.poll().await,
                (false, true) => self.write_down().await,
                (false, false) => continue,
            };
            if self.up != was_up {
                self.status();
            }
            // a device that is away is asked again after a pause that doubles, not every scan
            if !self.up {
                next_poll = Instant::now() + pause;
                pause = super::super::backoff(pause, was_up);
            } else if poll {
                next_poll = Instant::now() + self.fep.scan;
                pause = super::super::BACKOFF_FIRST;
            }
        }
    }

    fn status(&self) {
        let n = self.i + 1;
        match self.up {
            true => tracing::info!("modbus-client: connection {n}: session up"),
            false => tracing::warn!(
                "modbus-client: connection {n}: session down, its tags keep their last values"
            ),
        }
        if let Some(tag) = &self.fep.status {
            let _ = self.live.store.write(tag, Value::Binary(self.up));
        }
    }

    /// Send `v` down to output point `p` and tell its echo state how it went. `false`: the connection failed.
    async fn send(&mut self, p: &Point, v: Value) -> bool {
        let Some((fc, raw)) = wire(p, v) else {
            tracing::warn!(
                "modbus-client: tag `{}` is {v:?}, which a register cannot hold: not sent",
                p.tag
            );
            self.settle(p, Some(v));
            return true;
        };
        match self.live.ask(self.i, fc, p.addr, raw).await {
            Ok(_) => self.settle(p, Some(v)),
            // a refused write is not repeated: the next poll shows what the device holds
            Err(Fail::Exception(code)) => {
                tracing::warn!(
                    "modbus-client: write of tag `{}` refused with exception {code}",
                    p.tag
                );
                self.settle(p, Some(v));
            }
            Err(Fail::Link) => {
                self.settle(p, None);
                return false;
            }
        }
        true
    }

    /// End the write under way for `p`: `sent` is the value that went, `None` that it failed.
    fn settle(&mut self, p: &Point, sent: Option<Value>) {
        if let Some(out) = self.outs.get_mut(&p.tag) {
            match sent {
                Some(v) => out.echo.sent_ok(v),
                None => out.echo.sent_failed(),
            }
        }
    }

    /// Send every output tag's local change. `false`: the connection failed (the changes stay due).
    async fn write_down(&mut self) -> bool {
        let store = self.live.store.clone();
        for p in self.points.clone().iter().filter(|p| p.table.output()) {
            let out = self.outs.get_mut(&p.tag);
            let due = out.and_then(|o| o.due(&store, &p.tag, |v| key(p, v)));
            if let Some(v) = due {
                if !self.send(p, v).await {
                    return false;
                }
            }
        }
        true
    }

    /// One poll: local changes go down first (they win over what the poll reads), then every table is read. `false`: the
    /// device gave no answer; an exception is an answer.
    async fn poll(&mut self) -> bool {
        if !self.connected().await || !self.write_down().await {
            return false;
        }
        for (fc, start, qty) in plan_reads(&self.points, &self.split) {
            if !self.read(fc, start, qty).await {
                return false;
            }
        }
        true
    }

    /// The connection is open, or could be opened now.
    async fn connected(&self) -> bool {
        let mut link = self.live.links[self.i].lock().await;
        link.stream.is_some() || link.connect().await
    }

    /// One read of a poll, into the tags. `false`: the connection failed.
    async fn read(&mut self, fc: u8, start: u16, qty: u16) -> bool {
        match self.live.ask(self.i, fc, start, qty).await {
            // a local change that landed since the write-down pass goes down now, instead of the polled value in
            Ok(values) => {
                let local = self.take(fc, start, &values);
                if !self.send_all(local).await {
                    return false;
                }
            }
            // a merged read over a hole: read each address alone from the next poll on
            Err(Fail::Exception(2)) if qty > 1 => {
                self.split.extend((0..qty).map(|k| (fc, start + k)));
            }
            Err(Fail::Exception(code)) => {
                if self.refused.insert((fc, start)) {
                    tracing::warn!("modbus-client: read (function {fc}) of address {start} refused with exception {code}");
                }
            }
            Err(Fail::Link) => return false,
        }
        true
    }

    /// Send local changes down, in order. `false`: the connection failed, and the ones not sent are due again.
    async fn send_all(&mut self, local: Vec<(Point, Value)>) -> bool {
        let mut local = local.into_iter();
        while let Some((p, v)) = local.next() {
            if !self.send(&p, v).await {
                for (rest, _) in local {
                    self.settle(&rest, None);
                }
                return false;
            }
        }
        true
    }

    /// Put the values of one read into the tags of the points it covers. Returned: output points whose tag holds a local
    /// change instead, to be sent down.
    fn take(&mut self, fc: u8, start: u16, values: &[u16]) -> Vec<(Point, Value)> {
        let store = &self.live.store;
        let mut local = vec![];
        for p in self.points.iter().filter(|p| p.table.read_fc() == fc) {
            let item = p
                .addr
                .checked_sub(start)
                .and_then(|k| values.get(usize::from(k)));
            let Some(&raw) = item else {
                continue;
            };
            let v = polled(p, raw);
            let Some(out) = self.outs.get_mut(&p.tag) else {
                shared::input_polled(store, &p.tag, v);
                continue;
            };
            // on the wire an analog is its register
            let on_wire = match v {
                Value::Analog(_) => Value::Analog(f64::from(raw)),
                Value::Binary(_) => v,
            };
            let due = out.polled(store, &p.tag, v, on_wire, |x| key(p, x));
            local.extend(due.map(|l| (p.clone(), l)));
        }
        local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_function_address_and_quantity_or_value() {
        assert_eq!(read_req(3, 10, 2), [3, 0, 10, 0, 2]);
        assert_eq!(read_req(4, 30000, 1), [4, 0x75, 0x30, 0, 1]);
        assert_eq!(write_req(5, 1, 0xFF00), [5, 0, 1, 0xFF, 0]);
        assert_eq!(write_req(6, 40000, 0x7fff), [6, 0x9c, 0x40, 0x7f, 0xff]);
    }

    #[test]
    fn a_response_gives_one_number_per_item_or_says_why_not() {
        use Reply::{Bad, Exception};
        assert_eq!(response(3, 2, &[3, 4, 0, 1, 0, 2]), Ok(vec![1, 2]));
        assert_eq!(response(4, 1, &[4, 2, 0x40, 0x20]), Ok(vec![0x4020]));
        assert_eq!(response(1, 3, &[1, 1, 0b101]), Ok(vec![1, 0, 1]));
        assert_eq!(response(2, 9, &[2, 2, 0xff, 1]), Ok(vec![1; 9]));
        assert_eq!(response(5, 1, &[5, 0, 0, 0xff, 0]), Ok(vec![0xff00]));
        assert_eq!(
            response(6, 1, &[6, 0x9c, 0x40, 0x40, 0x20]),
            Ok(vec![0x4020])
        );
        assert_eq!(response(3, 1, &[0x83, 2]), Err(Exception(2)));
        assert_eq!(response(1, 1, &[0x81, 3]), Err(Exception(3)));
        // a byte count that disagrees with the quantity, a short or long frame, another function
        for (fc, qty, pdu) in [
            (3u8, 2u16, &[3u8, 2, 0, 1][..]),
            (3, 1, &[3, 2, 0]),
            (3, 1, &[3, 2, 0, 1, 9]),
            (1, 9, &[1, 1, 0xff]),
            (1, 1, &[1, 2, 1, 0]),
            (3, 1, &[4, 2, 0, 1]),
            (3, 1, &[0x84, 2]),
            (3, 1, &[0x83]),
            (3, 1, &[0x83, 2, 2]),
            (3, 1, &[]),
            (3, 1, &[3]),
            (6, 1, &[6, 0, 0, 1]),
            (5, 1, &[5, 0, 0, 0xff, 0, 0]),
            (16, 1, &[16, 0, 0, 0, 1]),
        ] {
            assert!(
                matches!(response(fc, qty, pdu), Err(Bad(_))),
                "{fc} {qty} {pdu:?}"
            );
        }
    }

    #[test]
    fn a_frame_with_another_transaction_id_is_discarded() {
        let f = |tid, pdu: &[u8]| codec::frame(tid, 1, pdu);
        let mut buf = [
            f(7, &[4, 2, 0, 1]),
            f(9, &[4, 2, 0, 2]),
            f(10, &[4, 2, 0, 3]),
        ]
        .concat();
        assert_eq!(take(&mut buf, 9), Some(Ok(vec![4, 2, 0, 2])));
        assert_eq!(buf, f(10, &[4, 2, 0, 3]), "what follows the answer stays");
        let mut buf = f(7, &[4, 2, 0, 1]);
        assert_eq!(take(&mut buf, 9), None, "still waiting");
        assert!(buf.is_empty());
        let mut part = f(9, &[4, 2, 0, 2]);
        part.truncate(8);
        assert_eq!(take(&mut part, 9), None);
        assert_eq!(part.len(), 8);
        let mut bad = vec![0, 9, 0, 1, 0, 3, 1, 4, 2];
        assert!(matches!(take(&mut bad, 9), Some(Err(_))), "not Modbus/TCP");
    }

    #[test]
    fn a_bennu_write_scales_like_stock_through_32_bits_without_a_clamp() {
        let r = Range {
            min: 0.0,
            max: 1000.0,
        };
        // conformance/stock/client-modbus.golden: WRITE=load-mw-setpoint:<v> and the FC6 value that left
        for (v, raw) in [
            (500.0, 0x7fff),
            (250.5, 0x4020),
            (-5.0, 0),
            (2000.0, 0xfffe),
            (1e3, 0xffff),
            (0.007, 0),
            (5.0, 0x0147),
            (7.0, 0x01ca),
            (16.0, 0x0418),
            (f64::NAN, 0),
            (f64::INFINITY, 0xffff),
        ] {
            assert_eq!(wrap(&r, v), raw, "{v}");
        }
        assert_eq!(
            wrap(&DEFAULT_RANGE, 5.0),
            0x55c2,
            "WRITE=no-such-register:5"
        );
    }

    fn parse(inner: &str, opts: &Opts) -> Result<(Client, Tags), ConfigError> {
        let xml = format!("<field-device><cycle-time>200</cycle-time><comms><modbus-client>{inner}</modbus-client></comms></field-device>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let node = doc
            .descendants()
            .find(|n| n.has_tag_name("modbus-client"))
            .unwrap();
        let mut tags = Tags::default();
        Client::parse(node, &mut tags, opts).map(|c| (c, tags))
    }

    const CONN: &str = "<modbus-connection><endpoint>tcp://127.0.0.1:1</endpoint>
        <coil><address>0</address><tag>co</tag></coil>
        <discrete-input><address>10000</address><tag>di</tag></discrete-input>
        <holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>hr</tag></holding-register>
        <input-register><address>30000</address><tag>ir</tag></input-register>
        </modbus-connection>";

    #[test]
    fn the_stock_schema_is_read_and_the_points_become_device_local_tags() {
        let (c, tags) = parse(
            &format!("{CONN}<command-interface>tcp://127.0.0.1:1330</command-interface>"),
            &Opts { strict: true },
        )
        .unwrap();
        assert_eq!(c.mode, Mode::Bennu);
        assert_eq!(c.cmdif.as_deref(), Some("tcp://127.0.0.1:1330"));
        assert_eq!(c.cycle, Duration::from_millis(200));
        let conn = &c.conns[0];
        assert_eq!(
            (conn.endpoint.as_str(), conn.unit, &conn.fep),
            ("tcp://127.0.0.1:1", 0, &None)
        );
        let got: Vec<_> = conn
            .points
            .iter()
            .map(|p| (p.tag.as_str(), p.table, p.addr, p.range, p.kind))
            .collect();
        assert_eq!(
            got,
            [
                ("co", Table::Coil, 0, DEFAULT_RANGE, Kind::Binary),
                ("di", Table::Discrete, 10000, DEFAULT_RANGE, Kind::Binary),
                (
                    "hr",
                    Table::Holding,
                    40000,
                    Range {
                        min: 0.0,
                        max: 1000.0
                    },
                    Kind::Analog
                ),
                ("ir", Table::Input, 30000, DEFAULT_RANGE, Kind::Analog),
            ]
        );
        // stock's range map is by address alone
        assert_eq!(conn.ranges.keys().collect::<Vec<_>>(), [&30000, &40000]);
        let shared = CONN.replace("<address>30000</address>", "<address>40000</address>");
        let (c, _) = parse(&shared, &Opts::default()).unwrap();
        assert_eq!(
            c.conns[0].ranges[&40000], DEFAULT_RANGE,
            "the input register's range, declared last"
        );
        assert_eq!(tags.kind("co"), Some(Kind::Binary));
        assert_eq!(tags.kind("hr"), Some(Kind::Analog));
        let (c, _) = parse(
            &CONN.replace("<coil>", "<unit-id>7</unit-id><coil>"),
            &Opts::default(),
        )
        .unwrap();
        assert_eq!(c.conns[0].unit, 7);
    }

    #[test]
    fn bad_keys_are_named_and_a_tag_mapped_twice_keeps_its_first_mapping() {
        let err = |inner: &str| parse(inner, &Opts::default()).err().expect("an error").0;
        assert!(err("<modbus-connection/>").contains("<modbus-connection> has no <endpoint>"));
        assert!(err(&CONN.replace("<address>0</address>", "")).contains("<coil> has no <address>"));
        assert!(err(&CONN.replace("<tag>co</tag>", "")).contains("<coil> has no <tag>"));
        assert!(
            err(&CONN.replace("<address>0</address>", "<address>70000</address>"))
                .contains("not a valid number")
        );
        assert!(err(&CONN.replace("<coil>", "<unit-id>300</unit-id><coil>"))
            .contains("not a valid number"));
        assert_eq!(
            err(&format!("<mode>poll</mode>{CONN}")),
            "<mode> `poll` is not bennu or fep"
        );
        let twice = format!("{CONN}{}", CONN.replace("127.0.0.1:1", "127.0.0.1:2"));
        let (c, _) = parse(&twice, &Opts::default()).unwrap();
        assert_eq!((c.conns[0].points.len(), c.conns[1].points.len()), (4, 0));
        assert!(parse(&twice, &Opts { strict: true }).is_err());
        // stock has no scan-rate: a warning, an error under --strict
        let scan = CONN.replace("<coil>", "<scan-rate>2</scan-rate><coil>");
        assert!(parse(&scan, &Opts::default()).unwrap().0.conns[0]
            .fep
            .is_none());
        assert_eq!(
            parse(&scan, &Opts { strict: true }).err().unwrap().0,
            "<scan-rate> is fep-only, ignored"
        );
    }

    fn pt(table: Table, addr: u16) -> Point {
        Point {
            tag: format!("t{addr}"),
            table,
            addr,
            range: Range {
                min: 0.0,
                max: 100.0,
            },
            kind: Kind::Analog,
        }
    }

    #[test]
    fn reads_merge_contiguous_addresses_up_to_the_protocol_limits() {
        let none = BTreeSet::new();
        let holding = |addrs: &[u16]| {
            addrs
                .iter()
                .map(|a| pt(Table::Holding, *a))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            plan_reads(&holding(&[0, 1, 2, 10]), &none),
            [(3, 0, 3), (3, 10, 1)]
        );
        assert_eq!(
            plan_reads(&holding(&[10, 2, 1, 0, 1]), &none),
            [(3, 0, 3), (3, 10, 1)],
            "any order, one address twice"
        );
        let many: Vec<u16> = (0..130).collect();
        assert_eq!(
            plan_reads(&holding(&many), &none),
            [(3, 0, 125), (3, 125, 5)]
        );
        let coils: Vec<Point> = (0..2001).map(|a| pt(Table::Coil, a)).collect();
        assert_eq!(plan_reads(&coils, &none), [(1, 0, 2000), (1, 2000, 1)]);
        // each table has its own requests, and the last address does not overflow
        let mixed = [
            pt(Table::Input, 5),
            pt(Table::Holding, 5),
            pt(Table::Discrete, 6),
            pt(Table::Coil, 65535),
            pt(Table::Coil, 65534),
        ];
        assert_eq!(
            plan_reads(&mixed, &none),
            [(1, 65534, 2), (2, 6, 1), (3, 5, 1), (4, 5, 1)]
        );
        assert_eq!(plan_reads(&[], &none), []);
        // a split address is read alone, and so is what follows it
        let split = BTreeSet::from([(3, 1)]);
        assert_eq!(
            plan_reads(&holding(&[0, 1, 2]), &split),
            [(3, 0, 1), (3, 1, 1), (3, 2, 1)]
        );
        let split = BTreeSet::from([(3, 0), (3, 1), (3, 2)]);
        assert_eq!(
            plan_reads(&holding(&[0, 1, 2, 3, 4]), &split),
            [(3, 0, 1), (3, 1, 1), (3, 2, 1), (3, 3, 2)]
        );
        assert_eq!(
            plan_reads(&holding(&[0, 1, 2]), &BTreeSet::from([(4, 1)])),
            [(3, 0, 3)],
            "another table's split"
        );
    }

    #[test]
    fn a_fep_value_is_unscaled_through_the_points_range_and_a_binary_register_is_nonzero() {
        let mut p = pt(Table::Holding, 0);
        let Value::Analog(x) = polled(&p, 32767) else {
            panic!()
        };
        assert!((x - 50.0).abs() < 0.01, "{x}");
        assert_eq!(polled(&p, 0), Value::Analog(0.0));
        p.kind = Kind::Binary;
        assert_eq!(polled(&p, 0), Value::Binary(false));
        assert_eq!(polled(&p, 7), Value::Binary(true));
        let coil = Point {
            kind: Kind::Binary,
            ..pt(Table::Coil, 3)
        };
        assert_eq!(polled(&coil, 1), Value::Binary(true));
        // down: clamped by Range::scale; a binary register writes 1 or 0; a coil FC5
        assert_eq!(wire(&p, Value::Binary(true)), Some((6, 1)));
        assert_eq!(wire(&p, Value::Binary(false)), Some((6, 0)));
        p.kind = Kind::Analog;
        assert_eq!(wire(&p, key(&p, Value::Analog(50.0))), Some((6, 32767)));
        assert_eq!(wire(&p, key(&p, Value::Analog(1e9))), Some((6, 65535)));
        assert_eq!(wire(&p, key(&p, Value::Analog(-1.0))), Some((6, 0)));
        assert_eq!(
            wire(&p, key(&p, Value::Analog(f64::NAN))),
            None,
            "NaN is never sent"
        );
        assert_eq!(wire(&coil, Value::Binary(true)), Some((5, 0xFF00)));
        assert_eq!(wire(&coil, Value::Binary(false)), Some((5, 0)));
    }

    #[test]
    fn fep_mode_takes_scan_rate_status_tag_and_type_and_refuses_an_empty_range() {
        let fep = format!(
            "<mode>fep</mode>{}",
            CONN.replace(
                "<coil>",
                "<scan-rate>2</scan-rate><status-tag>up</status-tag><coil>"
            )
        );
        let (c, tags) = parse(&fep, &Opts { strict: true }).unwrap();
        assert_eq!(c.mode, Mode::Fep);
        assert_eq!(
            c.conns[0].fep,
            Some(Fep {
                scan: Duration::from_secs(2),
                status: Some("up".into())
            })
        );
        assert_eq!(tags.kind("up"), Some(Kind::Binary));
        let typed = fep.replace("<tag>hr</tag>", "<tag>hr</tag><type>binary</type>");
        let (c, tags) = parse(&typed, &Opts { strict: true }).unwrap();
        assert_eq!(c.conns[0].points[2].kind, Kind::Binary);
        assert_eq!(tags.kind("hr"), Some(Kind::Binary));
        let e = parse(
            &fep.replace("<tag>hr</tag>", "<tag>hr</tag><type>bool</type>"),
            &Opts::default(),
        )
        .err()
        .unwrap();
        assert!(e.0.contains("<type> `bool` is not analog or binary"), "{e}");
        let flat = fep.replace(
            "<max-value>1000</max-value><min-value>0</min-value>",
            "<max-value>5</max-value><min-value>5</min-value>",
        );
        let e = parse(&flat, &Opts::default()).err().unwrap();
        assert!(
            e.0.contains("`hr`") && e.0.contains("<min-value> equals <max-value>"),
            "{e}"
        );
        // bennu mode keeps stock's tolerance of both
        let lax = CONN.replace(
            "<max-value>1000</max-value><min-value>0</min-value>",
            "<max-value>5</max-value><min-value>5</min-value><type>binary</type>",
        );
        assert_eq!(
            parse(&lax, &Opts { strict: true }).unwrap().0.conns[0].points[2].kind,
            Kind::Analog
        );
    }

    /// Final review, finding 4: the register a value goes down as is also what the echo rule compares, and reading a
    /// register back gives that register again.
    #[test]
    fn a_register_value_and_its_read_back_are_the_same_on_the_wire() {
        for range in [
            Range {
                min: 0.0,
                max: 1000.0,
            },
            Range {
                min: -1000.0,
                max: 2000.0,
            },
            Range {
                min: 0.0,
                max: 100.0,
            },
            Range {
                min: -3.3,
                max: 7.7,
            },
        ] {
            let p = Point {
                range,
                ..pt(Table::Holding, 0)
            };
            for raw in (0..=65535u16).step_by(7).chain([65535]) {
                assert_eq!(
                    key(&p, polled(&p, raw)),
                    Value::Analog(f64::from(raw)),
                    "{range:?} {raw}"
                );
            }
        }
        let p = Point {
            range: Range {
                min: 0.0,
                max: 1000.0,
            },
            ..pt(Table::Holding, 0)
        };
        assert_eq!(key(&p, Value::Analog(3.3333)), Value::Analog(218.0));
        assert_eq!(key(&p, Value::Analog(1e9)), Value::Analog(65535.0));
        assert_eq!(key(&p, Value::Analog(-1.0)), Value::Analog(0.0));
        assert!(matches!(key(&p, Value::Analog(f64::NAN)), Value::Analog(x) if x.is_nan()));
        let coil = Point {
            kind: Kind::Binary,
            ..pt(Table::Coil, 0)
        };
        assert_eq!(key(&coil, Value::Binary(true)), Value::Binary(true));
        let flag = Point {
            kind: Kind::Binary,
            ..pt(Table::Holding, 0)
        };
        assert_eq!(key(&flag, Value::Binary(true)), Value::Binary(true));
    }

    /// Final review, finding 3: when the link fails while several local changes found by a poll go down, none of them is
    /// left marked as under way.
    #[tokio::test]
    async fn a_link_failure_leaves_no_write_marked_as_under_way() {
        let points = vec![pt(Table::Holding, 0), pt(Table::Holding, 1)];
        let store = Store::new(crate::fd::store::Layout {
            internal: points
                .iter()
                .map(|p| (p.tag.clone(), Value::Analog(0.0)))
                .collect(),
            ..Default::default()
        });
        // nothing listens here
        let dead = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let live = Arc::new(Live {
            links: vec![Mutex::new(Link::new(format!("tcp://{dead}"), 1))],
            ranges: vec![BTreeMap::new()],
            points: BTreeMap::new(),
            store: store.clone(),
            log: Arc::new(EventLog::to("modbus-client", Box::new(std::io::sink()))),
        });
        let fep = Fep {
            scan: Duration::from_secs(1),
            status: None,
        };
        let mut session = Session::new(live, 0, points, fep);
        // both tags changed locally and landed; a read of both registers then finds them
        for tag in ["t0", "t1"] {
            store.write(tag, Value::Analog(50.0)).unwrap();
        }
        store.apply_internal();
        let local = session.take(3, 0, &[0, 0]);
        assert_eq!(local.len(), 2);
        assert!(!session.send_all(local).await, "the link is down");
        for tag in ["t0", "t1"] {
            let p = session
                .points
                .iter()
                .find(|p| p.tag == tag)
                .unwrap()
                .clone();
            let due = session
                .outs
                .get_mut(tag)
                .unwrap()
                .due(&store, tag, |v| key(&p, v));
            assert!(due.is_some(), "{tag} is due again, not stuck");
        }
    }
}
