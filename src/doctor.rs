//! `ferrofire doctor`: is the range plumbing healthy? Asks the provider's REP socket for its tags, listens on the publish
//! endpoint for a few seconds, and says what it saw: datagrams arriving, the group id, sequence gaps, other groups
//! leaking in (a switch that floods multicast), clock skew from `_ff.t`, tags gone stale. Every problem comes with a fix.
//! ponytail: the provider exposes no ERR/drop counters (QUERY, READ, WRITE only), so there is nothing to read; add the
//! check when a stats request exists.

use crate::bennu::{exit_on_err, group_for};
use crate::datagram::{Quality, Receiver, Reject};
use crate::probe::{fetch_tags, follow, json_str};
use crate::transport::{self, DEFAULT_PUBLISH};
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "ferrofire doctor [--server tcp://127.0.0.1:5555] [--listen udp://*;239.0.0.1:40000] [--seconds 3] [--json]";

/// A clock further than this from the provider's `_ff.t` is reported.
const SKEW_LIMIT_NS: i64 = 1_000_000_000;

/// Check the range connectivity of a provider.
#[derive(Debug, clap::Parser)]
#[command(name = "ferrofire doctor")]
pub struct DoctorArgs {
    /// REP endpoint of the provider
    #[arg(long, default_value = "tcp://127.0.0.1:5555")]
    server: String,
    /// Publish endpoint to listen on
    #[arg(long, default_value = DEFAULT_PUBLISH, value_name = "ENDPOINT")]
    listen: String,
    /// How long to listen for datagrams
    #[arg(long, default_value_t = 3.0, value_name = "SECONDS")]
    seconds: f64,
    /// Give up on a request after this many milliseconds
    #[arg(long, default_value_t = 2000)]
    timeout_ms: i32,
    /// One JSON object per check instead of text
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

/// One finding; `fix` says what to do about it (empty when `Ok`).
#[derive(Debug, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    pub fix: &'static str,
}

impl Check {
    fn ok(name: &'static str, detail: String) -> Check {
        Check {
            name,
            status: Status::Ok,
            detail,
            fix: "",
        }
    }

    fn bad(status: Status, name: &'static str, detail: String, fix: &'static str) -> Check {
        Check {
            name,
            status,
            detail,
            fix,
        }
    }

    pub fn text(&self) -> String {
        let tag = match self.status {
            Status::Ok => "ok  ",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        };
        let fix = match self.fix.is_empty() {
            true => String::new(),
            false => format!("\n      fix: {}", self.fix),
        };
        format!("{tag}  {}: {}{fix}", self.name, self.detail)
    }

    pub fn json(&self) -> String {
        let status = format!("{:?}", self.status).to_lowercase();
        let fix = match self.fix.is_empty() {
            true => "null".into(),
            false => json_str(self.fix),
        };
        format!(
            "{{\"check\":{},\"status\":\"{status}\",\"detail\":{},\"fix\":{fix}}}",
            json_str(self.name),
            json_str(&self.detail)
        )
    }
}

/// What listening on the publish endpoint saw. Feed it datagrams, then hand it to `diagnose`.
pub struct Heard {
    rx: Receiver,
    tags: Vec<String>,
    pub accepted: u64,
    pub other_groups: u64,
    pub unusable: u64,
    /// Local clock minus the provider's `_ff.t` at the last datagram, ns.
    pub skew_ns: Option<i64>,
}

impl Heard {
    pub fn new(group: &str, tags: Vec<String>) -> Heard {
        Heard {
            rx: Receiver::new(group, tags.iter().map(String::as_str)),
            tags,
            accepted: 0,
            other_groups: 0,
            unusable: 0,
            skew_ns: None,
        }
    }

    /// Count a datagram received at `now_ns` (unix ns).
    pub fn feed(&mut self, d: &[u8], now_ns: i64) {
        match self.rx.accept(d) {
            Ok(_) | Err(Reject::Stale) => {
                self.accepted += 1;
                self.skew_ns = sent_ns(d).map_or(self.skew_ns, |t| Some(now_ns - t));
            }
            Err(Reject::WrongGroup) => self.other_groups += 1,
            Err(_) => self.unusable += 1,
        }
    }

    fn stale(&self) -> Vec<&str> {
        let stale = |t: &&String| matches!(self.rx.quality(t), Some((Quality::Stale, _)));
        self.tags.iter().filter(stale).map(String::as_str).collect()
    }
}

/// The `_ff.t` (unix ns) a datagram carries, if any.
fn sent_ns(d: &[u8]) -> Option<i64> {
    let text = String::from_utf8_lossy(d);
    text.split("_ff.t:").nth(1)?.split(',').next()?.parse().ok()
}

