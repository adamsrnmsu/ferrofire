//! `ferrofire groundtruth`: a drop-in for pybennu's `groundtruth_monitor` (docs/ground-truth-elastic.md). It listens to a
//! provider's published datagrams (the same `udp://` endpoint and `[power-groundtruth-monitor]` / `[groundtruth-monitor]`
//! config.ini section) and writes one document per point, `{@timestamp, provider, device, field, value|status}`, into
//! `bennu-YYYY.MM.DD` through the bulk API. Differences from the stock script are the ones its problems call for: the index day is
//! UTC, the queue is bounded and the push retries with backoff, TLS and basic or API-key auth are available, and the 30 s
//! alert/batch limit skips only those points instead of the rest of the message.

use crate::bennu::{group_for, unframe};
use crate::elastic::{self, points, Config, Doc, Sink};
use crate::pypower::{daemon_cli, parse_ini, Paths};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SECTIONS: [&str; 2] = ["power-groundtruth-monitor", "groundtruth-monitor"];
pub const DEFAULT_ENDPOINT: &str = "udp://239.0.0.1:40000";
/// At most one burst of alert and batch points per this long.
const ALERT_EVERY: Duration = Duration::from_secs(30);
/// Documents a sender takes from the queue at a time.
const POP: usize = 500;

/// pybennu-groundtruth-monitor compatible daemon control: `ferrofire groundtruth [-d] -c config.ini start|stop|restart|status`.
#[derive(clap::Parser)]
#[command(name = "ferrofire groundtruth")]
pub struct Cli {
    #[arg(value_parser = ["start", "stop", "restart", "status"])]
    op: String,
    /// Config file (default /etc/sceptre/<env>.ini)
    #[arg(short = 'c', long = "config-file", value_name = "CONFIG_FILE")]
    conf: Option<String>,
    /// Environment name (pid file and default config suffix)
    #[arg(short = 'e', long, default_value = "bennu")]
    env: String,
    /// Run in the background
    #[arg(short = 'd', long = "daemonize")]
    daemon: bool,
}

// ---- filter --------------------------------------------------------------------------------------------------

/// `/provider/device/field` entries separated by single spaces; an entry matches when each part is a substring of the point's
/// provider, device and field. As in the stock monitor, an entry with fewer than three parts (or an empty one) matches everything.
#[derive(Debug, PartialEq)]
pub struct Filter(Vec<[String; 3]>);

impl Filter {
    pub fn parse(s: &str) -> Filter {
        Filter(
            s.split(' ')
                .map(|e| {
                    let mut p = e.split('/');
                    match (p.next(), p.next(), p.next(), p.next()) {
                        (_, Some(a), Some(b), Some(c)) => [a.into(), b.into(), c.into()],
                        _ => Default::default(),
                    }
                })
                .collect(),
        )
    }

    pub fn matches(&self, provider: &str, device: &str, field: &str) -> bool {
        self.0.iter().any(|[p, d, f]| {
            provider.contains(p.as_str())
                && device.contains(d.as_str())
                && field.contains(f.as_str())
        })
    }

    /// The entries that do not have the `/provider/device/field` shape: they match everything.
    pub fn loose(s: &str) -> Vec<&str> {
        s.split(' ').filter(|e| e.split('/').count() < 4).collect()
    }
}

// ---- the 30 s alert/batch limit -----------------------------------------------------------------------------

/// Points whose tag contains `alert` or `batch` are admitted in one burst per `every`: the first message after the window opens
/// lets all of its alert/batch points through and starts the next window. Every other point is always admitted.
pub struct Limiter {
    last: Option<Instant>,
    every: Duration,
}

impl Limiter {
    pub fn new(every: Duration) -> Limiter {
        Limiter { last: None, every }
    }

    /// Whether alert/batch points of a message arriving at `now` pass; asked once per message.
    pub fn admit(&mut self, now: Instant) -> bool {
        let open = self
            .last
            .is_none_or(|t| now.saturating_duration_since(t) >= self.every);
        if open {
            self.last = Some(now);
        }
        open
    }
}

fn rate_limited(tag: &str) -> bool {
    tag.contains("alert") || tag.contains("batch")
}

// ---- documents -----------------------------------------------------------------------------------------------

/// `device.field` -> (device, field); any other shape: the whole tag up to the first dot is the device and the field is empty.
pub fn split_tag(tag: &str) -> (&str, &str) {
    let mut p = tag.split('.');
    match (p.next(), p.next(), p.next()) {
        (Some(d), Some(f), None) => (d, f),
        (d, ..) => (d.unwrap_or(""), ""),
    }
}

