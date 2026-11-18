//! Modbus/TCP client (`<modbus-client>`, protocol plan row 8). In `bennu` mode it is the stock fd-client as captured
//! (conformance/stock/client-modbus.golden): nothing is polled, a command-interface READ is one live request and a WRITE one FC5 or
//! FC6. `fep` mode polls every connection into tags and writes local changes down (docs/field-device-clients.md).

use super::super::super::cmdif::{self, Points};
use super::super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::super::eventlog::EventLog;
use super::super::super::net;
use super::super::super::store::Store;
use super::super::client::{self as shared, Fep, Mode};
use super::super::Module;
use super::codec::{self, Parse};
use super::device::{Range, DEFAULT_RANGE};
use crate::model::{Kind, Value};
use roxmltree::Node;
use std::collections::BTreeMap;
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

fn connection(c: Node, mode: Mode, tags: &mut Tags, opts: &Opts) -> Result<Conn, ConfigError> {
    let endpoint = require(c, "endpoint")?;
    let unit = number::<u8>(c, "unit-id")?.unwrap_or(0);
    let fep = shared::fep_keys(c, mode, tags, opts)?;
    let (mut points, mut ranges) = (vec![], BTreeMap::new());
    for (name, table) in TABLES {
        for n in c.children().filter(|n| n.has_tag_name(name)) {
            let addr = number::<u16>(n, "address")?
                .ok_or_else(|| ConfigError(format!("<{name}> has no <address>")))?;
            let tag = require(n, "tag")?;
            let (range, kind) = match table.bits() {
                true => (DEFAULT_RANGE, Kind::Binary),
                false => (super::range(n)?, Kind::Analog),
            };
            if !table.bits() {
                ranges.insert(addr, range);
            }
            if shared::point_tag(tags, &tag, kind, opts)? {
                points.push(Point {
                    tag,
                    table,
                    addr,
                    range,
                    kind,
                });
            }
        }
    }
    Ok(Conn {
        endpoint,
        unit,
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
        let (mut links, mut ranges) = (vec![], vec![]);
        let mut points = BTreeMap::new();
        for (i, c) in self.conns.into_iter().enumerate() {
            points.extend(c.points.into_iter().map(|p| (p.tag.clone(), (i, p))));
            links.push(Mutex::new(Link::new(c.endpoint, c.unit)));
            ranges.push(c.ranges);
        }
        let live = Arc::new(Live {
            links,
            ranges,
            points,
            store,
            log: self.log,
        });
        // stock connects every connection once at start and then only when a request finds it closed
        for link in &live.links {
            link.lock().await.connect().await;
        }
        match &self.cmdif {
            Some(ep) => cmdif::serve(ep, Bennu(live), stop).await,
            None => stop.cancelled().await,
        }
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
    /// One request on connection `i`; the values, or why there are none (logged).
    async fn ask(&self, i: usize, fc: u8, addr: u16, n: u16) -> Result<Vec<u16>, String> {
        let pdu = self.links[i]
            .lock()
            .await
            .request(&read_req(fc, addr, n))
            .await;
        let qty = if fc > 4 { 1 } else { n };
        let r = pdu.and_then(|pdu| {
            response(fc, qty, &pdu).map_err(|e| match e {
                Reply::Exception(code) => format!("exception {code}"),
                Reply::Bad(why) => why.to_string(),
            })
        });
        match &r {
            Ok(_) => self.log.log(
                "info",
                format_args!("function {fc}"),
                format_args!("address {addr}, {n}: ok"),
            ),
            Err(why) => self.log.log(
                "error",
                format_args!("function {fc}"),
                format_args!("address {addr}, {n}: {why}"),
            ),
        }
        r
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
}
