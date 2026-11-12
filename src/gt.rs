//! The ground-truth page: `ferrofire run --gt-addr HOST:PORT` (and the `simulinksolver` install name) serve the live tag table
//! that `simulinkgt` used to: every published tag with its value, the age of that value and whether it is stale, as a small HTML page
//! (`/`) and as JSON (`/tags.json`). Read-only (GET only), off unless an address is given.
//!
//! The engine is never involved: the page reads the table `top::publish` already keeps (one lock per publish cycle), copies it and
//! lets go before it renders or writes anything, and each connection runs on its own short-lived thread with timeouts.

use crate::probe::json_str;
use crate::top;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Seconds without a publish after which a tag shows as stale (`ferrofire top --stale-after`'s default).
pub const STALE_AFTER: f64 = 2.0;
/// Connections served at once; the rest are closed unanswered.
const MAX_CONNS: usize = 16;
/// A client that has not sent its request line, or taken the reply, within this long is dropped.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// Largest request head read.
const MAX_HEAD: usize = 8 * 1024;

pub struct Row {
    pub tag: String,
    pub value: f64,
    pub age: f64,
}

impl Row {
    fn stale(&self, stale_after: f64) -> bool {
        self.age >= stale_after
    }
}

/// The table now: one row per tag, by name.
pub fn rows(now: Instant) -> Vec<Row> {
    top::tags(now)
        .into_iter()
        .map(|(tag, value, age)| Row { tag, value, age })
        .collect()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn html(rows: &[Row], stale_after: f64) -> String {
    let stale = rows.iter().filter(|r| r.stale(stale_after)).count();
    let mut out = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"2\"><title>ground truth</title>\
         <style>body{{font-family:monospace}}td,th{{padding:0 1em;text-align:right}}td:first-child,th:first-child{{text-align:left}}\
         .stale{{color:#b00;font-weight:bold}}</style></head><body><h1>ground truth</h1>\
         <p>{} tags, {} stale (no update for {}s or more). <a href=\"/tags.json\">JSON</a></p>\
         <table><tr><th>tag</th><th>value</th><th>age (s)</th><th></th></tr>",
        rows.len(),
        stale,
        stale_after
    );
    for r in rows {
        let s = r.stale(stale_after);
        out += &format!(
            "<tr{}><td>{}</td><td>{}</td><td>{:.3}</td><td>{}</td></tr>",
            if s { " class=\"stale\"" } else { "" },
            escape(&r.tag),
            r.value,
            r.age,
            if s { "STALE" } else { "" }
        );
    }
    out + "</table></body></html>"
}

/// `{"stale_after":2,"tags":[{"tag":..,"value":..,"age":..,"stale":..},..]}`; a value that is not finite is `null`.
pub fn json(rows: &[Row], stale_after: f64) -> String {
    let tags: Vec<String> = rows
        .iter()
        .map(|r| {
            let value = if r.value.is_finite() {
                r.value.to_string()
            } else {
                "null".into()
            };
            format!(
                "{{\"tag\":{},\"value\":{value},\"age\":{:.3},\"stale\":{}}}",
                json_str(&r.tag),
                r.age,
                r.stale(stale_after)
            )
        })
        .collect();
    format!(
        "{{\"stale_after\":{stale_after},\"tags\":[{}]}}",
        tags.join(",")
    )
}

/// (status line, content type, body) of one request line's answer; `rows` is only called for a page that needs it.
pub fn route(
    request_line: &str,
    stale_after: f64,
    rows: impl FnOnce() -> Vec<Row>,
) -> (&'static str, &'static str, String) {
    let mut p = request_line.split(' ');
    let (method, target) = (p.next().unwrap_or(""), p.next().unwrap_or(""));
    let path = target.split(['?', '#']).next().unwrap_or("");
    match (method, path) {
        ("GET", "/" | "/index.html") => (
            "200 OK",
            "text/html; charset=utf-8",
            html(&rows(), stale_after),
        ),
        ("GET", "/tags.json") => ("200 OK", "application/json", json(&rows(), stale_after)),
        ("GET", _) => ("404 Not Found", "text/plain", "not found\n".into()),
        _ => ("405 Method Not Allowed", "text/plain", "GET only\n".into()),
    }
}

/// The first line of the request head, once the head (or `MAX_HEAD` bytes of it) has arrived.
fn request_line(s: &mut TcpStream) -> Option<String> {
    let mut buf = vec![0u8; MAX_HEAD];
    let mut n = 0;
    while n < MAX_HEAD {
        match s.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
        if buf[..n].windows(2).any(|w| w == b"\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    head.lines().next().map(str::to_string)
}

fn handle(mut s: TcpStream, stale_after: f64) {
    let _ = s.set_read_timeout(Some(IO_TIMEOUT));
    let _ = s.set_write_timeout(Some(IO_TIMEOUT));
    let Some(line) = request_line(&mut s) else {
        return;
    };
    let (status, ctype, body) = route(&line, stale_after, || rows(Instant::now()));
    let allow = if status.starts_with("405") {
        "Allow: GET\r\n"
    } else {
        ""
    };
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{allow}Connection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Bind `addr` and serve in the background; returns the bound address (so `:0` can be asked for). Needs `top::publish` fed by the engine.
pub fn serve(addr: &str, stale_after: f64) -> Result<SocketAddr, String> {
    // `:8080` as simulinkgt took it means every interface
    let full = if addr.starts_with(':') {
        format!("0.0.0.0{addr}")
    } else {
        addr.to_string()
    };
    let l = TcpListener::bind(&full).map_err(|e| format!("--gt-addr {addr}: {e}"))?;
    let bound = l.local_addr().map_err(|e| e.to_string())?;
    let live = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
                live.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let live = live.clone();
            std::thread::spawn(move || {
                handle(s, stale_after);
                live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(bound)
}

/// `--gt-addr` / `--gt-stale-after`: serve the page when an address is given (true), do nothing when not (false).
pub fn start(addr: Option<&str>, stale_after: f64) -> Result<bool, String> {
    let Some(addr) = addr else {
        return Ok(false);
    };
    let bound = serve(addr, stale_after)?;
    println!("Info: ground truth page on http://{bound}/");
    Ok(true)
}

/// clap value parser: a number of seconds above zero.
pub fn positive(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(x) if x > 0.0 && x.is_finite() => Ok(x),
        _ => Err(format!("`{s}`: expected seconds above 0")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(tag: &str, value: f64, age: f64) -> Row {
        Row {
            tag: tag.into(),
            value,
            age,
        }
    }

    #[test]
    fn positive_seconds_only() {
        assert_eq!(positive("0.5"), Ok(0.5));
        for bad in ["0", "-1", "x", "nan", "inf", ""] {
            assert!(positive(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn stale_is_age_at_or_over_the_limit() {
        let rows = [r("a", 1.0, 1.99), r("b", 2.0, 2.0)];
        let j = json(&rows, 2.0);
        assert!(
            j.contains("\"tag\":\"a\",\"value\":1,\"age\":1.990,\"stale\":false"),
            "{j}"
        );
        assert!(
            j.contains("\"tag\":\"b\",\"value\":2,\"age\":2.000,\"stale\":true"),
            "{j}"
        );
        assert!(j.starts_with("{\"stale_after\":2,\"tags\":["));
    }

    #[test]
    fn json_escapes_names_and_nulls_non_finite_values() {
        let j = json(&[r("q\"x", f64::NAN, 0.0), r("i", f64::INFINITY, 0.0)], 2.0);
        assert!(j.contains("\"tag\":\"q\\\"x\",\"value\":null"), "{j}");
        assert!(j.contains("\"tag\":\"i\",\"value\":null"), "{j}");
    }

    #[test]
    fn html_escapes_marks_stale_rows_and_counts_them() {
        let h = html(&[r("<b>&", 1.5, 0.1), r("old", 0.0, 9.0)], 2.0);
        assert!(
            h.contains("<td>&lt;b&gt;&amp;</td><td>1.5</td><td>0.100</td><td></td>"),
            "{h}"
        );
        assert!(
            h.contains("<tr class=\"stale\"><td>old</td><td>0</td><td>9.000</td><td>STALE</td>"),
            "{h}"
        );
        assert!(h.contains("2 tags, 1 stale"), "{h}");
    }

    #[test]
    fn routes_are_get_only_and_ignore_the_query() {
        let none = || -> Vec<Row> { vec![] };
        assert_eq!(route("GET / HTTP/1.1", 2.0, none).0, "200 OK");
        assert_eq!(
            route("GET /tags.json?x=1 HTTP/1.1", 2.0, none).1,
            "application/json"
        );
        assert_eq!(route("GET /nope HTTP/1.1", 2.0, none).0, "404 Not Found");
        for m in ["POST", "PUT", "DELETE", "HEAD", ""] {
            assert_eq!(
                route(&format!("{m} / HTTP/1.1"), 2.0, none).0,
                "405 Method Not Allowed",
                "{m}"
            );
        }
    }

    #[test]
    fn a_missing_page_never_reads_the_table() {
        let (s, ..) = route("GET /nope HTTP/1.1", 2.0, || panic!("read the table"));
        assert_eq!(s, "404 Not Found");
    }
}
