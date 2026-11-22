//! Transport `bennu`: a module that speaks a protocol the bennu way, with no ffmod/1. The device serves a device-level command
//! interface (every tag) on `<command-interface>` and multicasts what changed each cycle to `<publish-group>` as datagram v1; the
//! optional `<command>` is kept running as for a socket module, with its stdin null.

use super::session::moved;
use super::External;
use crate::bennu::{datagrams_msg, group_for, parse_udp, send_all, udp_sender};
use crate::fd::cmdif::{self, Points};
use crate::fd::config::{require, ConfigError};
use crate::fd::store::{Store, WriteError};
use crate::model::Value;
use roxmltree::Node;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

/// The `<publish-group>` of `node`, which must be a `udp://` endpoint; the module's `<command-interface>` must be there too.
pub(super) fn checked(node: Node) -> Result<String, ConfigError> {
    require(node, "command-interface")?;
    let group = require(node, "publish-group")?;
    parse_udp(&group).map_err(|e| ConfigError(format!("<publish-group>: {e}")))?;
    Ok(group)
}

/// Every device tag, for the command interface.
pub struct StorePoints(pub Store);

impl Points for StorePoints {
    fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.0.snapshot().into_iter().map(|(t, _)| t).collect();
        names.sort();
        names
    }

    async fn read(&self, tag: &str) -> Result<Value, String> {
        self.0.get(tag).ok_or_else(|| "no such tag".into())
    }

    async fn write(&self, tag: &str, v: Value) -> Result<(), String> {
        self.0.write(tag, v).map_err(|e| match e {
            WriteError::UnknownTag => "no such tag".into(),
            WriteError::WrongKind => {
                let kind = self.0.kind(tag).map(|k| format!("{k:?}").to_lowercase());
                format!("the tag is {}", kind.unwrap_or_default())
            }
        })
    }
}

impl External {
    /// Serve the command interface, publish the changes and keep the `<command>` running, until `stop`.
    pub(super) async fn run_bennu(&self, store: Store, stop: CancellationToken) {
        let ep = self.spec.cmdif.clone().unwrap_or_default();
        // nobody waits for a bennu command's session: the sender only has to outlive `supervise`
        let up = watch::channel(false).0;
        tokio::join!(
            cmdif::serve(&ep, StorePoints(store.clone()), stop.clone()),
            publish(store, self.publish.clone(), self.spec.cycle, stop.clone()),
            self.supervise(&stop, &up),
        );
    }
}

/// How long one cycle's datagrams may wait for the socket.
const SEND_BUDGET: Duration = Duration::from_millis(100);

/// `tag:value,` for each point, as the stock providers write them.
fn message(points: &[&(String, Value)]) -> String {
    let one = |(t, v): &&(String, Value)| match v {
        Value::Binary(b) => format!("{t}:{b},"),
        Value::Analog(x) => format!("{t}:{},", cmdif::float(*x)),
    };
    points.iter().map(one).collect()
}

/// A non-blocking socket for `group`, and where it sends.
fn open(group: &str) -> Result<(UdpSocket, SocketAddr), String> {
    let (iface, dest) = parse_udp(group)?;
    let s = udp_sender(iface.as_deref(), dest)?;
    s.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok((s, dest))
}

/// The datagrams for `points`, numbered from `seq` as cycle `cyc`.
fn datagrams(group: &str, points: &[&(String, Value)], seq: u64, cyc: u64) -> Vec<Vec<u8>> {
    let t = SystemTime::now().duration_since(UNIX_EPOCH);
    let t = t.map_or(0, |d| d.as_nanos() as u64);
    datagrams_msg(&group_for(group), &message(points), seq, cyc, t)
}

