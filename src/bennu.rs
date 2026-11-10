//! Native bennu provider frontend (spec 2.1): ZMQ REP (QUERY/READ/WRITE) + UDP RADIO/DISH datagrams.
//! Wire formats are matched from the bennu protocol documentation only; no bennu code is used.
//! Pure pieces (hash, framing, chunking, request handling) are plain functions so tests need no sockets.

use crate::hint::{did_you_mean, lock, show, RateLimit};
use crate::model::SliceSpec;
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

/// Bytes kept free in every chunk for the sequence trailer: `_ff.seq:` + `_ff.cyc:` + `_ff.t:` with 20-digit values is 85.
pub const TRAILER_MAX: usize = 96;

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

/// The pseudo-tags appended to every datagram (ferrofire datagram v1): the per-endpoint datagram counter, the
/// publish-cycle counter and the send time in unix ns. Stock bennu ignores point names it does not subscribe to.
pub fn trailer(seq: u64, cyc: u64, t_ns: u64) -> String {
    format!("_ff.seq:{seq},_ff.cyc:{cyc},_ff.t:{t_ns},")
}

/// The wire chunks of a `tag:value,...` message: whole points, then the trailer, then a NUL. Datagram `i` of the
/// cycle carries `seq0 + i`. The tag part is cut `TRAILER_MAX` short of `CHUNK`, so no chunk exceeds `CHUNK`.
pub fn chunks_nul(msg: &str, seq0: u64, cyc: u64, t_ns: u64) -> Vec<Vec<u8>> {
    chunk_message(msg, CHUNK - TRAILER_MAX)
        .into_iter()
        .zip(seq0..)
        .map(|(c, seq)| {
            let mut b = (c + &trailer(seq, cyc, t_ns)).into_bytes();
            b.push(0);
            b
        })
        .collect()
}

/// Framed datagrams for one publish cycle of an already-built message.
pub fn datagrams_msg(group: &str, msg: &str, seq0: u64, cyc: u64, t_ns: u64) -> Vec<Vec<u8>> {
    chunks_nul(msg, seq0, cyc, t_ns)
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
    /// Limits the runtime "bad request" warnings to one per `WARN_EVERY`.
    pub warn: RateLimit,
}

/// Largest request the provider looks at; a bigger one is answered `ERR`, not parsed.
pub const MAX_REQUEST: usize = 64 * 1024;
const WARN_EVERY: Duration = Duration::from_secs(10);

impl Meta {
    pub fn of(m: &Model) -> Meta {
        let tags: Vec<String> = m.tags().map(|(t, _)| t.to_string()).collect();
        let writable = tags.iter().filter(|t| m.writable(t)).cloned().collect();
        Meta {
            tags,
            writable,
            warn: RateLimit::new(WARN_EVERY),
        }
    }
}

fn parse_value(s: &str) -> Option<Value> {
    match s {
        "true" => Some(Value::Binary(true)),
        "false" => Some(Value::Binary(false)),
        _ => s
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .map(Value::Analog),
    }
}

/// One request -> one reply, always (a REP socket must answer or it wedges). Both carry a trailing NUL.
pub fn handle(req: &[u8], meta: &Meta, sh: &Mutex<Shared>) -> Vec<u8> {
    if req.len() > MAX_REQUEST {
        let n = req.len();
        return with_nul(format!(
            "ERR=request too large ({n} bytes, limit {MAX_REQUEST})"
        ));
    }
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
    let sh = lock(sh);
    let find = |t: &str| sh.snap.iter().find(|(n, _)| n == t).map(|(_, v)| *v);
    let missing = |t: &str| {
        let hint = did_you_mean(t, sh.snap.iter().map(|(n, _)| n.as_str()));
        format!("ERR=Tag '{}' not found{hint}", show(t))
    };
    if !payload.contains(',') {
        return match find(payload) {
            Some(v) => format!("ACK={}", fmt_value(v)),
            None => missing(payload),
        };
    }
    let mut out = String::from("ACK=");
    for t in payload.split(',').filter(|t| !t.is_empty()) {
        match find(t) {
            Some(v) => out.push_str(&format!("{t}:{},", fmt_value(v))),
            None => return missing(t),
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
        return format!("ERR=Malformed request '{}'", show(req));
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
                    return format!("ERR=Malformed point '{}'", show(point));
                };
                if !meta.writable.contains(tag) {
                    return refuse(meta, tag);
                }
                let Some(v) = parse_value(val) else {
                    return format!("ERR=Bad value '{}' for '{tag}'", show(val));
                };
                batch.push((tag.to_string(), v));
            }
            // all-or-nothing: the field device's retry logic isolates a bad point
            lock(sh).queue.extend(batch);
            "ACK=".into()
        }
        _ => format!("ERR=Unknown command type '{}'", show(op)),
    }
}

/// The ERR for a WRITE to a tag that is unknown or read-only, with a suggestion; the warning is rate limited.
fn refuse(meta: &Meta, tag: &str) -> String {
    let hint = did_you_mean(tag, meta.writable.iter().map(String::as_str));
    let msg = format!("Tag '{}' is unknown or read-only{hint}", show(tag));
    if meta.warn.allow(Instant::now()) {
        tracing::warn!("rejected WRITE: {msg}");
    }
    format!("ERR={msg}")
}

