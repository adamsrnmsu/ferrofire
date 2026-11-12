//! The SCPI server over real sockets: lines as clients send them (pipelined, split, CRLF, cut by a half-close), a client that never
//! sends a newline, the connection limit and idle timeout (an idle client must not block the others), descriptor handling when
//! many clients come and go, a bind failure and stopping. The answers themselves are in tests/scpi_golden.rs.

mod common;

use common::scpi::{free_port, load};
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::Store;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

struct Device {
    store: Store,
    stop: CancellationToken,
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

fn start(inner: &str) -> Device {
    let port = free_port();
    let (server, store) = load(&format!(
        "<endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging>
         <hislip-endpoint>none</hislip-endpoint><vxi11-endpoint>none</vxi11-endpoint>{inner}"
    ));
    let stop = CancellationToken::new();
    let task = tokio::spawn(server.run(store.clone(), stop.clone()));
    Device {
        store,
        stop,
        port,
        task,
    }
}

async fn connect(port: u16) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the server never listened on {port}");
}

/// Everything the server sends until it closes or `wait` passes: the text, and whether it closed.
async fn drain(s: &mut TcpStream, wait: Duration) -> (String, bool) {
    let (mut got, mut buf) = (vec![], [0u8; 4096]);
    loop {
        match timeout(wait, s.read(&mut buf)).await {
            Err(_) => return (String::from_utf8(got).unwrap(), false),
            Ok(Ok(0)) | Ok(Err(_)) => return (String::from_utf8(got).unwrap(), true),
            Ok(Ok(n)) => got.extend_from_slice(&buf[..n]),
        }
    }
}

/// One line sent, one line read.
async fn ask(s: &mut TcpStream, line: &str) -> String {
    s.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    let mut got = vec![];
    while !got.ends_with(b"\n") {
        let mut b = [0u8; 1];
        timeout(Duration::from_secs(2), s.read_exact(&mut b))
            .await
            .expect("an answer")
            .unwrap();
        got.push(b[0]);
    }
    String::from_utf8(got).unwrap()
}

fn open_fds() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}

const IDN: &str = concat!("ferrofire,rtu-1,0,", env!("CARGO_PKG_VERSION"), "\n");

#[tokio::test]
async fn a_client_reads_and_sets_and_sees_the_error_queue_over_tcp() {
    let d = start("");
    let mut s = connect(d.port).await;
    assert_eq!(ask(&mut s, "*IDN?").await, IDN);
    assert_eq!(ask(&mut s, "MEAS:SCAL:LEVEL?").await, "12.5\n");
    s.write_all(b"SOUR:SETPOINT 700\n").await.unwrap();
    assert_eq!(ask(&mut s, "SYST:ERR?").await, "0,\"No error\"\n");
    assert!(d.store.is_queued("setpoint"));
    assert_eq!(d.store.get_f64("setpoint"), Some(400.0));
    s.write_all(b"SOUR:SETPOINT 7000\n").await.unwrap();
    assert!(ask(&mut s, "SYST:ERR?").await.starts_with("-222,"));
    d.stop.cancel();
}

