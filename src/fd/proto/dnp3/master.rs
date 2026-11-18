//! DNP3 master (`<dnp3-client>`, protocol plan row 9). This part is the codec: the requests the stock master (opendnp3 under
//! bennu) was seen to send, byte for byte (conformance/stock/client-dnp3-raw.golden), and the decoder for what an outstation
//! answers. The link, transport and application framing are the outstation's (`link`, `transport`, `app`).

use super::super::super::cmdif::{self, Points};
use super::super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::super::net;
use super::super::super::store::Store;
use super::super::client::{self as shared, Fep, Mode, Out};
use super::super::Module;
use super::app::{self, func, CON, FIN, FIR, UNS};
use super::link;
use super::transport::{self, Reassembler};
use crate::model::{Kind, Value};
use roxmltree::Node;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// The link control octet of every frame the master sends: DIR, PRM, unconfirmed user data.
pub const MASTER_DATA: u8 = 0xC4;

/// The object headers of a class poll: group 60, all objects, in the order given (stock's all-classes scan is 1, 2, 3, 0).
pub fn class_read(classes: &[u8]) -> Vec<u8> {
    classes
        .iter()
        .flat_map(|c| [60, if *c == 0 { 1 } else { c + 1 }, 0x06])
        .collect()
}

/// The object of the WRITE that clears IIN1.7: g80v1, index 7, value 0.
pub fn clear_restart() -> Vec<u8> {
    vec![80, 1, 0x00, 7, 7, 0]
}

/// A CROB as stock sends it: g12v1, one object with a two-octet index, LATCH_ON (3) or LATCH_OFF (4), count 1, 100 ms on and
/// off, status 0.
pub fn crob(idx: u16, on: bool) -> Vec<u8> {
    let mut o = vec![12, 1, 0x28, 1, 0];
    o.extend_from_slice(&idx.to_le_bytes());
    o.extend_from_slice(&[if on { 3 } else { 4 }, 1]);
    o.extend_from_slice(&100u32.to_le_bytes());
    o.extend_from_slice(&100u32.to_le_bytes());
    o.push(0);
    o
}

/// An analog output as stock sends it, whatever the point: g41v3 (float32), status 0.
pub fn ao_f32(idx: u16, v: f64) -> Vec<u8> {
    let mut o = vec![41, 3, 0x28, 1, 0];
    o.extend_from_slice(&idx.to_le_bytes());
    // stock's NaN leaves as 7f800001, not the quiet NaN a cast gives
    let bits = if v.is_nan() {
        0x7f80_0001
    } else {
        (v as f32).to_bits()
    };
    o.extend_from_slice(&bits.to_le_bytes());
    o.push(0);
    o
}

/// The object headers of an output-status read (`fep` mode only): every binary and analog output, default variation.
pub fn output_status() -> Vec<u8> {
    vec![10, 0, 0x06, 40, 0, 0x06]
}

/// The confirmation of a response that asked for one.
pub fn confirm(seq: u8, unsol: bool) -> Vec<u8> {
    vec![
        FIR | FIN | if unsol { UNS } else { 0 } | (seq & 0x0F),
        func::CONFIRM,
    ]
}

/// A point value in a response. Stock's handler takes `Binary` (groups 1 and 2) and `Analog` (30 and 32) and ignores the
/// rest; the output statuses (10, 11, 40, 42) are for `fep` mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seen {
    Binary(u16, bool),
    Analog(u16, f64),
    BinaryOutput(u16, bool),
    AnalogOutput(u16, f64),
}

/// A response that cannot be read to its end; nothing of it is used.
#[derive(Debug, PartialEq, Eq)]
pub enum Bad {
    Truncated,
    Qualifier(u8),
    /// A group and variation whose size is not known, so what follows cannot be found.
    Object(u8, u8),
    NotAResponse,
}

/// A decoded response fragment.
#[derive(Debug, PartialEq)]
pub struct Response {
    pub seq: u8,
    pub iin: u16,
    /// An unsolicited response (function 130).
    pub unsol: bool,
    /// The outstation wants a confirmation.
    pub con: bool,
    pub fin: bool,
    pub seen: Vec<Seen>,
    /// The object headers and data as received: a command's echo.
    pub objects: Vec<u8>,
}

impl Response {
    /// IIN1.7: the outstation restarted.
    pub fn restart(&self) -> bool {
        self.iin & app::iin::RESTART != 0
    }
}

/// How one object of a group and variation lies in a response.
enum Size {
    /// Packed, this many bits each.
    Bits(usize),
    Bytes(usize),
}

/// What one object takes in a response, in octets: (groups, variations, octets).
const SIZES: &[(&[u8], &[u8], usize)] = &[
    (&[1, 3, 10], &[2], 1),
    (&[2, 4, 11, 13], &[1], 1),
    (&[2, 4, 11, 13], &[2], 7),
    (&[2, 4], &[3], 3),
    (&[12], &[1], 11),
    // counters: flag and 32 or 16 bits, without the flag (20 and 21 only), with a time
    (&[20, 21, 22, 23], &[1], 5),
    (&[20, 21, 22, 23], &[2], 3),
    (&[20], &[5], 4),
    (&[21], &[9], 4),
    (&[20], &[6], 2),
    (&[21], &[10], 2),
    (&[21, 22, 23], &[5], 11),
    (&[21, 22, 23], &[6], 9),
    // analog inputs, their events, output status, output commands and output events
    (&[30, 32, 40, 41, 42], &[1], 5),
    (&[30, 32, 42], &[5], 5),
    (&[40, 41], &[3], 5),
    (&[30, 32, 40, 41, 42], &[2], 3),
    (&[30], &[3], 4),
    (&[30], &[4], 2),
    (&[30, 32, 42], &[6], 9),
    (&[32, 42], &[4], 9),
    (&[40, 41], &[4], 9),
    (&[32, 42], &[3, 7], 11),
    (&[32, 42], &[8], 15),
    (&[50, 51], &[1, 2], 6),
    (&[52], &[1, 2], 2),
];

fn size(group: u8, var: u8) -> Option<Size> {
    match (group, var) {
        (1 | 10 | 80, 1) => return Some(Size::Bits(1)),
        (3, 1) => return Some(Size::Bits(2)),
        // an octet string is as long as its variation says
        (110 | 111, 1..) => return Some(Size::Bytes(usize::from(var))),
        _ => {}
    }
    let row = SIZES
        .iter()
        .find(|(g, v, _)| g.contains(&group) && v.contains(&var));
    row.map(|(_, _, octets)| Size::Bytes(*octets))
}

/// Where the number is in an analog object: (groups, variations, offset, octets, floating point).
type Number = (&'static [u8], &'static [u8], usize, usize, bool);

/// The first row that fits.
const NUMBERS: &[Number] = &[
    (&[30], &[3], 0, 4, false),
    (&[30], &[4], 0, 2, false),
    (&[40], &[3], 1, 4, true),
    (&[40], &[4], 1, 8, true),
    (&[30, 32, 42], &[5, 7], 1, 4, true),
    (&[30, 32, 42], &[6, 8], 1, 8, true),
    (&[30, 32, 40, 42], &[2], 1, 2, false),
    (&[32, 42], &[4], 1, 2, false),
    (&[30, 32, 40, 42], &[1, 3], 1, 4, false),
];

/// A little-endian number of 2 or 4 octets (an integer), or of 4 or 8 (floating point).
fn number_in(b: &[u8], float: bool) -> f64 {
    match (b, float) {
        (&[a, b], _) => f64::from(i16::from_le_bytes([a, b])),
        (&[a, b, c, d], false) => f64::from(i32::from_le_bytes([a, b, c, d])),
        (&[a, b, c, d], true) => f64::from(f32::from_le_bytes([a, b, c, d])),
        (&[a, b, c, d, e, f, g, h], _) => f64::from_le_bytes([a, b, c, d, e, f, g, h]),
        _ => 0.0,
    }
}

