//! Server-side MMS over the full OSI stack (TPKT, COTP, session, presentation, ACSE, ISO 9506), shared by the IEC 61850
//! module and, next, ICCP/TASE.2. The six layers are `iec61850-rs`'s sans-IO `Association`; this file is the part
//! a protocol should not repeat: the tokio accept loop (a bind or `accept()` failure is logged and survived, one task
//! per association, a stop token that ends them all), a bounded outbound queue per association, and a ticker.
//! A protocol supplies a `Handler`: what to answer to a confirmed request, and which unsolicited PDUs (reports) are due.
//! Violations close the association: a malformed TPKT, COTP or ASN.1 is the `Association`'s `Closed` event; a client
//! that stops reading overflows its queue and is dropped.

use super::eventlog::EventLog;
use crate::hint::lock;
use iec61850_rs::common::{monotonic_now, Instant};
use iec61850_rs::proto::mms::association::{
    Association, AssociationConfig, AssociationEvent, Negotiated,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use iec61850_rs::proto::mms::association::PORT;

pub type AssocId = u64;
/// PDUs ready to go out unasked, and the association each belongs to.
pub type Due = Vec<(AssocId, Vec<u8>)>;

/// A reply that cannot be written within this long ends the association.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// The association checks its own deadlines (handshake, request timeouts) at this pace; the handler's tick runs at `TICK` (1 ms: GOOSE retransmits at T1, a few ms, and a coarser tick lumps frames together).
const POLL: Duration = Duration::from_millis(100);
const TICK: Duration = Duration::from_millis(1);

pub trait Handler: Send + 'static {
    /// May this remote IP (octets) hold an association at all? `false` closes the socket before a slot is taken (ICCP: a peer not
    /// in the bilateral table).
    fn allow(&mut self, _peer: &[u8]) -> bool {
        true
    }
    /// An association was accepted; `peer` is the remote IP's octets (empty if the socket will not say).
    fn opened(&mut self, _id: AssocId, _peer: Vec<u8>) {}
    /// The handshake finished and the peer's PDU size is known.
    fn established(&mut self, _id: AssocId, _n: &Negotiated) {}
    /// A whole confirmed-request PDU. Returns the encoded answer (empty: none) and what became due as a result.
    fn request(&mut self, id: AssocId, now: Instant, invoke_id: i64, pdu: &[u8]) -> (Vec<u8>, Due);
    /// Time passed (`TICK`): timers, frames to drain, reports due.
    fn tick(&mut self, now: Instant) -> Due;
    fn closed(&mut self, _id: AssocId, _now: Instant) {}
}

#[derive(Clone)]
pub struct Config {
    pub association: AssociationConfig,
    /// Associations held at once; beyond this an accepted socket is closed at once.
    pub max_associations: usize,
    /// PDUs queued for one slow client before it is dropped.
    pub outbound_queue: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            association: AssociationConfig::default(),
            max_associations: 16,
            outbound_queue: 256,
        }
    }
}

struct Hub<H> {
    handler: H,
    out: BTreeMap<AssocId, mpsc::Sender<Vec<u8>>>,
    next: AssocId,
}

impl<H> Hub<H> {
    /// Queue each PDU on its association. A full queue drops the sender, which ends that association's task.
    fn dispatch(&mut self, due: Due) {
        for (id, pdu) in due {
            if let Some(tx) = self.out.get(&id) {
                if tx.try_send(pdu).is_err() {
                    self.out.remove(&id);
                }
            }
        }
    }
}

pub struct Mms<H> {
    hub: Arc<Mutex<Hub<H>>>,
    cfg: Config,
}

impl<H> Clone for Mms<H> {
    fn clone(&self) -> Self {
        Mms {
            hub: self.hub.clone(),
            cfg: self.cfg.clone(),
        }
    }
}

impl<H: Handler> Mms<H> {
    pub fn new(handler: H, cfg: Config) -> Mms<H> {
        let hub = Hub {
            handler,
            out: BTreeMap::new(),
            next: 1,
        };
        Mms {
            hub: Arc::new(Mutex::new(hub)),
            cfg,
        }
    }

    /// Run `f` on the handler (a refresh from the datastore, say) and send what it made due.
    pub fn with(&self, f: impl FnOnce(&mut H) -> Due) {
        let mut hub = lock(&self.hub);
        let due = f(&mut hub.handler);
        hub.dispatch(due);
    }

    /// Take a slot for a new association, or `None` when all are in use.
    fn admit(&self, peer: Vec<u8>) -> Option<(AssocId, mpsc::Receiver<Vec<u8>>)> {
        let mut hub = lock(&self.hub);
        if hub.out.len() >= self.cfg.max_associations || !hub.handler.allow(&peer) {
            return None;
        }
        let id = hub.next;
        hub.next += 1;
        let (tx, rx) = mpsc::channel(self.cfg.outbound_queue.max(1));
        hub.out.insert(id, tx);
        hub.handler.opened(id, peer);
        Some((id, rx))
    }

    fn leave(&self, id: AssocId) {
        let mut hub = lock(&self.hub);
        hub.out.remove(&id);
        hub.handler.closed(id, monotonic_now());
    }

