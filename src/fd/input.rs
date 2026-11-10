//! The DISH-compatible receiver (SPEC 3.3): one UDP socket per `<input>`, joined to the multicast group, RADIO-framed
//! datagrams in. `datagram::Receiver` does the parsing and duplicate rejection; whatever it accepts is published to the store.

use super::config::InputSpec;
use super::store::Store;
use crate::bennu::{group_for, parse_udp};
use crate::datagram::Receiver;
use socket2::{Domain, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// How long a read waits before the stop flag is looked at again.
const POLL: Duration = Duration::from_millis(200);

/// A socket for `udp://[iface;]host:port`: address reuse so several devices (or inputs) share a port; a multicast group is joined.
pub fn bind(endpoint: &str) -> Result<UdpSocket, String> {
    let (iface, addr) = parse_udp(endpoint)?;
    let err = |e: std::io::Error| format!("{endpoint}: {e}");
    let s = Socket::new(Domain::IPV4, Type::DGRAM, None).map_err(err)?;
    s.set_reuse_address(true).map_err(err)?;
    s.set_reuse_port(true).map_err(err)?;
    let SocketAddr::V4(v4) = addr else {
        return Err(format!("{endpoint}: only IPv4 is supported"));
    };
    let to_bind = if v4.ip().is_multicast() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, v4.port()))
    } else {
        addr
    };
    s.bind(&to_bind.into()).map_err(err)?;
    if v4.ip().is_multicast() {
        let nic = match iface {
            Some(i) => i
                .parse()
                .map_err(|_| format!("interface `{i}`: only IPv4 addresses are supported"))?,
            None => Ipv4Addr::UNSPECIFIED,
        };
        s.join_multicast_v4(v4.ip(), &nic).map_err(err)?;
    }
    let sock: UdpSocket = s.into();
    sock.set_read_timeout(Some(POLL)).map_err(err)?;
    Ok(sock)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Publish what one datagram carries; a datagram the receiver rejects changes nothing.
pub fn apply(rx: &mut Receiver, store: &Store, datagram: &[u8], now: u64) {
    for (point, x) in rx.accept(datagram).unwrap_or_default() {
        store.publish(&point, x, now);
    }
}

/// Receive until `stop`. Bad datagrams never stop it.
pub fn serve(sock: &UdpSocket, mut rx: Receiver, store: &Store, stop: &CancellationToken) {
    let mut buf = vec![0u8; 65535];
    while !stop.is_cancelled() {
        match sock.recv(&mut buf) {
            Ok(n) => apply(&mut rx, store, &buf[..n], unix_now()),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => std::thread::sleep(POLL), // e.g. ICMP unreachable on a unicast feed; do not spin
        }
    }
}

