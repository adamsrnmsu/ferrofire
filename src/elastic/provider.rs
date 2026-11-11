//! The provider's `[elastic]` export (pybennu's `ElasticPusher`): every published tag becomes a document in
//! `<index_basename>-YYYY.MM.DD`, shaped like the ones pybennu's OPAL-RT provider writes (`groundtruth.*`), with the time
//! fields (`sceptre_time`, `simulator_time`, `time_drift`) and, when a frequency source is configured, `measurement.frequency` on
//! every document. The publish thread only stamps the time and queues the message; documents are built on the workers.

use super::{iso, points, template, Config, Doc, Frequency, Sink};
use crate::transport;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Snapshots a sender takes from the queue at a time: each is a document per tag.
const POP: usize = 4;

/// The engine's model time (f64 bits) as of the publish in progress; NaN when no engine sets it (the power solver has none).
static SIM_TIME: AtomicU64 = AtomicU64::new(f64::NAN.to_bits());

/// Called by the engine just before it publishes a snapshot.
pub fn set_sim_time(t: f64) {
    SIM_TIME.store(t.to_bits(), Ordering::Relaxed);
}

fn sim_time() -> f64 {
    f64::from_bits(SIM_TIME.load(Ordering::Relaxed))
}

/// One published message, stamped on the publish thread.
pub struct Stamped {
    msg: String,
    /// Wall clock at the publish.
    wall: Duration,
    /// The simulator's clock: its wall time at the first publish plus the model time since then (the wall time itself when no model time).
    simulator: Duration,
}

/// `simulator_time` for a publish at `wall` with model time `sim`, and the first `(wall, sim)` seen as the anchor.
/// The anchor makes the simulator clock start in step with the wall clock, so `time_drift` is how far the real-time engine has fallen
/// behind (or run ahead of) it since.
pub fn simulator_time(anchor: &mut Option<(Duration, f64)>, wall: Duration, sim: f64) -> Duration {
    if sim.is_nan() {
        return wall;
    }
    let (w0, s0) = *anchor.get_or_insert((wall, sim));
    w0 + Duration::from_secs_f64((sim - s0).max(0.0))
}

/// `time_drift`: the gap between the two clocks, in milliseconds (rtds.py: `abs(sceptre - rtds) * 1000`).
pub fn drift_ms(wall: Duration, simulator: Duration) -> f64 {
    (wall.as_secs_f64() - simulator.as_secs_f64()).abs() * 1000.0
}

/// The fields every document carries (pybennu's `msg_additions`); `ip` is left out unless it is a valid address.
pub fn constants(c: &Config, server: &str, publish: &str) -> Value {
    json!({
        "ecs": {"version": "8.11.0"},
        "agent": {"type": "sceptre-provider", "version": env!("CARGO_PKG_VERSION")},
        "observer": {"hostname": c.hostname, "name": c.hostname, "geo": {"timezone": "UTC"}},
        "sceptre": {
            "experiment": c.experiment, "topology": c.topology, "scenario": c.scenario,
            "provider": "ferrofire", "server_endpoint": server, "publish_endpoint": publish,
        },
    })
}

/// The `groundtruth` object of one point: `value` as the text published, `float` or `bool` as the type it parses as.
pub fn groundtruth(tag: &str, value: &str) -> Value {
    let mut g = json!({"tag": tag, "value": value, "description": ""});
    match (value, value.parse::<f64>()) {
        ("true" | "false", _) => {
            g["type"] = "bool".into();
            g["bool"] = (value == "true").into();
        }
        (_, Ok(x)) if x.is_finite() => {
            g["type"] = "float".into();
            g["float"] = x.into();
        }
        _ => g["type"] = "string".into(),
    }
    g
}

/// The frequency of a message: a fixed number, or the value of the configured tag if this message carries it.
pub fn frequency(f: &Option<Frequency>, pts: &[(&str, &str)]) -> Option<f64> {
    match f.as_ref()? {
        Frequency::Fixed(x) => Some(*x),
        Frequency::Tag(t) => pts
            .iter()
            .find(|(n, _)| n == t)?
            .1
            .parse()
            .ok()
            .filter(|x: &f64| x.is_finite()),
    }
}

/// The documents of one stamped message: one per point (the `_ff.*` datagram trailer is not data).
pub fn documents(c: &Config, base: &Value, m: &Stamped, ingested: Duration) -> Vec<Doc> {
    let pts: Vec<_> = points(&m.msg)
        .filter(|(t, _)| !t.starts_with("_ff."))
        .collect();
    let freq = frequency(&c.frequency, &pts);
    let (ts, sim) = (iso(m.wall), iso(m.simulator));
    let drift = drift_ms(m.wall, m.simulator);
    let index = super::index_name(&c.index_basename, m.wall);
    pts.iter()
        .map(|(tag, value)| {
            let mut d = base.clone();
            d["@timestamp"] = ts.clone().into();
            d["sceptre_time"] = ts.clone().into();
            d["simulator_time"] = sim.clone().into();
            d["time_drift"] = drift.into();
            d["event"] = json!({"ingested": iso(ingested)});
            d["groundtruth"] = groundtruth(tag, value);
            if let Some(f) = freq {
                d["measurement"] = json!({"frequency": f});
            }
            Doc {
                index: index.clone(),
                source: d.to_string(),
            }
        })
        .collect()
}

