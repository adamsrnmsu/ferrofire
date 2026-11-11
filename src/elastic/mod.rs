//! Export to Elasticsearch 7+/8 and OpenSearch 1+/2 through the `_bulk` API: the pieces `ferrofire groundtruth` (the stand-in for
//! pybennu's groundtruth_monitor) and the provider's `[elastic]` export (pybennu's elastic.py) share. Documents are built and
//! queued by the caller; a bounded drop-oldest queue and worker threads do all the network I/O, so a slow or dead server never
//! slows the caller (the engine tick). Everything with a decision in it is a plain function; docs/ground-truth-elastic.md.

pub mod http;
pub mod provider;
mod sink;

pub use provider::Publisher;
pub use sink::{Build, Queue, Sink, Stats};

use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---- configuration -------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Auth {
    None,
    Basic(String, String),
    ApiKey(String),
}

/// Where `measurement.frequency` comes from: a number, or the value of a published tag.
#[derive(Debug, Clone, PartialEq)]
pub enum Frequency {
    Fixed(f64),
    Tag(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// `http://host:9200`
    pub url: String,
    pub index_basename: String,
    /// Export worker threads.
    pub threads: usize,
    pub auth: Auth,
    /// PEM file of the CA that signed the server certificate (https); none: the system roots.
    pub ca_file: Option<String>,
    /// Most items held between the producer and the workers; past it the oldest are dropped.
    pub queue: usize,
    pub frequency: Option<Frequency>,
    pub experiment: String,
    pub topology: String,
    pub scenario: String,
    /// `observer.name` and `observer.hostname`.
    pub hostname: String,
}

/// Default `queue-size` of the monitor (documents, about 150 bytes each) and of the provider (snapshots: a document per tag, so a big
/// case is megabytes each and the bound has to be small).
pub const DEFAULT_QUEUE: usize = 100_000;
pub const DEFAULT_QUEUE_SNAPSHOTS: usize = 600;

/// An INI value by key, with `_` and `-` interchangeable (pybennu's YAML says `index_basename`, its INI files say `power-solver-service`).
fn get<'a>(s: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    s.get(k)
        .or_else(|| s.get(&k.replace('-', "_")))
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
}

/// `10.0.0.5` -> `http://10.0.0.5:9200`; a host and port gets `http://`; a URL stays (no trailing slash).
pub fn normalize_host(h: &str) -> String {
    let h = h.trim().trim_end_matches('/');
    if h.contains("://") {
        h.into()
    } else if h.contains(':') {
        format!("http://{h}")
    } else {
        format!("http://{h}:9200")
    }
}

/// Elasticsearch index names: lower case, none of `\/*?"<>| ,#:`, not starting with `-_+`.
pub fn check_basename(b: &str) -> Result<(), String> {
    let bad = |c: char| c.is_uppercase() || "\\/*?\"<>| ,#:".contains(c);
    if b.is_empty() || b.starts_with(['-', '_', '+']) || b.contains(bad) {
        return Err(format!("index name `{b}` must be lower case, not start with - _ +, and not contain \\ / * ? \" < > | , # : or spaces"));
    }
    Ok(())
}

fn number<T: std::str::FromStr>(
    s: &HashMap<String, String>,
    k: &str,
    default: T,
) -> Result<T, String> {
    get(s, k).map_or(Ok(default), |v| {
        v.parse()
            .map_err(|_| format!("{k} = `{v}` is not a whole number"))
    })
}

fn auth_of(s: &HashMap<String, String>, pre: &str) -> Result<Auth, String> {
    let (user, pass, key) = (
        get(s, &format!("{pre}user")),
        get(s, &format!("{pre}password")),
        get(s, &format!("{pre}api-key")),
    );
    match (user, pass, key) {
        (Some(_), _, Some(_)) => Err(format!(
            "set either {pre}api-key or {pre}user and {pre}password, not both"
        )),
        (_, _, Some(k)) => Ok(Auth::ApiKey(k.into())),
        (Some(u), Some(p), _) => Ok(Auth::Basic(u.into(), p.into())),
        (None, None, None) => Ok(Auth::None),
        _ => Err(format!("{pre}user and {pre}password go together")),
    }
}