// ---- sockets and threads -------------------------------------------------------------------------------------

pub static STOP: AtomicBool = AtomicBool::new(false);
pub extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Start logging; SIGINT and SIGTERM raise `STOP`, which every loop in the process polls.
pub fn install_signals() {
    crate::trace::init(); // every long-running subcommand starts here, so logging starts with it
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

const USAGE: &str = "ferrofire run [model.yaml | --model model.yaml] [--server tcp://0.0.0.0:5555] [--publish 'udp://*;239.0.0.1:40000'] [--publish-tags 'ENDPOINT=a,b,c' | 'ENDPOINT=@config.xml']... [--rate 0.1]";

pub fn main(args: RunArgs) {
    exit_on_err("run", USAGE, run(args));
}

/// A subcommand's failure: `ferrofire <cmd>: <error>` and its usage on stderr, exit 1.
pub fn exit_on_err(cmd: &str, usage: &str, res: Result<(), String>) {
    if let Err(e) = res {
        eprintln!("ferrofire {cmd}: {e}\nusage: {usage}");
        std::process::exit(1);
    }
}

/// Serve a model over the field-device REP protocol and publish its tags.
#[derive(Debug, clap::Parser)]
#[command(name = "ferrofire run")]
pub struct RunArgs {
    /// Model file (or give it with --model)
    #[arg(value_name = "MODEL.YAML", conflicts_with = "model")]
    pos_model: Option<String>,
    /// Model file
    #[arg(long, value_name = "MODEL.YAML")]
    model: Option<String>,
    /// REP endpoint
    #[arg(long, default_value = "tcp://0.0.0.0:5555")]
    pub server: String,
    /// All-tags publish endpoint, e.g. 'udp://*;239.0.0.1:40000' (default endpoint when no slices are given)
    #[arg(long, value_name = "ENDPOINT")]
    pub publish: Option<String>,
    /// Publish period in seconds (default: the model's publish_rate, else 0.1)
    #[arg(long, value_name = "SECONDS")]
    pub rate: Option<f64>,
    /// Per-RTU slice 'ENDPOINT=a,b,c' or 'ENDPOINT=@config.xml', repeatable
    #[arg(long = "publish-tags", value_name = "SPEC")]
    pub slices: Vec<String>,
}

impl RunArgs {
    /// `--model` or the positional model, whichever was given.
    pub fn path(&self) -> Option<&str> {
        self.model.as_deref().or(self.pos_model.as_deref())
    }
}

// ---- publish targets: one endpoint per RTU, each with its own slice of the tags ------------------------------

/// An endpoint and the tags it carries (empty: all of them).
#[derive(Debug, PartialEq, Clone)]
pub struct Slice {
    pub endpoint: String,
    pub tags: Vec<String>,
}

/// A slice as the model YAML or the command line wrote it: tags inline or read from a bennu `config.xml`
/// (relative paths resolve against the model's directory).
pub fn resolve_slice(spec: &SliceSpec, base: Option<&std::path::Path>) -> Result<Slice, String> {
    let mut tags = spec.tags.clone();
    if let Some(f) = &spec.tags_from {
        let path = base.map_or(f.into(), |b| b.join(f));
        tags.extend(crate::pull::tags_from_file(&path.to_string_lossy())?);
    }
    Ok(Slice {
        endpoint: spec.endpoint.clone(),
        tags,
    })
}

/// `ENDPOINT=a,b,c` or `ENDPOINT=@config.xml`.
pub fn parse_slice(arg: &str) -> Result<SliceSpec, String> {
    let (endpoint, rest) = arg.split_once('=').ok_or_else(|| {
        format!("--publish-tags `{arg}`: expected ENDPOINT=a,b,c or ENDPOINT=@config.xml")
    })?;
    let (tags, tags_from) = match rest.strip_prefix('@') {
        Some(f) => (vec![], Some(f.to_string())),
        None => (rest.split(',').map(String::from).collect(), None),
    };
    Ok(SliceSpec {
        endpoint: endpoint.into(),
        tags,
        tags_from,
    })
}

/// Endpoints to publish to, each with its tags (None: all). `--publish` is the all-tags endpoint; slices add
/// per-RTU endpoints; with neither, the default endpoint carries everything. Tags must exist, endpoints must differ
/// (two with one group would share a counter stream), and a tag may appear in several slices.
pub fn plan(publish: Option<&str>, slices: Vec<Slice>, known: &[String]) -> Result<Plan, String> {
    let mut out: Plan = vec![];
    if publish.is_some() || slices.is_empty() {
        out.push((publish.unwrap_or(transport::DEFAULT_PUBLISH).into(), None));
    }
    let known: HashSet<&String> = known.iter().collect();
    for s in slices {
        let bad: Vec<String> = s
            .tags
            .iter()
            .filter(|t| !known.contains(t))
            .map(|t| format!("{t}{}", did_you_mean(t, known.iter().map(|k| k.as_str()))))
            .collect();
        if !bad.is_empty() || s.tags.is_empty() {
            return Err(format!(
                "publish slice {}: {}",
                s.endpoint,
                if s.tags.is_empty() {
                    "no tags".into()
                } else {
                    format!("unknown tags: {}", bad.join(", "))
                }
            ));
        }
        out.push((s.endpoint, Some(s.tags)));
    }
    let mut groups = HashSet::new();
    for (ep, _) in &out {
        if !groups.insert(group_for(ep)) {
            return Err(format!(
                "publish endpoint {ep} is listed twice (same RADIO group)"
            ));
        }
    }
    Ok(out)
}

/// Endpoint and tags (None: all) per publish target.
pub type Plan = Vec<(String, Option<Vec<String>>)>;

/// A publisher and the tags it carries (None: all).
pub struct Target {
    pub out: Box<dyn Publisher>,
    pub tags: Option<HashSet<String>>,
}

/// Send each target its part of `snap`, in snapshot order.
pub fn publish_all(targets: &[Target], snap: &[(String, Value)]) {
    for t in targets {
        match &t.tags {
            None => t.out.publish(&snapshot_msg(snap)),
            Some(set) => {
                let part: Vec<_> = snap
                    .iter()
                    .filter(|(n, _)| set.contains(n))
                    .cloned()
                    .collect();
                t.out.publish(&snapshot_msg(&part));
            }
        }
    }
}

fn load_model(path: Option<&str>) -> Result<Model, String> {
    let path = path.ok_or("no model given")?;
    let yaml = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Model::load_in(&yaml, std::path::Path::new(path).parent()).map_err(|e| format!("{path}: {e}"))
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

/// One engine tick: apply queued writes, store the snapshot for the REP side, then advance the model.
/// Returns the snapshot taken before the step.
fn engine_tick(model: &mut Model, sh: &Mutex<Shared>) -> Vec<(String, Value)> {
    let writes = std::mem::take(&mut lock(sh).queue);
    for tag in model.apply_writes(&writes) {
        println!("Warn: write to {tag} rejected");
    }
    let snap = owned_snapshot(model);
    lock(sh).snap = snap.clone();
    model.step(model.dt);
    snap
}

/// Tick `k` (0-based): tick the engine, then publish its snapshot from the same thread on every `every`-th tick.
pub fn tick_and_publish(
    model: &mut Model,
    sh: &Mutex<Shared>,
    targets: &[Target],
    k: u64,
    every: u64,
) {
    let snap = engine_tick(model, sh);
    if k.is_multiple_of(every) {
        publish_all(targets, &snap);
    }
}

/// Engine: tick in real time (one model step per `dt` of wall clock) until `STOP`.
fn engine(model: &mut Model, sh: &Mutex<Shared>, targets: &[Target], every: u64) {
    let t0 = Instant::now();
    let mut k = 0u64;
    while !STOP.load(Ordering::SeqCst) {
        tick_and_publish(model, sh, targets, k, every);
        k += 1;
        let due = t0 + Duration::from_secs_f64(k as f64 * model.dt);
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
    }
}

fn targets_of(plan: Plan) -> Result<Vec<Target>, String> {
    plan.into_iter()
        .map(|(ep, tags)| {
            Ok(Target {
                out: transport::publisher(&ep)?,
                tags: tags.map(|t| t.into_iter().collect()),
            })
        })
        .collect()
}

fn run(a: RunArgs) -> Result<(), String> {
    let mut model = load_model(a.path())?;
    let period = publish_period(a.rate, &model)?;
    let meta = Meta::of(&model);
    let base = a.path().and_then(|p| std::path::Path::new(p).parent());
    let specs = model.publish_slices.iter().cloned();
    let specs: Vec<SliceSpec> = specs
        .chain(
            a.slices
                .iter()
                .map(|s| parse_slice(s))
                .collect::<Result<Vec<_>, _>>()?,
        )
        .collect();
    let slices = specs
        .iter()
        .map(|s| resolve_slice(s, base))
        .collect::<Result<Vec<_>, _>>()?;
    let targets = targets_of(plan(a.publish.as_deref(), slices, &meta.tags)?)?;
    let sh = Arc::new(Mutex::new(Shared {
        snap: owned_snapshot(&model),
        queue: vec![],
    }));
    install_signals();
    crate::log::start("run");

    let sh2 = sh.clone();
    let rep_t = transport::serve(&a.server, Box::new(move |req| handle(req, &meta, &sh2)))?;
    println!(
        "Info: ferrofire run: {} dt={}s; REP {}; {} every {period}s",
        a.path().unwrap_or(""),
        model.dt,
        a.server,
        targets
            .iter()
            .map(|t| t.out.describe())
            .collect::<Vec<_>>()
            .join("; ")
    );
    let every = ((period / model.dt).round() as u64).max(1);
    engine(&mut model, &sh, &targets, every);
    let _ = rep_t.join();
    crate::log::stop();
    println!("Info: stopped");
    Ok(())
}
