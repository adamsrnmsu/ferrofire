//! Transcripts of a HiSLIP session (`tests/golden/hislip.transcript`) and of VXI-11 calls (`tests/golden/vxi11.transcript`), with no
//! sockets: what the I/O-free parts of the two servers answer, frame by frame and XDR word by word. The format is in each file's header.
//! The expected bytes follow IVI-6.1 and VXI-11 as written (checked against pyvisa-py in the conformance run); docs/field-device-scpi.md
//! lists the choices where they leave room.

mod common;

use ferrofire::fd::proto::scpi::device::{shared, Instrument, Scope};
use ferrofire::fd::proto::scpi::hislip::{async_reply, frame, t, Ctx, Frame, Link, Sync};
use ferrofire::fd::proto::scpi::vxi11::{ints, put_opaque, Vxi11};
use std::sync::Arc;

const HISLIP: &str = include_str!("golden/hislip.transcript");
const VXI11: &str = include_str!("golden/vxi11.transcript");

/// A quoted payload: `\n`, `\"`, `\\`, `\0` and `\xNN` escapes.
fn unquote(s: &str, at: &str) -> Vec<u8> {
    let inner = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{at}: `{s}` is not quoted"));
    let (mut out, mut it) = (vec![], inner.bytes());
    while let Some(b) = it.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match it.next() {
            Some(b'n') => out.push(b'\n'),
            Some(b'0') => out.push(0),
            Some(b'x') => {
                let hex: String = it.by_ref().take(2).map(char::from).collect();
                out.push(u8::from_str_radix(&hex, 16).unwrap());
            }
            Some(c) => out.push(c),
            None => panic!("{at}: dangling backslash"),
        }
    }
    out
}