fn frequency_of(v: Option<&str>) -> Option<Frequency> {
    v.map(|v| {
        v.parse()
            .map_or_else(|_| Frequency::Tag(v.into()), Frequency::Fixed)
    })
}

impl Config {
    /// Read an INI section. `host_key` names the server key (`elasticsearch-ip` for the monitor, `host` for the provider) and `pre`
    /// prefixes `user`, `password`, `api-key` and `ca-file` (`elasticsearch-` for the monitor, none for the provider).
    pub fn parse(
        s: &HashMap<String, String>,
        host_key: &str,
        pre: &str,
        default_base: &str,
        default_queue: usize,
    ) -> Result<Config, String> {
        let index_basename = get(s, "index-basename").unwrap_or(default_base).to_string();
        check_basename(&index_basename)?;
        let c = Config {
            url: normalize_host(get(s, host_key).unwrap_or("127.0.0.1")),
            index_basename,
            threads: number(s, "threads", 3)?,
            auth: auth_of(s, pre)?,
            ca_file: get(s, &format!("{pre}ca-file")).map(String::from),
            queue: number(s, "queue-size", default_queue)?,
            frequency: frequency_of(get(s, "frequency")),
            experiment: get(s, "experiment").unwrap_or("").into(),
            topology: get(s, "topology").unwrap_or("").into(),
            scenario: get(s, "scenario").unwrap_or("").into(),
            hostname: get(s, "hostname").map_or_else(node_name, String::from),
        };
        if c.threads == 0 || c.queue == 0 {
            return Err("threads and queue-size must be at least 1".into());
        }
        Ok(c)
    }
}

/// The machine's host name (pybennu's `platform.node()`).
pub fn node_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown".into())
}

// ---- time (UTC) ----------------------------------------------------------------------------------------------

/// Wall clock as the time since the Unix epoch.
pub fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// `(year, month, day)` of a day count since 1970-01-01 (proleptic Gregorian; Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `2026-11-11T23:59:59.123Z`
pub fn iso(t: Duration) -> String {
    let s = t.as_secs();
    let (y, m, d) = civil((s / 86_400) as i64);
    let r = s % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        r / 3600,
        r % 3600 / 60,
        r % 60,
        t.subsec_millis()
    )
}

/// `<basename>-YYYY.MM.DD`, the UTC day of `t`: a document just before and just after midnight UTC lands in its own day's index.
pub fn index_name(base: &str, t: Duration) -> String {
    let (y, m, d) = civil((t.as_secs() / 86_400) as i64);
    format!("{base}-{y:04}.{m:02}.{d:02}")
}

// ---- documents and the bulk API ------------------------------------------------------------------------------

/// A document and the index it belongs in, already serialized.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub index: String,
    pub source: String,
}

/// The `_bulk` request body: an `index` action line and the source line per document.
pub fn bulk_body(docs: &[Doc]) -> String {
    let mut b = String::new();
    for d in docs {
        b.push_str(&json!({"index": {"_index": d.index}}).to_string());
        b.push('\n');
        b.push_str(&d.source);
        b.push('\n');
    }
    b
}

/// What a `_bulk` response says about the documents sent: the positions worth sending again (429, 5xx) and the reasons of the
/// ones refused for good (mapping errors and the like), `None` when the response is not a bulk response at all.
pub fn judge(resp: &[u8]) -> Option<(Vec<usize>, Vec<String>)> {
    let v: Value = serde_json::from_slice(resp).ok()?;
    if v["errors"] != Value::Bool(true) {
        return Some((vec![], vec![]));
    }
    let (mut again, mut refused) = (vec![], vec![]);
    for (i, r) in items(&v)?.into_iter().enumerate() {
        match verdict(r) {
            Ok(()) => {}
            Err(None) => again.push(i),
            Err(Some(why)) => refused.push(why),
        }
    }
    Some((again, refused))
}

