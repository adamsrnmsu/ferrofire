//! The Elasticsearch/OpenSearch export against a fake server: the bulk requests, auth, the index template, retry and backoff,
//! partial failures, a dead server never blocking the producer, and the two binaries (`ferrofire run --config` and
//! `ferrofire groundtruth`) end to end. Real Elasticsearch 8 and OpenSearch 2 containers (TLS, basic auth, API key): `make
//! conformance PROTO=elastic`.
#![cfg(feature = "elastic")]

mod common;

use common::es::FakeEs;
use ferrofire::bennu::{frame, group_for};
use ferrofire::elastic::{template, Auth, Build, Config, Doc, Sink};
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");

fn conf(url: &str) -> Config {
    Config {
        url: url.into(),
        index_basename: "bennu".into(),
        threads: 1,
        auth: Auth::None,
        ca_file: None,
        queue: 1000,
        frequency: None,
        experiment: String::new(),
        topology: String::new(),
        scenario: String::new(),
        hostname: "h".into(),
    }
}

fn doc(n: u32) -> Doc {
    Doc {
        index: "bennu-2026.11.11".into(),
        source: json!({"n": n}).to_string(),
    }
}

fn identity() -> Build<Doc> {
    Arc::new(|d, _| vec![d])
}

#[test]
fn a_batch_goes_out_as_one_authenticated_bulk_request() {
    let es = FakeEs::ok();
    let mut c = conf(&es.url);
    c.auth = Auth::Basic("elastic".into(), "pw".into());
    let sink = Sink::start(&c, None, 500, identity()).unwrap();
    (0..3).for_each(|n| sink.push(doc(n)));
    es.wait_docs(3, 5);
    sink.finish();
    let reqs = es.requests();
    let bulk = reqs.iter().find(|r| r.path == "/_bulk").unwrap();
    assert_eq!(bulk.method, "POST");
    assert_eq!(bulk.header("authorization"), Some("Basic ZWxhc3RpYzpwdw=="));
    assert_eq!(bulk.header("content-type"), Some("application/x-ndjson"));
    assert_eq!(
        bulk.actions()[0],
        json!({"index": {"_index": "bennu-2026.11.11"}})
    );
    assert_eq!(es.docs().len(), 3);
    assert_eq!(sink.stats.sent.load(Ordering::Relaxed), 3);
}

#[test]
fn an_api_key_is_sent_as_the_authorization_header() {
    let es = FakeEs::ok();
    let mut c = conf(&es.url);
    c.auth = Auth::ApiKey("abc==".into());
    let sink = Sink::start(&c, None, 500, identity()).unwrap();
    sink.push(doc(1));
    es.wait_docs(1, 5);
    sink.finish();
    assert_eq!(
        es.requests()[0].header("authorization"),
        Some("ApiKey abc==")
    );
}

#[test]
fn the_template_goes_in_once_before_any_document() {
    let es = FakeEs::ok();
    let mut c = conf(&es.url);
    c.threads = 3;
    let sink = Sink::start(
        &c,
        Some(("opalrt".into(), template("opalrt"))),
        500,
        identity(),
    )
    .unwrap();
    for n in 0..50 {
        sink.push(doc(n));
        std::thread::sleep(Duration::from_millis(5));
    }
    es.wait_docs(50, 5);
    sink.finish();
    let reqs = es.requests();
    let puts: Vec<_> = reqs.iter().filter(|r| r.method == "PUT").collect();
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].path, "/_index_template/opalrt");
    let body: Value = serde_json::from_str(&puts[0].body).unwrap();
    assert_eq!(body["index_patterns"][0], "opalrt-*");
    assert_eq!(reqs[0].method, "PUT"); // before the first bulk
    assert_eq!(es.docs().len(), 50);
}

