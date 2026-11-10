//! Native bennu provider frontend (spec 2.1): ZMQ REP (QUERY/READ/WRITE) + UDP RADIO/DISH datagrams.
//! Wire formats are matched from the bennu protocol documentation only; no bennu code is used.
//! Pure pieces (hash, framing, chunking, request handling) are plain functions so tests need no sockets.

use crate::model::{Model, Value};
use crate::transport::{self, Publisher};
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---- group hash ----------------------------------------------------------------------------------------------

/// libstdc++ `std::hash<std::string>` (64-bit `_Hash_bytes`, seed 0xc70f6907), reimplemented from the published algorithm.
pub fn libstdcxx_hash(data: &[u8]) -> u64 {
    const MUL: u64 = 0xc6a4_a793_5bd1_e995;
    let shift_mix = |v: u64| v ^ (v >> 47);
    let len = data.len();
    let mut hash = 0xc70f_6907u64 ^ (len as u64).wrapping_mul(MUL);
    let (chunks, rest) = data.as_chunks::<8>();
    for c in chunks {
        let d = shift_mix(u64::from_le_bytes(*c).wrapping_mul(MUL)).wrapping_mul(MUL);
        hash = (hash ^ d).wrapping_mul(MUL);
    }
    if !rest.is_empty() {
        let d = rest.iter().rev().fold(0u64, |a, &b| (a << 8) | b as u64);
        hash = (hash ^ d).wrapping_mul(MUL);
    }
    shift_mix(shift_mix(hash).wrapping_mul(MUL))
}

/// Strip an `iface;` part: `udp://10.1.1.2;239.0.0.1:40000` -> `udp://239.0.0.1:40000`.
pub fn strip_iface(ep: &str) -> String {
    match (ep.find(';'), ep.find("//")) {
        (Some(semi), Some(sl)) if sl + 2 <= semi => format!("{}{}", &ep[..sl + 2], &ep[semi + 1..]),
        _ => ep.to_string(),
    }
}

/// RADIO/DISH group: lowercase hex of the hash (no zero padding) minus its last digit (max group length is 15).
pub fn group_for(endpoint: &str) -> String {
    let h = format!("{:x}", libstdcxx_hash(strip_iface(endpoint).as_bytes()));
    h[..h.len() - 1].to_string()
}

// ---- framing -------------------------------------------------------------------------------------------------

/// UDP datagram as libzmq's RADIO writes it: `[u8 group_len][group][body]`.
pub fn frame(group: &str, body: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(1 + group.len() + body.len());
    d.push(group.len() as u8);
    d.extend_from_slice(group.as_bytes());
    d.extend_from_slice(body);
    d
}

/// Inverse of `frame` (what a DISH does); None for a malformed datagram.
pub fn unframe(d: &[u8]) -> Option<(&str, &[u8])> {
    let n = *d.first()? as usize;
    if n > d.len() - 1 {
        return None;
    }
    Some((std::str::from_utf8(&d[1..1 + n]).ok()?, &d[1 + n..]))
}

