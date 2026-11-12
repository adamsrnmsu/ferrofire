//! The SCPI server over HiSLIP on real sockets: the two-channel handshake, data in one and several frames, response framing,
//! the exact -410 / -420 of IEEE 488.2, device clear, the lock, the status query, the fatal errors, and the status system shared with the
//! raw socket. The framing is the crate's own `hislip::Reader` / `Frame`; the answers are checked as bytes.

mod common;

use common::scpi::{free_port, load};
use ferrofire::fd::proto::scpi::hislip::{frame, t::*, Frame, Reader};
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
    hislip: u16,
    raw: u16,
}

fn start(inner: &str) -> Device {
    let (hislip, raw) = (free_port(), free_port());
    let (server, store) = load(&format!(
        "<endpoint>tcp://127.0.0.1:{raw}</endpoint><hislip-endpoint>tcp://127.0.0.1:{hislip}</hislip-endpoint>
         <vxi11-endpoint>none</vxi11-endpoint><event-logging>/dev/null</event-logging>{inner}"
    ));
    let stop = CancellationToken::new();
    tokio::spawn(server.run(store.clone(), stop.clone()));
    Device {
        store,
        stop,
        hislip,
        raw,
    }
}

async fn connect(port: u16) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("nothing listens on {port}");
}

/// One channel of a HiSLIP connection.
struct Chan {
    s: TcpStream,
    rd: Reader,
}

impl Chan {
    async fn new(port: u16) -> Chan {
        Chan {
            s: connect(port).await,
            rd: Reader::default(),
        }
    }

    async fn send(&mut self, f: Frame) {
        self.s.write_all(&f.encode()).await.unwrap();
    }

    /// The next frame, `None` if the server closed or said nothing for a while.
    async fn recv_within(&mut self, wait: Duration) -> Option<Frame> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(f) = self.rd.take().unwrap() {
                return Some(f);
            }
            match timeout(wait, self.s.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => self.rd.push(&buf[..n]),
                _ => return None,
            }
        }
    }

    async fn recv(&mut self) -> Frame {
        self.recv_within(Duration::from_secs(3))
            .await
            .expect("a frame")
    }

    async fn call(&mut self, f: Frame) -> Frame {
        self.send(f).await;
        self.recv().await
    }

    /// The server closed this channel.
    async fn closed(&mut self) -> bool {
        let mut b = [0u8; 1];
        matches!(
            timeout(Duration::from_secs(3), self.s.read(&mut b)).await,
            Ok(Ok(0) | Err(_))
        )
    }
}

/// A client: both channels open, message ids counting from 0xFFFFFF00 by two, as VISA does.
struct Client {
    sync: Chan,
    asy: Chan,
    id: u32,
    session: u16,
}

async fn open(port: u16) -> Client {
    let mut sync = Chan::new(port).await;
    let r = sync
        .call(frame(INITIALIZE, 0, 0x0100_5858, b"hislip0"))
        .await;
    assert_eq!(
        (r.ty, r.ctl, r.param >> 16),
        (INITIALIZE_RESPONSE, 0, 0x0100)
    );
    let session = (r.param & 0xFFFF) as u16;
    let mut asy = Chan::new(port).await;
    let r = asy
        .call(frame(ASYNC_INITIALIZE, 0, session.into(), &[]))
        .await;
    assert_eq!((r.ty, r.ctl), (ASYNC_INITIALIZE_RESPONSE, 0));
    Client {
        sync,
        asy,
        id: 0xFFFF_FF00,
        session,
    }
}

impl Client {
    /// Send a message (DataEND), then read its whole response. `read_before` is the RMT-delivered bit a VISA client sets once it
    /// has read a response.
    async fn ask(&mut self, text: &str) -> String {
        self.send(text, 1).await;
        self.response().await
    }

    async fn send(&mut self, text: &str, rmt: u8) {
        let f = frame(DATA_END, rmt, self.id, text.as_bytes());
        self.sync.send(f).await;
        self.id += 2;
    }