/// The value in one byte-sized object `b` of group `g`, variation `v`, when it is a point value.
fn value(g: u8, v: u8, idx: u16, b: &[u8]) -> Option<Seen> {
    let state = b[0] & 0x80 != 0;
    let row = NUMBERS
        .iter()
        .find(|(gs, vs, ..)| gs.contains(&g) && vs.contains(&v));
    let analog = row.map_or(0.0, |&(_, _, at, octets, float)| {
        number_in(&b[at..at + octets], float)
    });
    match g {
        1 | 2 => Some(Seen::Binary(idx, state)),
        10 | 11 => Some(Seen::BinaryOutput(idx, state)),
        30 | 32 => Some(Seen::Analog(idx, analog)),
        40 | 42 => Some(Seen::AnalogOutput(idx, analog)),
        _ => None,
    }
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8], Bad> {
    if buf.len() < n {
        return Err(Bad::Truncated);
    }
    let (head, rest) = buf.split_at(n);
    *buf = rest;
    Ok(head)
}

fn uint(buf: &mut &[u8], width: usize) -> Result<usize, Bad> {
    Ok(take(buf, width)?
        .iter()
        .rev()
        .fold(0, |a, &x| a << 8 | usize::from(x)))
}

/// What a qualifier says about the objects that follow: (first index, count, octets of index prefix).
fn range(buf: &mut &[u8], q: u8) -> Result<(usize, usize, usize), Bad> {
    // (qualifier, octets of each number, a start and a stop instead of a count, octets of index prefix)
    const FORMS: [(u8, usize, bool, usize); 6] = [
        (0x00, 1, true, 0),
        (0x01, 2, true, 0),
        (0x07, 1, false, 0),
        (0x08, 2, false, 0),
        (0x17, 1, false, 1),
        (0x28, 2, false, 2),
    ];
    let form = FORMS.iter().find(|f| f.0 == q).ok_or(Bad::Qualifier(q))?;
    let &(_, width, start_stop, prefix) = form;
    let first = uint(buf, width)?;
    if !start_stop {
        return Ok((0, first, prefix));
    }
    let count = (uint(buf, width)? + 1).checked_sub(first);
    Ok((first, count.ok_or(Bad::Truncated)?, prefix))
}

/// `count` packed objects of `each` bits from index `start`.
fn packed(
    buf: &mut &[u8],
    g: u8,
    each: usize,
    (start, count): (usize, usize),
    seen: &mut Vec<Seen>,
) -> Result<(), Bad> {
    let data = take(buf, (count * each).div_ceil(8))?;
    for i in 0..count {
        let on = data[i * each / 8] >> (i * each % 8) & 1 == 1;
        match g {
            1 => seen.push(Seen::Binary((start + i) as u16, on)),
            10 => seen.push(Seen::BinaryOutput((start + i) as u16, on)),
            _ => {}
        }
    }
    Ok(())
}

/// `count` objects of `each` octets of group and variation `gv`, indexed from `start` or each by its own prefix.
fn sized(
    buf: &mut &[u8],
    gv: (u8, u8),
    each: usize,
    (start, count, prefix): (usize, usize, usize),
    seen: &mut Vec<Seen>,
) -> Result<(), Bad> {
    for i in 0..count {
        let idx = match prefix {
            0 => start + i,
            w => uint(buf, w)?,
        };
        seen.extend(value(gv.0, gv.1, idx as u16, take(buf, each)?));
    }
    Ok(())
}

/// One object header and its objects, off the front of `buf`.
fn objects(buf: &mut &[u8], seen: &mut Vec<Seen>) -> Result<(), Bad> {
    let head = take(buf, 3)?;
    let (g, v, q) = (head[0], head[1], head[2]);
    let (start, count, prefix) = range(buf, q)?;
    match (size(g, v).ok_or(Bad::Object(g, v))?, prefix) {
        (Size::Bits(each), 0) => packed(buf, g, each, (start, count), seen),
        (Size::Bits(_), _) => Err(Bad::Qualifier(q)),
        (Size::Bytes(each), _) => sized(buf, (g, v), each, (start, count, prefix), seen),
    }
}

/// Decode a response fragment (solicited or unsolicited). All of it must parse: a fragment with a truncated object, a count
/// past its end or an object of unknown size gives nothing. Never panics on input.
pub fn decode(fragment: &[u8]) -> Result<Response, Bad> {
    let mut buf = fragment;
    let head = take(&mut buf, 2)?;
    let unsol = match head[1] {
        func::RESPONSE => false,
        130 => true,
        _ => return Err(Bad::NotAResponse),
    };
    let iin = take(&mut buf, 2)?;
    let mut r = Response {
        seq: head[0] & 0x0F,
        iin: u16::from_le_bytes([iin[0], iin[1]]),
        unsol,
        con: head[0] & CON != 0,
        fin: head[0] & FIN != 0,
        seen: vec![],
        objects: buf.to_vec(),
    };
    while !buf.is_empty() {
        objects(&mut buf, &mut r.seen)?;
    }
    Ok(r)
}

// ---- the module: config, one session per connection, the command interface ------------------------------------------------

/// opendnp3's defaults, which the stock master runs with: how long a response may take, how long a failed startup task waits
/// before it is tried again, and the reconnect pause (doubling from the first to the cap).
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
/// `fep` mode: an outstation that has not answered within this long is taken for gone and the connection is made anew.
const FEP_TIMEOUT: Duration = Duration::from_millis(1000);
const TASK_RETRY: Duration = Duration::from_secs(5);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Stock's all-classes scan asks in this order.
const ALL_CLASSES: [u8; 4] = [1, 2, 3, 0];

/// One mapped point.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub tag: String,
    pub addr: u16,
    pub kind: Kind,
    pub output: bool,
    /// Select-before-operate, an output's `<sbo>`.
    pub sbo: bool,
}

pub struct Conn {
    pub endpoint: String,
    /// The outstation's link address.
    pub remote: u16,
    pub points: Vec<Point>,
    /// The recurring class scans: the classes of each and its period.
    pub scans: Vec<(Vec<u8>, Duration)>,
    pub fep: Option<Fep>,
}

pub struct Client {
    pub mode: Mode,
    /// The master's link address.
    pub local: u16,
    pub conns: Vec<Conn>,
    pub cmdif: Option<String>,
    pub cycle: Duration,
}

const KINDS: [(&str, Kind, bool); 4] = [
    ("binary-input", Kind::Binary, false),
    ("binary-output", Kind::Binary, true),
    ("analog-input", Kind::Analog, false),
    ("analog-output", Kind::Analog, true),
];

fn needed<T: std::str::FromStr>(n: Node, key: &str) -> Result<T, ConfigError> {
    number::<T>(n, key)?
        .ok_or_else(|| ConfigError(format!("<{}> has no <{key}>", n.tag_name().name())))
}

/// The points of a connection, in stock's order: binary inputs, binary outputs, analog inputs, analog outputs.
fn points(c: Node, tags: &mut Tags, opts: &Opts) -> Result<Vec<Point>, ConfigError> {
    let mut points = vec![];
    for (name, kind, output) in KINDS {
        for n in c.children().filter(|n| n.has_tag_name(name)) {
            let (addr, tag) = (needed::<u16>(n, "address")?, require(n, "tag")?);
            let sbo = output && super::flag(n, "sbo")?;
            if shared::point_tag(tags, &tag, kind, opts)? {
                points.push(Point {
                    tag,
                    addr,
                    kind,
                    output,
                    sbo,
                });
            }
        }
    }
    Ok(points)
}

