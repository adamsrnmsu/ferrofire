//! `<iec60870-5-104-client>` at run time. `bennu` mode: per connection the `client::Master` inside a socket loop that connects
//! once, as stock does, and stock's command interface. `fep` mode: per connection a `client::Poller` inside a socket loop that
//! reconnects with a backoff, and the command interface on the tags.

use super::super::super::cmdif::{self, Points};
use super::super::super::net;
use super::super::super::store::Store;
use super::super::client::{Mode, TagPoints};
use super::apci::{Action, Link, Params, Role};
use super::asdu;
use super::client::{route, Master, Poller};
use super::{Client, QUEUE, TICK, WRITE_TIMEOUT};
use crate::hint::lock;
use crate::model::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const CLIENT: &str = "iec60870-5-104-client";
/// lib60870's connect timeout t0 (`LIB/cs104/cs104_connection.c:44`).
const T0: Duration = Duration::from_secs(10);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);

pub(super) async fn run(client: Client, store: Store, stop: CancellationToken) {
    match client.mode {
        Mode::Bennu => bennu(client, store, stop).await,
        Mode::Fep => fep(client, store, stop).await,
    }
}

/// `bennu` mode: one task per connection, so a dead server stalls nothing else, and stock's command interface.
async fn bennu(client: Client, store: Store, stop: CancellationToken) {
    let mut by_tag = BTreeMap::new();
    for c in &client.conns {
        let (tx, rx) = mpsc::channel(QUEUE);
        let link = Arc::new(Shared {
            master: Mutex::new(Master::new(c, client.mode, client.strict)),
            cache: Mutex::new(
                (c.points.iter())
                    .map(|p| (p.tag.clone(), crate::fd::store::zero(p.kind)))
                    .collect(),
            ),
            store: store.clone(),
            tx,
        });
        by_tag.extend(c.points.iter().map(|p| (p.tag.clone(), link.clone())));
        tokio::spawn(master(c.endpoint.clone(), link, rx, stop.clone()));
    }
    match &client.cmdif {
        Some(ep) => cmdif::serve(ep, Bennu(by_tag), stop).await,
        None => stop.cancelled().await,
    }
}

/// What a connection's session and the command interface share.
struct Shared {
    master: Mutex<Master>,
    /// Stock's register values: what a READ answers. The device-local tags follow them.
    cache: Mutex<BTreeMap<String, Value>>,
    store: Store,
    /// Encoded commands for the session; nobody takes them once it has ended.
    tx: mpsc::Sender<Vec<u8>>,
}

impl Shared {
    fn set(&self, updates: impl IntoIterator<Item = (String, Value)>) {
        let mut cache = lock(&self.cache);
        for (tag, v) in updates {
            let _ = self.store.write(&tag, v);
            cache.insert(tag, v);
        }
    }
}

/// The command interface of a `bennu` mode client: a READ answers from the cache; a WRITE sets the cache by the value's kind
/// and hands the command to the session, and is ACKed whatever becomes of it, as stock's (STOCK.md, "Client").
struct Bennu(BTreeMap<String, Arc<Shared>>);

impl Points for Bennu {
    fn names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    async fn read(&self, tag: &str) -> Result<Value, String> {
        let unknown = || "readRegisterByTag(): Unable to find tag".to_string();
        let link = self.0.get(tag).ok_or_else(unknown)?;
        lock(&link.cache).get(tag).copied().ok_or_else(unknown)
    }

    async fn write(&self, tag: &str, v: Value) -> Result<(), String> {
        let link = self.0.get(tag).ok_or("writeBinary(): Unable to find tag")?;
        let (cmd, local) = {
            let m = lock(&link.master);
            (m.command(tag, v)?, m.local(tag, v))
        };
        // the session gone (stock sends nothing then), or `QUEUE` commands waiting for it: dropped, and ACKed
        if let Err(mpsc::error::TrySendError::Full(_)) = link.tx.try_send(cmd.encode()) {
            tracing::warn!(
                "{CLIENT}: {QUEUE} commands are waiting; the one for `{tag}` is dropped"
            );
        }
        link.set(local);
        Ok(())
    }
}

