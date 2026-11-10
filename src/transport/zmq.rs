use super::{Handler, Publisher, Requester};
use crate::bennu::{datagrams_msg, group_for, parse_udp, udp_sender, STOP};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Bind a REP socket on `endpoint` and answer each request with `handler` until `STOP` (polling, so it is noticed).
pub fn serve(endpoint: &str, handler: Handler) -> Result<std::thread::JoinHandle<()>, String> {
    let rep = ::zmq::Context::new()
        .socket(::zmq::REP)
        .map_err(|e| e.to_string())?;
    rep.bind(endpoint)
        .map_err(|e| format!("bind {endpoint}: {e}"))?;
    Ok(std::thread::spawn(move || {
        while !STOP.load(Ordering::SeqCst) {
            if !matches!(rep.poll(::zmq::POLLIN, 100), Ok(n) if n > 0) {
                continue;
            }
            if let Ok(req) = rep.recv_bytes(0) {
                let _ = rep.send(handler(&req), 0);
            }
        }
    }))
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
    Ok(Box::new(Radio {
        sock: udp_sender(iface.as_deref(), dest)?,
        dest,
        endpoint: endpoint.into(),
        group: group_for(endpoint),
        seq: AtomicU64::new(0),
        cyc: AtomicU64::new(0),
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
            let _ = self.sock.send_to(&d, self.dest);
        }
    }
    fn describe(&self) -> String {
        format!("RADIO {} group {}", self.endpoint, self.group)
    }
}