/// The recurring scans of a connection: the all-classes scan at the client's `rate` unless `<class-scan-rates>` says otherwise,
/// and a scan per class that has a rate. 0 is off.
fn scans(c: Node, rate: u32) -> Result<Vec<(Vec<u8>, Duration)>, ConfigError> {
    let rates = super::super::super::config::child(c, "class-scan-rates");
    let rate_of = |key: &str, default: u32| -> Result<u32, ConfigError> {
        Ok(match rates {
            Some(r) => number::<u32>(r, key)?.unwrap_or(default),
            None => default,
        })
    };
    let mut scans = vec![(ALL_CLASSES.to_vec(), rate_of("all", rate)?)];
    for class in 0..4u8 {
        scans.push((vec![class], rate_of(&format!("class{class}"), 0)?));
    }
    let on = scans.into_iter().filter(|(_, secs)| *secs != 0);
    Ok(on
        .map(|(classes, secs)| (classes, Duration::from_secs(u64::from(secs))))
        .collect())
}

fn connection(
    c: Node,
    rate: u32,
    mode: Mode,
    tags: &mut Tags,
    opts: &Opts,
) -> Result<Conn, ConfigError> {
    let endpoint = require(c, "endpoint")?;
    let remote = needed::<u16>(c, "address")?;
    let fep = shared::fep_keys(c, mode, tags, opts)?;
    Ok(Conn {
        endpoint,
        remote,
        points: points(c, tags, opts)?,
        scans: scans(c, rate)?,
        fep,
    })
}

impl Module for Client {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let mode = shared::mode(node)?;
        let local = needed::<u16>(node, "address")?;
        let rate = needed::<u32>(node, "scan-rate")?;
        let conns = node
            .children()
            .filter(|c| c.has_tag_name("dnp3-connection"))
            .map(|c| connection(c, rate, mode, tags, opts))
            .collect::<Result<_, _>>()?;
        Ok(Client {
            mode,
            local,
            conns,
            cmdif: text(node, "command-interface"),
            cycle: shared::cycle(node),
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let (mut by_tag, mut outputs) = (BTreeMap::new(), vec![]);
        for (i, c) in self.conns.into_iter().enumerate() {
            let (tx, rx) = mpsc::unbounded_channel();
            let link = Arc::new(Shared::new(&c.points, store.clone(), tx));
            by_tag.extend(c.points.iter().map(|p| (p.tag.clone(), link.clone())));
            outputs.extend(c.points.iter().map(|p| (p.tag.clone(), p.output)));
            let fep = c.fep.map(|keys| FepState {
                status: keys.status,
                cycle: self.cycle,
                outs: (c.points.iter().filter(|p| p.output))
                    .map(|p| (p.tag.clone(), Out::new(crate::fd::store::zero(p.kind))))
                    .collect(),
                todo: vec![],
                next_tick: Instant::now(),
                up: false,
            });
            let session = Session {
                shared: link,
                name: format!("connection {} ({})", i + 1, c.endpoint),
                endpoint: c.endpoint,
                local: self.local,
                remote: c.remote,
                rx,
                tseq: 0,
                integrity: true,
                clear: false,
                clear_failed: false,
                hold: Instant::now(),
                due: c.scans.iter().map(|_| Instant::now()).collect(),
                scans: c.scans,
                timeout: if fep.is_some() {
                    FEP_TIMEOUT
                } else {
                    RESPONSE_TIMEOUT
                },
                fep,
                answered: false,
            };
            // one task per connection: a slow or dead outstation stalls neither the scan cycle nor the other connections
            tokio::spawn(session.run(stop.clone()));
        }
        match (&self.cmdif, self.mode) {
            (Some(ep), Mode::Bennu) => cmdif::serve(ep, Bennu(by_tag), stop).await,
            (Some(ep), Mode::Fep) => {
                cmdif::serve(ep, shared::TagPoints::new(store, outputs), stop).await
            }
            (None, _) => stop.cancelled().await,
        }
    }
}

/// What `fep` mode adds to a session: the echo state of its outputs, and when it next looks for local changes.
struct FepState {
    status: Option<String>,
    cycle: Duration,
    outs: BTreeMap<String, Out>,
    /// Local changes a poll ran into: sent before anything else.
    todo: Vec<(String, Value)>,
    next_tick: Instant,
    up: bool,
}

/// A command for a session to send.
struct Command {
    /// SELECT then OPERATE, else DIRECT_OPERATE.
    sbo: bool,
    objects: Vec<u8>,
    /// The value is a NaN: stock compares the select's echo as numbers, a NaN never equals itself, and the operate is not sent.
    nan: bool,
}

/// What a connection's session and the command interface share.
struct Shared {
    store: Store,
    points: BTreeMap<String, Point>,
    /// Stock's address maps, one per kind, inputs and outputs together: a later point at an address takes it over.
    bin: BTreeMap<u16, String>,
    ana: BTreeMap<u16, String>,
    /// Stock's register cache: what a READ answers. The tags mirror it at the end of each scan cycle.
    cache: std::sync::Mutex<BTreeMap<String, Value>>,
    /// `fep` mode: (binary, output, index) -> tag. An input and an output at one index are two points.
    by_index: BTreeMap<(bool, bool, u16), String>,
    tx: mpsc::UnboundedSender<Command>,
}

/// An analog as the wire carries it down: a float32. The form the echo rule compares in `fep` mode.
fn float32(v: Value) -> Value {
    match v {
        Value::Analog(x) => Value::Analog(f64::from(x as f32)),
        Value::Binary(_) => v,
    }
}

impl Shared {
    fn new(points: &[Point], store: Store, tx: mpsc::UnboundedSender<Command>) -> Shared {
        let map = |kind| -> BTreeMap<u16, String> {
            let of_kind = points.iter().filter(|p| p.kind == kind);
            of_kind.map(|p| (p.addr, p.tag.clone())).collect()
        };
        Shared {
            store,
            bin: map(Kind::Binary),
            ana: map(Kind::Analog),
            cache: std::sync::Mutex::new(
                points
                    .iter()
                    .map(|p| (p.tag.clone(), crate::fd::store::zero(p.kind)))
                    .collect(),
            ),
            by_index: (points.iter())
                .map(|p| ((p.kind == Kind::Binary, p.output, p.addr), p.tag.clone()))
                .collect(),
            points: points.iter().map(|p| (p.tag.clone(), p.clone())).collect(),
            tx,
        }
    }

    fn set(&self, tag: Option<&String>, v: Value) {
        let Some(tag) = tag else { return };
        crate::hint::lock(&self.cache).insert(tag.clone(), v);
        let _ = self.store.write(tag, v);
    }

    fn set_binary(&self, addr: u16, on: bool) {
        self.set(self.bin.get(&addr), Value::Binary(on));
    }

    /// Stock keeps an analog in a float.
    fn set_analog(&self, addr: u16, x: f64) {
        self.set(self.ana.get(&addr), Value::Analog(f64::from(x as f32)));
    }

    /// `fep` mode: the tag a value in a response belongs to. An input value goes to the input at that index; when there is
    /// none it goes to the output there, for an outstation that reports its outputs as inputs (ferrofire's own does).
    fn fep_tag(&self, s: &Seen) -> Option<(&String, Value)> {
        let (binary, output, i, v) = match *s {
            Seen::Binary(i, on) => (true, false, i, Value::Binary(on)),
            Seen::BinaryOutput(i, on) => (true, true, i, Value::Binary(on)),
            Seen::Analog(i, x) => (false, false, i, Value::Analog(x)),
            Seen::AnalogOutput(i, x) => (false, true, i, Value::Analog(x)),
        };
        let at = |output| self.by_index.get(&(binary, output, i));
        let tag = at(output).or_else(|| at(true));
        tag.map(|t| (t, v))
    }

