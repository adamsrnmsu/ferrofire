//! `ferrofire pull-bridge`: pull a device's tags from a provider with one batched `READ=a,b,...` per tick and
//! republish the reply in the provider's update format, so a stock (push-subscribing) field device sees normal updates.
//! Writes are untouched: the field device still WRITEs to the provider itself.

use crate::bennu::{exit_on_err, install_signals, sleep_until, STOP};
use crate::hint::did_you_mean;
use crate::transport::{self, Publisher, Requester};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tracing::Level;

const USAGE: &str = "ferrofire pull-bridge --publish udp://127.0.0.1:40000 (--tags-from config.xml | --tags a,b,c) [--server tcp://127.0.0.1:5555] [--rate 100]  (--rate in ms)";

/// The command line as clap reads it; `resolve` turns it into the `PullArgs` the bridge runs on.
/// Publish to ENDPOINT what a field-device REP server answers to READ, polled every --rate ms.
#[derive(Debug, clap::Parser)]
#[command(name = "ferrofire pull-bridge", group = clap::ArgGroup::new("source").required(true).args(["tags", "tags_from"]))]
pub struct PullCli {
    /// Where the polled tags are published, e.g. udp://127.0.0.1:40000
    #[arg(long, value_name = "ENDPOINT")]
    publish: String,
    /// Comma-separated tags to poll
    #[arg(long, value_delimiter = ',', value_name = "A,B,C")]
    tags: Vec<String>,
    /// bennu field-device config.xml to take the tags from
    #[arg(long, value_name = "CONFIG.XML")]
    tags_from: Option<String>,
    /// REP endpoint of the provider
    #[arg(long, default_value = "tcp://127.0.0.1:5555")]
    server: String,
    /// Poll period in milliseconds
    #[arg(long = "rate", value_name = "MS", default_value_t = 100, value_parser = clap::value_parser!(u64).range(1..))]
    rate_ms: u64,
}

#[derive(Debug, PartialEq)]
pub struct PullArgs {
    pub server: String,
    pub rate_ms: u64,
    pub tags: Vec<String>,
    pub publish: String,
}

/// Most tags in one `READ`, so a request stays far under any transport's message limit; more tags go in several requests.
pub const MAX_TAGS_PER_READ: usize = 200;
/// Attempts at the startup `QUERY` before giving up on an unreachable provider (each waits up to the request timeout).
pub const STARTUP_TRIES: usize = 10;
/// Minimum gap between repeated "still failing" lines.
pub const LOG_EVERY: Duration = Duration::from_secs(10);

/// Drop repeats, keeping the first occurrence and the order.
fn unique(tags: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// The text between the first `open` and the next `close` after it.
fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = &s[s.find(open)? + open.len()..];
    Some(&rest[..rest.find(close)?])
}

/// The point names a bennu field-device config subscribes to: the `<name>` of every `<analog>` / `<binary>` in `<input>`.
/// ponytail: string scanning, not an XML parser; fine for the generated configs, swap in a parser if comments or CDATA turn up.
pub fn tags_from_config(xml: &str) -> Result<Vec<String>, String> {
    let input = between(xml, "<input>", "</input>").ok_or("no <input> section in the config")?;
    let mut tags = vec![];
    for kind in ["analog", "binary"] {
        let (open, close) = (format!("<{kind}>"), format!("</{kind}>"));
        for part in input.split(open.as_str()).skip(1) {
            let name =
                between(part, "", &close).and_then(|body| between(body, "<name>", "</name>"));
            tags.extend(name.map(|n| n.trim().to_string()));
        }
    }
    let tags = unique(tags);
    match tags.is_empty() {
        true => Err("no <analog>/<binary> points in the config's <input>".into()),
        false => Ok(tags),
    }
}

pub fn tags_from_file(path: &str) -> Result<Vec<String>, String> {
    let xml = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    tags_from_config(&xml).map_err(|e| format!("{path}: {e}"))
}

impl PullCli {
    /// Read `--tags-from` and drop repeated tags.
    pub fn resolve(self) -> Result<PullArgs, String> {
        let tags = match &self.tags_from {
            Some(f) => tags_from_file(f)?,
            None => self.tags,
        };
        Ok(PullArgs {
            server: self.server,
            rate_ms: self.rate_ms,
            tags: unique(tags),
            publish: self.publish,
        })
    }
}

/// `READ=a,b,c,`, one request per `MAX_TAGS_PER_READ` tags. The trailing comma keeps a one-tag request in batch form:
/// `READ=a` alone is the single-tag form and is answered with a bare value, not `a:value,`.
pub fn requests(tags: &[String]) -> Vec<Vec<u8>> {
    tags.chunks(MAX_TAGS_PER_READ)
        .map(|c| format!("READ={},", c.join(",")).into_bytes())
        .collect()
}