/// Bind and start the receiver thread for one `<input>`.
pub fn spawn(
    spec: &InputSpec,
    store: Store,
    stop: CancellationToken,
) -> Result<std::thread::JoinHandle<()>, String> {
    let sock = bind(&spec.endpoint)?;
    let rx = Receiver::new(
        &group_for(&spec.endpoint),
        spec.points.iter().map(String::as_str),
    );
    Ok(std::thread::spawn(move || serve(&sock, rx, &store, &stop)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bennu::frame;
    use crate::fd::store::Layout;
    use crate::model::Kind;
    use std::time::Instant;

    fn store() -> Store {
        Store::new(Layout {
            points: vec![
                ("a".into(), Kind::Analog, None),
                ("b".into(), Kind::Binary, None),
            ],
            external: vec![("ta".into(), "a".into()), ("tb".into(), "b".into())],
            internal: vec![],
        })
    }

    fn rx() -> Receiver {
        Receiver::new("g", ["a", "b"])
    }

    fn free_port() -> u16 {
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn apply_publishes_known_points_and_ignores_the_rest() {
        let (mut rx, s) = (rx(), store());
        apply(&mut rx, &s, &frame("g", b"a:2.5,b:true,c:1,\0"), 77);
        assert_eq!(
            (s.get_f64("ta"), s.get_bool("tb"), s.timestamp("ta")),
            (Some(2.5), Some(true), Some(77))
        );
        assert_eq!(s.dropped(), 0);
        apply(&mut rx, &s, &frame("other", b"a:9,\0"), 78);
        apply(&mut rx, &s, b"\xff\xff", 78);
        apply(&mut rx, &s, &frame("g", b"a:9"), 78); // no NUL
        assert_eq!(s.get_f64("ta"), Some(2.5));
        apply(&mut rx, &s, &frame("g", b"b:7,\0"), 79); // not a bool
        assert_eq!((s.get_bool("tb"), s.dropped()), (Some(true), 1));
    }

    /// Feed `datagrams` to a receiver on a loopback socket; wait until `done` holds or fail.
    fn feed(
        datagrams: &[Vec<u8>],
        s: &Store,
        sock: UdpSocket,
        to: SocketAddr,
        done: impl Fn(&Store) -> bool,
    ) {
        let stop = CancellationToken::new();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        std::thread::scope(|sc| {
            sc.spawn(|| serve(&sock, rx(), s, &stop));
            let t = Instant::now();
            while !done(s) && t.elapsed() < Duration::from_secs(5) {
                datagrams.iter().for_each(|d| drop(sender.send_to(d, to)));
                std::thread::sleep(Duration::from_millis(20));
            }
            stop.cancel();
        });
        assert!(done(s), "datagram never arrived");
    }

    #[test]
    fn serve_receives_unicast_datagrams_and_survives_garbage() {
        let s = store();
        let sock = bind("udp://127.0.0.1:0").unwrap();
        let to = sock.local_addr().unwrap();
        feed(
            &[vec![0xff; 40], frame("g", b"a:4.5,\0")],
            &s,
            sock,
            to,
            |s| s.get_f64("ta") == Some(4.5),
        );
    }

    #[test]
    fn serve_receives_from_a_multicast_group() {
        let port = free_port();
        let sock = bind(&format!("udp://239.0.0.7:{port}")).unwrap();
        let sender = UdpSocket::bind("0.0.0.0:0").unwrap();
        sender.set_multicast_loop_v4(true).unwrap();
        let s = store();
        let stop = CancellationToken::new();
        std::thread::scope(|sc| {
            sc.spawn(|| serve(&sock, rx(), &s, &stop));
            let t = Instant::now();
            while s.get_f64("ta") != Some(1.5) && t.elapsed() < Duration::from_secs(5) {
                let _ = sender.send_to(&frame("g", b"a:1.5,\0"), ("239.0.0.7", port));
                std::thread::sleep(Duration::from_millis(20));
            }
            stop.cancel();
        });
        assert_eq!(s.get_f64("ta"), Some(1.5));
    }

    #[test]
    fn bind_rejects_bad_endpoints() {
        assert!(bind("tcp://127.0.0.1:1")
            .unwrap_err()
            .contains("must be udp://"));
        assert!(bind("udp://[::1]:5000").is_err());
        let e = bind("udp://nonsense;239.0.0.7:5000").unwrap_err();
        assert!(e.contains("interface `nonsense`: only IPv4"), "{e}");
        assert!(bind(&format!("udp://127.0.0.1;239.0.0.7:{}", free_port())).is_ok());
    }

    #[test]
    fn spawn_runs_a_receiver_until_stopped() {
        let port = free_port();
        let ep = format!("udp://127.0.0.1:{port}");
        let s = Store::new(Layout {
            points: vec![("a".into(), Kind::Analog, None)],
            external: vec![("ta".into(), "a".into())],
            internal: vec![],
        });
        let stop = CancellationToken::new();
        let h = spawn(
            &InputSpec {
                endpoint: ep.clone(),
                points: vec!["a".into()],
            },
            s.clone(),
            stop.clone(),
        )
        .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let t = Instant::now();
        while s.get_f64("ta") != Some(8.0) && t.elapsed() < Duration::from_secs(5) {
            let _ = sender.send_to(&frame(&group_for(&ep), b"a:8,\0"), ("127.0.0.1", port));
            std::thread::sleep(Duration::from_millis(20));
        }
        stop.cancel();
        h.join().unwrap();
        assert_eq!(s.get_f64("ta"), Some(8.0));
        assert!(spawn(
            &InputSpec {
                endpoint: "nope".into(),
                points: vec![]
            },
            s,
            stop
        )
        .is_err());
    }
}
