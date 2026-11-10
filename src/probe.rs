//! `ferrofire probe`: the operator's view of a provider (bennu-probe equivalent). `query`, `read` and `write` talk to its REP
//! socket; `watch` decodes its datagrams (datagram v1) and shows each tag's value, age in publish periods, stale flag and
//! the count of datagrams missing from the `_ff.seq` run. `--json` prints one JSON object per line.

use crate::bennu::{
    exit_on_err, group_for, install_signals, parse_udp, parse_value, sleep_until, STOP,
};
use crate::datagram::{Quality, Receiver};
use crate::model::{Model, Value};
use crate::pull::{requests, update_msg, validate};
use crate::transport::{self, Requester, DEFAULT_PUBLISH};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const USAGE: &str = "ferrofire probe [--server tcp://127.0.0.1:5555] [--json] (query | read a,b | write a=1,b=2 [--confirm] [--dry-run] [--model m.yaml] | play script.yaml [--confirm] [--dry-run] [--keep-going] [--model m.yaml] | record --out run.csv [--tags a,b] [--duration s] | diff --map rtus.yaml [--tolerance 0.01] [--cycles 1] [--every s] | watch [--listen udp://*;239.0.0.1:40000] [--tags a,b])";

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
    /// Print the current value of tags (`*` and `?` expand against QUERY)
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
        /// Check the values and print the request that would be sent, send nothing
        #[arg(long)]
        dry_run: bool,
        /// Model YAML to check types and min/max against before sending
        #[arg(long, value_name = "FILE")]
        model: Option<String>,
    },
    /// Play a script of timed writes (exercise injects); audited like `write`
    Play {
        /// YAML list of `{at: seconds, write: tag=val[,..], wait_for: "tag >= 3"}`
        script: String,
        /// Do not ask before playing
        #[arg(long)]
        confirm: bool,
        /// Check the script and print each step, send nothing
        #[arg(long)]
        dry_run: bool,
        /// Go on after a step fails (the exit status still reports it)
        #[arg(long)]
        keep_going: bool,
        /// Model YAML to check types and min/max against before sending
        #[arg(long, value_name = "FILE")]
        model: Option<String>,
    },
    /// Compare each RTU's Modbus values with the provider's (tamper/MITM detection); exits 1 on any mismatch
    Diff(crate::diff::DiffArgs),
    /// Tag names from the last query (for shell completion)
    #[command(hide = true)]
    CachedTags,
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
    /// Record the published values to a CSV: one row per cycle (cycle, t, each tag and its stale flag)
    Record {
        /// CSV file to write
        #[arg(long, value_name = "FILE")]
        out: String,
        /// Publish endpoint to listen on
        #[arg(long, default_value = DEFAULT_PUBLISH, value_name = "ENDPOINT")]
        listen: String,
        /// Tags to record, globs allowed (default: all the provider has)
        #[arg(long, value_delimiter = ',', value_name = "A,B,C")]
        tags: Vec<String>,
        /// Publish periods without a refresh after which a tag is stale
        #[arg(long, default_value_t = 3)]
        stale_after: u64,
        /// Stop after this many seconds (default: until SIGINT/SIGTERM)
        #[arg(long, value_name = "SECONDS")]
        duration: Option<f64>,
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

pub(crate) fn json_str(s: &str) -> String {
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

/// Whole-string match of `pat` (`*` any run, `?` one char) against `s`.
pub fn glob_match(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some(('*', rest)) => (0..=s.len()).any(|i| go(rest, &s[i..])),
            Some((c, rest)) => s
                .split_first()
                .is_some_and(|(x, tail)| (*c == '?' || c == x) && go(rest, tail)),
        }
    }
    go(
        &pat.chars().collect::<Vec<_>>(),
        &s.chars().collect::<Vec<_>>(),
    )
}

/// Globs replaced by the tags they match (in `all` order, no repeats); plain names pass through for the provider to check.
/// A glob matching nothing is an error.
pub fn expand(pats: &[String], all: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for p in pats {
        if !p.contains(['*', '?']) {
            out.push(p.clone());
            continue;
        }
        let hits: Vec<&String> = all.iter().filter(|t| glob_match(p, t)).collect();
        if hits.is_empty() {
            return Err(format!("no tag matches `{p}`"));
        }
        out.extend(hits.into_iter().cloned());
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(t.clone()));
    Ok(out)
}