/// Startup check: every tag must exist at the provider (`QUERY` lists them), else the bridge would never publish.
pub fn validate(req: &mut dyn Requester, tags: &[String]) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..STARTUP_TRIES {
        match req.call(b"QUERY=") {
            Ok(reply) => {
                let known: HashSet<String> =
                    update_msg(&reply)?.split(',').map(String::from).collect();
                let bad: Vec<String> = tags
                    .iter()
                    .filter(|t| !known.contains(*t))
                    .map(|t| format!("{t}{}", did_you_mean(t, known.iter().map(String::as_str))))
                    .collect();
                return match bad.is_empty() {
                    true => Ok(()),
                    false => Err(format!("unknown tags at the provider: {}", bad.join(", "))),
                };
            }
            Err(e) => last = e,
        }
    }
    Err(format!("provider unreachable: {last}"))
}

/// The update message inside an `ACK=a:1,b:2,` reply; an `ERR=` (or anything else) is an error.
pub fn update_msg(reply: &[u8]) -> Result<String, String> {
    let r = String::from_utf8_lossy(reply);
    let r = r.trim_end_matches('\0');
    r.strip_prefix("ACK=")
        .map(String::from)
        .ok_or_else(|| format!("provider said `{r}`"))
}

/// One pull: ask (retrying each request once on timeout), republish the replies as one update. On failure nothing is
/// published, so the field device keeps its last values exactly as it does when a push provider goes quiet.
pub fn tick(
    req: &mut dyn Requester,
    requests: &[Vec<u8>],
    out: &dyn Publisher,
) -> Result<(), String> {
    let mut msg = String::new();
    for r in requests {
        let reply = req.call(r).or_else(|_| req.call(r))?;
        msg += &update_msg(&reply)?;
    }
    out.publish(&msg);
    Ok(())
}

/// Failure logging: the first line of an outage, then one every `LOG_EVERY` with the provider's text (an ERR stalls
/// every tag, so it must never go quiet), then one when it ends.
#[derive(Default)]
pub struct Outage {
    last: Option<Instant>,
}

impl Outage {
    pub fn log_line(&mut self, res: Result<(), String>, now: Instant) -> Option<(Level, String)> {
        match (res, self.last) {
            (Err(e), None) => {
                self.last = Some(now);
                Some((
                    Level::WARN,
                    format!("pull failed, keeping last values: {e}"),
                ))
            }
            (Err(e), Some(t)) if now.duration_since(t) >= LOG_EVERY => {
                self.last = Some(now);
                Some((
                    Level::WARN,
                    format!("pull still failing, keeping last values: {e}"),
                ))
            }
            (Ok(()), Some(_)) => {
                self.last = None;
                Some((Level::INFO, "pull recovered".into()))
            }
            _ => None,
        }
    }
}

/// The next tick slot (after slot `k`) that is still in the future: slots missed during a slow or failed tick are
/// skipped, not replayed back to back.
pub fn next_k(t0: Instant, period: Duration, now: Instant, k: u32) -> u32 {
    let due = (now.saturating_duration_since(t0).as_nanos() / period.as_nanos()) as u32 + 1;
    due.max(k + 1)
}

pub fn main(args: PullCli) {
    exit_on_err("pull-bridge", USAGE, run(args));
}

fn run(args: PullCli) -> Result<(), String> {
    let a = args.resolve()?;
    let timeout = (a.rate_ms * 2).max(100) as i32;
    let mut req = transport::requester(&a.server, timeout)?;
    let out = transport::publisher(&a.publish)?;
    validate(&mut *req, &a.tags)?;
    let (requests, period) = (requests(&a.tags), Duration::from_millis(a.rate_ms));
    install_signals();
    println!(
        "Info: ferrofire pull-bridge: {} tags from {} every {} ms; {}",
        a.tags.len(),
        a.server,
        a.rate_ms,
        out.describe()
    );
    let (t0, mut k, mut outage) = (Instant::now(), 0u32, Outage::default());
    while !STOP.load(Ordering::SeqCst) {
        let res = tick(&mut *req, &requests, &*out);
        match outage.log_line(res, Instant::now()) {
            Some((Level::WARN, m)) => tracing::warn!("{m}"),
            Some((_, m)) => tracing::info!("{m}"),
            None => {}
        }
        k = next_k(t0, period, Instant::now(), k);
        sleep_until(t0 + period * k, Duration::from_millis(2));
    }
    println!("Info: stopped");
    Ok(())
}