/// Split `a:1,b:2,` into chunks of whole `part,` pieces: empty parts skipped, an over-long single part returned alone.
pub fn chunk_message(msg: &str, mtu: usize) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for p in msg.split(',').filter(|p| !p.is_empty()) {
        if !cur.is_empty() && cur.len() + p.len() + 1 >= mtu {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(p);
        cur.push(',');
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Chunk size: UDP + IP + group headers still fit a 1500-byte Ethernet MTU.
pub const CHUNK: usize = 1400;

pub fn fmt_value(v: Value) -> String {
    match v {
        Value::Binary(b) => b.to_string(),
        Value::Analog(x) => x.to_string(),
    }
}

/// One publish tick as a `tag:value,...` message.
pub fn snapshot_msg(snap: &[(String, Value)]) -> String {
    snap.iter()
        .map(|(t, v)| format!("{t}:{},", fmt_value(*v)))
        .collect()
}

/// Datagrams for one publish tick (each `tag:value,...` chunk NUL-terminated, then framed).
pub fn datagrams(group: &str, snap: &[(String, Value)]) -> Vec<Vec<u8>> {
    datagrams_msg(group, &snapshot_msg(snap))
}

/// The wire chunks of a `tag:value,...` message, each NUL-terminated (both transports send these bytes).
pub fn chunks_nul(msg: &str) -> Vec<Vec<u8>> {
    chunk_message(msg, CHUNK)
        .into_iter()
        .map(|c| {
            let mut b = c.into_bytes();
            b.push(0);
            b
        })
        .collect()
}

/// Same as `datagrams`, from an already-built message.
pub fn datagrams_msg(group: &str, msg: &str) -> Vec<Vec<u8>> {
    chunks_nul(msg)
        .into_iter()
        .map(|b| frame(group, &b))
        .collect()
}

// ---- request handling ----------------------------------------------------------------------------------------

#[derive(Default)]
pub struct Shared {
    pub snap: Vec<(String, Value)>,
    pub queue: Vec<(String, Value)>,
}

pub struct Meta {
    pub tags: Vec<String>,
    pub writable: HashSet<String>,
}

impl Meta {
    pub fn of(m: &Model) -> Meta {
        let tags: Vec<String> = m.tags().map(|(t, _)| t.to_string()).collect();
        let writable = tags.iter().filter(|t| m.writable(t)).cloned().collect();
        Meta { tags, writable }
    }
}

fn parse_value(s: &str) -> Option<Value> {
    match s {
        "true" => Some(Value::Binary(true)),
        "false" => Some(Value::Binary(false)),
        _ => s.parse().ok().map(Value::Analog),
    }
}

/// One request -> one reply, always (a REP socket must answer or it wedges). Both carry a trailing NUL.
pub fn handle(req: &[u8], meta: &Meta, sh: &Mutex<Shared>) -> Vec<u8> {
    with_nul(reply(req, meta, sh))
}

/// Every reply on the wire, in both providers, ends with a NUL.
pub fn with_nul(reply: String) -> Vec<u8> {
    let mut r = reply.into_bytes();
    r.push(0);
    r
}

/// `READ=tag` -> `ACK=value`; `READ=a,b,...` -> `ACK=a:va,b:vb,` (the update format, request order), the first unknown tag is an ERR.
/// ponytail: one linear scan of the snapshot per requested tag; index it if batches of thousands are ever asked for.
fn read(payload: &str, sh: &Mutex<Shared>) -> String {
    let sh = sh.lock().unwrap();
    let find = |t: &str| sh.snap.iter().find(|(n, _)| n == t).map(|(_, v)| *v);
    if !payload.contains(',') {
        return match find(payload) {
            Some(v) => format!("ACK={}", fmt_value(v)),
            None => format!("ERR=Tag '{payload}' not found"),
        };
    }
    let mut out = String::from("ACK=");
    for t in payload.split(',').filter(|t| !t.is_empty()) {
        match find(t) {
            Some(v) => out.push_str(&format!("{t}:{},", fmt_value(v))),
            None => return format!("ERR=Tag '{t}' not found"),
        }
    }
    out
}

fn reply(req: &[u8], meta: &Meta, sh: &Mutex<Shared>) -> String {
    let Ok(req) = std::str::from_utf8(req) else {
        return "ERR=request is not UTF-8".into();
    };
    let req = req.trim_end_matches('\0');
    let Some((op, payload)) = req.split_once('=') else {
        return format!("ERR=Malformed request '{req}'");
    };
    match op.to_ascii_lowercase().as_str() {
        "query" => format!(
            "ACK={}",
            meta.tags
                .iter()
                .map(|t| format!("{t},"))
                .collect::<String>()
        ),
        "read" => read(payload, sh),
        "write" => {
            let mut batch = vec![];
            for point in payload.split(',').filter(|p| !p.is_empty()) {
                let Some((tag, val)) = point.split_once(':') else {
                    return format!("ERR=Malformed point '{point}'");
                };
                if !meta.writable.contains(tag) {
                    return format!("ERR=Tag '{tag}' is unknown or read-only");
                }
                let Some(v) = parse_value(val) else {
                    return format!("ERR=Bad value '{val}' for '{tag}'");
                };
                batch.push((tag.to_string(), v));
            }
            // all-or-nothing: the field device's retry logic isolates a bad point
            sh.lock().unwrap().queue.extend(batch);
            "ACK=".into()
        }
        _ => format!("ERR=Unknown command type '{op}'"),
    }
}

// ---- sockets and threads -------------------------------------------------------------------------------------

pub static STOP: AtomicBool = AtomicBool::new(false);
pub extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// SIGINT and SIGTERM raise `STOP`; every loop in the process polls it.
pub fn install_signals() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
}

/// Sleep in short slices until `deadline`, returning early on `STOP` so a stop signal is noticed promptly.
pub fn sleep_until(deadline: Instant, slice: Duration) {
    while !STOP.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(slice);
    }
}