#[test]
fn a_failing_server_is_retried_with_backoff_until_it_answers() {
    let mut calls = 0;
    let es = FakeEs::scripted(Box::new(move |_| {
        calls += 1;
        if calls <= 2 {
            (503, "busy".into())
        } else {
            (200, r#"{"errors":false,"items":[]}"#.into())
        }
    }));
    let sink = Sink::start(&conf(&es.url), None, 500, identity()).unwrap();
    let t = Instant::now();
    sink.push(doc(1));
    es.wait_docs(1, 10);
    sink.finish();
    assert_eq!(es.requests().len(), 3);
    assert!(
        t.elapsed() >= Duration::from_millis(1400),
        "0.5 s then 1 s of backoff, took {:?}",
        t.elapsed()
    );
    assert_eq!(
        (
            sink.stats.retries.load(Ordering::Relaxed),
            sink.stats.sent.load(Ordering::Relaxed)
        ),
        (2, 1)
    );
}

#[test]
fn rejected_items_are_resent_when_retryable_and_dropped_when_not() {
    let mut calls = 0;
    let es = FakeEs::scripted(Box::new(move |_| {
        calls += 1;
        if calls == 1 {
            let items = r#"{"errors":true,"items":[{"index":{"status":201}},{"index":{"status":429,"error":{"reason":"full"}}},{"index":{"status":400,"error":{"reason":"mapper_parsing_exception"}}}]}"#;
            (200, items.into())
        } else {
            (
                200,
                r#"{"errors":false,"items":[{"index":{"status":201}}]}"#.into(),
            )
        }
    }));
    let sink = Sink::start(&conf(&es.url), None, 500, identity()).unwrap();
    (0..3).for_each(|n| sink.push(doc(n)));
    for _ in 0..100 {
        if es.requests().len() >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    sink.finish();
    let reqs = es.requests();
    assert_eq!(reqs[0].docs().len(), 3);
    assert_eq!(reqs[1].docs(), vec![json!({"n": 1})]); // only the 429
    let s = &sink.stats;
    assert_eq!(
        (
            s.sent.load(Ordering::Relaxed),
            s.lost.load(Ordering::Relaxed)
        ),
        (2, 1)
    );
}

#[test]
fn a_dead_server_never_blocks_the_producer_and_the_oldest_are_dropped() {
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut c = conf(&format!("http://127.0.0.1:{dead}"));
    c.queue = 100;
    let sink = Sink::start(&c, None, 500, identity()).unwrap();
    let t = Instant::now();
    for n in 0..20_000 {
        sink.push(doc(n));
    }
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "pushing took {:?}",
        t.elapsed()
    );
    assert!(sink.queue.len() <= 100);
    assert!(
        sink.queue.dropped() >= 19_000 - 500,
        "dropped {}",
        sink.queue.dropped()
    );
    let t = Instant::now();
    sink.finish_within(Duration::from_millis(300));
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(
        sink.stats.lost.load(Ordering::Relaxed) > 0,
        "what could not be sent is counted"
    );
}

#[test]
fn a_backlog_of_big_snapshots_goes_out_in_bounded_requests_and_a_413_splits_them() {
    use std::sync::atomic::AtomicUsize;
    let (taken, biggest) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (t, b) = (taken.clone(), biggest.clone());
    let es = FakeEs::scripted(Box::new(move |r| {
        let n = r.docs().len();
        b.fetch_max(n, Ordering::SeqCst);
        if n > 150 {
            return (413, "request entity too large".into());
        }
        t.fetch_add(n, Ordering::SeqCst);
        (200, r#"{"errors":false,"items":[]}"#.into())
    }));
    // a snapshot of 400 documents, 30 of them waiting at once, four taken at a time (the provider's way)
    let build: Build<u32> = Arc::new(|n, _| {
        (0..400)
            .map(|i| Doc {
                index: "bennu-2026.11.11".into(),
                source: json!({"n": n, "i": i}).to_string(),
            })
            .collect()
    });
    let sink = Sink::start(&conf(&es.url), None, 4, build).unwrap();
    (0..30).for_each(|n| sink.push(n));
    for _ in 0..200 {
        if taken.load(Ordering::SeqCst) >= 12_000 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    sink.finish();
    assert_eq!(taken.load(Ordering::SeqCst), 12_000);
    assert!(
        biggest.load(Ordering::SeqCst) <= 1000,
        "no request over 1000 documents, saw {}",
        biggest.load(Ordering::SeqCst)
    );
    assert_eq!(
        (
            sink.stats.sent.load(Ordering::Relaxed),
            sink.stats.lost.load(Ordering::Relaxed)
        ),
        (12_000, 0)
    );
}

#[test]
fn a_document_the_server_always_refuses_is_dropped_and_counted_not_retried_forever() {
    let es = FakeEs::scripted(Box::new(|r| {
        if r.body.contains("HUGE") {
            (400, r#"{"error":"bad"}"#.into())
        } else {
            (200, r#"{"errors":false,"items":[]}"#.into())
        }
    }));
    let sink = Sink::start(&conf(&es.url), None, 500, identity()).unwrap();
    for v in ["a", "HUGE", "b", "c"] {
        sink.push(Doc {
            index: "bennu-2026.11.11".into(),
            source: json!({ "v": v }).to_string(),
        });
    }
    for _ in 0..100 {
        if sink.stats.sent.load(Ordering::Relaxed) >= 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    sink.finish();
    assert_eq!(
        (
            sink.stats.sent.load(Ordering::Relaxed),
            sink.stats.lost.load(Ordering::Relaxed)
        ),
        (3, 1)
    );
    assert_eq!(
        sink.stats.retries.load(Ordering::Relaxed),
        0,
        "no backoff: the refusal is final"
    );
}

fn terminate(mut c: Child) -> String {
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    c.wait().unwrap();
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    out
}

fn free_udp() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn server() -> String {
    let p = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    format!("tcp://127.0.0.1:{p}")
}

fn write_ini(name: &str, body: &str) -> String {
    let p = std::env::temp_dir().join(format!("ff-es-{}-{name}.ini", std::process::id()));
    std::fs::write(&p, body).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn run_exports_every_published_tag_with_the_pybennu_fields() {
    let es = FakeEs::ok();
    let ini = write_ini(
        "run",
        &format!("[elastic]\nenabled = true\nhost = {}\nindex_basename = opalrt-clean\nthreads = 1\nfrequency = lowerwater.height\nexperiment = e1\n", es.url),
    );
    let publish = format!("udp://*;127.0.0.1:{}", free_udp());
    let child = Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--server",
            &server(),
            "--publish",
            &publish,
            "--rate",
            "0.05",
            "--config",
            &ini,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let docs = es.wait_docs(30, 15);
    let out = terminate(child);
    assert!(
        out.contains("Elasticsearch http://127.0.0.1:") && out.contains("opalrt-clean-YYYY.MM.DD"),
        "{out}"
    );
    let reqs = es.requests();
    assert_eq!(reqs[0].path, "/_index_template/opalrt-clean");
    assert!(docs.len() >= 30, "{} docs", docs.len());
    let d = docs
        .iter()
        .find(|d| d["groundtruth"]["tag"] == "lock.height")
        .unwrap();
    for k in [
        "@timestamp",
        "sceptre_time",
        "simulator_time",
        "time_drift",
        "event",
        "ecs",
        "agent",
        "observer",
        "sceptre",
        "measurement",
    ] {
        assert!(d.get(k).is_some(), "{k} in {d}");
    }
    assert_eq!(d["sceptre"]["experiment"], "e1");
    assert_eq!(d["sceptre"]["provider"], "ferrofire");
    assert_eq!(d["sceptre"]["publish_endpoint"], publish.as_str());
    assert!(d["time_drift"].as_f64().unwrap() >= 0.0);
    // every document carries the frequency, so the newest one always does
    assert!(docs.iter().all(|d| d["measurement"]["frequency"].is_f64()));
    let index = reqs.iter().find(|r| r.path == "/_bulk").unwrap().actions()[0]["index"]["_index"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(index.starts_with("opalrt-clean-20"), "{index}");
    std::fs::remove_file(ini).ok();
}

#[test]
fn run_without_an_enabled_elastic_section_exports_nothing() {
    let es = FakeEs::ok();
    let ini = write_ini(
        "off",
        &format!("[elastic]\nenabled = false\nhost = {}\n", es.url),
    );
    let child = Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--server",
            &server(),
            "--publish",
            &format!("udp://*;127.0.0.1:{}", free_udp()),
            "--rate",
            "0.05",
            "--config",
            &ini,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let out = terminate(child);
    assert!(!out.contains("Elasticsearch"));
    assert!(es.requests().is_empty());
    std::fs::remove_file(ini).ok();
}

fn send(port: u16, endpoint: &str, bodies: &[&str], times: usize) {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    for _ in 0..times {
        for b in bodies {
            s.send_to(
                &frame(&group_for(endpoint), b.as_bytes()),
                ("127.0.0.1", port),
            )
            .unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn groundtruth_indexes_the_stream_with_the_stock_shape() {
    let es = FakeEs::ok();
    let port = free_udp();
    let endpoint = format!("udp://127.0.0.1:{port}");
    let ini = write_ini(
        "gt",
        &format!("[power-groundtruth-monitor]\npublish-endpoint = {endpoint}\nelasticsearch-ip = {}\nfilter = /127.0.0.1/load/ /127.0.0.1/gen/mw\n", es.url),
    );
    let child = Command::new(BIN)
        .args(["groundtruth", "-c", &ini, "start"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    send(port, &endpoint, &["load-1.mw:999.000,load-1.active:true,gen-2.mw:5,gen-2.mvar:6,bus-9.v:1,_ff.seq:1,_ff.cyc:1,_ff.t:2,\0"], 3);
    let docs = es.wait_docs(6, 10);
    let out = terminate(child);
    assert!(
        out.contains("Info: ferrofire groundtruth:") && out.contains("Info: stopped: sent"),
        "{out}"
    );
    assert_eq!(
        docs.len(),
        9,
        "load-1 (2 points) and gen-2.mw per message, three messages: {docs:?}"
    );
    let d = docs
        .iter()
        .find(|d| d["device"] == "load-1" && d["field"] == "mw")
        .unwrap();
    assert_eq!(
        (d["value"].as_f64(), d["provider"].as_str()),
        (Some(999.0), Some(endpoint.as_str()))
    );
    assert_eq!(d.as_object().unwrap().len(), 5);
    let a = docs.iter().find(|d| d["field"] == "active").unwrap();
    assert_eq!((a["status"].as_str(), a.get("value")), (Some("true"), None));
    assert!(docs
        .iter()
        .all(|d| d["device"] != "_ff" && d["device"] != "bus-9"));
    let bulk = es
        .requests()
        .into_iter()
        .find(|r| r.path == "/_bulk")
        .unwrap();
    let idx = bulk.actions()[0]["index"]["_index"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        idx.starts_with("bennu-20") && idx.len() == "bennu-2026.11.11".len(),
        "{idx}"
    );
    assert!(
        es.requests().iter().all(|r| r.method != "PUT"),
        "the monitor installs no template"
    );
    std::fs::remove_file(ini).ok();
}

#[test]
fn groundtruth_alert_limit_skips_only_alert_points() {
    let es = FakeEs::ok();
    let port = free_udp();
    let endpoint = format!("udp://127.0.0.1:{port}");
    let ini = write_ini(
        "gt2",
        &format!(
            "[groundtruth-monitor]\npublish-endpoint = {endpoint}\nelasticsearch-ip = {}\n",
            es.url
        ),
    );
    let child = Command::new(BIN)
        .args(["groundtruth", "-c", &ini, "start"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    send(port, &endpoint, &["a.mw:1,alert-1.state:high,b.mw:2,"], 4);
    let docs = es.wait_docs(9, 10);
    terminate(child);
    let count = |dev: &str| docs.iter().filter(|d| d["device"] == dev).count();
    assert_eq!(
        (count("a"), count("b")),
        (4, 4),
        "plain points are never limited"
    );
    assert_eq!(count("alert-1"), 1, "one alert burst per 30 s");
    std::fs::remove_file(ini).ok();
}

#[test]
fn groundtruth_reports_bad_invocations() {
    let o = Command::new(BIN)
        .args(["groundtruth", "-c", "/nonexistent.ini", "start"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("/nonexistent.ini does not exist"));
    let ini = write_ini("bad", "[groundtruth-monitor]\nelasticsearch-user = u\n");
    let o = Command::new(BIN)
        .args(["groundtruth", "-c", &ini, "start"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("go together"));
    let o = Command::new(BIN)
        .args(["groundtruth", "-e", "ff-test-none", "status"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("Daemon is not running"));
    std::fs::remove_file(ini).ok();
}

#[test]
fn groundtruth_daemon_verbs_pidfile_and_stream() {
    let es = FakeEs::ok();
    let port = free_udp();
    let endpoint = format!("udp://127.0.0.1:{port}");
    let ini = write_ini(
        "gtd",
        &format!(
            "[groundtruth-monitor]\npublish-endpoint = {endpoint}\nelasticsearch-ip = {}\n",
            es.url
        ),
    );
    let env = format!("gttest{}", std::process::id());
    let pidfile = format!("/var/run/bennu-ground-truth-mon-{env}.pid");
    let run = |args: &[&str]| {
        let o = Command::new(BIN)
            .arg("groundtruth")
            .args(args)
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };
    assert!(run(&["status", "-e", &env]).contains("Daemon is not running"));
    assert!(run(&["restart", "-c", &ini, "-e", &env]).contains("cannot restart"));
    run(&["start", "-d", "-c", &ini, "-e", &env]);
    std::thread::sleep(Duration::from_millis(800));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(run(&["status", "-e", &env]).contains(&format!("running as PID {pid}")));
    assert!(run(&["start", "-c", &ini, "-e", &env]).contains("already"));
    send(port, &endpoint, &["d.f:1,\0"], 3);
    assert!(es.wait_docs(3, 10).len() >= 3);
    run(&["stop", "-e", &env]);
    assert!(!std::path::Path::new(&pidfile).exists());
    assert!(run(&["status", "-e", &env]).contains("Daemon is not running"));
    for ext in ["out", "err"] {
        let _ = std::fs::remove_file(format!("/var/log/bennu-ground-truth-mon-{env}.{ext}"));
    }
    std::fs::remove_file(ini).ok();
}

#[test]
fn pypower_exports_its_snapshot_when_the_config_enables_elastic() {
    let case9 = concat!(env!("CARGO_MANIFEST_DIR"), "/target/golden/case9.py");
    let es = FakeEs::ok();
    let ini = write_ini(
        "pyp",
        &format!(
            "[power-solver-service]\nsolver-type = PyPower\nserver-endpoint = {}\npublish-endpoint = udp://*;127.0.0.1:{}\ncase-file = {case9}\n[elastic]\nenabled = true\nhost = {}\nindex_basename = power-clean\nfrequency = 60\n",
            server(),
            free_udp(),
            es.url
        ),
    );
    let child = Command::new(BIN)
        .args(["pypower", "start", "-c", &ini, "-e", "ffelastic"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let docs = es.wait_docs(200, 20);
    let out = terminate(child);
    assert!(out.contains("power-clean-YYYY.MM.DD"), "{out}");
    assert!(docs.len() >= 200, "{} docs", docs.len());
    let d = docs
        .iter()
        .find(|d| d["groundtruth"]["tag"] == "bus-1.voltage")
        .unwrap();
    assert!(d["groundtruth"]["float"].is_f64());
    assert_eq!(d["measurement"]["frequency"], 60.0);
    assert_eq!(
        d["time_drift"], 0.0,
        "the power solver has no model time: both clocks are the wall clock"
    );
    assert!(es.requests()[0].path.ends_with("/power-clean"));
    std::fs::remove_file(ini).ok();
}