/// `value` (a float) when the text parses as a finite one, `status` (the text) otherwise.
pub fn value_field(v: &str) -> (&'static str, Value) {
    match v.trim().parse::<f64>() {
        Ok(x) if x.is_finite() => ("value", json!(x)),
        _ => ("status", json!(v)),
    }
}

pub struct Point<'a> {
    pub provider: &'a str,
    pub tag: &'a str,
    pub value: &'a str,
}

/// The document of one point.
pub fn document(p: &Point, base: &str, at: Duration) -> Doc {
    let (device, field) = split_tag(p.tag);
    let (key, value) = value_field(p.value);
    let mut d = json!({"@timestamp": elastic::iso(at), "provider": p.provider, "device": device, "field": field});
    d[key] = value;
    Doc {
        index: elastic::index_name(base, at),
        source: d.to_string(),
    }
}

/// The documents of one received message. The `_ff.*` pseudo-points of datagram v1 are not data. Alert/batch points are
/// subject to the limiter (one decision per message); the filter is applied last, as the stock monitor does.
pub fn documents(
    msg: &str,
    provider: &str,
    base: &str,
    filter: &Filter,
    limiter: &mut Limiter,
    now: Instant,
    at: Duration,
) -> Vec<Doc> {
    let mut gate = None;
    let mut out = vec![];
    for (tag, value) in points(msg).filter(|(t, _)| !t.starts_with("_ff.")) {
        if rate_limited(tag) && !*gate.get_or_insert_with(|| limiter.admit(now)) {
            continue;
        }
        let (device, field) = split_tag(tag);
        if filter.matches(provider, device, field) {
            out.push(document(
                &Point {
                    provider,
                    tag,
                    value,
                },
                base,
                at,
            ));
        }
    }
    out
}

/// The body of a datagram of `group` (the chunk's trailing NUL included), if it is one.
pub fn body_of<'a>(datagram: &'a [u8], group: &str) -> Option<&'a str> {
    let (g, body) = unframe(datagram)?;
    (g == group).then(|| std::str::from_utf8(body).ok())?
}

// ---- config and run ------------------------------------------------------------------------------------------

pub struct Setup {
    pub endpoint: String,
    pub filter: String,
    pub es: Config,
}

/// The section the stock script reads: `power-groundtruth-monitor` if present, else `groundtruth-monitor`.
pub fn setup(ini: &HashMap<String, HashMap<String, String>>) -> Result<Setup, String> {
    let empty = HashMap::new();
    let s = SECTIONS.iter().find_map(|n| ini.get(*n)).unwrap_or(&empty);
    let get = |k: &str| s.get(k).map(|v| v.trim().to_string());
    Ok(Setup {
        endpoint: get("publish-endpoint").unwrap_or_else(|| DEFAULT_ENDPOINT.into()),
        filter: get("filter").unwrap_or_else(|| "/".into()),
        es: Config::parse(
            s,
            "elasticsearch-ip",
            "elasticsearch-",
            "bennu",
            elastic::DEFAULT_QUEUE,
        )?,
    })
}

fn load(c: &Cli) -> Result<Setup, String> {
    let path = c
        .conf
        .clone()
        .unwrap_or_else(|| format!("/etc/sceptre/{}.ini", c.env));
    if !std::path::Path::new(&path).exists() {
        return Err(format!(
            "The file {path} does not exist - please provide a valid config file."
        ));
    }
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    setup(&parse_ini(&src))
}

fn serve(s: Setup) -> Result<(), String> {
    crate::bennu::install_signals();
    let identity: elastic::Build<Doc> = Arc::new(|d, _| vec![d]);
    let sink = Sink::start(&s.es, None, POP, identity)?;
    let (group, filter) = (group_for(&s.endpoint), Filter::parse(&s.filter));
    // the default filter matches everything on purpose; only a configured one with a loose entry is worth a warning
    let loose = if s.filter == "/" {
        vec![]
    } else {
        Filter::loose(&s.filter)
    };
    for e in loose {
        tracing::warn!("filter entry `{e}` is not /provider/device/field: it matches every point (as in the stock monitor)");
    }
    println!(
        "Info: ferrofire groundtruth: {} -> {} index {}-YYYY.MM.DD",
        s.endpoint, s.es.url, s.es.index_basename
    );
    let mut limiter = Limiter::new(ALERT_EVERY);
    let res = crate::probe::follow(&s.endpoint, None, |dg| {
        let Some(msg) = body_of(dg, &group) else {
            return;
        };
        for d in documents(
            msg,
            &s.endpoint,
            &s.es.index_basename,
            &filter,
            &mut limiter,
            Instant::now(),
            elastic::now(),
        ) {
            sink.push(d);
        }
    });
    let (dropped, stats) = (sink.queue.dropped(), sink.stats.clone());
    sink.finish();
    println!(
        "Info: stopped: sent {} documents, refused {}, retried {} requests, dropped {dropped} (queue full)",
        stats.sent.load(Ordering::Relaxed),
        stats.lost.load(Ordering::Relaxed),
        stats.retries.load(Ordering::Relaxed)
    );
    res
}