    async fn response(&mut self) -> String {
        let mut out = vec![];
        loop {
            let f = self.sync.recv().await;
            assert!(matches!(f.ty, DATA | DATA_END), "{f:?}");
            out.extend(&f.payload);
            if f.ty == DATA_END {
                return String::from_utf8(out).unwrap();
            }
        }
    }

    /// The error queue entry.
    async fn error(&mut self) -> String {
        self.ask("SYST:ERR?").await
    }
}

const IDN: &str = concat!("ferrofire,rtu-1,0,", env!("CARGO_PKG_VERSION"), "\n");

#[tokio::test]
async fn a_client_opens_both_channels_and_runs_queries_and_commands() {
    let d = start("");
    let mut c = open(d.hislip).await;
    assert_eq!(c.ask("*IDN?\n").await, IDN);
    assert_eq!(
        c.ask("MEAS:SCAL:LEVEL?").await,
        "12.5\n",
        "no newline needed with END"
    );
    c.send("SOUR:SETPOINT 700", 1).await;
    assert_eq!(c.error().await, "0,\"No error\"\n");
    assert!(d.store.is_queued("setpoint"));
    c.send("SOUR:SETPOINT 7000", 1).await;
    assert!(c.error().await.starts_with("-222,"));
    let r = c.sync.call(frame(DATA_END, 1, c.id, b"*IDN?")).await;
    assert_eq!(
        (r.ty, r.param),
        (DATA_END, c.id),
        "the response carries the message id of the query"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_message_in_several_frames_and_a_long_response_in_several_frames() {
    let d = start("");
    let mut c = open(d.hislip).await;
    c.sync.send(frame(DATA, 1, c.id, b"*ID")).await;
    c.sync.send(frame(DATA, 1, c.id, b"N?;*OP")).await;
    c.sync.send(frame(DATA_END, 1, c.id, b"C?")).await;
    assert_eq!(
        c.response().await,
        format!("{}1\n", IDN.trim_end().to_owned() + ";")
    );
    let query = ["*IDN?"; 20].join(";");
    c.send(&query, 1).await;
    let first = c.sync.recv().await;
    assert_eq!(
        first.ty, DATA,
        "more than 256 bytes: cut to the default maximum message size"
    );
    assert_eq!(first.payload.len(), 256);
    let mut got = first.payload.clone();
    loop {
        let f = c.sync.recv().await;
        got.extend(f.payload);
        if f.ty == DATA_END {
            break;
        }
    }
    assert_eq!(got.len(), 20 * IDN.len() - 1 + 1);
    let max = 1 << 16;
    let r = c
        .asy
        .call(frame(ASYNC_MAX_SIZE, 0, 0, &(max as u64).to_be_bytes()))
        .await;
    assert_eq!(
        (r.ty, r.payload),
        (ASYNC_MAX_SIZE_RESPONSE, (1u64 << 20).to_be_bytes().to_vec())
    );
    c.send(&query, 1).await;
    let f = c.sync.recv().await;
    assert_eq!(
        (f.ty, f.payload.len()),
        (DATA_END, got.len()),
        "now one frame"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_new_message_over_an_unread_response_is_interrupted_and_a_dropped_one_unterminated() {
    let d = start("");
    let mut c = open(d.hislip).await;
    c.send("*IDN?", 0).await;
    assert_eq!(c.response().await, IDN);
    c.send("*OPC?", 0).await;
    let f = c.sync.recv().await;
    assert_eq!(
        (f.ty, f.param),
        (INTERRUPTED, 0xFFFF_FF00),
        "the id of the response that was not read"
    );
    assert_eq!(c.response().await, "1\n", "the new message is run");
    assert_eq!(c.error().await, "-410,\"Query INTERRUPTED\"\n");
    c.sync.send(frame(DATA, 1, c.id, b"*IDN?")).await;
    c.id += 2;
    c.sync.send(frame(DATA_END, 1, c.id, b"*OPC?")).await;
    c.id += 2;
    assert_eq!(c.response().await, "1\n");
    assert_eq!(c.error().await, "-420,\"Query UNTERMINATED\"\n");
    // the second connection sees the same queue; a client that goes away mid-message leaves -420 for the instrument
    let mut other = open(d.hislip).await;
    other.sync.send(frame(DATA, 1, other.id, b"*IDN?")).await;
    other.sync.s.shutdown().await.unwrap();
    drop(other);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(c.error().await, "-420,\"Query UNTERMINATED\"\n");
    d.stop.cancel();
}

#[tokio::test]
async fn the_async_channel_reports_status_and_accepts_remote_local() {
    let d = start("");
    let mut c = open(d.hislip).await;
    c.send("*IDN?", 0).await;
    let s = c.asy.call(frame(ASYNC_STATUS_QUERY, 0, c.id, &[])).await;
    assert_eq!(
        (s.ty, s.ctl),
        (ASYNC_STATUS_RESPONSE, 16),
        "MAV: the response is not read yet"
    );
    assert_eq!(c.response().await, IDN);
    let s = c.asy.call(frame(ASYNC_STATUS_QUERY, 1, c.id, &[])).await;
    assert_eq!(s.ctl, 0);
    c.send("*FOO", 1).await;
    let s = c.asy.call(frame(ASYNC_STATUS_QUERY, 1, c.id, &[])).await;
    assert_eq!(s.ctl, 4, "the error queue is not empty");
    let r = c.asy.call(frame(ASYNC_REMOTE_LOCAL, 3, c.id, &[])).await;
    assert_eq!((r.ty, r.ctl, r.param), (ASYNC_REMOTE_LOCAL_RESPONSE, 0, 0));
    d.stop.cancel();
}

#[tokio::test]
async fn a_device_clear_drops_what_is_pending_and_the_session_goes_on() {
    let d = start("");
    let mut c = open(d.hislip).await;
    c.sync.send(frame(DATA, 1, c.id, b"*IDN?")).await;
    let a = c.asy.call(frame(ASYNC_DEVICE_CLEAR, 0, 0, &[])).await;
    assert_eq!((a.ty, a.ctl), (ASYNC_DEVICE_CLEAR_ACK, 0));
    let done = c.sync.call(frame(DEVICE_CLEAR_COMPLETE, 0, 0, &[])).await;
    assert_eq!((done.ty, done.ctl), (DEVICE_CLEAR_ACK, 0));
    c.id = 0xFFFF_FF00;
    assert_eq!(c.ask("*OPC?").await, "1\n");
    assert_eq!(
        c.error().await,
        "0,\"No error\"\n",
        "a clear is not an error"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn one_session_holds_the_lock_and_the_others_wait_for_it() {
    let d = start("");
    let (mut a, mut b) = (open(d.hislip).await, open(d.hislip).await);
    let lock = |ms| frame(ASYNC_LOCK, 1, ms, b"");
    assert_eq!(a.asy.call(frame(ASYNC_LOCK_INFO, 0, 0, &[])).await.ctl, 0);
    assert_eq!(a.asy.call(lock(1000)).await.ctl, 1, "granted");
    assert_eq!(b.asy.call(frame(ASYNC_LOCK_INFO, 0, 0, &[])).await.ctl, 1);
    assert_eq!(
        b.asy.call(lock(100)).await.ctl,
        0,
        "held: b's request times out"
    );
    b.send("*IDN?", 1).await;
    assert!(
        b.sync
            .recv_within(Duration::from_millis(300))
            .await
            .is_none(),
        "b's message waits for the lock"
    );
    assert_eq!(a.ask("*OPC?").await, "1\n", "the holder is served");
    let r = a.asy.call(frame(ASYNC_LOCK, 0, a.id, &[])).await;
    assert_eq!((r.ty, r.ctl), (ASYNC_LOCK_RESPONSE, 1), "released");
    assert_eq!(b.response().await, IDN, "and b's message runs");
    assert_eq!(
        a.asy.call(frame(ASYNC_LOCK, 0, 0, &[])).await.ctl,
        3,
        "nothing to release"
    );
    assert_eq!(a.asy.call(lock(0)).await.ctl, 1);
    drop(a);
    assert_eq!(b.ask("*OPC?").await, "1\n", "a lock dies with its session");
    d.stop.cancel();
}

#[tokio::test]
async fn bad_headers_and_bad_sequences_are_fatal_and_odd_messages_an_error() {
    let d = start("");
    let fatal = |f: &Frame, code: u8| assert_eq!((f.ty, f.ctl), (FATAL_ERROR, code), "{f:?}");
    let mut s = Chan::new(d.hislip).await;
    s.s.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    fatal(&s.recv().await, 1);
    assert!(s.closed().await);
    let mut s = Chan::new(d.hislip).await;
    let mut big = frame(DATA, 0, 0, &[]).encode();
    big[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    s.s.write_all(&big).await.unwrap();
    fatal(&s.recv().await, 1);
    let mut s = Chan::new(d.hislip).await;
    fatal(&s.call(frame(DATA_END, 0, 0, b"*IDN?")).await, 3);
    let mut s = Chan::new(d.hislip).await;
    fatal(&s.call(frame(ASYNC_INITIALIZE, 0, 999, &[])).await, 3);
    let mut s = Chan::new(d.hislip).await;
    fatal(
        &s.call(frame(INITIALIZE, 0, 0x0100_5858, b"hislip7")).await,
        0,
    );
    // a sync channel with no async one
    let mut s = Chan::new(d.hislip).await;
    s.call(frame(INITIALIZE, 0, 0, b"")).await;
    fatal(&s.call(frame(DATA_END, 0, 0, b"*IDN?")).await, 2);
    // a second Initialize, a second AsyncInitialize
    let mut c = open(d.hislip).await;
    fatal(&c.sync.call(frame(INITIALIZE, 0, 0, b"hislip0")).await, 3);
    let mut again = Chan::new(d.hislip).await;
    fatal(
        &again
            .call(frame(ASYNC_INITIALIZE, 0, c.session.into(), &[]))
            .await,
        3,
    );
    let mut c = open(d.hislip).await;
    let e = c.sync.call(frame(250, 0, 0, &[])).await;
    assert_eq!(
        (e.ty, e.ctl),
        (ERROR, 1),
        "an unrecognized message type is an Error, not the end"
    );
    let e = c.asy.call(frame(250, 0, 0, &[])).await;
    assert_eq!((e.ty, e.ctl), (ERROR, 1));
    assert_eq!(c.ask("*OPC?").await, "1\n");
    d.stop.cancel();
}

#[tokio::test]
async fn hislip_and_the_raw_socket_share_one_status_system() {
    let d = start("");
    let mut c = open(d.hislip).await;
    let mut raw = connect(d.raw).await;
    raw.write_all(b"*ESE 32\nFOO\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(c.ask("*ESE?").await, "32\n");
    assert_eq!(c.error().await, "-113,\"Undefined header;FOO\"\n");
    d.stop.cancel();
    let d = start("<status-scope>connection</status-scope>");
    let (mut a, mut b) = (open(d.hislip).await, open(d.hislip).await);
    a.send("*FOO", 1).await;
    assert_eq!(b.error().await, "0,\"No error\"\n");
    assert!(a.error().await.starts_with("-113,"));
    d.stop.cancel();
}

#[tokio::test]
async fn stopping_and_the_session_limit_close_connections() {
    let d = start("<max-connections>2</max-connections>");
    let c = open(d.hislip).await;
    let mut third = Chan::new(d.hislip).await;
    let _ = third
        .s
        .write_all(&frame(INITIALIZE, 0, 0, b"hislip0").encode())
        .await;
    assert!(
        third
            .recv_within(Duration::from_millis(500))
            .await
            .is_none(),
        "no place: closed at once"
    );
    drop(c);
    d.stop.cancel();
}
