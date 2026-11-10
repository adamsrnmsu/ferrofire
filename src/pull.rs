//! `ferrofire pull-bridge`: pull a device's tags from a provider with one batched `READ=a,b,...` per tick and
//! republish the reply in the provider's update format, so a stock (push-subscribing) field device sees normal updates.
//! Writes are untouched: the field device still WRITEs to the provider itself.

use crate::bennu::{exit_on_err, install_signals, sleep_until, STOP};
use crate::transport::{self, Publisher, Requester};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const USAGE: &str = "ferrofire pull-bridge --publish udp://127.0.0.1:40000 (--tags-from config.xml | --tags a,b,c) [--server tcp://127.0.0.1:5555] [--transport zmq|nng] [--rate 100]  (--rate in ms)";

#[derive(Debug, PartialEq)]
pub struct PullArgs {
    pub server: String,
    pub transport: Option<String>,
    pub rate_ms: u64,
    pub tags: Vec<String>,
    pub publish: String,
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
    tags.dedup();
    match tags.is_empty() {
        true => Err("no <analog>/<binary> points in the config's <input>".into()),
        false => Ok(tags),
    }
}

fn parse_rate(v: &str) -> Result<u64, String> {
    v.parse().map_err(|e| format!("--rate: {e}"))
}

fn tags_from_file(path: &str) -> Result<Vec<String>, String> {
    let xml = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    tags_from_config(&xml).map_err(|e| format!("{path}: {e}"))
}

impl PullArgs {
    fn set(&mut self, flag: &str, v: &str) -> Result<(), String> {
        match flag {
            "--rate" => self.rate_ms = parse_rate(v)?,
            "--tags" => self.tags = v.split(',').map(String::from).collect(),
            "--tags-from" => self.tags = tags_from_file(v)?,
            _ => return self.set_text(flag, v),
        }
        Ok(())
    }

    fn set_text(&mut self, flag: &str, v: &str) -> Result<(), String> {
        match flag {
            "--server" => self.server = v.into(),
            "--transport" => self.transport = Some(v.into()),
            "--publish" => self.publish = v.into(),
            _ => return Err(format!("unknown argument `{flag}`")),
        }
        Ok(())
    }

    pub fn parse(args: &[String]) -> Result<PullArgs, String> {
        let mut a = PullArgs {
            server: "tcp://127.0.0.1:5555".into(),
            transport: None,
            rate_ms: 100,
            tags: vec![],
            publish: String::new(),
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
            a.set(flag, v)?;
        }
        a.check()
    }

    fn check(self) -> Result<PullArgs, String> {
        match (self.publish.is_empty(), self.tags.is_empty(), self.rate_ms) {
            (true, ..) => Err("--publish is required".into()),
            (_, true, _) => Err("--tags or --tags-from is required".into()),
            (.., 0) => Err("--rate must be > 0".into()),
            _ => Ok(self),
        }
    }
}

/// `READ=a,b,c`
pub fn request(tags: &[String]) -> Vec<u8> {
    format!("READ={}", tags.join(",")).into_bytes()
}

/// The update message inside an `ACK=a:1,b:2,` reply; an `ERR=` (or anything else) is an error.
pub fn update_msg(reply: &[u8]) -> Result<String, String> {
    let r = String::from_utf8_lossy(reply);
    let r = r.trim_end_matches('\0');
    r.strip_prefix("ACK=")
        .map(String::from)
        .ok_or_else(|| format!("provider said `{r}`"))
}

/// One pull: ask (retrying once on timeout), republish the reply. On failure nothing is published, so the field
/// device keeps its last values exactly as it does when a push provider goes quiet.
pub fn tick(req: &mut dyn Requester, request: &[u8], out: &dyn Publisher) -> Result<(), String> {
    let reply = req.call(request).or_else(|_| req.call(request))?;
    out.publish(&update_msg(&reply)?);
    Ok(())
}

/// One log line per outage and one when it ends; returns whether the pull is failing now.
fn log_change(res: Result<(), String>, failing: bool) -> bool {
    let now = res.is_err();
    match res {
        Err(e) if !failing => eprintln!("Warn: pull failed, keeping last values: {e}"),
        Ok(()) if failing => eprintln!("Info: pull recovered"),
        _ => {}
    }
    now
}

pub fn main(args: &[String]) {
    exit_on_err("pull-bridge", USAGE, run(args));
}

fn run(args: &[String]) -> Result<(), String> {
    let a = PullArgs::parse(args)?;
    let t = transport::select(a.transport.as_deref(), None, None)?;
    let timeout = (a.rate_ms * 2).max(100) as i32;
    let mut req = t.requester(&a.server, timeout)?;
    let out = t.publisher(&a.publish)?;
    let (request, period) = (request(&a.tags), Duration::from_millis(a.rate_ms));
    install_signals();
    println!(
        "Info: ferrofire pull-bridge: {} tags from {} every {} ms ({}); {}",
        a.tags.len(),
        a.server,
        a.rate_ms,
        t.name(),
        out.describe()
    );
    let (t0, mut k, mut failing) = (Instant::now(), 0u32, false);
    while !STOP.load(Ordering::SeqCst) {
        failing = log_change(tick(&mut *req, &request, &*out), failing);
        k += 1;
        sleep_until(t0 + period * k, Duration::from_millis(2));
    }
    println!("Info: stopped");
    Ok(())
}
