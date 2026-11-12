//! The ground-truth page end to end: a real listener, the table `top::publish` keeps, and clients (docs/testing.md, bead ww-1df.3).
use ferrofire::gt;
use ferrofire::model::Value;
use ferrofire::top;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

fn get(addr: SocketAddr, req: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = s.write_all(req.as_bytes());
    let mut out = String::new();
    let _ = s.read_to_string(&mut out); // a dropped connection may reset instead of closing
    out
}

fn body(resp: &str) -> &str {
    resp.split_once("\r\n\r\n").unwrap().1
}

#[test]
fn serves_the_live_table_as_html_and_json_with_age_and_staleness() {
    top::publish(
        &[
            ("gt.fresh".into(), Value::Analog(1.5)),
            ("gt.<x>".into(), Value::Binary(true)),
        ],
        Instant::now(),
    );
    top::publish(
        &[("gt.old".into(), Value::Analog(7.0))],
        Instant::now() - Duration::from_secs(60),
    );
    let addr = gt::serve("127.0.0.1:0", 2.0).unwrap();

    let j = get(addr, "GET /tags.json HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(j.starts_with("HTTP/1.1 200 OK\r\n"), "{j}");
    assert!(
        j.contains("Content-Type: application/json\r\n")
            && j.contains("Cache-Control: no-store\r\n")
    );
    let b = body(&j);
    assert!(b.contains("\"tag\":\"gt.fresh\",\"value\":1.5,"), "{b}");
    assert!(b.contains("\"tag\":\"gt.<x>\",\"value\":1,"), "{b}");
    let old = b.split("\"tag\":\"gt.old\"").nth(1).unwrap();
    assert!(old.contains("\"stale\":true"), "{b}");
    assert!(
        b.split("\"tag\":\"gt.fresh\"")
            .nth(1)
            .unwrap()
            .contains("\"stale\":false"),
        "{b}"
    );
    assert!(
        body(&j).len().to_string()
            == j.split("Content-Length: ")
                .nth(1)
                .unwrap()
                .split("\r\n")
                .next()
                .unwrap()
    );

    let h = get(addr, "GET / HTTP/1.0\r\n\r\n");
    assert!(h.contains("Content-Type: text/html"), "{h}");
    assert!(body(&h).contains("<td>gt.fresh</td><td>1.5</td>"), "{h}");
    assert!(
        body(&h).contains("gt.&lt;x&gt;") && body(&h).contains("STALE"),
        "{h}"
    );

    assert!(get(addr, "GET /nope HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
    let p = get(
        addr,
        "POST /tags.json HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
    );
    assert!(
        p.starts_with("HTTP/1.1 405") && p.contains("Allow: GET\r\n"),
        "{p}"
    );
    assert!(get(addr, "garbage\r\n\r\n").starts_with("HTTP/1.1 405"));
}

#[test]
fn an_idle_client_neither_blocks_other_clients_nor_the_publisher() {
    let addr = gt::serve("127.0.0.1:0", 2.0).unwrap();
    let _idle: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(addr).unwrap()).collect();
    let t = Instant::now();
    let big: Vec<(String, Value)> = (0..800)
        .map(|i| (format!("gtb.{i}"), Value::Analog(i as f64)))
        .collect();
    for _ in 0..200 {
        top::publish(&big, Instant::now()); // what the engine does each cycle, while clients read
    }
    let r = get(addr, "GET /tags.json HTTP/1.1\r\n\r\n");
    assert!(
        r.starts_with("HTTP/1.1 200"),
        "served although four clients sit silent"
    );
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        t.elapsed()
    );
}

#[test]
fn connections_past_the_cap_are_closed_unanswered_and_the_rest_still_served() {
    let addr = gt::serve("127.0.0.1:0", 2.0).unwrap();
    let idle: Vec<TcpStream> = (0..16).map(|_| TcpStream::connect(addr).unwrap()).collect(); // the cap
    assert_eq!(
        get(addr, "GET / HTTP/1.1\r\n\r\n"),
        "",
        "the 17th is dropped"
    );
    drop(idle);
    let mut ok = String::new();
    for _ in 0..40 {
        ok = get(addr, "GET / HTTP/1.1\r\n\r\n"); // slots free as the idle clients are seen to leave
        if !ok.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
}

#[test]
fn a_bare_port_binds_every_interface_like_simulinkgt() {
    let addr = gt::serve(":0", 2.0).unwrap();
    assert!(addr.ip().is_unspecified() && addr.port() != 0, "{addr}");
}

#[test]
fn a_bad_address_is_an_error_naming_the_flag() {
    let e = gt::serve("not an address", 2.0).unwrap_err();
    assert!(e.starts_with("--gt-addr not an address:"), "{e}");
}

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn run_serves_its_own_tags_on_the_page_only_when_asked() {
    let (server, gt) = (
        format!("tcp://127.0.0.1:{}", free_port()),
        format!("127.0.0.1:{}", free_port()),
    );
    let publish = "udp://*;127.0.0.1:9"; // discard port: nobody listens, the page does not care
    let mut c = std::process::Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--server",
            &server,
            "--publish",
            publish,
            "--rate",
            "0.05",
        ])
        .args(["--gt-addr", &gt])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let addr: SocketAddr = gt.parse().unwrap();
    let mut got = String::new();
    for _ in 0..50 {
        if let Ok(mut s) = TcpStream::connect(addr) {
            s.write_all(b"GET /tags.json HTTP/1.1\r\n\r\n").unwrap();
            got.clear();
            s.read_to_string(&mut got).unwrap();
            if got.contains("lowerwater.height") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert!(c.wait().unwrap().success());
    assert!(
        got.contains("\"tag\":\"lowerwater.height\"") && got.contains("\"stale\":false"),
        "{got}"
    );
    assert!(
        out.contains(&format!("Info: ground truth page on http://{gt}/")),
        "{out}"
    );
}

#[test]
fn run_without_the_flag_listens_on_no_page_and_bad_values_are_refused() {
    let o = std::process::Command::new(BIN)
        .args(["run", MODEL, "--gt-addr", "not an address"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--gt-addr not an address:"));
    let o = std::process::Command::new(BIN)
        .args([
            "run",
            MODEL,
            "--gt-addr",
            "127.0.0.1:0",
            "--gt-stale-after",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2)); // a clap error, before anything is loaded
    assert!(String::from_utf8_lossy(&o.stderr).contains("expected seconds above 0"));
}