    /// Stock's SOE handler: binary inputs and analog inputs by index, into whichever tag has that address; the rest is ignored.
    fn take(&self, r: &Response) {
        for s in &r.seen {
            match *s {
                Seen::Binary(i, on) => self.set_binary(i, on),
                Seen::Analog(i, x) => self.set_analog(i, x),
                Seen::BinaryOutput(..) | Seen::AnalogOutput(..) => {}
            }
        }
    }
}

/// The command interface of a `bennu` mode master: a READ answers from the cache and never fails; a WRITE updates the cache
/// first and hands the command to the session, which drops it when the outstation is away. The reply is always an ACK. Stock.
struct Bennu(BTreeMap<String, Arc<Shared>>);

impl Points for Bennu {
    fn names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    async fn read(&self, tag: &str) -> Result<Value, String> {
        let link = self
            .0
            .get(tag)
            .ok_or("readRegisterByTag(): Unable to find tag")?;
        let cache = crate::hint::lock(&link.cache);
        cache
            .get(tag)
            .copied()
            .ok_or_else(|| "readRegisterByTag(): Unable to find tag".into())
    }

    /// `true`/`false` is a CROB and a number an analog output at the point's address, whatever the point is (an input too).
    async fn write(&self, tag: &str, v: Value) -> Result<(), String> {
        let link = self.0.get(tag).ok_or("writeBinary(): Unable to find tag")?;
        let p = &link.points[tag];
        let (objects, nan) = match v {
            Value::Binary(on) => {
                link.set_binary(p.addr, on);
                (crob(p.addr, on), false)
            }
            Value::Analog(x) => {
                link.set_analog(p.addr, x);
                (ao_f32(p.addr, x), x.is_nan())
            }
        };
        let _ = link.tx.send(Command {
            sbo: p.sbo,
            objects,
            nan,
        });
        Ok(())
    }
}

/// The connection is gone.
struct Lost;

/// One connection's socket: link frames out, application fragments in.
struct Wire {
    stream: TcpStream,
    local: u16,
    remote: u16,
    buf: Vec<u8>,
    asm: Reassembler,
}

impl Wire {
    async fn send(&mut self, tseq: &mut u8, fragment: &[u8]) -> Result<(), Lost> {
        let mut out = vec![];
        for seg in transport::segment(fragment, tseq) {
            out.extend(link::encode(MASTER_DATA, self.remote, self.local, &seg));
        }
        self.stream.write_all(&out).await.map_err(|_| Lost)
    }

    /// The next application fragment the outstation sent to this master. Other frames (another address, link-layer requests)
    /// are stepped over. Cancel-safe: bytes read stay in the buffer.
    /// ponytail: link-layer requests (link status, reset) are not answered; opendnp3 answers them. Add when an outstation needs it.
    async fn recv(&mut self) -> Result<Vec<u8>, Lost> {
        loop {
            if let Some(r) = self.buffered() {
                return r;
            }
            let mut chunk = [0u8; 1024];
            match self.stream.read(&mut chunk).await {
                Ok(n @ 1..) => self.buf.extend_from_slice(&chunk[..n]),
                _ => return Err(Lost),
            }
        }
    }

    /// The next fragment the frames already in the buffer complete; `None`: more bytes are needed.
    fn buffered(&mut self) -> Option<Result<Vec<u8>, Lost>> {
        loop {
            let (f, used) = match link::parse(&self.buf) {
                link::Parse::Frame(f, used) => (f, used),
                link::Parse::Need(_) => return None,
                link::Parse::Bad(why) => {
                    tracing::warn!("dnp3-client: closing the connection: {why}");
                    return Some(Err(Lost));
                }
            };
            self.buf.drain(..used);
            // unconfirmed user data from our outstation to us
            let ours = f.primary() && f.func() == 4 && f.dst == self.local && f.src == self.remote;
            if let (true, Ok(Some(fragment))) = (ours, self.asm.push(&f.user)) {
                return Some(Ok(fragment));
            }
        }
    }
}

/// No response carries this sequence number: what `answer` is told to await when nothing is.
const NOTHING_AWAITED: u8 = 0xFF;

/// One connection of the master: the stock task order (a command, clear-restart, the startup integrity poll, the scan that is
/// due) over one request at a time.
struct Session {
    shared: Arc<Shared>,
    name: String,
    endpoint: String,
    local: u16,
    remote: u16,
    rx: mpsc::UnboundedReceiver<Command>,
    /// The transport sequence: it runs on across reconnects (stock); the application sequence restarts at 0.
    tseq: u8,
    /// The class 0 poll: wanted at start, and again whenever a response says the outstation restarted.
    integrity: bool,
    /// The write that clears IIN1.7 is wanted.
    clear: bool,
    /// The outstation kept IIN1.7 through a clear: it is not asked again (opendnp3 disables the task for good).
    clear_failed: bool,
    /// A startup task that got no answer waits until here.
    hold: Instant,
    scans: Vec<(Vec<u8>, Duration)>,
    /// When each scan is next due.
    due: Vec<Instant>,
    /// How long a response may take.
    timeout: Duration,
    fep: Option<FepState>,
    /// The connection in use has brought at least one response.
    answered: bool,
}

enum Task {
    Command(Command),
    Clear,
    Integrity,
    Scan(usize),
}

impl Session {
    async fn run(mut self, stop: CancellationToken) {
        // stock: an attempt at once, then 1, 2, 4 ... 60 s apart; a session that was up is retried at once
        let (mut wait, mut pause) = (Duration::ZERO, RETRY_MIN);
        loop {
            tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(wait) => {} }
            self.answered = false;
            let stream = tokio::time::timeout(RESPONSE_TIMEOUT, net::connect(&self.endpoint)).await;
            if let Ok(Ok(stream)) = stream {
                let _ = stream.set_nodelay(true);
                tracing::info!("dnp3-client: {}: connected", self.name);
                tokio::select! { _ = stop.cancelled() => return, _ = self.session(stream) => {} }
                tracing::warn!("dnp3-client: {}: connection lost", self.name);
                self.went_down();
            }
            // ponytail: a connection that never brought a response (a relay in front of a dead outstation, a silent peer)
            // backs off like a refused one, where opendnp3 would reconnect at once, in a tight loop
            (wait, pause) = match self.answered {
                true => (Duration::ZERO, RETRY_MIN),
                false => (pause, (pause * 2).min(RETRY_MAX)),
            };
        }
    }

    /// The next thing to send, by stock's priorities.
    fn next(&mut self) -> Option<Task> {
        if let Ok(c) = self.rx.try_recv() {
            return Some(Task::Command(c));
        }
        let now = Instant::now();
        match (now >= self.hold, self.clear, self.integrity) {
            (true, true, _) => return Some(Task::Clear),
            (true, _, true) => return Some(Task::Integrity),
            _ => {}
        }
        let first = (0..self.due.len()).min_by_key(|i| self.due[*i])?;
        (self.due[first] <= now).then_some(Task::Scan(first))
    }

