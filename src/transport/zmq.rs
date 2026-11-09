//! libzmq REP for requests; updates leave as UDP datagrams in libzmq's RADIO framing (what a DISH expects).
use super::{Handler, Publisher, Transport};
use crate::bennu::{datagrams_msg, group_for, parse_udp, udp_sender, STOP};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;

pub struct Zmq {
    ctx: zmq::Context,
}

impl Zmq {
    pub fn new() -> Zmq {
        Zmq {
            ctx: zmq::Context::new(),
        }
    }
}

struct Radio {
    sock: UdpSocket,
    dest: SocketAddr,
    endpoint: String,
    group: String,
}

impl Transport for Zmq {
    fn name(&self) -> &'static str {
        "zmq"
    }
    fn default_publish(&self) -> &'static str {
        "udp://*;239.0.0.1:40000"
    }

    /// Bind a REP socket on `endpoint` and answer each request with `handler` until `STOP` (polling, so it is noticed).
    fn serve(
        &self,
        endpoint: &str,
        handler: Handler,
    ) -> Result<std::thread::JoinHandle<()>, String> {
        let rep = self.ctx.socket(zmq::REP).map_err(|e| e.to_string())?;
        rep.bind(endpoint)
            .map_err(|e| format!("bind {endpoint}: {e}"))?;
        Ok(std::thread::spawn(move || {
            while !STOP.load(Ordering::SeqCst) {
                if !matches!(rep.poll(zmq::POLLIN, 100), Ok(n) if n > 0) {
                    continue;
                }
                if let Ok(req) = rep.recv_bytes(0) {
                    let _ = rep.send(handler(&req), 0);
                }
            }
        }))
    }

    fn publisher(&self, endpoint: &str) -> Result<Box<dyn Publisher>, String> {
        let (iface, dest) = parse_udp(endpoint)?;
        Ok(Box::new(Radio {
            sock: udp_sender(iface.as_deref(), dest)?,
            dest,
            endpoint: endpoint.into(),
            group: group_for(endpoint),
        }))
    }
}

impl Publisher for Radio {
    fn publish(&self, msg: &str) {
        for d in datagrams_msg(&self.group, msg) {
            let _ = self.sock.send_to(&d, self.dest);
        }
    }
    fn describe(&self) -> String {
        format!("RADIO {} group {}", self.endpoint, self.group)
    }
}