/// `udp://[iface;]host:port` -> (iface, destination)
pub fn parse_udp(ep: &str) -> Result<(Option<String>, SocketAddr), String> {
    let rest = ep
        .strip_prefix("udp://")
        .ok_or_else(|| format!("publish endpoint `{ep}` must be udp://[iface;]group:port"))?;
    let (iface, dest) = match rest.rsplit_once(';') {
        Some((i, d)) => (Some(i.to_string()).filter(|i| i != "*"), d),
        None => (None, rest),
    };
    let addr = dest
        .to_socket_addrs()
        .map_err(|e| format!("{dest}: {e}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| format!("{dest}: no IPv4 address"))?;
    Ok((iface, addr))
}

/// IP_MULTICAST_IF: send multicast datagrams out of the interface that owns `a`.
fn set_multicast_if(s: &UdpSocket, a: Ipv4Addr) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let ia = libc::in_addr {
        s_addr: u32::from(a).to_be(),
    };
    let rc = unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            &ia as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::in_addr>() as u32,
        )
    };
    match rc {
        0 => Ok(()),
        _ => Err(format!(
            "IP_MULTICAST_IF {a}: {}",
            std::io::Error::last_os_error()
        )),
    }
}

fn multicast_setup(s: &UdpSocket, iface: Option<&str>) -> Result<(), String> {
    s.set_multicast_ttl_v4(1).map_err(|e| e.to_string())?; // libzmq multicast_hops default
    s.set_multicast_loop_v4(true).map_err(|e| e.to_string())?; // same-host subscribers work
    let Some(i) = iface else { return Ok(()) };
    let a: Ipv4Addr = i
        .parse()
        .map_err(|_| format!("interface `{i}`: only IPv4 addresses are supported"))?;
    set_multicast_if(s, a)
}

pub fn udp_sender(iface: Option<&str>, dest: SocketAddr) -> Result<UdpSocket, String> {
    let s = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    if dest.ip().is_multicast() {
        multicast_setup(&s, iface)?;
    }
    Ok(s)
}

const USAGE: &str = "ferrofire run [model.yaml | --model model.yaml] [--server tcp://0.0.0.0:5555] [--publish 'udp://*;239.0.0.1:40000'] [--rate 0.1] [--transport zmq|nng] [--nng-sendbuf 128]";

pub fn main(args: &[String]) {
    exit_on_err("run", USAGE, run(args));
}

/// A subcommand's failure: `ferrofire <cmd>: <error>` and its usage on stderr, exit 1.
pub fn exit_on_err(cmd: &str, usage: &str, res: Result<(), String>) {
    if let Err(e) = res {
        eprintln!("ferrofire {cmd}: {e}\nusage: {usage}");
        std::process::exit(1);
    }
}

#[derive(Debug)]
pub struct RunArgs {
    pub path: Option<String>,
    pub server: String,
    /// None: the chosen transport's default endpoint.
    pub publish: Option<String>,
    pub rate: Option<f64>,
    /// None: the model's `transport:`, else zmq.
    pub transport: Option<String>,
    /// None: the model's `nng_sendbuf:`, else the default.
    pub nng_sendbuf: Option<usize>,
}

const FLAGS: [&str; 6] = [
    "--model",
    "--server",
    "--publish",
    "--rate",
    "--transport",
    "--nng-sendbuf",
];

impl RunArgs {
    fn set(&mut self, flag: &str, v: String) -> Result<(), String> {
        match flag {
            "--model" => self.path = Some(v),
            "--server" => self.server = v,
            "--publish" => self.publish = Some(v),
            "--transport" => self.transport = Some(v),
            "--nng-sendbuf" => {
                self.nng_sendbuf = Some(v.parse().map_err(|e| format!("--nng-sendbuf: {e}"))?)
            }
            _ => self.rate = Some(v.parse::<f64>().map_err(|e| format!("--rate: {e}"))?),
        }
        Ok(())
    }