    /// Accept associations on `listener` until `stop`.
    pub async fn serve(self, listener: TcpListener, log: Arc<EventLog>, stop: CancellationToken) {
        tokio::spawn(ticker(self.clone(), stop.clone()));
        let mut failing = false;
        loop {
            let accepted =
                tokio::select! { _ = stop.cancelled() => return, r = listener.accept() => r };
            match accepted {
                Ok((stream, addr)) => {
                    failing = false;
                    let peer = match addr.ip() {
                        std::net::IpAddr::V4(a) => a.octets().to_vec(),
                        std::net::IpAddr::V6(a) => a.octets().to_vec(),
                    };
                    // a full house closes the socket (an IED has a few association slots)
                    if let Some((id, rx)) = self.admit(peer) {
                        tokio::spawn(connection(self.clone(), id, stream, rx, stop.clone()));
                    }
                }
                Err(e) => {
                    // out of descriptors or memory: say so once, wait, and keep accepting
                    if !failing {
                        log.log(
                            "error",
                            "mms accept",
                            &format!("accept failed: {e}; retrying"),
                        );
                    }
                    failing = true;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

async fn ticker<H: Handler>(mms: Mms<H>, stop: CancellationToken) {
    let mut every = tokio::time::interval(TICK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! { _ = stop.cancelled() => return, _ = every.tick() => {} }
        mms.with(|h| h.tick(monotonic_now()));
    }
}

/// Hand the association's events to the handler and queue its answers. Returns what became due for any association, and
/// whether this one is over. Shared by the connection task and the fuzz target, which feeds it bytes directly.
pub fn step<H: Handler>(
    h: &mut H,
    id: AssocId,
    assoc: &mut Association,
    now: Instant,
    up: &mut bool,
) -> (Due, bool) {
    let (mut due, mut closed) = (vec![], false);
    while let Some(ev) = assoc.poll_event() {
        match ev {
            AssociationEvent::Request { invoke_id, pdu } => {
                let (answer, more) = h.request(id, now, invoke_id, &pdu);
                due.extend(more);
                if !answer.is_empty() && assoc.send_encoded(&answer).is_err() {
                    closed = true;
                }
            }
            AssociationEvent::Established(n) => {
                *up = true;
                h.established(id, &n)
            }
            AssociationEvent::Closed(_) | AssociationEvent::Refused { .. } => closed = true,
            // responses, reports, rejects and undecodable PDUs from the client are no reason to drop it
            _ => {}
        }
    }
    (due, closed)
}

fn drain_events<H: Handler>(
    mms: &Mms<H>,
    id: AssocId,
    assoc: &mut Association,
    now: Instant,
    up: &mut bool,
) -> bool {
    let mut hub = lock(&mms.hub);
    let (due, closed) = step(&mut hub.handler, id, assoc, now, up);
    hub.dispatch(due);
    closed
}

/// Write what the association has queued. False when the peer is gone or too slow.
async fn flush(assoc: &mut Association, stream: &mut TcpStream) -> bool {
    while let Some(p) = assoc.poll_transmit() {
        let p = p.to_vec();
        if !matches!(
            tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(&p)).await,
            Ok(Ok(()))
        ) {
            return false;
        }
    }
    true
}

/// One association, from the COTP connection request to the release.
async fn connection<H: Handler>(
    mms: Mms<H>,
    id: AssocId,
    mut stream: TcpStream,
    mut rx: mpsc::Receiver<Vec<u8>>,
    stop: CancellationToken,
) {
    let _ = stream.set_nodelay(true);
    let mut assoc = Association::server(mms.cfg.association.clone());
    let mut buf = vec![0u8; 8192];
    let mut poll = tokio::time::interval(POLL);
    // the association arms its handshake deadline only once a COTP request arrives: a socket that stays silent (or stops
    // mid-TPKT) would hold one of the few slots for ever, so the same budget runs from accept to established
    let (mut up, hello) = (
        false,
        Duration::from_millis(mms.cfg.association.connect_timeout_ms),
    );
    let giveup = tokio::time::sleep(if hello.is_zero() {
        Duration::from_secs(86_400 * 365)
    } else {
        hello
    });
    tokio::pin!(giveup);
    loop {
        let now = monotonic_now();
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = poll.tick() => assoc.on_timeout(now),
            _ = &mut giveup, if !up => break,
            r = stream.read(&mut buf) => match r {
                Ok(0) | Err(_) => break,
                Ok(n) => assoc.on_bytes(now, &buf[..n]),
            },
            // None: the queue overflowed and the hub dropped the sender
            pdu = rx.recv() => match pdu {
                Some(pdu) => { let _ = assoc.send_encoded(&pdu); }
                None => break,
            },
        }
        if drain_events(&mms, id, &mut assoc, now, &mut up) || !flush(&mut assoc, &mut stream).await
        {
            break;
        }
    }
    // a final flush lets a release response or an abort reach the peer
    let _ = flush(&mut assoc, &mut stream).await;
    mms.leave(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts what the glue tells it.
    #[derive(Default)]
    struct Log {
        opened: Vec<(AssocId, Vec<u8>)>,
        closed: Vec<AssocId>,
    }

    impl Handler for Log {
        fn opened(&mut self, id: AssocId, peer: Vec<u8>) {
            self.opened.push((id, peer));
        }
        fn request(&mut self, _: AssocId, _: Instant, _: i64, _: &[u8]) -> (Vec<u8>, Due) {
            (vec![], vec![])
        }
        fn tick(&mut self, _: Instant) -> Due {
            vec![]
        }
        fn closed(&mut self, id: AssocId, _: Instant) {
            self.closed.push(id);
        }
    }

    fn mms(max: usize, queue: usize) -> Mms<Log> {
        Mms::new(
            Log::default(),
            Config {
                max_associations: max,
                outbound_queue: queue,
                ..Config::default()
            },
        )
    }

    #[test]
    fn slots_are_numbered_limited_and_given_back() {
        let m = mms(2, 4);
        let (a, _ra) = m.admit(vec![10, 0, 0, 1]).unwrap();
        let (b, _rb) = m.admit(vec![]).unwrap();
        assert_eq!((a, b), (1, 2));
        assert!(m.admit(vec![]).is_none(), "full");
        m.leave(a);
        let (c, _rc) = m.admit(vec![]).unwrap();
        assert_eq!(c, 3, "ids are not reused");
        let hub = lock(&m.hub);
        assert_eq!(hub.handler.opened[0], (1, vec![10, 0, 0, 1]));
        assert_eq!(hub.handler.closed, [1]);
    }

    #[test]
    fn a_handler_can_refuse_a_peer_before_it_takes_a_slot() {
        struct Gate(Vec<u8>);
        impl Handler for Gate {
            fn allow(&mut self, peer: &[u8]) -> bool {
                peer == self.0.as_slice()
            }
            fn request(&mut self, _: AssocId, _: Instant, _: i64, _: &[u8]) -> (Vec<u8>, Due) {
                (vec![], vec![])
            }
            fn tick(&mut self, _: Instant) -> Due {
                vec![]
            }
        }
        let m = Mms::new(Gate(vec![10, 0, 0, 1]), Config::default());
        assert!(m.admit(vec![10, 0, 0, 2]).is_none(), "not on the list");
        assert!(lock(&m.hub).out.is_empty(), "no slot was taken");
        assert_eq!(lock(&m.hub).next, 1, "no id was spent");
        assert!(m.admit(vec![10, 0, 0, 1]).is_some());
    }

    #[test]
    fn a_client_that_stops_reading_is_dropped_when_its_queue_fills() {
        let m = mms(4, 2);
        let (id, mut rx) = m.admit(vec![]).unwrap();
        let (other, _ro) = m.admit(vec![]).unwrap();
        m.with(|_| {
            vec![
                (id, vec![1]),
                (id, vec![2]),
                (other, vec![9]),
                (77, vec![0]),
            ]
        });
        assert_eq!(rx.try_recv().unwrap(), [1]);
        assert_eq!(rx.try_recv().unwrap(), [2]);
        assert!(
            lock(&m.hub).out.contains_key(&id),
            "two fit in a queue of two"
        );
        m.with(|_| vec![(id, vec![3]), (id, vec![4]), (id, vec![5])]);
        assert!(
            !lock(&m.hub).out.contains_key(&id),
            "the third did not fit: the sender is gone"
        );
        assert!(
            lock(&m.hub).out.contains_key(&other),
            "others are untouched"
        );
    }

    #[test]
    fn a_queue_of_zero_still_holds_one() {
        let m = mms(1, 0);
        let (id, mut rx) = m.admit(vec![]).unwrap();
        m.with(|_| vec![(id, vec![1])]);
        assert_eq!(rx.try_recv().unwrap(), [1]);
    }

    #[test]
    fn defaults_are_an_ieds_few_slots() {
        let c = Config::default();
        assert_eq!((c.max_associations, c.outbound_queue), (16, 256));
    }

    #[tokio::test]
    async fn a_socket_that_never_completes_the_handshake_is_closed() {
        let mut cfg = Config::default();
        cfg.association.connect_timeout_ms = 300;
        let m = Mms::new(Log::default(), cfg);
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let log = Arc::new(EventLog::to("mms", Box::new(std::io::sink())));
        let stop = CancellationToken::new();
        tokio::spawn(m.clone().serve(l, log, stop.clone()));
        let mut silent = TcpStream::connect(addr).await.unwrap();
        let mut half = TcpStream::connect(addr).await.unwrap();
        half.write_all(&[3, 0, 0]).await.unwrap(); // a TPKT header that never finishes
        for s in [&mut silent, &mut half] {
            let mut b = [0u8; 8];
            let r = tokio::time::timeout(Duration::from_secs(3), s.read(&mut b)).await;
            assert!(
                matches!(r, Ok(Ok(0)) | Ok(Err(_))),
                "closed, not held: {r:?}"
            );
        }
        assert_eq!(
            lock(&m.hub).handler.closed.len(),
            2,
            "both slots given back"
        );
        stop.cancel();
    }
}