/// Split a transcript line into words, keeping a quoted string (and an `s"..."` one) whole.
fn words(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut escaped) = (vec![], String::new(), false, false);
    for c in line.chars() {
        match (quoted, escaped, c) {
            (_, true, _) => {
                cur.push(c);
                escaped = false;
            }
            (true, false, '\\') => {
                cur.push(c);
                escaped = true;
            }
            (_, false, '"') => {
                cur.push(c);
                quoted = !quoted;
            }
            (false, false, ' ') if !cur.is_empty() => out.push(std::mem::take(&mut cur)),
            (false, false, ' ') => {}
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn body_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'))
}

fn type_of(name: &str) -> u8 {
    use t::*;
    [
        ("INITIALIZE", INITIALIZE),
        ("ERROR", ERROR),
        ("FATAL_ERROR", FATAL_ERROR),
        ("ASYNC_LOCK", ASYNC_LOCK),
        ("DATA", DATA),
        ("DATA_END", DATA_END),
        ("DEVICE_CLEAR_COMPLETE", DEVICE_CLEAR_COMPLETE),
        ("DEVICE_CLEAR_ACK", DEVICE_CLEAR_ACK),
        ("ASYNC_REMOTE_LOCAL", ASYNC_REMOTE_LOCAL),
        ("ASYNC_REMOTE_LOCAL_RESPONSE", ASYNC_REMOTE_LOCAL_RESPONSE),
        ("TRIGGER", TRIGGER),
        ("INTERRUPTED", INTERRUPTED),
        ("ASYNC_MAX_SIZE", ASYNC_MAX_SIZE),
        ("ASYNC_MAX_SIZE_RESPONSE", ASYNC_MAX_SIZE_RESPONSE),
        ("ASYNC_DEVICE_CLEAR", ASYNC_DEVICE_CLEAR),
        ("ASYNC_STATUS_QUERY", ASYNC_STATUS_QUERY),
        ("ASYNC_STATUS_RESPONSE", ASYNC_STATUS_RESPONSE),
        ("ASYNC_DEVICE_CLEAR_ACK", ASYNC_DEVICE_CLEAR_ACK),
    ]
    .iter()
    .find(|(n, _)| *n == name)
    .unwrap_or_else(|| panic!("no HiSLIP message type {name}"))
    .1
}

/// `TYPE CTL PARAM "payload"` as a frame.
fn parse_frame(w: &[String], at: &str, version: &str) -> Frame {
    let payload = unquote(&w[3], at);
    let payload = String::from_utf8_lossy(&payload)
        .replace("{version}", version)
        .into_bytes();
    frame(
        type_of(&w[0]),
        w[1].parse().unwrap(),
        u32::from_str_radix(&w[2], 16).unwrap(),
        &payload,
    )
}

#[test]
fn the_hislip_transcript_is_what_the_session_answers() {
    let (server, store) = common::scpi::load("");
    let ins = server.instrument();
    let status = shared();
    let (link, mut sync) = (Link::new(status.clone()), Sync::new(status));
    let c = Ctx {
        ins,
        store: &store,
        link: &link,
    };
    let version = env!("CARGO_PKG_VERSION");
    let mut lines = body_lines(HISLIP).peekable();
    while let Some((n, line)) = lines.next() {
        let at = format!("tests/golden/hislip.transcript:{}", n + 1);
        let w = words(
            line.strip_prefix("> ")
                .unwrap_or_else(|| panic!("{at}: expected `> `")),
        );
        let sent = parse_frame(&w[1..], &at, version);
        let got = match w[0].as_str() {
            "sync" => sync
                .recv(sent, &c)
                .unwrap_or_else(|e| panic!("{at}: fatal {e:?}")),
            "async" => vec![async_reply(&sent, &link)],
            other => panic!("{at}: channel {other}"),
        };
        let mut want = vec![];
        while let Some((m, l)) = lines.next_if(|(_, l)| l.starts_with('<')) {
            let at = format!("tests/golden/hislip.transcript:{}", m + 1);
            want.push(parse_frame(&words(l[1..].trim_start()), &at, version));
        }
        assert_eq!(got, want, "{at}: after `{line}`");
    }
}

/// An XDR argument or result word: a decimal number, or `s"text"` as opaque data.
fn xdr(tokens: &[String], at: &str) -> Vec<u8> {
    let mut out = vec![];
    for t in tokens {
        match t.strip_prefix('s') {
            Some(q) if q.starts_with('"') => {
                let text = String::from_utf8_lossy(&unquote(q, at))
                    .replace("{version}", env!("CARGO_PKG_VERSION"));
                put_opaque(&mut out, text.as_bytes());
            }
            _ => out.extend(ints(&[t.parse().unwrap_or_else(|_| panic!("{at}: `{t}`"))])),
        }
    }
    out
}

#[test]
fn the_vxi11_transcript_is_what_the_service_answers() {
    let (server, store) = common::scpi::load("");
    let ins = server.instrument();
    let ins = Instrument {
        idn: ins.idn.clone(),
        routes: ins.routes.clone(),
        max_line: ins.max_line,
    };
    let svc = Vxi11::new(Arc::new(ins), store, (Scope::Instrument, shared()), 4881, 4);
    let mut conn = svc.connection();
    let mut lines = body_lines(VXI11).peekable();
    while let Some((n, line)) = lines.next() {
        let at = format!("tests/golden/vxi11.transcript:{}", n + 1);
        if line == "@ connect" {
            svc.close(conn);
            conn = svc.connection();
            continue;
        }
        let w = words(
            line.strip_prefix("> ")
                .unwrap_or_else(|| panic!("{at}: expected `> `")),
        );
        let (prog, vers) = match w[0].as_str() {
            "pmap" => (100_000, 2),
            "core" => (0x0607AF, 1),
            "abort" => (0x0607B0, 1),
            _ => (12_345, 1),
        };
        let proc: u32 = w.get(1).map_or(0, |p| p.parse().unwrap());
        let mut call = ints(&[1, 0, 2, prog, vers, proc, 0, 0, 0, 0]);
        call.extend(xdr(&w[2.min(w.len())..], &at));
        let reply = svc
            .call(conn, &call)
            .unwrap_or_else(|| panic!("{at}: no reply"));
        let (_, expect) = lines
            .next_if(|(_, l)| l.starts_with('<'))
            .unwrap_or_else(|| panic!("{at}: no `<` line"));
        let e = words(expect[1..].trim_start());
        let want = match e[0].as_str() {
            "ok" => [ints(&[1, 1, 0, 0, 0, 0]), xdr(&e[1..], &at)].concat(),
            "err" => [
                ints(&[1, 1, 0, 0, 0, e[1].parse().unwrap()]),
                xdr(&e[2..], &at),
            ]
            .concat(),
            other => panic!("{at}: `{other}`"),
        };
        assert_eq!(
            reply.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            want.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "{at}: after `{line}`"
        );
    }
}