/// A transport publisher that, instead of sending anywhere, queues the message for export.
pub struct Publisher {
    sink: Sink<Stamped>,
    describe: String,
    anchor: Mutex<Option<(Duration, f64)>>,
}

impl Publisher {
    /// Start the export workers; the index template goes in before the first document.
    pub fn start(c: &Config, server: &str, publish: &str) -> Result<Publisher, String> {
        let (conf, base) = (c.clone(), constants(c, server, publish));
        let build = Arc::new(move |m: Stamped, ingested| documents(&conf, &base, &m, ingested));
        let name = c.index_basename.clone();
        Ok(Publisher {
            sink: Sink::start(c, Some((name.clone(), template(&name))), POP, build)?,
            describe: format!("Elasticsearch {} index {name}-YYYY.MM.DD", c.url),
            anchor: Mutex::new(None),
        })
    }
}

/// Dropping the publisher at shutdown flushes what is queued (for a few seconds at most) and stops the workers.
impl Drop for Publisher {
    fn drop(&mut self) {
        self.sink.finish();
        let s = &self.sink.stats;
        tracing::info!(
            "elastic: sent {} documents, refused {}, retried {} requests, dropped {} (queue full)",
            s.sent.load(Ordering::Relaxed),
            s.lost.load(Ordering::Relaxed),
            s.retries.load(Ordering::Relaxed),
            self.sink.queue.dropped()
        );
    }
}

/// The `[elastic]` section of a config.ini: `Some` only when `enabled` is true (pybennu's `ElasticSettings.enabled` defaults to false).
pub fn from_ini(ini: &HashMap<String, HashMap<String, String>>) -> Result<Option<Config>, String> {
    let Some(s) = ini.get("elastic") else {
        return Ok(None);
    };
    let on = s.get("enabled").map(|v| v.trim().to_ascii_lowercase());
    if !matches!(on.as_deref(), Some("true" | "yes" | "on" | "1")) {
        return Ok(None);
    }
    Config::parse(
        s,
        "host",
        "",
        "default-index-basename-changeme",
        super::DEFAULT_QUEUE_SNAPSHOTS,
    )
    .map(Some)
}

/// The export for a config.ini file, started; `None` when the file has no enabled `[elastic]` section.
pub fn from_file(path: &str, server: &str, publish: &str) -> Result<Option<Publisher>, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    match from_ini(&crate::pypower::parse_ini(&src))? {
        Some(c) => Publisher::start(&c, server, publish).map(Some),
        None => Ok(None),
    }
}

impl transport::Publisher for Publisher {
    fn publish(&self, msg: &str) {
        let wall = super::now();
        let simulator = simulator_time(&mut crate::hint::lock(&self.anchor), wall, sim_time());
        self.sink.push(Stamped {
            msg: msg.to_string(),
            wall,
            simulator,
        });
    }

    fn describe(&self) -> String {
        self.describe.clone()
    }