fn has_glob(pats: &[String]) -> bool {
    pats.iter().any(|p| p.contains(['*', '?']))
}

/// Where the last QUERY answer is kept for shell completion. ponytail: one file, the last provider queried; key it by server if completion must follow `--server`.
pub fn cache_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".cache")))?;
    Some(base.join("ferrofire").join("tags"))
}

/// QUERY the provider and remember the answer for completion (a cache that cannot be written is ignored).
pub(crate) fn fetch_tags(req: &mut dyn Requester) -> Result<Vec<String>, String> {
    let tags = tag_list(&ask(req, b"QUERY=")?)?;
    if let Some(p) = cache_path() {
        let _ = p.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(p, tags.join("\n") + "\n");
    }
    Ok(tags)
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

/// Each `tag=value` against the model's type and limits for that tag (tags the model does not publish are left to the provider).
pub fn check_points(model: &Model, points: &[String]) -> Result<(), String> {
    for p in points {
        let (tag, val) = p.split_once('=').unwrap_or((p, ""));
        let v = parse_value(val).ok_or_else(|| format!("Bad value '{val}' for '{tag}'"))?;
        if let Some(spec) = model.write_spec(tag) {
            spec.check(tag, v)?;
        }
    }
    Ok(())
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

fn csv(a: impl ToString, b: impl ToString, cells: impl Iterator<Item = String>) -> String {
    [a.to_string(), b.to_string()]
        .into_iter()
        .chain(cells)
        .collect::<Vec<_>>()
        .join(",")
}

/// Turns datagrams into CSV rows, one per publish cycle: `cycle,t,<tag>,<tag>_stale,..` (empty until a tag has a value).
/// A cycle's row is written once a later cycle shows up (its chunks may arrive in any order), and the last one by `finish`.
pub struct Recorder {
    w: Watcher,
    /// The row of the newest cycle so far, and that cycle.
    pending: Option<(u64, String)>,
}

impl Recorder {
    pub fn new(group: &str, tags: Vec<String>, stale_after: u64) -> Recorder {
        Recorder {
            w: Watcher::new(group, tags, stale_after),
            pending: None,
        }
    }

    pub fn header(&self) -> String {
        csv(
            "cycle",
            "t",
            self.w
                .tags
                .iter()
                .flat_map(|t| [t.clone(), format!("{t}_stale")]),
        )
    }

    /// `t` is seconds since the recording began. Returns the row of the cycle this datagram finished, if it did.
    pub fn feed(&mut self, d: &[u8], t: f64) -> Option<String> {
        let points = self.w.rx.accept(d).ok()?;
        self.w.last.extend(points);
        let cyc = self.w.rx.head();
        let first_t = match &self.pending {
            Some((c, row)) if *c == cyc => row.split(',').nth(1).and_then(|x| x.parse().ok()),
            _ => None,
        };
        let row = self.row(cyc, first_t.unwrap_or(t));
        match self.pending.replace((cyc, row)) {
            Some((c, done)) if c != cyc => Some(done),
            _ => None,
        }
    }

    pub fn finish(&mut self) -> Option<String> {
        self.pending.take().map(|(_, row)| row)
    }

    fn row(&self, cyc: u64, t: f64) -> String {
        let cells = self.w.tags.iter().flat_map(|tag| {
            match (self.w.last.get(tag), self.w.rx.quality(tag)) {
                (Some(v), Some((q, _))) => {
                    [v.to_string(), u8::from(q == Quality::Stale).to_string()]
                }
                _ => [String::new(), String::new()],
            }
        });
        csv(cyc, format!("{t:.3}"), cells)
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

/// Hands every datagram on `endpoint` to `on` until SIGINT/SIGTERM or `duration` seconds have passed.
pub(crate) fn follow(
    endpoint: &str,
    duration: Option<f64>,
    mut on: impl FnMut(&[u8]),
) -> Result<(), String> {
    let sock = listen(endpoint)?;
    install_signals();
    let end = duration.map(|d| Instant::now() + Duration::from_secs_f64(d));
    let mut buf = vec![0u8; 65536];
    while !STOP.load(Ordering::SeqCst) && end.is_none_or(|e| Instant::now() < e) {
        match sock.recv(&mut buf) {
            Ok(n) => on(&buf[..n]),
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

fn watch(endpoint: &str, mut w: Watcher, json: bool) -> Result<(), String> {
    follow(endpoint, None, |d| print(&w.feed(d), json))
}

fn record(endpoint: &str, mut r: Recorder, out: &str, duration: Option<f64>) -> Result<(), String> {
    let mut f = std::fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
    let (mut start, mut err) = (None, None);
    writeln!(f, "{}", r.header()).map_err(|e| e.to_string())?;
    follow(endpoint, duration, |d| {
        let t = start
            .get_or_insert_with(Instant::now)
            .elapsed()
            .as_secs_f64();
        if let Some(Err(e)) = r.feed(d, t).map(|row| writeln!(f, "{row}")) {
            err.get_or_insert(e.to_string());
        }
    })?;
    if let Some(row) = r.finish() {
        writeln!(f, "{row}").map_err(|e| e.to_string())?;
    }
    err.map_or(Ok(()), Err)
}

pub(crate) fn ask(req: &mut dyn Requester, r: &[u8]) -> Result<Vec<u8>, String> {
    req.call(r).or_else(|_| req.call(r))
}

fn run(cli: ProbeCli) -> Result<(), String> {
    let mut req = transport::requester(&cli.server, cli.timeout_ms)?;
    let (req, json) = (&mut *req, cli.json);
    match cli.cmd {
        Sub::Query => query(req, json),
        Sub::CachedTags => cached_tags(),
        Sub::Diff(a) => crate::diff::run(req, a, cli.timeout_ms, json),
        Sub::Read { tags } => read(req, tags, json),
        Sub::Write {
            points,
            confirm,
            dry_run,
            model,
        } => write(req, &cli.server, points, (confirm, dry_run, model)),
        Sub::Play {
            script,
            confirm,
            dry_run,
            keep_going,
            model,
        } => play(
            req,
            &cli.server,
            &script,
            Flags {
                confirm,
                dry_run,
                keep_going,
                model,
            },
        ),
        stream => stream_cmd(req, stream, json),
    }
}

/// `watch` and `record`: the subcommands that listen on the publish endpoint.
fn stream_cmd(req: &mut dyn Requester, cmd: Sub, json: bool) -> Result<(), String> {
    match cmd {
        Sub::Record {
            out,
            listen,
            tags,
            stale_after,
            duration,
        } => {
            let tags = watched(req, tags)?;
            let r = Recorder::new(&group_for(&listen), tags, stale_after);
            record(&listen, r, &out, duration)
        }
        Sub::Watch {
            listen,
            tags,
            stale_after,
        } => {
            let tags = watched(req, tags)?;
            let w = Watcher::new(&group_for(&listen), tags, stale_after);
            watch(&listen, w, json)
        }
        _ => unreachable!("not a stream subcommand"),
    }
}

fn query(req: &mut dyn Requester, json: bool) -> Result<(), String> {
    let tags = fetch_tags(req)?;
    print_lines(tags.iter().map(|t| tag_line(t, json)));
    Ok(())
}

fn cached_tags() -> Result<(), String> {
    let text =
        std::fs::read_to_string(cache_path().ok_or("no cache directory")?).unwrap_or_default();
    print_lines(text.lines().map(String::from));
    Ok(())
}

fn read(req: &mut dyn Requester, tags: Vec<String>, json: bool) -> Result<(), String> {
    let tags = if has_glob(&tags) {
        expand(&tags, &fetch_tags(req)?)?
    } else {
        tags
    };
    for r in requests(&tags) {
        print(&read_rows(&ask(req, &r)?)?, json);
    }
    Ok(())
}

/// Ask, send (once: a write is not retried), then the audit line, whatever the provider said.
fn write(
    req: &mut dyn Requester,
    server: &str,
    points: Vec<String>,
    (confirm, dry_run, model): (bool, bool, Option<String>),
) -> Result<(), String> {
    let points = &expand_points(req, points)?[..];
    let r = write_request(points)?;
    if let Some(f) = model {
        let yaml = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
        check_points(
            &Model::load_in(&yaml, std::path::Path::new(&f).parent())?,
            points,
        )?;
    }
    if dry_run {
        print_lines(std::iter::once(format!(
            "dry run, would send {}",
            String::from_utf8_lossy(&r)
        )));
        return Ok(());
    }
    confirmed(confirm, points, std::io::stdin().lock())?;
    send_write(req, server, &r, points)
}

/// Send a write (once: it is not retried), then the audit line, whatever the provider said.
fn send_write(
    req: &mut dyn Requester,
    server: &str,
    r: &[u8],
    points: &[String],
) -> Result<(), String> {
    let res = req.call(r).and_then(|reply| update_msg(&reply));
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
        return fetch_tags(req);
    }
    let tags = if has_glob(&tags) {
        expand(&tags, &fetch_tags(req)?)?
    } else {
        tags
    };
    validate(req, &tags)?;
    Ok(tags)
}

/// `bus-*.v=1` -> the same value on every tag the glob matches.
fn expand_points(req: &mut dyn Requester, points: Vec<String>) -> Result<Vec<String>, String> {
    if !has_glob(
        &points
            .iter()
            .map(|p| p.split('=').next().unwrap_or("").to_string())
            .collect::<Vec<_>>(),
    ) {
        return Ok(points);
    }
    let all = fetch_tags(req)?;
    let mut out = vec![];
    for p in points {
        let (t, v) = p.split_once('=').unwrap_or((&p, ""));
        for tag in expand(&[t.to_string()], &all)? {
            out.push(if v.is_empty() {
                tag
            } else {
                format!("{tag}={v}")
            });
        }
    }
    Ok(out)
}

/// One line of a `play` script: write `write` at `at` seconds after the start, once `wait_for` (if any) holds.
#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default)]
    pub at: f64,
    pub write: String,
    pub wait_for: Option<String>,
}

impl Step {
    fn points(&self) -> Vec<String> {
        self.write.split(',').map(|p| p.trim().into()).collect()
    }
}

/// The script's steps, checked: `at` finite, non-negative and not going back in time; each write and condition well formed.
pub fn parse_script(yaml: &str) -> Result<Vec<Step>, String> {
    let steps: Vec<Step> = serde_saphyr::from_str(yaml).map_err(|e| e.to_string())?;
    let mut prev = 0.0;
    for (i, s) in steps.iter().enumerate() {
        let n = i + 1;
        if !s.at.is_finite() || s.at < prev {
            return Err(format!(
                "step {n}: `at: {}` must be >= 0 and not earlier than the step before",
                s.at
            ));
        }
        prev = s.at;
        write_request(&s.points()).map_err(|e| format!("step {n}: {e}"))?;
        if let Some(c) = &s.wait_for {
            Cond::parse(c).map_err(|e| format!("step {n}: {e}"))?;
        }
    }
    Ok(steps)
}

/// A number, or `true`/`false` as 1/0.
fn number(s: &str) -> Option<f64> {
    parse_value(s).map(|v| match v {
        Value::Analog(v) => v,
        Value::Binary(b) => b as u8 as f64,
    })
}

/// `tag >= 3`: a tag, a comparison and a number (or `true`/`false`).
#[derive(Debug, PartialEq)]
pub struct Cond {
    tag: String,
    op: &'static str,
    rhs: f64,
}

impl Cond {
    pub fn parse(s: &str) -> Result<Cond, String> {
        // longest operators first, so `>=` is not read as `>`
        let (i, op) = [">=", "<=", "==", "!=", ">", "<"]
            .into_iter()
            .filter_map(|op| s.find(op).map(|i| (i, op)))
            .min_by_key(|&(i, op)| (i, -(op.len() as i32)))
            .ok_or_else(|| format!("`{s}`: expected `tag >= value` (or <=, ==, !=, >, <)"))?;
        let (tag, rhs) = (s[..i].trim(), s[i + op.len()..].trim());
        let rhs = number(rhs).ok_or_else(|| format!("`{s}`: `{rhs}` is not a number"))?;
        if tag.is_empty() {
            return Err(format!("`{s}`: no tag before `{op}`"));
        }
        Ok(Cond {
            tag: tag.into(),
            op,
            rhs,
        })
    }

    pub fn holds(&self, v: f64) -> bool {
        match self.op {
            ">=" => v >= self.rhs,
            "<=" => v <= self.rhs,
            "==" => v == self.rhs,
            "!=" => v != self.rhs,
            ">" => v > self.rhs,
            _ => v < self.rhs,
        }
    }
}

/// How long a `wait_for` may take before the step fails. ponytail: fixed; make it a script field if a scenario needs longer.
const WAIT_LIMIT: Duration = Duration::from_secs(60);
const WAIT_POLL: Duration = Duration::from_millis(200);

/// Poll the provider until `c` holds or `limit` passes (`limit` 0: one look).
pub fn wait_for(req: &mut dyn Requester, c: &Cond, limit: Duration) -> Result<(), String> {
    let end = Instant::now() + limit;
    loop {
        let reply = ask(req, &requests(std::slice::from_ref(&c.tag))[0])?;
        let row = read_rows(&reply)?.pop().ok_or("empty reply")?;
        let v = number(&row.value)
            .ok_or_else(|| format!("`{}` is `{}`, not a number", row.tag, row.value))?;
        if c.holds(v) {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!(
                "wait_for `{} {} {}` not met in time",
                c.tag, c.op, c.rhs
            ));
        }
        sleep_until(Instant::now() + WAIT_POLL, WAIT_POLL);
    }
}

struct Flags {
    confirm: bool,
    dry_run: bool,
    keep_going: bool,
    model: Option<String>,
}

/// The script at `path`, parsed and (with `--model`) every write checked against the model before anything is sent.
fn load_script(path: &str, model: Option<&str>) -> Result<Vec<Step>, String> {
    let steps = parse_script(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?)?;
    if let Some(m) = model {
        let yaml = std::fs::read_to_string(m).map_err(|e| format!("{m}: {e}"))?;
        let model = Model::load_in(&yaml, std::path::Path::new(m).parent())?;
        for (i, s) in steps.iter().enumerate() {
            check_points(&model, &s.points()).map_err(|e| format!("step {}: {e}", i + 1))?;
        }
    }
    Ok(steps)
}

fn dry_run_lines(steps: &[Step]) -> Result<Vec<String>, String> {
    steps
        .iter()
        .map(|s| {
            let r = String::from_utf8_lossy(&write_request(&s.points())?).into_owned();
            let w = s
                .wait_for
                .as_ref()
                .map_or(String::new(), |c| format!(" once {c}"));
            Ok(format!("dry run, at {}s{w} would send {r}", s.at))
        })
        .collect()
}

/// Play the script: each step at its time since the start, its `wait_for` first, then the write with an audit line.
/// A failing step stops the run unless `--keep-going`, which finishes the script and then reports how many failed.
fn play(req: &mut dyn Requester, server: &str, path: &str, f: Flags) -> Result<(), String> {
    let steps = load_script(path, f.model.as_deref())?;
    if f.dry_run {
        print_lines(dry_run_lines(&steps)?.into_iter());
        return Ok(());
    }
    let all: Vec<String> = steps.iter().flat_map(Step::points).collect();
    confirmed(f.confirm, &all, std::io::stdin().lock())?;
    run_steps(req, server, &steps, f.keep_going)
}

fn run_steps(
    req: &mut dyn Requester,
    server: &str,
    steps: &[Step],
    keep_going: bool,
) -> Result<(), String> {
    install_signals();
    let start = Instant::now();
    let mut failed = 0;
    for (i, s) in steps.iter().enumerate() {
        sleep_until(
            start + Duration::from_secs_f64(s.at),
            Duration::from_millis(50),
        );
        if STOP.load(Ordering::SeqCst) {
            return Err("interrupted".into());
        }
        if let Err(e) = run_step(req, server, s) {
            eprintln!("step {}: {e}", i + 1);
            failed += 1;
            if !keep_going {
                return Err(format!("step {} failed", i + 1));
            }
        }
    }
    match failed {
        0 => Ok(()),
        n => Err(format!("{n} step(s) failed")),
    }
}

fn run_step(req: &mut dyn Requester, server: &str, s: &Step) -> Result<(), String> {
    if let Some(c) = &s.wait_for {
        wait_for(req, &Cond::parse(c)?, WAIT_LIMIT)?;
    }
    let points = expand_points(req, s.points())?;
    send_write(req, server, &write_request(&points)?, &points)
}
