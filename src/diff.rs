//! `ferrofire probe diff`: poll each RTU over Modbus/TCP and compare what it says with what the provider says (READ), to
//! catch an RTU, a man in the middle or a bad register map that disagrees with the truth. One TCP connection per RTU is
//! opened on the first read and kept for every cycle after it (stock bennu opens and closes one per read and leaks fds,
//! ww-1p6); a failed read drops it (closing the fd) and the next read opens a fresh one.
//!
//! The register map is YAML, one entry per RTU, mirroring the `<modbus-server>` keys of bennu's config.xml:
//! `name: {endpoint: host:port, unit: 0, points: [{tag, kind: coil|discrete-input|holding-register|input-register, address, min, max}]}`.
//! Analog registers are scaled over `[min,max]` as the field device does (`field-device-spec.md` 4.1).

use crate::bennu::{install_signals, parse_value, sleep_until, STOP};
use crate::model::Value;
use crate::probe::{ask, json_str, read_rows};
use crate::pull::requests;
use crate::transport::Requester;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    /// Register map YAML (one entry per RTU)
    #[arg(long, value_name = "FILE")]
    map: String,
    /// Allowed difference as a fraction of the point's span (`max - min`; coils span 1)
    #[arg(long, default_value_t = 0.01)]
    tolerance: f64,
    /// Number of comparison cycles; 0 runs until SIGINT/SIGTERM (blue-team monitor)
    #[arg(long, default_value_t = 1)]
    cycles: u64,
    /// Seconds between cycles
    #[arg(long, default_value_t = 1.0, value_name = "SECONDS")]
    every: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Coil,
    DiscreteInput,
    HoldingRegister,
    InputRegister,
}