pub fn run_cli(c: Cli) {
    let paths = Paths {
        pid: format!("/var/run/bennu-ground-truth-mon-{}.pid", c.env),
        out: format!("/var/log/bennu-ground-truth-mon-{}.out", c.env),
        err: format!("/var/log/bennu-ground-truth-mon-{}.err", c.env),
    };
    if let Err(e) = daemon_cli(&c.op, &paths, c.daemon, || load(&c), serve) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bennu::frame;

    const EP: &str = "udp://239.0.0.1:40000";

    fn run(msg: &str, filter: &str, lim: &mut Limiter, now: Instant) -> Vec<Value> {
        documents(
            msg,
            EP,
            "bennu",
            &Filter::parse(filter),
            lim,
            now,
            Duration::from_millis(1_762_905_599_500),
        )
        .iter()
        .map(|d| serde_json::from_str(&d.source).unwrap())
        .collect()
    }

    #[test]
    fn tags_split_like_the_stock_monitor() {
        assert_eq!(split_tag("load-1_bus-101.mw"), ("load-1_bus-101", "mw"));
        assert_eq!(split_tag("plain"), ("plain", ""));
        assert_eq!(split_tag("a.b.c"), ("a", ""));
        assert_eq!(split_tag(""), ("", ""));
        assert_eq!(split_tag("a."), ("a", ""));
    }

    #[test]
    fn values_are_floats_or_status_text() {
        assert_eq!(value_field("999.000"), ("value", json!(999.0)));
        assert_eq!(value_field(" 1e3 "), ("value", json!(1000.0)));
        assert_eq!(value_field("true"), ("status", json!("true")));
        assert_eq!(value_field("nan"), ("status", json!("nan"))); // JSON cannot carry it
        assert_eq!(value_field("inf"), ("status", json!("inf")));
        assert_eq!(value_field(""), ("status", json!("")));
    }

    #[test]
    fn the_document_has_the_stock_shape() {
        let d = run(
            "load-1_bus-101.mw:999.000,load-1_bus-101.active:true,",
            "/",
            &mut Limiter::new(ALERT_EVERY),
            Instant::now(),
        );
        assert_eq!(
            d[0],
            json!({"@timestamp": "2025-11-11T23:59:59.500Z", "provider": EP, "device": "load-1_bus-101", "field": "mw", "value": 999.0})
        );
        assert_eq!(d[1]["status"], "true");
        assert!(d[1].get("value").is_none());
        let doc = document(
            &Point {
                provider: EP,
                tag: "a.b",
                value: "1",
            },
            "bennu",
            Duration::from_millis(1_762_905_599_999),
        );
        assert_eq!(doc.index, "bennu-2025.11.11");
        let doc = document(
            &Point {
                provider: EP,
                tag: "a.b",
                value: "1",
            },
            "ff-bennu",
            Duration::from_millis(1_762_905_600_000),
        );
        assert_eq!(doc.index, "ff-bennu-2025.11.12"); // index-basename is honoured
    }

    #[test]
    fn the_datagram_trailer_is_not_data() {
        let d = run(
            "a.b:1,_ff.seq:7,_ff.cyc:2,_ff.t:99,\0",
            "/",
            &mut Limiter::new(ALERT_EVERY),
            Instant::now(),
        );
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn filter_syntax() {
        let f = Filter::parse("/239.0.0.1/load/mw /x/bus/");
        assert!(f.matches(EP, "load-1", "mw"));
        assert!(f.matches("udp://x", "bus-3", "anything"));
        assert!(!f.matches(EP, "load-1", "mvar"));
        assert!(!f.matches("udp://y", "gen", "mw"));
        // substring on every part; an empty part matches anything
        assert!(Filter::parse("//load/").matches("p", "xloadx", "f"));
        // the default and any entry that is not /a/b/c match everything
        for loose in ["/", "", "/p", "/p/d", "a  b", "x"] {
            assert!(Filter::parse(loose).matches("zzz", "zzz", "zzz"), "{loose}");
        }
        // an extra part is ignored
        assert!(Filter::parse("/p/d/f/extra").matches("pp", "dd", "ff"));
        assert!(!Filter::parse("/p/d/f/extra").matches("q", "dd", "ff"));
    }

    #[test]
    fn loose_filter_entries_are_found() {
        assert_eq!(Filter::loose("/"), vec!["/"]);
        assert_eq!(Filter::loose("/a/b/c /a/b /x/y/z"), vec!["/a/b"]);
        assert!(Filter::loose("/a/b/c").is_empty());
    }

    #[test]
    fn filtering_keeps_only_matching_points() {
        let d = run(
            "load.mw:1,gen.mw:2,load.mvar:3,",
            "/239/load/mw",
            &mut Limiter::new(ALERT_EVERY),
            Instant::now(),
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0]["device"], "load");
        assert_eq!(d[0]["field"], "mw");
    }

    #[test]
    fn the_limiter_opens_once_per_window() {
        let t = Instant::now();
        let mut l = Limiter::new(Duration::from_secs(30));
        assert!(l.admit(t));
        assert!(!l.admit(t + Duration::from_secs(29)));
        assert!(l.admit(t + Duration::from_secs(30)));
        assert!(!l.admit(t + Duration::from_secs(59)));
        assert!(l.admit(t + Duration::from_secs(60)));
    }

    #[test]
    fn the_limit_skips_alert_points_and_keeps_the_rest_of_the_message() {
        let t = Instant::now();
        let mut l = Limiter::new(ALERT_EVERY);
        let msg = "a.mw:1,batch-1.status:full,b.mw:2,alert.high:3,c.mw:4,";
        // first message: window open, everything passes
        assert_eq!(run(msg, "/", &mut l, t).len(), 5);
        // inside the window: only the alert/batch points are skipped (stock dropped the rest of the message from the first one on)
        let d = run(msg, "/", &mut l, t + Duration::from_secs(1));
        assert_eq!(
            d.iter()
                .map(|x| x["device"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        // a message with no alert/batch point does not use up the window
        let mut l = Limiter::new(ALERT_EVERY);
        assert_eq!(run("a.mw:1,", "/", &mut l, t).len(), 1);
        assert_eq!(run("alert.x:1,", "/", &mut l, t).len(), 1);
        // window reopens after 30 s
        assert_eq!(
            run("alert.x:1,", "/", &mut l, t + Duration::from_secs(1)).len(),
            0
        );
        assert_eq!(
            run("alert.x:1,", "/", &mut l, t + Duration::from_secs(31)).len(),
            1
        );
    }

    #[test]
    fn datagram_bodies() {
        let g = group_for(EP);
        let dg = frame(&g, b"a.b:1,\0");
        assert_eq!(body_of(&dg, &g), Some("a.b:1,\0"));
        assert_eq!(body_of(&frame("other", b"a.b:1,\0"), &g), None);
        assert_eq!(body_of(&frame(&g, &[0xff, 0xfe]), &g), None);
        assert_eq!(body_of(b"", &g), None);
    }

    fn ini(s: &str) -> HashMap<String, HashMap<String, String>> {
        parse_ini(s)
    }

    #[test]
    fn config_sections_like_the_stock_script() {
        let s = setup(&ini("[power-groundtruth-monitor]\npublish-endpoint = udp://239.0.0.1:40001\nelasticsearch-ip = 10.0.0.9\nfilter = /a/b/c\n")).unwrap();
        assert_eq!(
            (s.endpoint.as_str(), s.filter.as_str(), s.es.url.as_str()),
            ("udp://239.0.0.1:40001", "/a/b/c", "http://10.0.0.9:9200")
        );
        // the other section name works; power-groundtruth-monitor wins when both exist
        let s = setup(&ini("[groundtruth-monitor]\nelasticsearch-ip = h\n[power-groundtruth-monitor]\nelasticsearch-ip = p\n")).unwrap();
        assert_eq!(s.es.url, "http://p:9200");
        let s = setup(&ini("[groundtruth-monitor]\nelasticsearch-ip = h\n")).unwrap();
        assert_eq!(s.es.url, "http://h:9200");
        // the stock defaults when nothing is set
        let s = setup(&ini("[other]\nx = 1\n")).unwrap();
        assert_eq!(
            (s.endpoint.as_str(), s.filter.as_str(), s.es.url.as_str()),
            (DEFAULT_ENDPOINT, "/", "http://127.0.0.1:9200")
        );
        assert_eq!(s.es.index_basename, "bennu");
    }

    #[test]
    fn config_extras_for_tls_and_auth() {
        let s = setup(&ini("[groundtruth-monitor]\nelasticsearch-ip = https://es:9200\nelasticsearch-user = u\nelasticsearch-password = p\nelasticsearch-ca-file = /ca.pem\nqueue-size = 77\n")).unwrap();
        assert_eq!(s.es.auth, elastic::Auth::Basic("u".into(), "p".into()));
        assert_eq!((s.es.ca_file.as_deref(), s.es.queue), (Some("/ca.pem"), 77));
        assert!(setup(&ini("[groundtruth-monitor]\nelasticsearch-user = u\n")).is_err());
    }
}