/// The ASDUs the link delivered go to the master and its updates to the cache; what it sends is returned. `None`: the link
/// closed (`client::route`).
fn take(link: &Shared, acts: Vec<Action>, endpoint: &str) -> Option<Vec<u8>> {
    let deliver = |a: &asdu::Asdu| link.set(lock(&link.master).on_asdu(a));
    route(acts, deliver)
        .map_err(|why| tracing::error!("{CLIENT}: {endpoint}: closing the connection: {why}"))
        .ok()
}

/// Write `out`; `false` when the peer has not taken it within `WRITE_TIMEOUT` or the socket failed.
async fn write(s: &mut net::Conn, out: &[u8]) -> bool {
    out.is_empty()
        || matches!(
            tokio::time::timeout(WRITE_TIMEOUT, s.write_all(out)).await,
            Ok(Ok(()))
        )
}

/// One connection: connect once, then `session`. Never again after that, and nothing at all if the first connect fails, as stock
/// (`MOD/ClientConnection.cpp:44-56`).
async fn master(
    endpoint: String,
    link: Arc<Shared>,
    rx: mpsc::Receiver<Vec<u8>>,
    stop: CancellationToken,
) {
    let conn = tokio::select! {
        _ = stop.cancelled() => return,
        c = tokio::time::timeout(T0, net::connect(&endpoint)) => c,
    };
    let Ok(Ok(mut s)) = conn else {
        tracing::error!(
            "{CLIENT}: Error -- Could not start IEC60870-5-104-CLIENT -- RTU Connection: {endpoint}"
        );
        tracing::error!("{CLIENT}: Exiting.");
        return;
    };
    s.nodelay();
    s.keepalive();
    let mut apci = Link::new(Params::STANDARD, Role::Client);
    // a peer that stopped reading takes no S-frame either
    if session(&mut s, &mut apci, &link, rx, &stop, &endpoint).await {
        if let Some(ack) = apci.confirm() {
            write(&mut s, &ack).await;
        }
    }
}

/// STARTDT and the GI back to back, then the server's data and our commands until the server goes, the link closes or the
/// device stops. `false`: a write stalled.
async fn session(
    s: &mut net::Conn,
    apci: &mut Link,
    link: &Shared,
    mut rx: mpsc::Receiver<Vec<u8>>,
    stop: &CancellationToken,
    endpoint: &str,
) -> bool {
    // a WRITE made while the connect was under way is not sent: stock's cannot send before it is connected (still ACKed)
    while rx.try_recv().is_ok() {}
    let now = Instant::now();
    let mut acts = apci.start(now);
    for gi in lock(&link.master).on_start() {
        acts.extend(apci.send(gi.encode(), now).unwrap_or_default());
    }
    let (mut tick, mut buf) = (tokio::time::interval(TICK), [0u8; 4096]);
    loop {
        let Some(out) = take(link, acts, endpoint) else {
            return true;
        };
        if !write(s, &out).await {
            return false;
        }
        let now = Instant::now();
        acts = tokio::select! {
            _ = stop.cancelled() => return true,
            _ = tick.tick() => apci.tick(now),
            Some(cmd) = rx.recv() => apci.send(cmd, now).unwrap_or_else(|| {
                // lib60870 refuses an ASDU while k I-frames are unacknowledged, and stock's WRITE was ACKed all the same
                tracing::warn!("{CLIENT}: {endpoint}: the k window is full; a command is dropped");
                vec![]
            }),
            r = s.read(&mut buf) => match r {
                Ok(0) | Err(_) => return true,
                Ok(n) => apci.on_bytes(&buf[..n], now),
            },
        };
    }
}

