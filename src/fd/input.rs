//! The DISH-compatible receiver (SPEC 3.3): one UDP socket per `<input>`, joined to the multicast group, RADIO-framed
//! datagrams in. `datagram::Receiver` does the parsing and duplicate rejection; whatever it accepts is published to the store.

use super::config::InputSpec;
use super::store::Store;
use crate::bennu::{group_for, parse_udp};
use crate::datagram::Receiver;
use socket2::{Domain, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// How long a read waits before the stop flag is looked at again.
const POLL: Duration = Duration::from_millis(200);

/// SO_RCVBUF we ask for (ww-c2i): the 8112-tag stream is 130 back-to-back 1.4 KB datagrams a cycle, more than the 212 992-byte
/// default holds while the parser is busy. An unprivileged process is held to net.core.rmem_max.
const RCVBUF: usize = 8 << 20;
/// A datagram is at most this long (UDP).
const MAX_DG: usize = 65535;
/// Datagrams are drained into this arena before any is parsed.
const ARENA: usize = 1 << 20;

/// A socket for `udp://[iface;]host:port`: address reuse so several devices (or inputs) share a port; a multicast group is joined.
pub fn bind(endpoint: &str) -> Result<UdpSocket, String> {
    let (iface, addr) = parse_udp(endpoint)?;
    open(iface, addr).map_err(|e| format!("{endpoint}: {e}"))
}

fn open(iface: Option<String>, addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let s = shared_socket()?;
    match addr.ip() {
        IpAddr::V4(group) if group.is_multicast() => {
            s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, addr.port())).into())?;
            s.join_multicast_v4(&group, &interface(iface)?)?;
        }
        _ => s.bind(&addr.into())?,
    }
    // best effort: a refusal leaves the default; the granted size (the kernel doubles it, wmem/rmem_max caps it) is logged
    let _ = s.set_recv_buffer_size(RCVBUF);
    tracing::debug!(
        granted = s.recv_buffer_size().unwrap_or(0),
        "input SO_RCVBUF"
    );
    let sock: UdpSocket = s.into();
    sock.set_read_timeout(Some(POLL))?;
    Ok(sock)
}

fn shared_socket() -> std::io::Result<Socket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
    s.set_reuse_address(true)?;
    s.set_reuse_port(true)?;
    Ok(s)
}

/// The `iface;` part of an endpoint as the address of the interface to join on (any, when absent).
fn interface(iface: Option<String>) -> std::io::Result<Ipv4Addr> {
    let Some(i) = iface else {
        return Ok(Ipv4Addr::UNSPECIFIED);
    };
    i.parse().map_err(|_| {
        let msg = format!("interface `{i}`: only IPv4 addresses are supported");
        std::io::Error::new(std::io::ErrorKind::InvalidInput, msg)
    })
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

/// Receive until `stop`. Bad datagrams never stop it. Each wake-up drains every queued datagram (up to `ARENA` bytes) before parsing
/// any, so the kernel queue empties as fast as recv(2) can and a slow parse costs buffer space, not datagrams.
pub fn serve(sock: &UdpSocket, mut rx: Receiver, store: &Store, stop: &CancellationToken) {
    let mut arena = vec![0u8; ARENA];
    let mut spans: Vec<(usize, usize)> = vec![];
    while !stop.is_cancelled() {
        match sock.recv(&mut arena[..MAX_DG]) {
            Ok(n) => spans.push((0, n)),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => {
                std::thread::sleep(POLL); // e.g. ICMP unreachable on a unicast feed; do not spin
                continue;
            }
        }
        let mut end = spans[0].1;
        while ARENA - end >= MAX_DG {
            let Ok(n) = crate::sys::recv_nowait(sock, &mut arena[end..]) else {
                break;
            };
            spans.push((end, n));
            end += n;
        }
        let now = unix_now();
        for (at, n) in spans.drain(..) {
            apply(&mut rx, store, &arena[at..at + n], now);
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

    /// ww-c2i: back-to-back 1.4 KB datagrams (the 8112-tag stream is 130 per cycle) land while the parser is busy elsewhere; the socket
    /// buffer must hold four cycles of them (the 212 992-byte default holds about 130), so not one datagram, and so not one tag, is lost.
    #[test]
    fn a_burst_the_parser_has_not_reached_yet_is_not_dropped() {
        const N: usize = 4 * 130;
        let names: Vec<String> = (0..N).map(|i| format!("p{i}")).collect();
        let s = Store::new(Layout {
            points: names
                .iter()
                .map(|n| (n.clone(), Kind::Analog, None))
                .collect(),
            external: names.iter().map(|n| (format!("t{n}"), n.clone())).collect(),
            internal: vec![],
        });
        let sock = bind("udp://127.0.0.1:0").unwrap();
        let to = sock.local_addr().unwrap();
        eprintln!(
            "RCVBUF {:?}",
            socket2::SockRef::from(&sock).recv_buffer_size()
        );
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        for n in &names {
            let pad = "x:0,".repeat(330); // about 1.4 KB: the stream's chunk size
            sender
                .send_to(&frame("g", format!("{n}:1,{pad}\0").as_bytes()), to)
                .unwrap();
        }
        let stop = CancellationToken::new();
        let rx = Receiver::new("g", names.iter().map(String::as_str));
        std::thread::scope(|sc| {
            sc.spawn(|| serve(&sock, rx, &s, &stop));
            let t = Instant::now();
            while names
                .iter()
                .any(|n| s.get_f64(&format!("t{n}")) != Some(1.0))
                && t.elapsed() < Duration::from_secs(5)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            stop.cancel();
        });
        let got = names
            .iter()
            .filter(|n| s.get_f64(&format!("t{n}")) == Some(1.0))
            .count();
        assert_eq!(got, N, "{} of {N} datagrams lost", N - got);
    }
}
