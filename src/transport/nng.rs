//! NNG REP0 for requests, PUB0 for updates: one message per publish cycle (TCP segments it; no group framing, SUB0 filters by prefix).
//! PUB0 drops, never blocks, when a subscriber pipe's send queue is full, so that queue (`nng_sendbuf`) is sized above the burst.
use super::{Handler, Publisher, Requester, Transport};
use crate::bennu::STOP;
use nng::options::{Options, RecvTimeout, SendBufferSize};
use nng::{Protocol, Socket};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct Nng {
    pub sendbuf: i32,
}

fn listening(p: Protocol, endpoint: &str) -> Result<Socket, String> {
    let s = Socket::new(p).map_err(|e| e.to_string())?;
    s.set_opt::<RecvTimeout>(Some(Duration::from_millis(100)))
        .map_err(|e| e.to_string())?;
    s.listen(endpoint)
        .map_err(|e| format!("bind {endpoint}: {e}"))?;
    Ok(s)
}

struct Pub {
    sock: Socket,
    endpoint: String,
    sendbuf: i32,
}

impl Transport for Nng {
    fn name(&self) -> &'static str {
        "nng"
    }
    fn default_publish(&self) -> &'static str {
        "tcp://0.0.0.0:5556"
    }

    /// REP0 on `endpoint`; the 100 ms receive timeout is how the loop notices `STOP`.
    fn serve(
        &self,
        endpoint: &str,
        handler: Handler,
    ) -> Result<std::thread::JoinHandle<()>, String> {
        let rep = listening(Protocol::Rep0, endpoint)?;
        Ok(std::thread::spawn(move || {
            while !STOP.load(Ordering::SeqCst) {
                if let Ok(req) = rep.recv() {
                    let _ = rep.send(handler(&req).as_slice());
                }
            }
        }))
    }

    fn requester(&self, endpoint: &str, timeout_ms: i32) -> Result<Box<dyn Requester>, String> {
        let s = Socket::new(Protocol::Req0).map_err(|e| e.to_string())?;
        s.set_opt::<RecvTimeout>(Some(Duration::from_millis(timeout_ms as u64)))
            .map_err(|e| e.to_string())?;
        s.dial_async(endpoint)
            .map_err(|e| format!("dial {endpoint}: {e}"))?; // the provider may still be starting
        Ok(Box::new(Req(s)))
    }

    fn publisher(&self, endpoint: &str) -> Result<Box<dyn Publisher>, String> {
        let sock = Socket::new(Protocol::Pub0).map_err(|e| e.to_string())?;
        sock.set_opt::<SendBufferSize>(self.sendbuf)
            .map_err(|e| e.to_string())?;
        sock.listen(endpoint)
            .map_err(|e| format!("bind {endpoint}: {e}"))?;
        Ok(Box::new(Pub {
            sock,
            endpoint: endpoint.into(),
            sendbuf: self.sendbuf,
        }))
    }
}

/// A timed-out NNG REQ0 may simply send again (a new send cancels the old one).
struct Req(Socket);

impl Requester for Req {
    fn call(&mut self, req: &[u8]) -> Result<Vec<u8>, String> {
        self.0.send(req).map_err(|e| e.1.to_string())?;
        Ok(self.0.recv().map_err(|e| e.to_string())?.to_vec())
    }
}

impl Publisher for Pub {
    fn publish(&self, msg: &str) {
        let mut b = Vec::with_capacity(msg.len() + 1);
        b.extend_from_slice(msg.as_bytes());
        b.push(0);
        let _ = self.sock.send(b.as_slice());
    }
    fn describe(&self) -> String {
        format!("PUB {} (sendbuf {})", self.endpoint, self.sendbuf)
    }
}
