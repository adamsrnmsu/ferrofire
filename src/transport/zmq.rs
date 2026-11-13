use super::{Handler, Publisher, Requester};
use crate::bennu::{
    cycle_bytes, datagrams_msg, group_for, grow_sndbuf, parse_udp, send_all, udp_sender, with_nul,
    STOP,
};
use crate::hint::{show, RateLimit};
use crate::sys::wait_writable;
use ::zeromq::{Socket, SocketRecv, SocketSend};
use std::net::{SocketAddr, UdpSocket};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::time::timeout;

/// Wait this long for a request or a reply send before looking at `STOP` again / giving up on a vanished peer.
const IO_TIMEOUT: Duration = Duration::from_millis(1000);
/// At most one "UDP send failed" line per this long.
const SEND_WARN_EVERY: Duration = Duration::from_secs(10);
/// Longest one publish cycle waits in total for a full send buffer to drain before dropping the rest.
const SEND_WAIT: Duration = Duration::from_millis(10);

/// The pure-Rust `zeromq` crate is async; each socket owner drives it from its own single-threaded runtime.
fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

/// `zeromq` wants a concrete address; libzmq's `tcp://*:port` means every interface.
fn endpoint_of(endpoint: &str) -> String {
    endpoint.replace("tcp://*:", "tcp://0.0.0.0:")
}

/// Bind a REP socket on `endpoint` and answer each request with `handler` until `STOP` (polling, so it is noticed).
/// A REP that skips a reply wedges its client, so every request gets exactly one: a handler that panics or a request
/// in several frames is answered with an `ERR`, and the loop goes on.
pub fn serve(endpoint: &str, handler: Handler) -> Result<std::thread::JoinHandle<()>, String> {
    let rt = runtime()?;
    let mut rep = ::zeromq::RepSocket::new();
    rt.block_on(rep.bind(&endpoint_of(endpoint)))
        .map_err(|e| format!("bind {endpoint}: {e}"))?;
    Ok(std::thread::spawn(move || {
        rt.block_on(async {
            while !STOP.load(Ordering::SeqCst) {
                let Ok(Ok(msg)) = timeout(Duration::from_millis(100), rep.recv()).await else {
                    continue;
                };
                let parts: Vec<Vec<u8>> = msg.into_vec().iter().map(|f| f.to_vec()).collect();
                let _ = timeout(IO_TIMEOUT, rep.send(answer(&handler, &parts).into())).await;
            }
        });
    }))
}

/// The one reply to a request of `parts` frames; never panics.
fn answer(handler: &Handler, parts: &[Vec<u8>]) -> Vec<u8> {
    let [req] = parts else {
        return with_nul(format!(
            "ERR=request has {} frames, expected 1",
            parts.len()
        ));
    };
    catch_unwind(AssertUnwindSafe(|| handler(req))).unwrap_or_else(|_| {
        let what = show(&String::from_utf8_lossy(req));
        with_nul(format!("ERR=internal error handling request '{what}'"))
    })
}

/// Connect a REQ client to `endpoint` (the provider's REP); calls give up after `timeout_ms`.
/// The connection is made on the first call (and again after a failed one), so an absent provider is a timeout, not a startup error.
pub fn requester(endpoint: &str, timeout_ms: i32) -> Result<Box<dyn Requester>, String> {
    // the connect is lazy, so a malformed endpoint would otherwise only ever show as timeouts
    if !endpoint.contains("://") {
        return Err(format!("bad endpoint {endpoint}: expected tcp://host:port"));
    }
    Ok(Box::new(Req {
        rt: runtime()?,
        endpoint: endpoint_of(endpoint),
        timeout: Duration::from_millis(timeout_ms.max(0) as u64),
        sock: None,
    }))
}

/// A RADIO-framed UDP publisher for `udp://[iface;]host:port`. Counters are per endpoint.
pub fn publisher(endpoint: &str) -> Result<Box<dyn Publisher>, String> {
    let (iface, dest) = parse_udp(endpoint)?;
    let sock = udp_sender(iface.as_deref(), dest)?;
    // a full send buffer must cost a datagram, never the engine tick
    sock.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(Box::new(Radio {
        sock,
        dest,
        endpoint: endpoint.into(),
        group: group_for(endpoint),
        seq: AtomicU64::new(0),
        cyc: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
        warn: RateLimit::new(SEND_WARN_EVERY),
    }))
}

struct Radio {
    sock: UdpSocket,
    dest: SocketAddr,
    endpoint: String,
    group: String,
    /// Datagrams sent so far; the next datagram carries `_ff.seq` = this.
    seq: AtomicU64,
    /// Publish cycles so far; the next cycle carries `_ff.cyc` = this.
    cyc: AtomicU64,
    /// Datagrams `send_to` refused (ENOBUFS, unreachable, network down, ...).
    dropped: AtomicU64,
    warn: RateLimit,
}

/// A REQ that timed out is wedged (it must not send twice), so a failed call drops it and the next call connects a fresh one.
struct Req {
    rt: tokio::runtime::Runtime,
    endpoint: String,
    timeout: Duration,
    sock: Option<::zeromq::ReqSocket>,
}

impl Requester for Req {
    fn call(&mut self, req: &[u8]) -> Result<Vec<u8>, String> {
        let _in_rt = self.rt.enter(); // dropping a zeromq socket needs a runtime too
        let mut sock = self.sock.take();
        let endpoint = self.endpoint.clone();
        let t = self.timeout;
        let r = self.rt.block_on(async {
            timeout(t, async {
                let s = match &mut sock {
                    Some(s) => s,
                    None => {
                        let mut s = ::zeromq::ReqSocket::new();
                        s.connect(&endpoint).await?;
                        sock.insert(s)
                    }
                };
                s.send(req.to_vec().into()).await?;
                s.recv().await
            })
            .await
        });
        match r {
            Ok(Ok(m)) => {
                self.sock = sock;
                Ok(m.get(0).map(|f| f.to_vec()).unwrap_or_default())
            }
            Ok(Err(e)) => Err(format!("{}: {e}", self.endpoint)),
            Err(_) => Err(format!("{}: timed out", self.endpoint)),
        }
    }
}

impl Publisher for Radio {
    fn publish(&self, msg: &str) {
        let t_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let cyc = self.cyc.fetch_add(1, Ordering::SeqCst);
        let seq0 = self.seq.load(Ordering::SeqCst);
        let ds = datagrams_msg(&self.group, msg, seq0, cyc, t_ns);
        self.seq.fetch_add(ds.len() as u64, Ordering::SeqCst);
        grow_sndbuf(&self.sock, cycle_bytes(&ds));
        let (n, err) = send_all(
            &ds,
            |d| self.sock.send_to(d, self.dest),
            |t| wait_writable(&self.sock, t),
            SEND_WAIT,
        );
        if let Some(e) = err {
            let total = self.dropped.fetch_add(n, Ordering::SeqCst) + n;
            if self.warn.allow(Instant::now()) {
                tracing::warn!(
                    "{}: UDP send failed ({e}); {total} datagrams dropped so far",
                    self.endpoint
                );
            }
        }
    }
    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }
    fn describe(&self) -> String {
        format!("RADIO {} group {}", self.endpoint, self.group)
    }
}