/// `fep` mode: one `Poller` task per connection, and the command interface on the tags.
async fn fep(client: Client, store: Store, stop: CancellationToken) {
    let mut outputs = vec![];
    for c in &client.conns {
        outputs.extend(c.points.iter().map(|p| (p.tag.clone(), p.output)));
        let p = Poller::new(c, store.clone(), client.cycle, Instant::now());
        tokio::spawn(poller(p, stop.clone()));
    }
    match &client.cmdif {
        Some(ep) => cmdif::serve(ep, TagPoints::new(store, outputs), stop).await,
        None => stop.cancelled().await,
    }
}

/// Connect, poll until the connection is lost, and again: at once after a connection whose server ended a GI, else 1, 2, 4 ...
/// 60 s apart (as the DNP3 FEP).
async fn poller(mut p: Poller, stop: CancellationToken) {
    let (mut wait, mut pause) = (Duration::ZERO, RETRY_MIN);
    loop {
        tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(wait) => {} }
        // a refused attempt after a good connection backs off too
        p.answered = false;
        if let Ok(Ok(mut s)) = tokio::time::timeout(T0, net::connect(&p.name)).await {
            s.nodelay();
            s.keepalive();
            let why = tokio::select! { _ = stop.cancelled() => return, why = polling(&mut s, &mut p) => why };
            tracing::warn!("{CLIENT}: {}: connection lost: {why}", p.name);
            p.lost();
        }
        (wait, pause) = match p.answered {
            true => (Duration::ZERO, RETRY_MIN),
            false => (pause, (pause * 2).min(RETRY_MAX)),
        };
    }
}

/// One connection of a `Poller`, until it is lost: why.
async fn polling(s: &mut net::Conn, p: &mut Poller) -> String {
    let mut out = Ok(p.connect(Instant::now()));
    let (mut tick, mut buf) = (tokio::time::interval(p.period(TICK)), [0u8; 4096]);
    loop {
        match out {
            Ok(b) if write(s, &b).await => {}
            Ok(_) => return "a write stalled".into(),
            Err(why) => return why,
        }
        out = tokio::select! {
            _ = tick.tick() => p.tick(Instant::now()),
            r = s.read(&mut buf) => match r {
                Ok(0) | Err(_) => Err("the server closed it".into()),
                Ok(n) => p.on_bytes(&buf[..n], Instant::now()),
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::super::client::Conn;
    use super::super::server::Point;
    use super::*;
    use crate::model::Kind;

    /// What the link delivers: an ASDU updates the cache and the tag; one of a type stock does not handle is ignored; one too
    /// short for its header, or the link closing, ends the session.
    #[test]
    fn take_applies_ignores_or_ends() {
        let conn = Conn {
            endpoint: "tcp://127.0.0.1:1".into(),
            points: vec![Point {
                addr: 2,
                tag: "ai".into(),
                kind: Kind::Analog,
                output: false,
            }],
            fep: None,
        };
        let (tx, _rx) = mpsc::channel(1);
        let link = Shared {
            master: Mutex::new(Master::new(&conn, Mode::Bennu, false)),
            cache: Mutex::default(),
            store: Store::new(Default::default()),
            tx,
        };
        let float = vec![13, 1, 1, 0, 1, 0, 2, 0, 0, 0, 0, 0x2a, 0x42, 0];
        let acts = vec![
            Action::Deliver(float),
            Action::Deliver(vec![58, 1, 3, 0, 1, 0]),
            Action::Send(vec![1, 2]),
        ];
        assert_eq!(take(&link, acts, "x"), Some(vec![1, 2]));
        assert_eq!(lock(&link.cache).get("ai"), Some(&Value::Analog(42.5)));
        assert_eq!(take(&link, vec![Action::Deliver(vec![13, 1])], "x"), None);
        assert_eq!(take(&link, vec![Action::Close("t1".into())], "x"), None);
    }
}
