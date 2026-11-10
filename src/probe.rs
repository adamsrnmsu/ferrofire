//! `ferrofire probe`: the operator's view of a provider (bennu-probe equivalent). `query`, `read` and `write` talk to its REP
//! socket; `watch` decodes its datagrams (datagram v1) and shows each tag's value, age in publish periods, stale flag and
//! the count of datagrams missing from the `_ff.seq` run. `--json` prints one JSON object per line.

use crate::bennu::{exit_on_err, group_for, install_signals, parse_udp, STOP};
use crate::datagram::{Quality, Receiver};
use crate::pull::{requests, update_msg, validate};
use crate::transport::{self, Requester, DEFAULT_PUBLISH};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USAGE: &str = "ferrofire probe [--server tcp://127.0.0.1:5555] [--json] (query | read a,b | write a=1,b=2 [--confirm] | watch [--listen udp://*;239.0.0.1:40000] [--tags a,b])";

#[derive(Debug, clap::Parser)]
#[command(name = "ferrofire probe")]
pub struct ProbeCli {
    /// REP endpoint of the provider
    #[arg(long, global = true, default_value = "tcp://127.0.0.1:5555")]
    server: String,
    /// One JSON object per line instead of text
    #[arg(long, global = true)]
    json: bool,
    /// Give up on a request after this many milliseconds
    #[arg(long, global = true, default_value_t = 2000)]
    timeout_ms: i32,
    #[command(subcommand)]
    cmd: Sub,
}

#[derive(Debug, clap::Subcommand)]
enum Sub {
    /// List the provider's tags
    Query,
    /// Print the current value of tags
    Read {
        #[arg(value_delimiter = ',', required = true, value_name = "A,B,C")]
        tags: Vec<String>,
    },
    /// Set writable tags; audited on stderr
    Write {
        #[arg(value_delimiter = ',', required = true, value_name = "TAG=VALUE[,..]")]
        points: Vec<String>,
        /// Do not ask before writing
        #[arg(long)]
        confirm: bool,
    },
    /// Follow the published datagrams: value, age, stale, sequence gaps
    Watch {
        /// Publish endpoint to listen on
        #[arg(long, default_value = DEFAULT_PUBLISH, value_name = "ENDPOINT")]
        listen: String,
        /// Tags to show (default: all the provider has)
        #[arg(long, value_delimiter = ',', value_name = "A,B,C")]
        tags: Vec<String>,
        /// Publish periods without a refresh after which a tag is stale
        #[arg(long, default_value_t = 3)]
        stale_after: u64,
    },
}

/// One line of output: a tag and its value, plus (from `watch`) age in publish periods, stale flag and seq gaps so far.
#[derive(Debug, PartialEq)]
pub struct Row {
    pub tag: String,
    pub value: String,
    pub live: Option<(u64, bool, u64)>,
}

impl Row {
    pub fn text(&self) -> String {
        let live = self.live.map_or(String::new(), |(age, stale, gaps)| {
            let q = if stale { "STALE" } else { "ok" };
            format!(" age={age} {q} gaps={gaps}")
        });
        format!("{}={}{live}", self.tag, self.value)
    }

    pub fn json(&self) -> String {
        let value = match self.value.parse::<f64>() {
            Ok(v) if v.is_finite() => self.value.clone(),
            _ if self.value == "true" || self.value == "false" => self.value.clone(),
            _ => json_str(&self.value),
        };
        let live = self.live.map_or(String::new(), |(age, stale, gaps)| {
            format!(",\"age\":{age},\"stale\":{stale},\"seq_gaps\":{gaps}")
        });
        format!(
            "{{\"tag\":{},\"value\":{value}{live}}}",
            json_str(&self.tag)
        )
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out + "\""
}

/// The tag names in a `QUERY=` reply.
pub fn tag_list(reply: &[u8]) -> Result<Vec<String>, String> {
    Ok(update_msg(reply)?
        .split(',')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect())
}

/// The `tag:value,...` pairs of a `READ=` reply, in order.
pub fn read_rows(reply: &[u8]) -> Result<Vec<Row>, String> {
    update_msg(reply)?
        .split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (tag, value) = p
                .split_once(':')
                .ok_or_else(|| format!("malformed reply point `{p}`"))?;
            Ok(Row {
                tag: tag.into(),
                value: value.into(),
                live: None,
            })
        })
        .collect()
}

