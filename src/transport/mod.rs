//! libzmq: a REP socket for requests, updates as UDP datagrams in libzmq's RADIO framing (what a DISH expects).
//! NNG was evaluated and dropped (2026-11-09, docs/bench/transport-comparison.md): UDP multicast stays the primary path.
//! The traits exist so the pull-bridge and the pypower loop can be tested without sockets.

mod zmq;
pub use zmq::{publisher, requester, serve};

/// Answers one request with one reply (a REP socket must always answer).
pub type Handler = Box<dyn Fn(&[u8]) -> Vec<u8> + Send>;

pub trait Publisher: Send {
    /// Send a `tag:value,...` message: one datagram per ~1400 B chunk, each NUL-terminated and sequenced.
    fn publish(&self, msg: &str);
    /// What the startup banner says about the publish side.
    fn describe(&self) -> String;
    /// Datagrams that could not be sent so far (a publisher that cannot fail reports none).
    fn dropped(&self) -> u64 {
        0
    }
}

/// Client side of the request/reply half: one request, one reply, or an error after `timeout_ms`.
pub trait Requester: Send {
    fn call(&mut self, req: &[u8]) -> Result<Vec<u8>, String>;
}

/// Publish endpoint used when `--publish` is not given.
pub const DEFAULT_PUBLISH: &str = "udp://*;239.0.0.1:40000";