/// Each cycle send what changed in `store` to `group` as datagram v1 (the first cycle sends everything), until `stop`.
async fn publish(store: Store, group: String, cycle: Duration, stop: CancellationToken) {
    let (sock, dest) = match open(&group) {
        Ok(x) => x,
        Err(e) => return tracing::error!("publish-group {group}: {e}"),
    };
    let (mut last, mut seq, mut cyc) = (vec![], 0, 0);
    let mut tick = interval(cycle);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! { _ = stop.cancelled() => return, _ = tick.tick() => {} }
        let now = store.snapshot();
        let changed: Vec<_> = match last.is_empty() {
            true => now.iter().collect(),
            false => moved(&now, &last).collect(),
        };
        if !changed.is_empty() {
            let ds = datagrams(&group, &changed, seq, cyc);
            (seq, cyc) = (seq + ds.len() as u64, cyc + 1);
            let wait = |d| crate::sys::wait_writable(&sock, d);
            let (dropped, err) = send_all(&ds, |d| sock.send_to(d, dest), wait, SEND_BUDGET);
            if let Some(e) = err {
                tracing::warn!("publish-group {group}: {dropped} datagrams not sent: {e}");
            }
        }
        last = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bennu::unframe;
    use crate::datagram::Receiver;
    use crate::fd::store::Layout;
    use crate::model::Kind;
    use std::net::UdpSocket;

    fn store(points: &[(&str, Kind)]) -> Store {
        Store::new(Layout {
            points: points
                .iter()
                .map(|(n, k)| (n.to_string(), *k, None))
                .collect(),
            external: points
                .iter()
                .map(|(n, _)| (n.to_string(), n.to_string()))
                .collect(),
            internal: vec![],
        })
    }

    #[tokio::test]
    async fn query_lists_every_device_tag() {
        let p = StorePoints(store(&[
            ("b", Kind::Binary),
            ("a", Kind::Analog),
            ("c", Kind::Analog),
        ]));
        assert_eq!(cmdif::reply("QUERY=", &p).await, b"ACK=a,b,c,\0");
    }

    #[tokio::test]
    async fn write_goes_through_the_store() {
        let s = store(&[("a", Kind::Analog), ("b", Kind::Binary)]);
        let p = StorePoints(s.clone());
        assert_eq!(
            cmdif::reply("WRITE=a:2.5", &p).await,
            b"ACK=Wrote tag a -- 2.5\0"
        );
        assert_eq!(s.queued("a"), Some(Value::Analog(2.5)));
        let bad = cmdif::reply("WRITE=b:1", &p).await;
        assert_eq!(bad, b"ERR=Failed writing tag 'b': the tag is binary\0");
        assert_eq!(cmdif::reply("READ=a", &p).await, b"ACK=a:0.000000\0");
    }

    /// Run `publish` at `udp://127.0.0.1:PORT`, with a listener on it; the datagrams it gets, once `change` has run.
    async fn published(s: &Store, change: impl FnOnce(&Store)) -> (String, Vec<Vec<u8>>) {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let group = format!("udp://127.0.0.1:{}", rx.local_addr().unwrap().port());
        let stop = CancellationToken::new();
        let task = tokio::spawn(publish(
            s.clone(),
            group.clone(),
            Duration::from_millis(20),
            stop.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        // the first cycle sent everything; what comes now is the change
        while rx.recv(&mut [0; 2000]).is_ok() {}
        change(s);
        let mut got = vec![];
        let mut buf = [0u8; 2000];
        while let Ok(n) = rx.recv(&mut buf) {
            got.push(buf[..n].to_vec());
        }
        stop.cancel();
        task.await.unwrap();
        (group, got)
    }

    // the listener blocks its thread: the publisher needs another
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn changes_are_multicast_as_datagram_v1() {
        let s = store(&[
            ("a", Kind::Analog),
            ("b", Kind::Binary),
            ("c", Kind::Analog),
        ]);
        let (group, got) = published(&s, |s| {
            s.publish("a", 1.5, 1);
            s.publish("b", 1.0, 1);
        })
        .await;
        assert_eq!(got.len(), 1, "{got:?}");
        let (g, body) = unframe(&got[0]).unwrap();
        assert_eq!(g, group_for(&group));
        assert!(String::from_utf8_lossy(body).starts_with("a:1.500000,b:true,"));
        let mut rx = Receiver::new(g, ["a", "b", "c"]);
        assert_eq!(
            rx.accept(&got[0]),
            Ok(vec![("a".into(), 1.5), ("b".into(), 1.0)])
        );
    }

    // the listener blocks its thread: the publisher needs another
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_large_change_set_is_chunked() {
        let pts: Vec<_> = (0..400)
            .map(|i| (format!("tag-{i:04}"), Kind::Analog))
            .collect();
        let s = store(
            &pts.iter()
                .map(|(n, k)| (n.as_str(), *k))
                .collect::<Vec<_>>(),
        );
        let (_, got) = published(&s, |s| {
            for (n, _) in &pts {
                s.publish(n, 1.0, 1);
            }
        })
        .await;
        assert!(got.len() > 1);
        assert!(got.iter().all(|d| d.len() <= 1400));
        let mut rx = Receiver::new(
            unframe(&got[0]).unwrap().0,
            pts.iter().map(|(n, _)| n.as_str()),
        );
        let n: usize = got.iter().map(|d| rx.accept(d).unwrap().len()).sum();
        assert_eq!(n, 400);
    }
}
