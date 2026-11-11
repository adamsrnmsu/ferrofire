//! The export's HTTPS client against a rustls server with a throwaway CA: delivery with the CA file and basic auth, and the three ways a
//! connection must be refused (an unknown issuer, a certificate for another name, a CA file that is not one). Needs `openssl` (as
//! tests/mqtt_net.rs does).
#![cfg(feature = "elastic")]

mod common;

use common::es::{pki, FakeEs};
use ferrofire::elastic::http::Http;
use ferrofire::elastic::{Auth, Config, Doc, Sink};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-tls-{}-{name}", std::process::id()));
    pki(&d);
    d
}

fn conf(url: &str, ca: Option<PathBuf>) -> Config {
    Config {
        url: url.into(),
        index_basename: "bennu".into(),
        threads: 1,
        auth: Auth::Basic("elastic".into(), "pw".into()),
        ca_file: ca.map(|c| c.to_string_lossy().into_owned()),
        queue: 100,
        frequency: None,
        experiment: String::new(),
        topology: String::new(),
        scenario: String::new(),
        hostname: "h".into(),
    }
}

fn bulk(c: &Config) -> Result<(u16, Vec<u8>), String> {
    Http::new(c)?.request("POST", "/_bulk", "application/x-ndjson", b"")
}

#[test]
fn a_server_certified_by_the_ca_file_is_trusted_and_gets_the_credentials() {
    let d = dir("ok");
    let es = FakeEs::tls(&d);
    let c = conf(&es.url, Some(d.join("ca.crt")));
    let (code, body) = bulk(&c).unwrap();
    assert_eq!(
        (code, body.as_slice()),
        (200, &br#"{"errors":false,"items":[]}"#[..])
    );
    assert_eq!(
        es.requests()[0].header("authorization"),
        Some("Basic ZWxhc3RpYzpwdw==")
    );
    // the whole export path over TLS
    let sink = Sink::start(&c, None, 500, Arc::new(|d: Doc, _| vec![d])).unwrap();
    sink.push(Doc {
        index: "bennu-2026.11.11".into(),
        source: r#"{"a":1}"#.into(),
    });
    assert_eq!(es.wait_docs(1, 10).len(), 1);
    sink.finish_within(Duration::from_secs(1));
}

#[test]
fn a_certificate_from_another_ca_is_refused() {
    let (good, other) = (dir("a"), dir("b"));
    let es = FakeEs::tls(&good);
    let e = bulk(&conf(&es.url, Some(other.join("ca.crt")))).unwrap_err();
    assert!(e.contains("invalid peer certificate"), "{e}"); // BadSignature: same subject name, other key
    assert!(es.requests().is_empty());
}

#[test]
fn without_a_ca_file_only_the_system_roots_count() {
    let es = FakeEs::tls(&dir("sys"));
    let e = bulk(&conf(&es.url, None)).unwrap_err();
    assert!(
        e.contains("UnknownIssuer") || e.contains("no CA certificates"),
        "{e}"
    );
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let d = dir("name");
    let es = FakeEs::tls(&d);
    let url = es.url.replace("127.0.0.1", "localhost"); // the certificate names only the IP
    let e = bulk(&conf(&url, Some(d.join("ca.crt")))).unwrap_err();
    assert!(e.contains("not valid for name"), "{e}");
}

#[test]
fn a_ca_file_that_is_not_one_stops_the_start() {
    let d = dir("bad");
    let e = Http::new(&conf("https://127.0.0.1:9200", Some(d.join("srv.key"))))
        .err()
        .unwrap();
    assert!(e.contains("srv.key"), "{e}");
}
