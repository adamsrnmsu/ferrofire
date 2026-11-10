use super::{Handler, Publisher, Requester};
use crate::bennu::{datagrams_msg, group_for, parse_udp, udp_sender, with_nul, STOP};
use crate::hint::{show, RateLimit};
use std::net::{SocketAddr, UdpSocket};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Wait this long for a request or a reply send before looking at `STOP` again / giving up on a vanished peer.
const IO_TIMEOUT_MS: i32 = 1000;
/// At most one "UDP send failed" line per this long.
const SEND_WARN_EVERY: Duration = Duration::from_secs(10);

/// Bind a REP socket on `endpoint` and answer each request with `handler` until `STOP` (polling, so it is noticed).
/// A REP that skips a reply wedges its client, so every request gets exactly one: a handler that panics or a request
/// in several frames is answered with an `ERR`, and the loop goes on.
pub fn serve(endpoint: &str, handler: Handler) -> Result<std::thread::JoinHandle<()>, String> {
    let rep = ::zmq::Context::new()
        .socket(::zmq::REP)
        .map_err(|e| e.to_string())?;
    rep.set_rcvtimeo(IO_TIMEOUT_MS).map_err(|e| e.to_string())?;
    rep.set_sndtimeo(IO_TIMEOUT_MS).map_err(|e| e.to_string())?;
    rep.bind(endpoint)
        .map_err(|e| format!("bind {endpoint}: {e}"))?;
    Ok(std::thread::spawn(move || {
        while !STOP.load(Ordering::SeqCst) {
            if !matches!(rep.poll(::zmq::POLLIN, 100), Ok(n) if n > 0) {
                continue;
            }
            if let Ok(parts) = rep.recv_multipart(0) {
                let _ = rep.send(answer(&handler, &parts), 0);
            }
        }
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
pub fn requester(endpoint: &str, timeout_ms: i32) -> Result<Box<dyn Requester>, String> {
    let ctx = ::zmq::Context::new();
    Ok(Box::new(Req {
        sock: Req::connect(&ctx, endpoint, timeout_ms)?,
        ctx,
        endpoint: endpoint.into(),
        timeout_ms,
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

/// A zmq REQ that timed out is wedged (it must not send twice), so a failed call drops it and connects a fresh one.
struct Req {
    ctx: ::zmq::Context,
    endpoint: String,
    timeout_ms: i32,
    sock: ::zmq::Socket,
}

impl Req {
    fn connect(
        ctx: &::zmq::Context,
        endpoint: &str,
        timeout_ms: i32,
    ) -> Result<::zmq::Socket, String> {
        let s = ctx.socket(::zmq::REQ).map_err(|e| e.to_string())?;
        s.set_rcvtimeo(timeout_ms).map_err(|e| e.to_string())?;
        s.set_sndtimeo(timeout_ms).map_err(|e| e.to_string())?;
        s.set_linger(0).map_err(|e| e.to_string())?;
        s.connect(endpoint)
            .map_err(|e| format!("connect {endpoint}: {e}"))?;
        Ok(s)
    }
}

impl Requester for Req {
    fn call(&mut self, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = self
            .sock
            .send(req, 0)
            .and_then(|_| self.sock.recv_bytes(0))
            .map_err(|e| e.to_string());
        if r.is_err() {
            self.sock = Req::connect(&self.ctx, &self.endpoint, self.timeout_ms)?;
        }
        r
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
        for d in ds {
            if let Err(e) = self.sock.send_to(&d, self.dest) {
                let n = self.dropped.fetch_add(1, Ordering::SeqCst) + 1;
                if self.warn.allow(Instant::now()) {
                    tracing::warn!(
                        "{}: UDP send failed ({e}); {n} datagrams dropped so far",
                        self.endpoint
                    );
                }
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