    /// `[model.yaml | --model model.yaml] [--server ep] [--publish ep] [--rate seconds] [--transport nng|zmq] [--nng-sendbuf n]`
    pub fn parse(args: &[String]) -> Result<RunArgs, String> {
        let mut r = RunArgs {
            path: None,
            server: "tcp://0.0.0.0:5555".into(),
            publish: None,
            rate: None,
            transport: None,
            nng_sendbuf: None,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if FLAGS.contains(&a.as_str()) {
                let v = it
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{a} needs a value"))?;
                r.set(a, v)?;
            } else if !a.starts_with('-') && r.path.is_none() {
                r.path = Some(a.clone());
            } else {
                return Err(format!("unknown argument `{a}`"));
            }
        }
        Ok(r)
    }
}

fn load_model(path: Option<&str>) -> Result<Model, String> {
    let path = path.ok_or("no model given")?;
    let yaml = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Model::load_in(&yaml, std::path::Path::new(path).parent())
}

/// Publish period: `--rate`, else the model's `publish_rate`, else 0.1 s.
pub fn publish_period(rate: Option<f64>, model: &Model) -> Result<f64, String> {
    let period = rate.or(model.publish_rate).unwrap_or(0.1);
    if period <= 0.0 {
        return Err("publish rate must be > 0".into());
    }
    Ok(period)
}

fn owned_snapshot(model: &Model) -> Vec<(String, Value)> {
    model
        .snapshot()
        .into_iter()
        .map(|(t, v)| (t.to_string(), v))
        .collect()
}

/// Publisher thread: one message per `period` until `STOP`.
fn spawn_publisher(
    sh: Arc<Mutex<Shared>>,
    out: Box<dyn Publisher>,
    period: f64,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut k = 0u64;
        while !STOP.load(Ordering::SeqCst) {
            out.publish(&snapshot_msg(&sh.lock().unwrap().snap));
            k += 1;
            sleep_until(
                t0 + Duration::from_secs_f64(k as f64 * period),
                Duration::from_millis(5),
            );
        }
    })
}

/// One engine tick: apply queued writes, publish the snapshot, then advance the model.
fn engine_tick(model: &mut Model, sh: &Mutex<Shared>) {
    let writes = std::mem::take(&mut sh.lock().unwrap().queue);
    for tag in model.apply_writes(&writes) {
        println!("Warn: write to {tag} rejected");
    }
    sh.lock().unwrap().snap = owned_snapshot(model);
    model.step(model.dt);
}

/// Engine: tick in real time (one model step per `dt` of wall clock) until `STOP`.
fn engine(model: &mut Model, sh: &Mutex<Shared>) {
    let t0 = Instant::now();
    let mut k = 0u64;
    while !STOP.load(Ordering::SeqCst) {
        engine_tick(model, sh);
        k += 1;
        let due = t0 + Duration::from_secs_f64(k as f64 * model.dt);
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let a = RunArgs::parse(args)?;
    let mut model = load_model(a.path.as_deref())?;
    let period = publish_period(a.rate, &model)?;
    let meta = Meta::of(&model);
    let sh = Arc::new(Mutex::new(Shared {
        snap: owned_snapshot(&model),
        queue: vec![],
    }));
    install_signals();

    let t = transport::select(
        a.transport.as_deref(),
        model.transport.as_deref(),
        a.nng_sendbuf.or(model.nng_sendbuf),
    )?;
    let sh2 = sh.clone();
    let rep_t = t.serve(&a.server, Box::new(move |req| handle(req, &meta, &sh2)))?;
    let publish = a.publish.as_deref().unwrap_or(t.default_publish());
    let out = t.publisher(publish)?;
    println!(
        "Info: ferrofire run: {} dt={}s; transport {}; REP {}; {} every {period}s",
        a.path.as_deref().unwrap_or(""),
        model.dt,
        t.name(),
        a.server,
        out.describe()
    );
    let pub_t = spawn_publisher(sh.clone(), out, period);

    engine(&mut model, &sh);
    let _ = rep_t.join();
    let _ = pub_t.join();
    println!("Info: stopped");
    Ok(())
}