/// The result object of each item of a bulk response (`{"index": {result}}`), in order.
fn items(v: &Value) -> Option<Vec<&Value>> {
    v["items"]
        .as_array()?
        .iter()
        .map(|i| i.as_object()?.values().next())
        .collect()
}

/// One bulk item: taken (`Ok`), worth sending again (`Err(None)`: 429, 5xx) or refused for good (`Err(Some(status and reason))`).
fn verdict(item: &Value) -> Result<(), Option<String>> {
    match item["status"].as_u64().unwrap_or(0) {
        200..=299 => Ok(()),
        429 | 500.. => Err(None),
        s => Err(Some(format!(
            "{s} {}",
            item["error"]["reason"].as_str().unwrap_or("?")
        ))),
    }
}

/// Pause before attempt `n` (0-based) after a failure: 0.5 s doubling to a 30 s ceiling.
pub fn backoff(n: u32) -> Duration {
    Duration::from_millis(500u64 << n.min(6)).min(Duration::from_secs(30))
}

/// The `tag:value` pairs of a published message. Empty parts and parts with no `:` are skipped; as in pybennu's monitor the value
/// ends at the next `:`.
pub fn points(msg: &str) -> impl Iterator<Item = (&str, &str)> {
    msg.split(',').filter_map(|p| {
        let mut it = p.split(':');
        Some((it.next()?, it.next()?)).filter(|(t, _)| !t.is_empty())
    })
}

// ---- the provider's index template ----------------------------------------------------------------------------

fn ty(t: &str) -> Value {
    json!({ "type": t })
}

fn props(p: Value) -> Value {
    json!({ "properties": p })
}

/// pybennu's `ES_BASE_TYPE_MAPPING` (elastic.py), field for field.
pub fn mapping() -> Value {
    json!({
        "@timestamp": ty("date"),
        "rtds_time": ty("date"),
        "sceptre_time": ty("date"),
        "simulator_time": ty("date"),
        "time_drift": ty("double"),
        "event": props(json!({"ingested": ty("date")})),
        "ecs": props(json!({"version": ty("keyword")})),
        "agent": props(json!({"type": ty("keyword"), "version": ty("keyword")})),
        "observer": props(json!({
            "hostname": ty("keyword"), "ip": ty("ip"), "name": ty("keyword"),
            "geo": props(json!({"timezone": ty("keyword")})),
        })),
        "network": props(json!({"protocol": ty("keyword"), "transport": ty("keyword")})),
        "pmu": props(json!({
            "name": ty("keyword"), "label": ty("keyword"), "ip": ty("ip"), "port": ty("integer"), "id": ty("long"),
        })),
        "measurement": props(json!({
            "stream": ty("byte"), "status": ty("keyword"), "time": ty("double"), "frequency": ty("double"),
            "dfreq": ty("double"), "channel": ty("keyword"),
            "phasor": props(json!({"id": ty("byte"), "real": ty("double"), "angle": ty("double")})),
        })),
        "modbus": props(json!({
            "register": ty("integer"), "name": ty("keyword"), "register_type": ty("keyword"), "data_type": ty("keyword"),
            "unit_type": ty("keyword"), "description": ty("keyword"), "sceptre_tag": ty("keyword"), "raw_value": ty("keyword"),
            "human_value": ty("keyword"), "ip": ty("ip"), "port": ty("integer"),
        })),
        "groundtruth": props(json!({
            "description": ty("keyword"), "tag": ty("keyword"), "type": ty("keyword"), "value": ty("keyword"),
            "float": ty("double"), "bool": ty("boolean"), "int": ty("integer"),
        })),
        "sceptre": props(json!({
            "experiment": ty("keyword"), "topology": ty("keyword"), "scenario": ty("keyword"), "provider": ty("keyword"),
            "server_endpoint": ty("keyword"), "publish_endpoint": ty("keyword"),
        })),
        "node": ty("keyword"),
    })
}

