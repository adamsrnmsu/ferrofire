//! Native bennu provider frontend (spec 2.1): ZMQ REP (QUERY/READ/WRITE) + UDP RADIO/DISH datagrams.
//! Wire formats are matched from the bennu protocol documentation only; no bennu code is used.
//! Pure pieces (hash, framing, chunking, request handling) are plain functions so tests need no sockets.

use crate::model::{Model, Value};
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
    let mut chunks = data.chunks_exact(8);
    for c in &mut chunks {
        let d = shift_mix(u64::from_le_bytes(c.try_into().unwrap()).wrapping_mul(MUL)).wrapping_mul(MUL);
        hash = (hash ^ d).wrapping_mul(MUL);
    }
    let rest = chunks.remainder();
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
    if n > d.len() - 1 { return None; }
    Some((std::str::from_utf8(&d[1..1 + n]).ok()?, &d[1 + n..]))
}

/// Split `a:1,b:2,` into chunks of whole `part,` pieces: empty parts skipped, an over-long single part returned alone.
pub fn chunk_message(msg: &str, mtu: usize) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for p in msg.split(',').filter(|p| !p.is_empty()) {
        if !cur.is_empty() && cur.len() + p.len() + 1 >= mtu { out.push(std::mem::take(&mut cur)); }
        cur.push_str(p);
        cur.push(',');
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

/// Chunk size: UDP + IP + group headers still fit a 1500-byte Ethernet MTU.
pub const CHUNK: usize = 1400;

pub fn fmt_value(v: Value) -> String {
    match v { Value::Binary(b) => b.to_string(), Value::Analog(x) => x.to_string() }
}

/// Datagrams for one publish tick (each `tag:value,...` chunk NUL-terminated, then framed).
pub fn datagrams(group: &str, snap: &[(String, Value)]) -> Vec<Vec<u8>> {
    datagrams_msg(group, &snap.iter().map(|(t, v)| format!("{t}:{},", fmt_value(*v))).collect::<String>())
}

/// Same, from an already-built `tag:value,...` message.
pub fn datagrams_msg(group: &str, msg: &str) -> Vec<Vec<u8>> {
    chunk_message(msg, CHUNK).into_iter().map(|c| { let mut b = c.into_bytes(); b.push(0); frame(group, &b) }).collect()
}

// ---- request handling ----------------------------------------------------------------------------------------

#[derive(Default)]
pub struct Shared {
    pub snap: Vec<(String, Value)>,
    pub queue: Vec<(String, Value)>,
}

pub struct Meta { pub tags: Vec<String>, pub writable: HashSet<String> }

impl Meta {
    pub fn of(m: &Model) -> Meta {
        let tags: Vec<String> = m.tags().map(|(t, _)| t.to_string()).collect();
        let writable = tags.iter().filter(|t| m.writable(t)).cloned().collect();
        Meta { tags, writable }
    }
}

fn parse_value(s: &str) -> Option<Value> {
    match s { "true" => Some(Value::Binary(true)), "false" => Some(Value::Binary(false)), _ => s.parse().ok().map(Value::Analog) }
}

/// One request -> one reply, always (a REP socket must answer or it wedges). Both carry a trailing NUL.
pub fn handle(req: &[u8], meta: &Meta, sh: &Mutex<Shared>) -> Vec<u8> {
    let mut r = reply(req, meta, sh).into_bytes();
    r.push(0);
    r
}

fn reply(req: &[u8], meta: &Meta, sh: &Mutex<Shared>) -> String {
    let Ok(req) = std::str::from_utf8(req) else { return "ERR=request is not UTF-8".into() };
    let req = req.trim_end_matches('\0');
    let Some((op, payload)) = req.split_once('=') else { return format!("ERR=Malformed request '{req}'") };
    match op.to_ascii_lowercase().as_str() {
        "query" => format!("ACK={}", meta.tags.iter().map(|t| format!("{t},")).collect::<String>()),
        "read" => match sh.lock().unwrap().snap.iter().find(|(t, _)| t == payload) {
            Some((_, v)) => format!("ACK={}", fmt_value(*v)),
            None => format!("ERR=Tag '{payload}' not found"),
        },
        "write" => {
            let mut batch = vec![];
            for point in payload.split(',').filter(|p| !p.is_empty()) {
                let Some((tag, val)) = point.split_once(':') else { return format!("ERR=Malformed point '{point}'") };
                if !meta.writable.contains(tag) { return format!("ERR=Tag '{tag}' is unknown or read-only"); }
                let Some(v) = parse_value(val) else { return format!("ERR=Bad value '{val}' for '{tag}'") };
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
pub extern "C" fn on_signal(_: libc::c_int) { STOP.store(true, Ordering::SeqCst); }

/// `udp://[iface;]host:port` -> (iface, destination)
pub fn parse_udp(ep: &str) -> Result<(Option<String>, SocketAddr), String> {
    let rest = ep.strip_prefix("udp://").ok_or_else(|| format!("publish endpoint `{ep}` must be udp://[iface;]group:port"))?;
    let (iface, dest) = match rest.rsplit_once(';') { Some((i, d)) => (Some(i.to_string()).filter(|i| i != "*"), d), None => (None, rest) };
    let addr = dest.to_socket_addrs().map_err(|e| format!("{dest}: {e}"))?.find(SocketAddr::is_ipv4).ok_or_else(|| format!("{dest}: no IPv4 address"))?;
    Ok((iface, addr))
}

pub fn udp_sender(iface: Option<&str>, dest: SocketAddr) -> Result<UdpSocket, String> {
    let s = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    if dest.ip().is_multicast() {
        s.set_multicast_ttl_v4(1).map_err(|e| e.to_string())?; // libzmq multicast_hops default
        s.set_multicast_loop_v4(true).map_err(|e| e.to_string())?; // same-host subscribers work
        if let Some(i) = iface {
            let a: Ipv4Addr = i.parse().map_err(|_| format!("interface `{i}`: only IPv4 addresses are supported"))?;
            use std::os::fd::AsRawFd;
            let ia = libc::in_addr { s_addr: u32::from(a).to_be() };
            let rc = unsafe { libc::setsockopt(s.as_raw_fd(), libc::IPPROTO_IP, libc::IP_MULTICAST_IF, &ia as *const _ as *const libc::c_void, std::mem::size_of::<libc::in_addr>() as u32) };
            if rc != 0 { return Err(format!("IP_MULTICAST_IF {a}: {}", std::io::Error::last_os_error())); }
        }
    }
    Ok(s)
}

const USAGE: &str = "rustbelt run [model.yaml | --model model.yaml] [--server tcp://0.0.0.0:5555] [--publish 'udp://*;239.0.0.1:40000'] [--rate 0.1]";

pub fn main(args: &[String]) {
    if let Err(e) = run(args) { eprintln!("rustbelt run: {e}\nusage: {USAGE}"); std::process::exit(1); }
}

fn run(args: &[String]) -> Result<(), String> {
    let (mut path, mut server, mut publish, mut rate) = (None, "tcp://0.0.0.0:5555".to_string(), "udp://*;239.0.0.1:40000".to_string(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--model" => path = Some(val()?),
            "--server" => server = val()?,
            "--publish" => publish = val()?,
            "--rate" => rate = Some(val()?.parse::<f64>().map_err(|e| format!("--rate: {e}"))?),
            p if !p.starts_with('-') && path.is_none() => path = Some(p.to_string()),
            _ => return Err(format!("unknown argument `{a}`")),
        }
    }
    let path = path.ok_or("no model given")?;
    let yaml = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut model = Model::load_in(&yaml, std::path::Path::new(&path).parent())?;
    let dt = model.dt;
    let period = rate.or(model.publish_rate).unwrap_or(0.1);
    if period <= 0.0 { return Err("publish rate must be > 0".into()); }

    let meta = Arc::new(Meta::of(&model));
    let sh = Arc::new(Mutex::new(Shared { snap: model.snapshot().into_iter().map(|(t, v)| (t.to_string(), v)).collect(), queue: vec![] }));
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }

    // REP server
    let ctx = zmq::Context::new();
    let rep = ctx.socket(zmq::REP).map_err(|e| e.to_string())?;
    rep.bind(&server).map_err(|e| format!("bind {server}: {e}"))?;
    let (m2, s2) = (meta.clone(), sh.clone());
    let rep_t = std::thread::spawn(move || {
        while !STOP.load(Ordering::SeqCst) {
            if !matches!(rep.poll(zmq::POLLIN, 100), Ok(n) if n > 0) { continue; }
            if let Ok(req) = rep.recv_bytes(0) { let _ = rep.send(handle(&req, &m2, &s2), 0); }
        }
    });

    // UDP publisher
    let (iface, dest) = parse_udp(&publish)?;
    let sock = udp_sender(iface.as_deref(), dest)?;
    let group = group_for(&publish);
    let s3 = sh.clone();
    let pub_t = std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut k = 0u64;
        while !STOP.load(Ordering::SeqCst) {
            let snap = s3.lock().unwrap().snap.clone();
            for d in datagrams(&group, &snap) { let _ = sock.send_to(&d, dest); }
            k += 1;
            // sleep in short slices so a stop signal is noticed promptly
            while !STOP.load(Ordering::SeqCst) && Instant::now() < t0 + Duration::from_secs_f64(k as f64 * period) { std::thread::sleep(Duration::from_millis(5)); }
        }
    });
    println!("Info: rustbelt run: {path} dt={dt}s; REP {server}; RADIO {publish} group {} every {period}s", group_for(&publish));

    // engine: drain writes, step, publish snapshot, real-time pace
    let t0 = Instant::now();
    let mut k = 0u64;
    while !STOP.load(Ordering::SeqCst) {
        let writes = std::mem::take(&mut sh.lock().unwrap().queue);
        if !writes.is_empty() {
            for tag in model.apply_writes(&writes) { println!("Warn: write to {tag} rejected"); }
        }
        sh.lock().unwrap().snap = model.snapshot().into_iter().map(|(t, v)| (t.to_string(), v)).collect();
        model.step(dt);
        k += 1;
        if let Some(d) = (t0 + Duration::from_secs_f64(k as f64 * dt)).checked_duration_since(Instant::now()) { std::thread::sleep(d); }
    }
    let _ = rep_t.join();
    let _ = pub_t.join();
    println!("Info: stopped");
    Ok(())
}