    fn dropped(&self) -> u64 {
        self.sink.queue.dropped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elastic::Auth;

    fn conf(freq: Option<Frequency>) -> Config {
        Config {
            url: "http://x:9200".into(),
            index_basename: "rtds-clean".into(),
            threads: 1,
            auth: Auth::None,
            ca_file: None,
            queue: 10,
            frequency: freq,
            experiment: "exp".into(),
            topology: "".into(),
            scenario: "".into(),
            hostname: "prov".into(),
        }
    }

    fn stamped(msg: &str) -> Stamped {
        Stamped {
            msg: msg.into(),
            wall: Duration::from_millis(1_762_905_599_500),
            simulator: Duration::from_millis(1_762_905_599_250),
        }
    }

    #[test]
    fn the_simulator_clock_is_anchored_at_the_first_publish() {
        let mut a = None;
        let w = Duration::from_secs(1000);
        assert_eq!(simulator_time(&mut a, w, 5.0), w);
        assert_eq!(
            simulator_time(&mut a, w + Duration::from_millis(1100), 6.0),
            w + Duration::from_secs(1)
        );
        // model time that goes backwards (a reset) never puts the clock before the anchor
        assert_eq!(simulator_time(&mut a, w, 1.0), w);
        // no model time (the power solver): the simulator clock is the wall clock
        assert_eq!(simulator_time(&mut None, w, f64::NAN), w);
    }

    #[test]
    fn drift_is_the_absolute_gap_in_ms() {
        let (a, b) = (Duration::from_millis(5250), Duration::from_millis(5000));
        assert_eq!(drift_ms(a, b), 250.0);
        assert_eq!(drift_ms(b, a), 250.0);
        assert_eq!(drift_ms(a, a), 0.0);
    }

    #[test]
    fn groundtruth_types() {
        let g = groundtruth("lock.height", "2.5");
        assert_eq!(
            (g["type"].as_str(), g["float"].as_f64(), g["value"].as_str()),
            (Some("float"), Some(2.5), Some("2.5"))
        );
        assert!(g.get("bool").is_none());
        let g = groundtruth("uppervalve.open", "true");
        assert_eq!(
            (g["type"].as_str(), g["bool"].as_bool()),
            (Some("bool"), Some(true))
        );
        assert_eq!(groundtruth("x", "false")["bool"], false);
        for v in ["nan", "inf", "open"] {
            let g = groundtruth("x", v);
            assert_eq!(g["type"], "string");
            assert!(g.get("float").is_none(), "{v}");
        }
    }

    #[test]
    fn frequency_sources() {
        let pts = [("bus.freq", "59.98"), ("x", "nan")];
        assert_eq!(frequency(&None, &pts), None);
        assert_eq!(frequency(&Some(Frequency::Fixed(60.0)), &[]), Some(60.0));
        assert_eq!(
            frequency(&Some(Frequency::Tag("bus.freq".into())), &pts),
            Some(59.98)
        );
        assert_eq!(
            frequency(&Some(Frequency::Tag("missing".into())), &pts),
            None
        );
        assert_eq!(frequency(&Some(Frequency::Tag("x".into())), &pts), None);
    }

    #[test]
    fn a_message_becomes_one_document_per_tag_in_the_utc_day_index() {
        let c = conf(Some(Frequency::Tag("g.freq".into())));
        let base = constants(&c, "tcp://0.0.0.0:5555", "udp://239.0.0.1:40000");
        let docs = documents(
            &c,
            &base,
            &stamped("g.freq:60.01,v.open:true,_ff.seq:3,_ff.cyc:1,_ff.t:9,\0"),
            Duration::from_secs(1_762_905_600),
        );
        assert_eq!(docs.len(), 2); // the trailer is not data
        assert!(docs.iter().all(|d| d.index == "rtds-clean-2025.11.11"));
        let d: Value = serde_json::from_str(&docs[1].source).unwrap();
        assert_eq!(d["@timestamp"], "2025-11-11T23:59:59.500Z");
        assert_eq!(d["sceptre_time"], d["@timestamp"]);
        assert_eq!(d["simulator_time"], "2025-11-11T23:59:59.250Z");
        assert_eq!(d["time_drift"], 250.0);
        assert_eq!(d["event"]["ingested"], "2025-11-12T00:00:00.000Z");
        assert_eq!(d["measurement"]["frequency"], 60.01); // on every document, not only the tag's own
        assert_eq!(d["groundtruth"]["tag"], "v.open");
        assert_eq!(d["groundtruth"]["bool"], true);
        assert_eq!(
            (d["ecs"]["version"].as_str(), d["agent"]["type"].as_str()),
            (Some("8.11.0"), Some("sceptre-provider"))
        );
        assert_eq!(d["agent"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            (
                d["observer"]["name"].as_str(),
                d["observer"]["hostname"].as_str()
            ),
            (Some("prov"), Some("prov"))
        );
        assert!(d["observer"].get("ip").is_none());
        assert_eq!(d["sceptre"]["experiment"], "exp");
        assert_eq!(d["sceptre"]["provider"], "ferrofire");
        assert_eq!(d["sceptre"]["publish_endpoint"], "udp://239.0.0.1:40000");
        assert_eq!(d["sceptre"]["server_endpoint"], "tcp://0.0.0.0:5555");
    }

    #[test]
    fn the_elastic_section_is_opt_in() {
        let ini = |s: &str| crate::pypower::parse_ini(s);
        assert_eq!(from_ini(&ini("[power-solver-service]\nx = 1\n")), Ok(None));
        assert_eq!(
            from_ini(&ini("[elastic]\nhost = http://h:9200\n")),
            Ok(None)
        );
        assert_eq!(from_ini(&ini("[elastic]\nenabled = false\n")), Ok(None));
        let c = from_ini(&ini("[elastic]\nenabled = True\nhost = http://h:9200\nindex_basename = rtds-clean\nthreads = 2\n")).unwrap().unwrap();
        assert_eq!(
            (c.url.as_str(), c.index_basename.as_str(), c.threads),
            ("http://h:9200", "rtds-clean", 2)
        );
        let c = from_ini(&ini("[elastic]\nenabled = yes\n"))
            .unwrap()
            .unwrap();
        assert_eq!(c.index_basename, "default-index-basename-changeme");
        assert_eq!(c.queue, super::super::DEFAULT_QUEUE_SNAPSHOTS);
        assert!(from_ini(&ini("[elastic]\nenabled = true\nthreads = 0\n")).is_err());
    }

    #[test]
    fn no_frequency_source_no_measurement() {
        let c = conf(None);
        let docs = documents(&c, &constants(&c, "", ""), &stamped("a:1,"), Duration::ZERO);
        let d: Value = serde_json::from_str(&docs[0].source).unwrap();
        assert!(d.get("measurement").is_none());
    }
}