#[tokio::test]
async fn pipelined_split_and_crlf_lines_are_all_answered_in_order() {
    let d = start("");
    let mut s = connect(d.port).await;
    s.write_all(b"*IDN?\r\n*OPC?\n*TST?\n").await.unwrap();
    assert_eq!(
        drain(&mut s, Duration::from_millis(300)).await,
        (format!("{IDN}1\n0\n"), false)
    );
    for byte in b"*OPC?\n" {
        s.write_all(&[*byte]).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(
        drain(&mut s, Duration::from_millis(300)).await,
        ("1\n".into(), false)
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_message_cut_by_a_half_close_is_answered_before_the_server_closes() {
    let d = start("");
    let mut s = connect(d.port).await;
    s.write_all(b"*IDN?").await.unwrap();
    s.shutdown().await.unwrap();
    assert_eq!(
        drain(&mut s, Duration::from_secs(2)).await,
        (IDN.into(), true)
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_bare_carriage_return_ends_a_line_too() {
    let d = start("");
    let mut s = connect(d.port).await;
    s.write_all(b"*IDN?\r*OPC?\r*TST?\r\n").await.unwrap();
    assert_eq!(
        drain(&mut s, Duration::from_millis(300)).await,
        (format!("{IDN}1\n0\n"), false)
    );
    d.stop.cancel();
}

#[tokio::test]
async fn the_status_system_is_the_instruments_unless_the_scope_is_the_connection() {
    let d = start("");
    let (mut a, mut b) = (connect(d.port).await, connect(d.port).await);
    a.write_all(b"*ESE 32\nFOO\n").await.unwrap();
    assert_eq!(ask(&mut b, "*ESE?").await, "32\n");
    assert_eq!(
        ask(&mut b, "SYST:ERR?").await,
        "-113,\"Undefined header;FOO\"\n"
    );
    assert_eq!(
        ask(&mut a, "SYST:ERR:COUN?").await,
        "0\n",
        "b emptied the queue a filled"
    );
    d.stop.cancel();
    let d = start("<status-scope>connection</status-scope>");
    let (mut a, mut b) = (connect(d.port).await, connect(d.port).await);
    a.write_all(b"*ESE 32\nFOO\n").await.unwrap();
    assert_eq!(ask(&mut b, "*ESE?").await, "0\n");
    assert_eq!(ask(&mut b, "SYST:ERR:COUN?").await, "0\n");
    assert_eq!(ask(&mut a, "SYST:ERR:COUN?").await, "1\n");
    d.stop.cancel();
}

#[tokio::test]
async fn an_overlong_line_and_garbage_cost_an_error_and_not_the_connection() {
    let d = start("<max-line>100</max-line>");
    let mut s = connect(d.port).await;
    s.write_all(&[b'A'; 5000]).await.unwrap();
    s.write_all(b"\n").await.unwrap();
    assert_eq!(
        ask(&mut s, "SYST:ERR?").await,
        "-363,\"Input buffer overrun\"\n"
    );
    s.write_all(&[0xFFu8; 100_000]).await.unwrap();
    s.write_all(b"\n").await.unwrap();
    assert_eq!(
        ask(&mut s, "SYST:ERR?").await,
        "-363,\"Input buffer overrun\"\n"
    );
    for junk in [
        "\0\0\0",
        "\"",
        "::::",
        ";;;;",
        "*",
        "?",
        "SOUR:SETPOINT \"",
        "\u{7f}",
    ] {
        s.write_all(format!("{junk}\n").as_bytes()).await.unwrap();
    }
    // eight junk lines are one syntax error each, and ";;;;" is five empty units
    assert_eq!(ask(&mut s, "SYST:ERR:COUN?").await, "12\n");
    assert_eq!(ask(&mut s, "*IDN?").await, IDN);
    d.stop.cancel();
}

#[tokio::test]
async fn at_the_connection_limit_a_new_client_is_closed_and_the_others_are_served() {
    let d = start("<max-connections>2</max-connections>");
    let (mut a, mut b) = (connect(d.port).await, connect(d.port).await);
    assert_eq!(ask(&mut a, "*IDN?").await, IDN);
    assert_eq!(ask(&mut b, "*IDN?").await, IDN);
    let mut c = connect(d.port).await;
    let _ = c.write_all(b"*IDN?\n").await;
    assert_eq!(
        drain(&mut c, Duration::from_secs(2)).await,
        (String::new(), true)
    );
    assert_eq!(ask(&mut a, "*OPC?").await, "1\n");
    drop(a);
    let mut again = None;
    for _ in 0..50 {
        let mut e = connect(d.port).await;
        let _ = e.write_all(b"*OPC?\n").await;
        if drain(&mut e, Duration::from_millis(200)).await == ("1\n".into(), false) {
            again = Some(e);
            break;
        }
    }
    assert!(again.is_some(), "a place frees up when a client leaves");
    assert_eq!(ask(&mut b, "*OPC?").await, "1\n");
    d.stop.cancel();
}

#[tokio::test]
async fn an_idle_client_is_dropped_and_never_holds_up_the_others() {
    let d = start("<idle-timeout>1</idle-timeout>");
    let mut quiet = connect(d.port).await;
    let mut busy = connect(d.port).await;
    for _ in 0..4 {
        assert_eq!(ask(&mut busy, "*OPC?").await, "1\n");
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    assert_eq!(
        drain(&mut quiet, Duration::from_millis(500)).await,
        (String::new(), true)
    );
    d.stop.cancel();
}

#[tokio::test]
async fn clients_coming_and_going_leave_no_descriptors_behind() {
    let d = start("");
    let before = open_fds();
    for i in 0..300 {
        let mut s = connect(d.port).await;
        if i % 3 == 0 {
            assert_eq!(ask(&mut s, "*OPC?").await, "1\n");
        } else if i % 3 == 1 {
            s.write_all(b"*IDN?\nSOUR:SETPOINT").await.unwrap();
        }
    }
    let mut s = connect(d.port).await;
    assert_eq!(ask(&mut s, "*OPC?").await, "1\n");
    drop(s);
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let (Some(before), Some(after)) = (before, open_fds()) {
        assert!(after <= before + 3, "descriptors {before} -> {after}");
    }
    d.stop.cancel();
}

#[tokio::test]
async fn a_port_already_in_use_is_logged_and_the_task_ends() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let (server, store) = load(&format!(
        "<endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging>
         <hislip-endpoint>none</hislip-endpoint><vxi11-endpoint>none</vxi11-endpoint>"
    ));
    let task = tokio::spawn(server.run(store, CancellationToken::new()));
    timeout(Duration::from_secs(2), task)
        .await
        .expect("run returns when the bind fails")
        .unwrap();
}

#[tokio::test]
async fn stopping_closes_the_clients_and_the_listener() {
    let d = start("");
    let mut s = connect(d.port).await;
    assert_eq!(ask(&mut s, "*OPC?").await, "1\n");
    d.stop.cancel();
    assert_eq!(
        drain(&mut s, Duration::from_secs(2)).await,
        (String::new(), true)
    );
    timeout(Duration::from_secs(2), d.task)
        .await
        .unwrap()
        .unwrap();
    assert!(TcpStream::connect(("127.0.0.1", d.port)).await.is_err());
}