    /// When `next` will have something without a command arriving.
    fn wake(&self) -> Instant {
        let scan = self.due.iter().min().copied();
        let startup = (self.clear || self.integrity).then_some(self.hold);
        let tick = self.fep.as_ref().map(|f| f.next_tick);
        scan.into_iter()
            .chain(startup)
            .chain(tick)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600))
    }

    /// Serve one connection until it is lost.
    async fn session(&mut self, stream: TcpStream) {
        let mut wire = Wire {
            stream,
            local: self.local,
            remote: self.remote,
            buf: vec![],
            asm: Reassembler::new(app::MAX_APDU),
        };
        let mut seq = 0u8;
        // what was asked for while the outstation was away is not sent late
        while self.rx.try_recv().is_ok() {}
        if let Some(fep) = &mut self.fep {
            fep.next_tick = Instant::now(); // what changed while the outstation was away goes down before anything is read
        }
        loop {
            if self.write_down(&mut wire, &mut seq).await.is_err() {
                return;
            }
            let Some(task) = self.next() else {
                let wake = self.wake();
                tokio::select! {
                    c = self.rx.recv() => match c {
                        Some(c) => if self.run_task(&mut wire, &mut seq, Task::Command(c)).await.is_err() { return },
                        None => return,
                    },
                    _ = tokio::time::sleep_until(wake) => {}
                    // nothing is awaited: an unsolicited response is taken, anything else discarded
                    r = wire.recv() => match r {
                        Ok(fragment) => if self.answer(&mut wire, &fragment, &mut { NOTHING_AWAITED }, 0).await.is_err() { return },
                        Err(Lost) => return,
                    },
                }
                continue;
            };
            if self.run_task(&mut wire, &mut seq, task).await.is_err() {
                return;
            }
        }
    }

    async fn run_task(&mut self, wire: &mut Wire, seq: &mut u8, task: Task) -> Result<(), Lost> {
        match task {
            Task::Command(c) => self.command(wire, seq, &c).await.map(|_| ()),
            Task::Clear => self.clear_restart(wire, seq).await,
            Task::Integrity => self.integrity_poll(wire, seq).await,
            Task::Scan(i) => self.scan(wire, seq, i).await,
        }
    }

    async fn clear_restart(&mut self, wire: &mut Wire, seq: &mut u8) -> Result<(), Lost> {
        let r = self
            .exchange(wire, seq, func::WRITE, &clear_restart())
            .await?;
        match r {
            Some(r) => {
                self.clear = false;
                if r.restart() {
                    tracing::warn!("dnp3-client: {}: the outstation kept IIN1.7 through a clear-restart; not asking again", self.name);
                    self.clear_failed = true;
                }
            }
            None => self.hold = Instant::now() + TASK_RETRY,
        }
        Ok(())
    }

    async fn integrity_poll(&mut self, wire: &mut Wire, seq: &mut u8) -> Result<(), Lost> {
        // done once answered; a response that shows a restart asks for it again (`absorb`)
        self.integrity = false;
        match self
            .exchange(wire, seq, func::READ, &class_read(&[0]))
            .await?
        {
            Some(_) => self.polled(wire, seq).await?,
            None => {
                self.integrity = true;
                self.hold = Instant::now() + TASK_RETRY;
            }
        }
        Ok(())
    }

    async fn scan(&mut self, wire: &mut Wire, seq: &mut u8, i: usize) -> Result<(), Lost> {
        let objects = class_read(&self.scans[i].0);
        if self
            .exchange(wire, seq, func::READ, &objects)
            .await?
            .is_some()
        {
            self.polled(wire, seq).await?;
        }
        self.due[i] = Instant::now() + self.scans[i].1;
        Ok(())
    }

    /// Send a command: DIRECT_OPERATE, or SELECT and, when the outstation echoes it unchanged, OPERATE. `true`: the outstation
    /// took it (its echo carries status 0).
    async fn command(&mut self, wire: &mut Wire, seq: &mut u8, c: &Command) -> Result<bool, Lost> {
        let func = match c.sbo {
            true => func::SELECT,
            false => func::DIRECT_OPERATE,
        };
        let echoed = |r: &Option<Response>| r.as_ref().is_some_and(|r| r.objects == c.objects);
        let first = self.exchange(wire, seq, func, &c.objects).await?;
        if !c.sbo || !echoed(&first) || c.nan {
            return Ok(!c.sbo && echoed(&first));
        }
        let second = self.exchange(wire, seq, func::OPERATE, &c.objects).await?;
        Ok(echoed(&second))
    }

    /// `fep` mode, after a scan was answered: the session is up, and the outputs' status is read as well, so every output
    /// has a polled value (an outstation that reports outputs only as output status, like stock v2026.11.03, gives none in
    /// a class scan).
    async fn polled(&mut self, wire: &mut Wire, seq: &mut u8) -> Result<(), Lost> {
        if self.fep.is_none() {
            return Ok(());
        }
        self.exchange(wire, seq, func::READ, &output_status())
            .await?;
        self.set_up(true);
        Ok(())
    }

    fn set_up(&mut self, up: bool) {
        let Some(fep) = &mut self.fep else { return };
        if fep.up != up {
            fep.up = up;
            if let Some(tag) = &fep.status {
                let _ = self.shared.store.write(tag, Value::Binary(up));
            }
        }
    }

    /// The connection is gone: the status says so, and a write that was waiting to leave is due again.
    fn went_down(&mut self) {
        self.set_up(false);
        if let Some(fep) = &mut self.fep {
            for (tag, _) in fep.todo.drain(..) {
                if let Some(out) = fep.outs.get_mut(&tag) {
                    out.echo.sent_failed();
                }
            }
        }
    }

    /// `fep` mode: send the local changes a poll ran into and, once per cycle, every output tag's local change.
    async fn write_down(&mut self, wire: &mut Wire, seq: &mut u8) -> Result<(), Lost> {
        let store = self.shared.store.clone();
        let Some(fep) = &mut self.fep else {
            return Ok(());
        };
        if Instant::now() >= fep.next_tick {
            fep.next_tick = Instant::now() + fep.cycle;
            for (tag, out) in fep.outs.iter_mut() {
                let due = out.due(&store, tag, float32);
                fep.todo.extend(due.map(|v| (tag.clone(), v)));
            }
        }
        while let Some((tag, v)) = self.fep.as_mut().and_then(|f| f.todo.pop()) {
            self.send_down(wire, seq, &tag, v).await?;
        }
        Ok(())
    }

    /// Command output `tag` to `v` and tell its echo state how it went.
    async fn send_down(
        &mut self,
        wire: &mut Wire,
        seq: &mut u8,
        tag: &str,
        v: Value,
    ) -> Result<(), Lost> {
        let p = &self.shared.points[tag];
        let (objects, nan) = match v {
            Value::Binary(on) => (crob(p.addr, on), false),
            Value::Analog(x) => (ao_f32(p.addr, x), x.is_nan()),
        };
        let c = Command {
            sbo: p.sbo,
            objects,
            nan,
        };
        // a NaN is never sent; a command the outstation refuses is not repeated: the next poll shows what it holds
        let sent = match nan {
            true => Ok(false),
            false => self.command(wire, seq, &c).await,
        };
        if matches!(sent, Ok(false)) {
            tracing::warn!(
                "dnp3-client: {}: tag `{tag}` = {v:?} was not taken by the outstation",
                self.name
            );
        }
        if let Some(out) = self.fep.as_mut().and_then(|f| f.outs.get_mut(tag)) {
            match sent {
                Ok(_) => out.echo.sent_ok(v),
                Err(Lost) => out.echo.sent_failed(),
            }
        }
        sent.map(|_| ())
    }

    /// The points of a response. `bennu` mode: stock's handler, into the cache. `fep` mode: each value into the tag of its
    /// point (`Shared::fep_tag`), an output's through its echo state.
    fn take(&mut self, r: &Response) {
        let Some(fep) = &mut self.fep else {
            return self.shared.take(r);
        };
        let store = &self.shared.store;
        for s in &r.seen {
            let Some((tag, v)) = self.shared.fep_tag(s) else {
                continue;
            };
            match fep.outs.get_mut(tag) {
                Some(out) => {
                    let local = out.polled(store, tag, v, float32(v), float32);
                    fep.todo.extend(local.map(|l| (tag.clone(), l)));
                }
                None => shared::input_polled(store, tag, v),
            }
        }
    }

    /// What every response brings: its points, a confirmation when it asks for one, and the restart flag.
    async fn absorb(&mut self, wire: &mut Wire, r: &Response) -> Result<(), Lost> {
        self.answered = true;
        self.take(r);
        if r.con {
            wire.send(&mut self.tseq, &confirm(r.seq, r.unsol)).await?;
        }
        if r.restart() && !self.clear_failed {
            (self.clear, self.integrity) = (true, true);
        }
        Ok(())
    }

    /// One fragment that arrived while the response with sequence `want` is awaited: `Some` when it is that response's last
    /// fragment. An unsolicited response is taken; a response with another sequence number, or a fragment that does not
    /// parse, is discarded and changes nothing.
    /// ponytail: a response in several fragments is taken fragment by fragment and the last one returned; the next request's
    /// sequence number is this request's plus one whatever the outstation counted to.
    async fn answer(
        &mut self,
        wire: &mut Wire,
        fragment: &[u8],
        want: &mut u8,
        func: u8,
    ) -> Result<Option<Response>, Lost> {
        let r = match decode(fragment) {
            Ok(r) if r.unsol => {
                self.absorb(wire, &r).await?;
                return Ok(None);
            }
            Ok(r) if r.seq == *want => r,
            Ok(r) => {
                tracing::warn!(
                    "dnp3-client: {}: discarded a response with sequence {}, not awaited",
                    self.name,
                    r.seq
                );
                return Ok(None);
            }
            Err(why) => {
                tracing::warn!("dnp3-client: {}: discarded a fragment: {why:?}", self.name);
                return Ok(None);
            }
        };
        // the clear-restart's own answer is judged by its task, not as news of a restart
        let was = (self.clear, self.integrity);
        self.absorb(wire, &r).await?;
        if func == func::WRITE {
            (self.clear, self.integrity) = was;
        }
        *want = (*want + 1) & 0x0F;
        Ok(r.fin.then_some(r))
    }

    /// One request and its response, `None` when none came in time (`bennu` mode: stock keeps the connection and tries the
    /// next task; a FEP takes the outstation for gone).
    async fn exchange(
        &mut self,
        wire: &mut Wire,
        seq: &mut u8,
        func: u8,
        objects: &[u8],
    ) -> Result<Option<Response>, Lost> {
        let mut want = *seq;
        *seq = (*seq + 1) & 0x0F;
        wire.send(&mut self.tseq, &app::request(want, func, objects))
            .await?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let Ok(fragment) = tokio::time::timeout_at(deadline, wire.recv()).await else {
                tracing::warn!(
                    "dnp3-client: {}: no response to function {func} (sequence {want})",
                    self.name
                );
                return match self.fep {
                    Some(_) => Err(Lost),
                    None => Ok(None),
                };
            };
            if let Some(r) = self.answer(wire, &fragment?, &mut want, func).await? {
                return Ok(Some(r));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s = s.replace(' ', "");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The user data of the one link frame in a golden line: transport octet, then the application fragment.
    fn user(frame: &str) -> Vec<u8> {
        match link::parse(&unhex(frame)) {
            link::Parse::Frame(f, _) => f.user,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_startup_requests_are_the_stock_masters_bytes() {
        // conformance/stock/client-dnp3-raw.golden, the four frames after "# server started"
        let integrity = app::request(0, app::func::READ, &class_read(&[0]));
        assert_eq!(integrity, unhex("c0013c0106"));
        let mut seq = 0;
        let seg = transport::segment(&integrity, &mut seq);
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &seg[0]),
            unhex("05640bc40a000100acd1c0c0013c0106ff50")
        );
        let clear = app::request(1, app::func::WRITE, &clear_restart());
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &transport::segment(&clear, &mut seq)[0]),
            unhex("05640ec40a0001002529c1c102500100070700ff81")
        );
        let again = app::request(2, app::func::READ, &class_read(&[0]));
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &transport::segment(&again, &mut seq)[0]),
            unhex("05640bc40a000100acd1c2c2013c01064430")
        );
        let scan = app::request(3, app::func::READ, &class_read(&[1, 2, 3, 0]));
        assert_eq!(
            link::encode(MASTER_DATA, 10, 1, &transport::segment(&scan, &mut seq)[0]),
            unhex("056414c40a0001008fedc3c3013c02063c03063c04063c0106b2a0")
        );
    }

    #[test]
    fn the_commands_are_the_stock_masters_bytes() {
        // WRITE=load-breaker-toggle:true / :false (direct operate), WRITE=gen-toggle:true (select, then operate)
        let f = |seq, func, objects: &[u8]| app::request(seq, func, objects);
        assert_eq!(
            f(8, app::func::DIRECT_OPERATE, &crob(5, true)),
            user("05641ac40a0001008a1cc8c8050c0128010005000301640000005ef26400000000005b")[1..]
        );
        assert_eq!(
            f(9, app::func::DIRECT_OPERATE, &crob(5, false))[2..],
            unhex("0c0128010005000401640000006400000000")
        );
        assert_eq!(
            f(10, app::func::SELECT, &crob(6, true)),
            user("05641ac40a0001008a1ccaca030c01280100060003016400000060096400000000005b")[1..]
        );
        assert_eq!(
            f(11, app::func::OPERATE, &crob(6, true)),
            user("05641ac40a0001008a1ccbcb040c01280100060003016400000024926400000000005b")[1..]
        );
        // WRITE=gen-mw-setpoint:7 and WRITE=load-mw-setpoint:42.5 / 1e39 / 0.1 / nan / -2.5: always AnalogOutputFloat32
        assert_eq!(
            f(13, app::func::DIRECT_OPERATE, &ao_f32(2, 7.0)),
            unhex("cd05290328010002000000e04000")
        );
        assert_eq!(ao_f32(1, 42.5), unhex("2903280100010000002a4200"));
        assert_eq!(ao_f32(1, 1e39), unhex("290328010001000000807f00"));
        assert_eq!(ao_f32(1, 0.1), unhex("29032801000100cdcccc3d00"));
        assert_eq!(ao_f32(1, f64::NAN), unhex("290328010001000100807f00"));
        assert_eq!(ao_f32(1, -2.5), unhex("29032801000100000020c000"));
        assert_eq!(output_status(), unhex("0a00062800 06"));
        assert_eq!(app::request(5, app::func::CONFIRM, &[]), [0xc5, 0]);
        assert_eq!(confirm(5, true), [0xd5, 0]);
        assert_eq!(confirm(21, false), [0xc5, 0]);
    }

    #[test]
    fn the_stock_outstations_responses_decode_to_the_points_they_carry() {
        // the first integrity response: IIN1.7 (restart) and IIN1.4 (need time)
        let r = decode(&user("0564344401000a00285dc0c08190000102000202011e0600000082b90100000000000000000a020005060101a19d2801000102010000000001000000000540")[1..]).unwrap();
        assert_eq!(
            (r.seq, r.iin, r.unsol, r.con, r.fin),
            (0, 0x0090, false, false, true)
        );
        assert!(r.restart());
        use Seen::{Analog, AnalogOutput, Binary, BinaryOutput};
        assert_eq!(
            r.seen,
            [
                Binary(2, false),
                Analog(0, 0.0),
                BinaryOutput(5, false),
                BinaryOutput(6, false),
                AnalogOutput(1, 0.0),
                AnalogOutput(2, 0.0)
            ]
        );
        // a scan after the inputs changed
        let r = decode(&user("0564344401000a00285dc7c78110000102000202811e060000002292010000000000506f400a020005068181492e280100010201fa000000010c000000fe5c")[1..]).unwrap();
        assert_eq!((r.seq, r.iin), (7, 0x0010));
        assert!(!r.restart());
        assert_eq!(
            r.seen,
            [
                Binary(2, true),
                Analog(0, 250.5),
                BinaryOutput(5, true),
                BinaryOutput(6, true),
                AnalogOutput(1, 250.0),
                AnalogOutput(2, 12.0)
            ]
        );
        // the answer to the clear-restart write has no objects; a command echo is kept as bytes and has no points
        let r = decode(&unhex("c1811000")).unwrap();
        assert_eq!((r.seq, r.seen.len(), r.objects.len()), (1, 0, 0));
        let r = decode(
            &user("05641c4401000a007636c8c88110000c01280100050003016400961900006400000000005b")
                [1..],
        )
        .unwrap();
        assert_eq!(r.objects, crob(5, true));
        assert_eq!(r.seen, []);
    }

    fn resp(objects: &str) -> Result<Vec<Seen>, Bad> {
        decode(&[&[0xc0, 0x81, 0, 0][..], &unhex(objects)].concat()).map(|r| r.seen)
    }

    #[test]
    fn every_variation_an_outstation_may_choose_is_read() {
        use Seen::{Analog, AnalogOutput, Binary, BinaryOutput};
        // g1v1: packed bits 3..=12
        assert_eq!(
            resp("010100030ca502").unwrap(),
            [0xa5u16, 0x02]
                .iter()
                .flat_map(|b| (0..8).map(move |i| b >> i & 1 == 1))
                .take(10)
                .enumerate()
                .map(|(i, b)| Binary(3 + i as u16, b))
                .collect::<Vec<_>>()
        );
        // g2 events with a one-octet and a two-octet index prefix; v2 carries a time, v3 a relative time
        assert_eq!(
            resp("0201170207810900").unwrap(),
            [Binary(7, true), Binary(9, false)]
        );
        assert_eq!(
            resp("020228010034128100000000000 0").unwrap(),
            [Binary(0x1234, true)]
        );
        assert_eq!(resp("02031701058 10000").unwrap(), [Binary(5, true)]);
        // g30: 32 and 16 bit, with and without flag, float and double; two-octet start and stop
        assert_eq!(resp("1e01000101 01 2a000000").unwrap(), [Analog(1, 42.0)]);
        assert_eq!(
            resp("1e02010001 0001 01 feff").unwrap(),
            [Analog(256, -2.0)]
        );
        assert_eq!(resp("1e03000000 d6ffffff").unwrap(), [Analog(0, -42.0)]);
        assert_eq!(resp("1e04000000 0700").unwrap(), [Analog(0, 7.0)]);
        assert_eq!(resp("1e05000000 01 0000e040").unwrap(), [Analog(0, 7.0)]);
        // g32 events: v1 (i32), v5 (float), v7 (float with time), v8 (double with time)
        assert_eq!(resp("2001170103 01 2a000000").unwrap(), [Analog(3, 42.0)]);
        assert_eq!(resp("2005170103 01 0000e040").unwrap(), [Analog(3, 7.0)]);
        assert_eq!(
            resp("2007170103 01 0000e040 000000000000").unwrap(),
            [Analog(3, 7.0)]
        );
        assert_eq!(
            resp("2008170103 01 0000000000506f40 000000000000").unwrap(),
            [Analog(3, 250.5)]
        );
        // output status: g10v1 packed, g40v2 (i16), v3 (float), v4 (double)
        assert_eq!(
            resp("0a01000001 02").unwrap(),
            [BinaryOutput(0, false), BinaryOutput(1, true)]
        );
        assert_eq!(resp("2802000000 01 0500").unwrap(), [AnalogOutput(0, 5.0)]);
        assert_eq!(
            resp("2803000000 01 0000e040").unwrap(),
            [AnalogOutput(0, 7.0)]
        );
        assert_eq!(
            resp("2804000000 01 0000000000506f40").unwrap(),
            [AnalogOutput(0, 250.5)]
        );
        // what the stock handler ignores is stepped over: double-bit inputs, counters, a time, then a binary
        assert_eq!(
            resp("0302000000 81 1401000000 01 05000000 3201070 1 010203040506 0102000202 81")
                .unwrap(),
            [Binary(2, true)]
        );
        assert_eq!(resp("").unwrap(), []);
    }

    #[test]
    fn a_malformed_response_is_refused_whole() {
        // cut anywhere, the golden scan response is truncated, never a panic and never half a result
        let whole = user("0564344401000a00285dc7c78110000102000202811e060000002292010000000000506f400a020005068181492e280100010201fa000000010c000000fe5c");
        let whole = &whole[1..];
        let ends = [10, 24, 31]; // the fragment is complete after each object header's data
        for n in 0..whole.len() {
            let r = decode(&whole[..n]);
            match n >= 4 && (n == 4 || ends.contains(&n)) {
                true => assert!(r.is_ok(), "{n}: {r:?}"),
                false => assert!(r.is_err(), "{n}: {r:?}"),
            }
        }
        // a count past the end, a stop before the start, an unknown qualifier, an object nobody can size
        assert_eq!(resp("1e05000009 010000e040"), Err(Bad::Truncated));
        assert_eq!(resp("010200ff00"), Err(Bad::Truncated));
        assert_eq!(resp("0201280 0ff").map(|_| ()), Err(Bad::Truncated));
        assert_eq!(resp("01020502"), Err(Bad::Qualifier(5)));
        assert_eq!(
            resp("01020602"),
            Err(Bad::Qualifier(6)),
            "all-objects is a request's qualifier"
        );
        assert_eq!(resp("7001000000aa"), Err(Bad::Object(0x70, 1)));
        assert_eq!(resp("1e09000000aa"), Err(Bad::Object(30, 9)));
        // a request is not a response
        assert_eq!(
            decode(&unhex("c0013c0106")).map(|_| ()),
            Err(Bad::NotAResponse)
        );
        assert_eq!(decode(&[0xc0, 0x81, 0]).map(|_| ()), Err(Bad::Truncated));
        assert_eq!(decode(&[]).map(|_| ()), Err(Bad::Truncated));
        // an unsolicited response is one, and says so
        let r = decode(&unhex("f38200000201170102 81")).unwrap();
        assert_eq!((r.seq, r.unsol, r.con), (3, true, true));
        assert_eq!(r.seen, [Seen::Binary(2, true)]);
    }

    fn parse(inner: &str, opts: &Opts) -> Result<(Client, Tags), ConfigError> {
        let xml = format!("<field-device><cycle-time>200</cycle-time><comms><dnp3-client>{inner}</dnp3-client></comms></field-device>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let node = doc
            .descendants()
            .find(|n| n.has_tag_name("dnp3-client"))
            .unwrap();
        let mut tags = Tags::default();
        Client::parse(node, &mut tags, opts).map(|c| (c, tags))
    }

    const HEAD: &str = "<address>1</address><scan-rate>5</scan-rate>";
    const CONN: &str =
        "<dnp3-connection><endpoint>tcp://127.0.0.1:20000</endpoint><address>10</address>
        <binary-input><address>2</address><tag>bi</tag></binary-input>
        <binary-output><address>5</address><tag>bo</tag><sbo>1</sbo></binary-output>
        <analog-input><address>0</address><tag>ai</tag></analog-input>
        <analog-output><address>1</address><tag>ao</tag></analog-output>
        </dnp3-connection>";

    #[test]
    fn the_stock_schema_is_read() {
        let s = Duration::from_secs;
        let (c, tags) = parse(
            &format!("{HEAD}{CONN}<command-interface>tcp://127.0.0.1:1330</command-interface>"),
            &Opts { strict: true },
        )
        .unwrap();
        assert_eq!(
            (c.mode, c.local, c.cmdif.as_deref()),
            (Mode::Bennu, 1, Some("tcp://127.0.0.1:1330"))
        );
        let conn = &c.conns[0];
        assert_eq!(
            (conn.endpoint.as_str(), conn.remote, &conn.fep),
            ("tcp://127.0.0.1:20000", 10, &None)
        );
        let got: Vec<_> = conn
            .points
            .iter()
            .map(|p| (p.tag.as_str(), p.addr, p.kind, p.output, p.sbo))
            .collect();
        assert_eq!(
            got,
            [
                ("bi", 2, Kind::Binary, false, false),
                ("bo", 5, Kind::Binary, true, true),
                ("ai", 0, Kind::Analog, false, false),
                ("ao", 1, Kind::Analog, true, false),
            ]
        );
        assert_eq!(tags.kind("bo"), Some(Kind::Binary));
        assert_eq!(tags.kind("ao"), Some(Kind::Analog));
        // the all-classes scan at the client's scan-rate, nothing per class
        assert_eq!(conn.scans, [(vec![1, 2, 3, 0], s(5))]);
        let rates = |r: &str| {
            let conn = CONN.replace(
                "</dnp3-connection>",
                &format!("<class-scan-rates>{r}</class-scan-rates></dnp3-connection>"),
            );
            parse(&format!("{HEAD}{conn}"), &Opts::default())
                .unwrap()
                .0
                .conns
                .remove(0)
                .scans
        };
        assert_eq!(rates(""), [(vec![1, 2, 3, 0], s(5))]);
        assert_eq!(rates("<all>0</all>"), []);
        assert_eq!(
            rates("<all>9</all><class0>3</class0><class2>7</class2>"),
            [(vec![1, 2, 3, 0], s(9)), (vec![0], s(3)), (vec![2], s(7))]
        );
        assert_eq!(
            rates("<class1>1</class1><class3>2</class3>"),
            [(vec![1, 2, 3, 0], s(5)), (vec![1], s(1)), (vec![3], s(2))]
        );
        // scan-rate 0: no recurring scan at all
        let (c, _) = parse(
            &format!("<address>1</address><scan-rate>0</scan-rate>{CONN}"),
            &Opts::default(),
        )
        .unwrap();
        assert!(c.conns[0].scans.is_empty());
        // an sbo on an input means nothing, as in stock
        let odd = CONN.replace("<tag>bi</tag>", "<tag>bi</tag><sbo>1</sbo>");
        assert!(
            !parse(&format!("{HEAD}{odd}"), &Opts::default())
                .unwrap()
                .0
                .conns[0]
                .points[0]
                .sbo
        );
    }

    #[test]
    fn missing_and_bad_keys_are_named_and_the_fep_keys_follow_the_mode() {
        let err = |inner: &str| parse(inner, &Opts::default()).err().expect("an error").0;
        assert!(err(&format!("<scan-rate>5</scan-rate>{CONN}"))
            .contains("<dnp3-client> has no <address>"));
        assert!(err(&format!("<address>1</address>{CONN}"))
            .contains("<dnp3-client> has no <scan-rate>"));
        assert!(err(&format!(
            "{HEAD}<dnp3-connection><address>10</address></dnp3-connection>"
        ))
        .contains("<dnp3-connection> has no <endpoint>"));
        assert!(err(&format!(
            "{HEAD}{}",
            CONN.replace("<address>10</address>", "")
        ))
        .contains("<dnp3-connection> has no <address>"));
        assert!(err(&format!(
            "{HEAD}{}",
            CONN.replace("<address>2</address>", "")
        ))
        .contains("<binary-input> has no <address>"));
        assert!(err(&format!("{HEAD}{}", CONN.replace("<tag>ao</tag>", "")))
            .contains("<analog-output> has no <tag>"));
        assert!(err(&format!(
            "{HEAD}{}",
            CONN.replace("<sbo>1</sbo>", "<sbo>yes</sbo>")
        ))
        .contains("<sbo> `yes` is not a boolean"));
        assert!(err(&format!(
            "{HEAD}{}",
            CONN.replace("<address>5</address>", "<address>70000</address>")
        ))
        .contains("not a valid number"));
        assert_eq!(
            err(&format!("{HEAD}<mode>poll</mode>{CONN}")),
            "<mode> `poll` is not bennu or fep"
        );
        let status = CONN.replace(
            "</dnp3-connection>",
            "<status-tag>up</status-tag></dnp3-connection>",
        );
        assert_eq!(
            parse(&format!("{HEAD}{status}"), &Opts { strict: true })
                .err()
                .unwrap()
                .0,
            "<status-tag> is fep-only, ignored"
        );
        let (c, tags) = parse(
            &format!("{HEAD}<mode>fep</mode>{status}"),
            &Opts { strict: true },
        )
        .unwrap();
        assert_eq!(
            (c.mode, c.conns[0].fep.as_ref().unwrap().status.as_deref()),
            (Mode::Fep, Some("up"))
        );
        assert_eq!(tags.kind("up"), Some(Kind::Binary));
        // the same tag in two connections: the first keeps it
        let twice = format!("{HEAD}{CONN}{}", CONN.replace("20000", "20001"));
        let (c, _) = parse(&twice, &Opts::default()).unwrap();
        assert_eq!((c.conns[0].points.len(), c.conns[1].points.len()), (4, 0));
        assert!(parse(&twice, &Opts { strict: true }).is_err());
    }

    #[test]
    fn the_address_maps_are_stocks_one_per_kind_with_the_later_point_winning() {
        let pt = |tag: &str, addr, kind, output| Point {
            tag: tag.into(),
            addr,
            kind,
            output,
            sbo: false,
        };
        let points = [
            pt("bi", 5, Kind::Binary, false),
            pt("bo", 5, Kind::Binary, true),
            pt("ai", 0, Kind::Analog, false),
            pt("ao", 1, Kind::Analog, true),
        ];
        let store = Store::new(crate::fd::store::Layout {
            internal: points
                .iter()
                .map(|p| (p.tag.clone(), Value::Analog(0.0)))
                .collect(),
            ..Default::default()
        });
        let (tx, _rx) = mpsc::unbounded_channel();
        let s = Shared::new(&points, store, tx);
        assert_eq!(
            s.bin[&5], "bo",
            "the output, declared after the input, has the address"
        );
        // a binary input at index 5 lands in the output's tag; output status is ignored; an analog is kept as a float32
        s.take(&Response {
            seq: 0,
            iin: 0,
            unsol: false,
            con: false,
            fin: true,
            objects: vec![],
            seen: vec![
                Seen::Binary(5, true),
                Seen::BinaryOutput(5, false),
                Seen::Analog(0, 0.1),
                Seen::AnalogOutput(1, 9.0),
                Seen::Analog(7, 1.0),
            ],
        });
        let cache = s.cache.lock().unwrap().clone();
        assert_eq!(cache["bo"], Value::Binary(true));
        assert_eq!(cache["bi"], Value::Binary(false));
        assert_eq!(cache["ai"], Value::Analog(f64::from(0.1f32)));
        assert_eq!(cache["ao"], Value::Analog(0.0));
    }

    /// Final review, finding 5: in a real outstation binary input 0 and binary output 0 are two points.
    #[test]
    fn fep_keeps_an_input_and_an_output_at_one_index_apart() {
        let pt = |tag: &str, addr, kind, output| Point {
            tag: tag.into(),
            addr,
            kind,
            output,
            sbo: false,
        };
        let points = [
            pt("bi0", 0, Kind::Binary, false),
            pt("bo0", 0, Kind::Binary, true),
            pt("bo5", 5, Kind::Binary, true),
            pt("ai0", 0, Kind::Analog, false),
            pt("ao0", 0, Kind::Analog, true),
        ];
        let store = Store::new(crate::fd::store::Layout::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        let s = Shared::new(&points, store, tx);
        let tag = |seen| s.fep_tag(&seen).map(|(t, v)| (t.as_str(), v));
        assert_eq!(
            tag(Seen::Binary(0, true)),
            Some(("bi0", Value::Binary(true)))
        );
        assert_eq!(
            tag(Seen::BinaryOutput(0, true)),
            Some(("bo0", Value::Binary(true)))
        );
        assert_eq!(tag(Seen::Analog(0, 1.5)), Some(("ai0", Value::Analog(1.5))));
        assert_eq!(
            tag(Seen::AnalogOutput(0, 2.5)),
            Some(("ao0", Value::Analog(2.5)))
        );
        // an outstation that reports its outputs as inputs (ours): no input has index 5, so the output takes it
        assert_eq!(
            tag(Seen::Binary(5, true)),
            Some(("bo5", Value::Binary(true)))
        );
        assert_eq!(tag(Seen::Binary(9, true)), None);
        assert_eq!(tag(Seen::BinaryOutput(9, true)), None);
        // the analog output's key is the float32 that leaves
        assert_eq!(
            float32(Value::Analog(0.1)),
            Value::Analog(f64::from(0.1f32))
        );
        assert_eq!(float32(Value::Binary(true)), Value::Binary(true));
    }
}