impl Kind {
    /// The Modbus function code that reads this kind.
    pub fn fc(self) -> u8 {
        match self {
            Kind::Coil => 1,
            Kind::DiscreteInput => 2,
            Kind::HoldingRegister => 3,
            Kind::InputRegister => 4,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Point {
    pub tag: String,
    pub kind: Kind,
    pub address: u16,
    min: Option<f64>,
    max: Option<f64>,
}

impl Point {
    /// `(min, max)` the register is scaled over; bits span `[0,1]`. Registers default to the stock `[-1000, 2000]`.
    pub fn span(&self) -> (f64, f64) {
        match self.kind {
            Kind::Coil | Kind::DiscreteInput => (0.0, 1.0),
            _ => (self.min.unwrap_or(-1000.0), self.max.unwrap_or(2000.0)),
        }
    }

    fn value(&self, raw: u16) -> f64 {
        let (lo, hi) = self.span();
        match self.kind {
            Kind::Coil | Kind::DiscreteInput => raw as f64,
            _ => unscale(raw, lo, hi),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Rtu {
    endpoint: String,
    #[serde(default)]
    pub unit: u8,
    pub points: Vec<Point>,
}

pub fn parse_map(yaml: &str) -> Result<BTreeMap<String, Rtu>, String> {
    serde_saphyr::from_str(yaml).map_err(|e| e.to_string())
}

/// A raw 16-bit register back to engineering units: the inverse of the field device's `slope = 65535 / (max - min)`.
pub fn unscale(raw: u16, min: f64, max: f64) -> f64 {
    min + raw as f64 * (max - min) / 65535.0
}

/// MBAP header plus a read of one coil/register (transaction id 0: one request is in flight at a time, always).
pub fn request(unit: u8, fc: u8, addr: u16) -> [u8; 12] {
    let a = addr.to_be_bytes();
    [0, 0, 0, 0, 0, 6, unit, fc, a[0], a[1], 0, 1]
}

/// The value in a reply PDU to a one-item read: a bit (FC1/2) or a register (FC3/4); an exception is an error.
pub fn parse_reply(fc: u8, pdu: &[u8]) -> Result<u16, String> {
    match *pdu {
        [f, code, ..] if f == fc | 0x80 => Err(format!("exception {code}")),
        [f, ..] if f != fc => Err(format!("unexpected function {f:#x}")),
        _ => match (fc, pdu) {
            (1 | 2, [_, 1, b, ..]) => Ok((b & 1) as u16),
            (3 | 4, [_, 2, h, l, ..]) => Ok(u16::from_be_bytes([*h, *l])),
            _ => Err("short or malformed reply".into()),
        },
    }
}

/// One RTU's Modbus/TCP client. The stream is made on the first read and reused; any I/O error drops it.
pub struct Modbus {
    addr: String,
    unit: u8,
    timeout: Duration,
    conn: Option<TcpStream>,
}

fn connect(addr: &str, t: Duration) -> Result<TcpStream, String> {
    let sa = (addr.to_socket_addrs().map_err(|e| format!("{addr}: {e}"))?)
        .next()
        .ok_or_else(|| format!("{addr}: no address"))?;
    let c = TcpStream::connect_timeout(&sa, t).map_err(|e| format!("{addr}: {e}"))?;
    let _ = (c.set_read_timeout(Some(t)), c.set_write_timeout(Some(t)));
    Ok(c)
}

/// Send one request, return the PDU of the reply (the MBAP length counts the unit byte).
fn exchange(c: &mut TcpStream, frame: &[u8; 12]) -> std::io::Result<Vec<u8>> {
    c.write_all(frame)?;
    let mut h = [0u8; 7];
    c.read_exact(&mut h)?;
    let mut pdu = vec![0u8; (u16::from_be_bytes([h[4], h[5]]) as usize).saturating_sub(1)];
    c.read_exact(&mut pdu)?;
    Ok(pdu)
}

impl Modbus {
    pub fn new(addr: &str, unit: u8, timeout: Duration) -> Modbus {
        Modbus {
            addr: addr.into(),
            unit,
            timeout,
            conn: None,
        }
    }

    pub fn read(&mut self, fc: u8, addr: u16) -> Result<u16, String> {
        let c = match &mut self.conn {
            Some(c) => c,
            None => self.conn.insert(connect(&self.addr, self.timeout)?),
        };
        match exchange(c, &request(self.unit, fc, addr)) {
            Ok(pdu) => parse_reply(fc, &pdu),
            Err(e) => {
                self.conn = None;
                Err(format!("{}: {e}", self.addr))
            }
        }
    }
}

/// Whether the RTU's value and the provider's are further apart than `tol` of the span.
pub fn differs(modbus: f64, provider: f64, min: f64, max: f64, tol: f64) -> bool {
    (modbus - provider).abs() > tol * (max - min)
}

/// A mismatch `(modbus, provider)` or an RTU that could not be read.
pub struct Finding {
    pub rtu: String,
    pub tag: String,
    pub what: Result<(f64, f64), String>,
}

impl Finding {
    pub fn text(&self, t: f64) -> String {
        let head = format!("t={t:.3} rtu={} tag={}", self.rtu, self.tag);
        match &self.what {
            Ok((m, p)) => format!("{head} modbus={m} provider={p} delta={}", (m - p).abs()),
            Err(e) => format!("{head} error={e}"),
        }
    }

    pub fn json(&self, t: f64) -> String {
        let head = format!(
            "{{\"t\":{t:.3},\"rtu\":{},\"tag\":{}",
            json_str(&self.rtu),
            json_str(&self.tag)
        );
        match &self.what {
            Ok((m, p)) => format!(
                "{head},\"modbus\":{m},\"provider\":{p},\"delta\":{}}}",
                (m - p).abs()
            ),
            Err(e) => format!("{head},\"error\":{}}}", json_str(e)),
        }
    }
}

pub struct Rig {
    name: String,
    points: Vec<Point>,
    link: Modbus,
}

impl Rig {
    /// Read every point (stopping at the first one that cannot be read: a dead or lying link says the same about the rest).
    fn check(&mut self, truth: &HashMap<String, f64>, tol: f64) -> Vec<Finding> {
        let mut out = vec![];
        for p in &self.points {
            let f = |what| Finding {
                rtu: self.name.clone(),
                tag: p.tag.clone(),
                what,
            };
            match self.link.read(p.kind.fc(), p.address) {
                Err(e) => {
                    out.push(f(Err(e)));
                    break;
                }
                Ok(raw) => {
                    let (m, want, (lo, hi)) = (p.value(raw), truth[&p.tag], p.span());
                    if differs(m, want, lo, hi, tol) {
                        out.push(f(Ok((m, want))));
                    }
                }
            }
        }
        out
    }
}

fn number(v: Value) -> f64 {
    match v {
        Value::Binary(b) => b as u8 as f64,
        Value::Analog(x) => x,
    }
}

/// The provider's value of every tag the map names, one READ per batch.
fn truth(req: &mut dyn Requester, rigs: &[Rig]) -> Result<HashMap<String, f64>, String> {
    let tags: BTreeSet<String> = rigs
        .iter()
        .flat_map(|r| r.points.iter().map(|p| p.tag.clone()))
        .collect();
    let mut out = HashMap::new();
    for r in requests(&tags.into_iter().collect::<Vec<_>>()) {
        for row in read_rows(&ask(req, &r)?)? {
            let v = parse_value(&row.value).ok_or_else(|| format!("bad value for {}", row.tag))?;
            out.insert(row.tag, number(v));
        }
    }
    Ok(out)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn rigs(path: &str, timeout: Duration) -> Result<Vec<Rig>, String> {
    let yaml = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(parse_map(&yaml)?
        .into_iter()
        .map(|(name, r)| Rig {
            link: Modbus::new(&r.endpoint, r.unit, timeout),
            name,
            points: r.points,
        })
        .collect())
}

/// One comparison of every RTU with the provider; prints the findings and returns how many there were.
fn cycle(req: &mut dyn Requester, rigs: &mut [Rig], tol: f64, json: bool) -> Result<u64, String> {
    let (t, want) = (now(), truth(req, rigs)?);
    let found: Vec<Finding> = rigs.iter_mut().flat_map(|r| r.check(&want, tol)).collect();
    for f in &found {
        println!("{}", if json { f.json(t) } else { f.text(t) });
    }
    Ok(found.len() as u64)
}

/// Compare the RTUs with the provider `cycles` times (0: until a stop signal), printing each mismatch as it is found.
/// Fails at the end if there were any. A provider that cannot be read ends the run (there is no truth to compare with).
pub fn run(
    req: &mut dyn Requester,
    a: DiffArgs,
    timeout_ms: i32,
    json: bool,
) -> Result<(), String> {
    let mut rigs = rigs(&a.map, Duration::from_millis(timeout_ms.max(1) as u64))?;
    install_signals();
    let (mut n, mut found) = (0, 0);
    while !STOP.load(Ordering::SeqCst) && (a.cycles == 0 || n < a.cycles) {
        if n > 0 {
            sleep_until(
                Instant::now() + Duration::from_secs_f64(a.every),
                Duration::from_millis(50),
            );
        }
        n += 1;
        found += cycle(req, &mut rigs, a.tolerance, json)?;
    }
    match found {
        0 => Ok(()),
        n => Err(format!(
            "{n} mismatch(es) between the RTUs and the provider"
        )),
    }
}
