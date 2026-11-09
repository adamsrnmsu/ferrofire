//! NNG REP0 for requests, PUB0 for updates (one message per chunk, no group framing: SUB0 filters by prefix).
use super::{Handler, Publisher, Transport};
use crate::bennu::{chunks_nul, STOP};
use nng::options::{Options, RecvTimeout};
use nng::{Protocol, Socket};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct Nng;

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

    fn publisher(&self, endpoint: &str) -> Result<Box<dyn Publisher>, String> {
        Ok(Box::new(Pub {
            sock: listening(Protocol::Pub0, endpoint)?,
            endpoint: endpoint.into(),
        }))
    }
}

impl Publisher for Pub {
    fn publish(&self, msg: &str) {
        for c in chunks_nul(msg) {
            let _ = self.sock.send(c.as_slice());
        }
    }
    fn describe(&self) -> String {
        format!("PUB {}", self.endpoint)
    }
}