/// `a=1,b=true` -> `WRITE=a:1,b:true,`. The provider checks tag and value; a malformed point is caught here.
pub fn write_request(points: &[String]) -> Result<Vec<u8>, String> {
    let mut req = String::from("WRITE=");
    for p in points {
        match p.split_once('=') {
            Some((t, v)) if !t.is_empty() && !v.is_empty() => req += &format!("{t}:{v},"),
            _ => return Err(format!("`{p}`: expected tag=value")),
        }
    }
    Ok(req.into_bytes())
}

/// The audit record of a write: time (unix ms), who, where, what and the provider's answer.
pub fn audit_line(
    now_ms: u128,
    source: &str,
    server: &str,
    points: &[String],
    result: &str,
) -> String {
    format!(
        "audit: write t={now_ms} by={source} server={server} points={} result={result}",
        points.join(",")
    )
}

/// The yes/no before a write that was not `--confirm`ed.
fn confirmed(confirm: bool, points: &[String], mut input: impl BufRead) -> Result<(), String> {
    if !confirm {
        eprint!("write {} ? [y/N] ", points.join(","));
        let mut line = String::new();
        input.read_line(&mut line).map_err(|e| e.to_string())?;
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            return Err("not confirmed (pass --confirm to skip the question)".into());
        }
    }
    Ok(())
}

/// Turns datagrams into rows: one when a watched tag's value or stale flag differs from what was last shown.
pub struct Watcher {
    rx: Receiver,
    tags: Vec<String>,
    last: HashMap<String, f64>,
    shown: HashMap<String, (f64, bool)>,
}

impl Watcher {
    pub fn new(group: &str, tags: Vec<String>, stale_after: u64) -> Watcher {
        let mut rx = Receiver::new(group, tags.iter().map(String::as_str));
        rx.set_stale_after(stale_after);
        Watcher {
            rx,
            tags,
            last: HashMap::new(),
            shown: HashMap::new(),
        }
    }

    /// A datagram that is rejected shows nothing: the held values stay as shown.
    pub fn feed(&mut self, d: &[u8]) -> Vec<Row> {
        let Ok(points) = self.rx.accept(d) else {
            return vec![];
        };
        self.last.extend(points);
        let mut rows = vec![];
        for tag in &self.tags {
            let (Some((q, age)), Some(&v)) = (self.rx.quality(tag), self.last.get(tag)) else {
                continue;
            };
            let state = (v, q == Quality::Stale);
            if self.shown.insert(tag.clone(), state) != Some(state) {
                rows.push(Row {
                    tag: tag.clone(),
                    value: v.to_string(),
                    live: Some((age, state.1, self.rx.gaps())),
                });
            }
        }
        rows
    }
}

pub fn main(cli: ProbeCli) {
    exit_on_err("probe", USAGE, run(cli));
}

fn tag_line(tag: &str, json: bool) -> String {
    match json {
        true => format!("{{\"tag\":{}}}", json_str(tag)),
        false => tag.into(),
    }
}

fn print_lines(lines: impl Iterator<Item = String>) {
    let mut out = std::io::stdout().lock();
    for l in lines {
        let _ = writeln!(out, "{l}");
    }
    let _ = out.flush();
}

fn print(rows: &[Row], json: bool) {
    print_lines(rows.iter().map(|r| if json { r.json() } else { r.text() }));
}