/// Everything doctor looked at.
pub struct Obs<'a> {
    pub tags: Result<usize, String>,
    pub listen: &'a str,
    pub joined: Result<(), String>,
    pub seconds: f64,
    pub heard: &'a Heard,
}

pub fn diagnose(o: &Obs) -> Vec<Check> {
    let mut out = vec![provider(o)];
    out.push(group(o));
    if let Err(e) = &o.joined {
        let fix = "use udp://[iface;]group:port with a multicast group (224.0.0.0/4) and an IPv4 interface address of this host";
        out.push(Check::bad(Status::Fail, "multicast join", e.clone(), fix));
        return out;
    }
    out.push(arriving(o));
    if o.heard.accepted > 0 {
        out.extend(
            [gaps(o), bystanders(o), skew(o), stale(o)]
                .into_iter()
                .flatten(),
        );
    }
    out
}

fn provider(o: &Obs) -> Check {
    match &o.tags {
        Ok(n) => Check::ok("provider", format!("REP answers QUERY: {n} tags")),
        Err(e) => Check::bad(
            Status::Fail,
            "provider",
            e.clone(),
            "start `ferrofire run` (or the provider), check --server and that the REP port is not firewalled",
        ),
    }
}

fn group(o: &Obs) -> Check {
    let id = group_for(o.listen);
    Check::ok("group", format!("id {id} for {}", o.listen))
}

fn arriving(o: &Obs) -> Check {
    let h = o.heard;
    if h.accepted > 0 {
        return Check::ok("datagrams", format!("{} in {}s", h.accepted, o.seconds));
    }
    let fix = "the provider's --publish must be the same endpoint string as --listen (the group id hashes it); check the multicast route, the host firewall for UDP, and an IGMP querier on the switch";
    let detail = match h.other_groups {
        0 => format!("none in {}s", o.seconds),
        n => format!(
            "none for this group in {}s, {n} for other groups",
            o.seconds
        ),
    };
    Check::bad(Status::Fail, "datagrams", detail, fix)
}

fn gaps(o: &Obs) -> Option<Check> {
    let n = o.heard.rx.gaps();
    Some(match n {
        0 => Check::ok("seq gaps", "none".into()),
        n => Check::bad(
            Status::Warn,
            "seq gaps",
            format!("{n} datagrams missing from the _ff.seq run"),
            "loss between provider and here: check switch queues, NIC receive buffers and the publish rate",
        ),
    })
}

fn bystanders(o: &Obs) -> Option<Check> {
    Some(match o.heard.other_groups {
        0 => Check::ok("bystanders", "no other groups on this port".into()),
        n => Check::bad(
            Status::Warn,
            "bystanders",
            format!("{n} datagrams for other groups reached this socket"),
            "the switch is flooding multicast: enable IGMP snooping and an IGMP querier so groups reach only their members",
        ),
    })
}

fn skew(o: &Obs) -> Option<Check> {
    let ns = o.heard.skew_ns?;
    let ms = ns as f64 / 1e6;
    Some(match ns.abs() <= SKEW_LIMIT_NS {
        true => Check::ok("clock skew", format!("{ms:.1} ms")),
        false => Check::bad(
            Status::Warn,
            "clock skew",
            format!("{ms:.0} ms against the provider's _ff.t"),
            "sync both clocks (chrony or PTP); recorded and logged times will not line up until then",
        ),
    })
}

fn stale(o: &Obs) -> Option<Check> {
    let stale = o.heard.stale();
    Some(match stale.is_empty() {
        true => Check::ok("stale tags", "none".into()),
        false => Check::bad(
            Status::Warn,
            "stale tags",
            format!("{}: {}", stale.len(), stale[..stale.len().min(5)].join(", ")),
            "the provider is not refreshing these tags: check what drives them (a stopped solver or a dead RTU)",
        ),
    })
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

/// Print the findings; exit status 1 if any failed.
fn report(checks: &[Check], json: bool) -> bool {
    for c in checks {
        println!("{}", if json { c.json() } else { c.text() });
    }
    checks.iter().all(|c| c.status != Status::Fail)
}

fn run(a: DoctorArgs) -> Result<bool, String> {
    let tags = transport::requester(&a.server, a.timeout_ms).and_then(|mut r| fetch_tags(&mut *r));
    let names = tags.clone().unwrap_or_default();
    let mut heard = Heard::new(&group_for(&a.listen), names);
    let joined = follow(&a.listen, Some(a.seconds), |d| heard.feed(d, now_ns()));
    let obs = Obs {
        tags: tags.map(|t| t.len()),
        listen: &a.listen,
        joined,
        seconds: a.seconds,
        heard: &heard,
    };
    Ok(report(&diagnose(&obs), a.json))
}

pub fn main(a: DoctorArgs) {
    match run(a) {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => exit_on_err("doctor", USAGE, Err(e)),
    }
}