/// The composable index template body for `<basename>-*` (`PUT _index_template/<basename>`; Elasticsearch 7.8+, OpenSearch 1+).
pub fn template(base: &str) -> Value {
    json!({
        "index_patterns": [format!("{base}-*")],
        "priority": 100,
        "template": {
            // an explicit refresh_interval switches off search-idle: without it an index nobody searches stops refreshing, and the
            // document counts a SCORCH providerdata check reads (`_stats`) stand still while documents arrive
            "settings": { "index": { "refresh_interval": "1s" } },
            "mappings": { "properties": mapping() },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ini(kv: &[(&str, &str)]) -> HashMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn hosts() {
        assert_eq!(normalize_host("10.0.0.5"), "http://10.0.0.5:9200");
        assert_eq!(normalize_host(" es:9201 "), "http://es:9201");
        assert_eq!(
            normalize_host("https://es.example:9243/"),
            "https://es.example:9243"
        );
    }

    #[test]
    fn basenames() {
        assert!(check_basename("opalrt-clean").is_ok());
        for bad in [
            "", "Upper", "-x", "_x", "+x", "a b", "a/b", "a*", "a,b", "a#b", "a:b",
        ] {
            assert!(check_basename(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_defaults_and_keys() {
        let c = Config::parse(&ini(&[]), "host", "", "default", 7).unwrap();
        assert_eq!(c.url, "http://127.0.0.1:9200");
        assert_eq!(
            (c.index_basename.as_str(), c.threads, c.queue),
            ("default", 3, 7)
        );
        assert_eq!(c.auth, Auth::None);
        assert_eq!((c.ca_file, c.frequency), (None, None));
        let c = Config::parse(
            &ini(&[
                ("host", "https://es:9200"),
                ("index_basename", "rtds-clean"),
                ("threads", "2"),
                ("queue-size", "50"),
                ("user", "u"),
                ("password", "p"),
                ("ca-file", "/ca.pem"),
                ("frequency", "60"),
                ("experiment", "e"),
                ("hostname", "prov"),
            ]),
            "host",
            "",
            "x",
            7,
        )
        .unwrap();
        assert_eq!(
            (
                c.url.as_str(),
                c.index_basename.as_str(),
                c.threads,
                c.queue
            ),
            ("https://es:9200", "rtds-clean", 2, 50)
        );
        assert_eq!(c.auth, Auth::Basic("u".into(), "p".into()));
        assert_eq!(c.ca_file.as_deref(), Some("/ca.pem"));
        assert_eq!(
            (c.frequency, c.experiment.as_str(), c.hostname.as_str()),
            (Some(Frequency::Fixed(60.0)), "e", "prov")
        );
        let c = Config::parse(
            &ini(&[
                ("elasticsearch-ip", "10.1.1.1"),
                ("elasticsearch-api-key", "k"),
                ("frequency", "bus-1.freq"),
            ]),
            "elasticsearch-ip",
            "elasticsearch-",
            "bennu",
            DEFAULT_QUEUE,
        )
        .unwrap();
        assert_eq!(
            (c.url.as_str(), c.auth),
            ("http://10.1.1.1:9200", Auth::ApiKey("k".into()))
        );
        assert_eq!(c.frequency, Some(Frequency::Tag("bus-1.freq".into())));
    }

    #[test]
    fn config_errors() {
        let e = |kv: &[(&str, &str)]| Config::parse(&ini(kv), "host", "", "x", 7).unwrap_err();
        assert!(e(&[("threads", "0")]).contains("at least 1"));
        assert!(e(&[("queue-size", "0")]).contains("at least 1"));
        assert!(e(&[("threads", "many")]).contains("not a whole number"));
        assert!(e(&[("index-basename", "Big")]).contains("lower case"));
        assert!(e(&[("user", "u")]).contains("go together"));
        assert!(e(&[("password", "p")]).contains("go together"));
        assert!(e(&[("user", "u"), ("password", "p"), ("api-key", "k")]).contains("not both"));
    }

    #[test]
    fn time_is_utc() {
        assert_eq!(iso(Duration::ZERO), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso(Duration::from_millis(1_762_905_599_123)),
            "2025-11-11T23:59:59.123Z"
        );
        assert_eq!(
            iso(Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00.000Z"
        ); // leap day
        assert_eq!(
            iso(Duration::from_secs(4_102_444_800)),
            "2100-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn index_day_is_the_utc_day_of_the_timestamp() {
        let before = Duration::from_millis(1_762_905_599_999); // 2025-11-11T23:59:59.999Z
        assert_eq!(index_name("bennu", before), "bennu-2025.11.11");
        assert_eq!(
            index_name("bennu", before + Duration::from_millis(1)),
            "bennu-2025.11.12"
        );
        assert_eq!(
            index_name("opalrt-clean", Duration::from_secs(951_782_400)),
            "opalrt-clean-2000.02.29"
        );
    }

    #[test]
    fn bulk_body_is_ndjson() {
        let d = |i: &str, s: &str| Doc {
            index: i.into(),
            source: s.into(),
        };
        let b = bulk_body(&[d("a-1", r#"{"x":1}"#), d("a-2", r#"{"x":2}"#)]);
        assert_eq!(b, "{\"index\":{\"_index\":\"a-1\"}}\n{\"x\":1}\n{\"index\":{\"_index\":\"a-2\"}}\n{\"x\":2}\n");
        assert_eq!(bulk_body(&[]), "");
    }

    #[test]
    fn judging_a_bulk_response() {
        assert_eq!(
            judge(br#"{"errors":false,"items":[]}"#),
            Some((vec![], vec![]))
        );
        let r = br#"{"errors":true,"items":[
            {"index":{"status":201}},
            {"index":{"status":429,"error":{"reason":"queue full"}}},
            {"index":{"status":400,"error":{"reason":"mapper_parsing_exception"}}},
            {"index":{"status":503}},
            {"index":{"status":200}}]}"#;
        let (again, refused) = judge(r).unwrap();
        assert_eq!(again, vec![1, 3]);
        assert_eq!(refused, vec!["400 mapper_parsing_exception"]);
        assert_eq!(judge(b"not json"), None);
        assert_eq!(judge(br#"{"errors":true}"#), None);
        assert_eq!(judge(br#"{"errors":true,"items":[1]}"#), None);
    }

    #[test]
    fn backoff_doubles_to_a_ceiling() {
        let s: Vec<u64> = (0..9).map(|n| backoff(n).as_millis() as u64).collect();
        assert_eq!(
            s,
            vec![500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]
        );
        assert_eq!(backoff(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn message_points() {
        let p: Vec<_> = points("a.b:1,c:true,,bad,:x,d.e:1:2,\0").collect();
        assert_eq!(p, vec![("a.b", "1"), ("c", "true"), ("d.e", "1")]);
    }

    #[test]
    fn template_covers_the_pybennu_fields() {
        let t = template("opalrt-clean");
        assert_eq!(t["index_patterns"][0], "opalrt-clean-*");
        let m = &t["template"]["mappings"]["properties"];
        assert_eq!(m["@timestamp"]["type"], "date");
        assert_eq!(m["time_drift"]["type"], "double");
        assert_eq!(
            m["measurement"]["properties"]["frequency"]["type"],
            "double"
        );
        assert_eq!(m["observer"]["properties"]["ip"]["type"], "ip");
        assert_eq!(m["groundtruth"]["properties"]["bool"]["type"], "boolean");
        assert_eq!(m["node"]["type"], "keyword");
        assert_eq!(m.as_object().unwrap().len(), 16);
        assert_eq!(t["template"]["settings"]["index"]["refresh_interval"], "1s");
    }
}