/// The interface to join a group on: `*` or none is the default one.
pub fn nic(iface: Option<String>) -> Result<Ipv4Addr, String> {
    iface.map_or(Ok(Ipv4Addr::UNSPECIFIED), |i| {
        i.parse()
            .map_err(|_| format!("interface `{i}`: only IPv4 addresses are supported"))
    })
}

/// The socket `watch` listens on: joined to the group for a multicast endpoint, plainly bound otherwise.
fn listen(endpoint: &str) -> Result<UdpSocket, String> {
    let (iface, addr) = parse_udp(endpoint)?;
    let IpAddr::V4(ip) = addr.ip() else {
        return Err("only IPv4 endpoints are supported".into());
    };
    // ponytail: no SO_REUSEADDR, so a second subscriber on this host and port cannot bind; add a sys wrapper if probing next to a field device is needed
    let bind = if ip.is_multicast() {
        Ipv4Addr::UNSPECIFIED
    } else {
        ip
    };
    let s = UdpSocket::bind((bind, addr.port())).map_err(|e| format!("{addr}: {e}"))?;
    if ip.is_multicast() {
        s.join_multicast_v4(&ip, &nic(iface)?)
            .map_err(|e| e.to_string())?;
    }
    s.set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| e.to_string())?;
    Ok(s)
}

fn watch(endpoint: &str, mut w: Watcher, json: bool) -> Result<(), String> {
    let sock = listen(endpoint)?;
    install_signals();
    let mut buf = vec![0u8; 65536];
    while !STOP.load(Ordering::SeqCst) {
        match sock.recv(&mut buf) {
            Ok(n) => print(&w.feed(&buf[..n]), json),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn ask(req: &mut dyn Requester, r: &[u8]) -> Result<Vec<u8>, String> {
    req.call(r).or_else(|_| req.call(r))
}

fn run(cli: ProbeCli) -> Result<(), String> {
    let mut req = transport::requester(&cli.server, cli.timeout_ms)?;
    let (req, json) = (&mut *req, cli.json);
    match cli.cmd {
        Sub::Query => query(req, json),
        Sub::Read { tags } => read(req, &tags, json),
        Sub::Write { points, confirm } => write(req, &cli.server, &points, confirm),
        Sub::Watch {
            listen,
            tags,
            stale_after,
        } => {
            let tags = watched(req, tags)?;
            watch(
                &listen,
                Watcher::new(&group_for(&listen), tags, stale_after),
                json,
            )
        }
    }
}

fn query(req: &mut dyn Requester, json: bool) -> Result<(), String> {
    let tags = tag_list(&ask(req, b"QUERY=")?)?;
    print_lines(tags.iter().map(|t| tag_line(t, json)));
    Ok(())
}

fn read(req: &mut dyn Requester, tags: &[String], json: bool) -> Result<(), String> {
    for r in requests(tags) {
        print(&read_rows(&ask(req, &r)?)?, json);
    }
    Ok(())
}

/// Ask, send (once: a write is not retried), then the audit line, whatever the provider said.
fn write(
    req: &mut dyn Requester,
    server: &str,
    points: &[String],
    confirm: bool,
) -> Result<(), String> {
    let r = write_request(points)?;
    confirmed(confirm, points, std::io::stdin().lock())?;
    let res = req.call(&r).and_then(|reply| update_msg(&reply));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let user = std::env::var("USER").unwrap_or_else(|_| "?".into());
    let shown = res.as_ref().map_or_else(|e| e.clone(), |_| "ACK".into());
    eprintln!(
        "{}",
        audit_line(
            now,
            &format!("{user}@pid{}", std::process::id()),
            server,
            points,
            &shown
        )
    );
    res.map(|_| ())
}

/// The tags to watch: those named (checked against the provider) or all it has.
fn watched(req: &mut dyn Requester, tags: Vec<String>) -> Result<Vec<String>, String> {
    if tags.is_empty() {
        return tag_list(&ask(req, b"QUERY=")?);
    }
    validate(req, &tags)?;
    Ok(tags)
}
