//! Endpoints shared by every protocol module (`<endpoint>`): `tcp://ip:port`, `udp://ip:port` (DNP3 only), or a bare path for a
//! serial device.
//! No wildcard is assumed, as stock: the address in the config is the address bound.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio_util::sync::CancellationToken;

#[derive(Debug, PartialEq, Eq)]
pub enum Endpoint {
    Tcp(SocketAddr),
    Udp(SocketAddr),
    Serial(String),
}

pub fn parse(ep: &str) -> Result<Endpoint, String> {
    match ep.split_once("://") {
        Some(("tcp", addr)) => addr.parse().map(Endpoint::Tcp).map_err(|_| {
            format!(
                "endpoint `{}`: expected tcp://ip:port",
                crate::hint::show(ep)
            )
        }),
        Some(("udp", addr)) => addr.parse().map(Endpoint::Udp).map_err(|_| {
            format!(
                "endpoint `{}`: expected udp://ip:port",
                crate::hint::show(ep)
            )
        }),
        Some((scheme, _)) => Err(format!(
            "endpoint `{}`: unsupported scheme `{}`",
            crate::hint::show(ep),
            crate::hint::show(scheme)
        )),
        None if ep.is_empty() => Err("endpoint is empty".into()),
        None => Ok(Endpoint::Serial(ep.into())),
    }
}

fn tcp(ep: &str) -> Result<SocketAddr, String> {
    match parse(ep)? {
        Endpoint::Tcp(a) => Ok(a),
        Endpoint::Serial(p) => Err(format!(
            "serial endpoint {p}: this server takes tcp:// only"
        )),
        Endpoint::Udp(a) => Err(format!("udp://{a}: this server takes tcp:// only")),
    }
}

/// Bind a server endpoint. A failure is the caller's to log: the rest of the device keeps running.
pub async fn listen(ep: &str) -> Result<TcpListener, String> {
    let addr = tcp(ep)?;
    TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))
}

/// A bound server endpoint: a TCP listener, a UDP socket, or a serial port, which is one connection for the life of the device.
pub enum Bound {
    Tcp(TcpListener),
    Udp(UdpSocket),
    #[cfg(feature = "proto-serial")]
    Serial(String, tokio_serial::SerialStream),
}

/// Bind a server endpoint that may be a serial device path (`<modbus-server>`, `<dnp3-server>`). A failure is the caller's to log.
pub async fn bind(ep: &str) -> Result<Bound, String> {
    match parse(ep)? {
        Endpoint::Tcp(_) => listen(ep).await.map(Bound::Tcp),
        Endpoint::Udp(a) => UdpSocket::bind(a)
            .await
            .map(Bound::Udp)
            .map_err(|e| format!("bind udp://{a}: {e}")),
        #[cfg(feature = "proto-serial")]
        Endpoint::Serial(p) => open_serial(&p).map(|s| Bound::Serial(p, s)),
        #[cfg(not(feature = "proto-serial"))]
        Endpoint::Serial(p) => Err(format!("serial endpoint {p}: built without proto-serial")),
    }
}

/// Serve a serial port until `stop`. The port is one connection that never ends on its own: when `serve` returns (a read error,
/// such as EIO after a pty's other side closed, or a protocol close), the port is opened again after the accept pause. A failed
/// reopen goes to `report` once per run of failures and is retried.
#[cfg(feature = "proto-serial")]
pub async fn serial_loop<F: std::future::Future<Output = ()>>(
    path: &str,
    port: tokio_serial::SerialStream,
    stop: &CancellationToken,
    mut serve: impl FnMut(Conn) -> F,
    report: impl Fn(&str),
) {
    const FIRST: Duration = Duration::from_millis(100);
    let (mut port, mut pause) = (Some(port), None);
    loop {
        if let Some(p) = port.take() {
            pause = None;
            tokio::select! { _ = stop.cancelled() => return, _ = serve(Conn::Serial(p)) => {} }
        }
        let p = pause.map_or(FIRST, backoff);
        tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(p) => {} }
        match open_serial(path) {
            Ok(s) => port = Some(s),
            Err(e) => {
                if pause.is_none() {
                    report(&e);
                }
                pause = Some(p);
            }
        }
    }
}

/// Probe an idle client after 30 s, every 10 s: a peer that vanished without a FIN does not hold its descriptor for ever.
pub fn keepalive(stream: &TcpStream) {
    use socket2::{SockRef, TcpKeepalive};
    let ka = TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    let _ = SockRef::from(stream).set_tcp_keepalive(&ka);
}

/// Pause after a failed `accept`: 100 ms, doubling to 1 s while it keeps failing.
fn backoff(pause: Duration) -> Duration {
    (pause * 2).min(Duration::from_secs(1))
}

/// Accept connections until `stop`, handing each to `on_conn`. A failed accept (out of descriptors or memory) goes to `report`
/// once per run of failures, is waited out with a growing pause and retried: the listener is never given up on, so a burst of
/// connections that used every descriptor costs service only while it lasts (bennu #79 stopped accepting for good).
pub async fn accept_loop(
    listener: &TcpListener,
    stop: &CancellationToken,
    mut on_conn: impl FnMut(TcpStream),
    report: impl Fn(&std::io::Error),
) {
    const FIRST: Duration = Duration::from_millis(100);
    let mut pause = None;
    loop {
        let accepted =
            tokio::select! { _ = stop.cancelled() => return, r = listener.accept() => r };
        match accepted {
            Ok((stream, _)) => {
                pause = None;
                on_conn(stream);
            }
            Err(e) => {
                if pause.is_none() {
                    report(&e);
                }
                let p = pause.map_or(FIRST, backoff);
                pause = Some(p);
                tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(p) => {} }
            }
        }
    }
}

/// A client connection: a TCP socket, a UDP socket connected to its peer (a write is one datagram, a read one datagram, cut to
/// the buffer; a refused datagram fails the next write), or a serial port.
#[derive(Debug)]
pub enum Conn {
    Tcp(TcpStream),
    Udp(UdpSocket),
    #[cfg(feature = "proto-serial")]
    Serial(tokio_serial::SerialStream),
}

/// Forward to whichever stream `Conn` holds; a UDP socket has calls of its own.
macro_rules! each {
    ($c:expr, $s:ident => $e:expr, udp $u:pat => $on_udp:expr) => {
        match $c {
            Conn::Tcp($s) => $e,
            #[cfg(feature = "proto-serial")]
            Conn::Serial($s) => $e,
            Conn::Udp($u) => $on_udp,
        }
    };
}

impl Conn {
    /// Send small writes at once (TCP; a serial port and a UDP socket have no Nagle).
    pub fn nodelay(&self) {
        if let Conn::Tcp(t) = self {
            let _ = t.set_nodelay(true);
        }
    }

    /// TCP keepalive (see `keepalive`); a serial port has no peer to probe.
    pub fn keepalive(&self) {
        if let Conn::Tcp(t) = self {
            keepalive(t);
        }
    }

    /// Write what fits right now, without waiting.
    pub fn try_write(&mut self, buf: &[u8]) -> io::Result<usize> {
        each!(self, s => s.try_write(buf), udp u => u.try_send(buf))
    }
}

impl AsyncRead for Conn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        each!(&mut *self, s => Pin::new(s).poll_read(cx, buf), udp u => u.poll_recv(cx, buf))
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        each!(&mut *self, s => Pin::new(s).poll_write(cx, buf), udp u => u.poll_send(cx, buf))
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        each!(&mut *self, s => Pin::new(s).poll_flush(cx), udp _ => Poll::Ready(Ok(())))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        each!(&mut *self, s => Pin::new(s).poll_shutdown(cx), udp _ => Poll::Ready(Ok(())))
    }
}

/// Open a serial device as stock does: 9600 8N1, no flow control, raw. Stock Modbus never applies its stored 9600 8N1 and runs
/// the port at OS defaults (asio opens it raw); opendnp3 applies `SerialSettings` and its 500 ms `asyncOpenDelay` is never read.
#[cfg(feature = "proto-serial")]
pub fn open_serial(path: &str) -> Result<tokio_serial::SerialStream, String> {
    use tokio_serial::SerialPortBuilderExt;
    tokio_serial::new(path, 9600)
        .open_native_async()
        .map_err(|e| format!("open {path}: {e}"))
}

/// Connect a client endpoint: `tcp://ip:port`, `udp://ip:port` (a socket on an ephemeral port, connected to the peer: it
/// succeeds whether or not anyone listens, and a refused datagram fails a later write), or a serial device path.
pub async fn connect(ep: &str) -> Result<Conn, String> {
    match parse(ep)? {
        Endpoint::Tcp(addr) => TcpStream::connect(addr)
            .await
            .map(Conn::Tcp)
            .map_err(|e| format!("connect {addr}: {e}")),
        Endpoint::Udp(addr) => {
            let any: SocketAddr = if addr.is_ipv4() {
                (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
            } else {
                (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
            };
            let sock = UdpSocket::bind(any)
                .await
                .map_err(|e| format!("bind for udp://{addr}: {e}"))?;
            sock.connect(addr)
                .await
                .map(|()| Conn::Udp(sock))
                .map_err(|e| format!("connect udp://{addr}: {e}"))
        }
        #[cfg(feature = "proto-serial")]
        Endpoint::Serial(p) => open_serial(&p).map(Conn::Serial),
        #[cfg(not(feature = "proto-serial"))]
        Endpoint::Serial(p) => Err(format!("serial endpoint {p}: built without proto-serial")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoints() {
        assert_eq!(
            parse("tcp://10.0.0.5:502"),
            Ok(Endpoint::Tcp("10.0.0.5:502".parse().unwrap()))
        );
        assert_eq!(
            parse("/dev/ttyS0"),
            Ok(Endpoint::Serial("/dev/ttyS0".into()))
        );
        assert!(parse("tcp://host:502")
            .unwrap_err()
            .contains("expected tcp://ip:port"));
        assert_eq!(
            parse("udp://1.2.3.4:5"),
            Ok(Endpoint::Udp("1.2.3.4:5".parse().unwrap()))
        );
        assert!(parse("udp://host:5")
            .unwrap_err()
            .contains("expected udp://ip:port"));
        assert!(parse("sctp://1.2.3.4:5")
            .unwrap_err()
            .contains("unsupported scheme `sctp`"));
        assert_eq!(parse(""), Err("endpoint is empty".into()));
    }

    #[cfg(feature = "proto-serial")]
    #[tokio::test]
    async fn a_serial_port_is_opened_again_after_its_session_ends() {
        let (_master, slave) = tokio_serial::SerialStream::pair().unwrap();
        let path = tokio_serial::SerialPort::name(&slave).unwrap();
        let Ok(Bound::Serial(p, port)) = bind(&path).await else {
            panic!("{path} should open as a serial endpoint")
        };
        let (stop, n) = (CancellationToken::new(), std::cell::Cell::new(0));
        let session = |_c: Conn| {
            n.set(n.get() + 1);
            if n.get() == 3 {
                stop.cancel();
            }
            async {}
        };
        serial_loop(&p, port, &stop, session, |e| panic!("{e}")).await;
        assert_eq!(n.get(), 3);
        assert!(bind("/dev/no-such-tty")
            .await
            .err()
            .unwrap()
            .starts_with("open /dev/no-such-tty"));
    }

    #[test]
    fn the_accept_pause_doubles_up_to_one_second() {
        let ms = |n| Duration::from_millis(n);
        assert_eq!(backoff(ms(100)), ms(200));
        assert_eq!(backoff(ms(400)), ms(800));
        assert_eq!(backoff(ms(800)), ms(1000));
        assert_eq!(backoff(ms(1000)), ms(1000));
    }

    #[tokio::test]
    async fn accept_loop_hands_over_connections_until_stopped() {
        let l = listen("tcp://127.0.0.1:0").await.unwrap();
        let ep = format!("tcp://{}", l.local_addr().unwrap());
        let stop = CancellationToken::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let s2 = stop.clone();
        let task = tokio::spawn(async move {
            accept_loop(
                &l,
                &s2,
                |s| tx.send(s).unwrap(),
                |_| panic!("no error expected"),
            )
            .await
        });
        let _c1 = connect(&ep).await.unwrap();
        let _c2 = connect(&ep).await.unwrap();
        assert!(rx.recv().await.is_some() && rx.recv().await.is_some());
        stop.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn listen_and_connect_meet() {
        let l = listen("tcp://127.0.0.1:0").await.unwrap();
        let ep = format!("tcp://{}", l.local_addr().unwrap());
        let (a, b) = tokio::join!(l.accept(), connect(&ep));
        assert!(a.is_ok() && b.is_ok());
        assert!(listen("tcp://127.0.0.1:99999").await.is_err());
        assert!(connect("/dev/no-such-tty")
            .await
            .unwrap_err()
            .starts_with("open /dev/no-such-tty"));
        assert!(listen("/dev/ttyS0")
            .await
            .unwrap_err()
            .contains("tcp:// only"));
        assert!(listen("tcp://nope").await.is_err());
        assert!(listen("udp://127.0.0.1:0")
            .await
            .unwrap_err()
            .contains("tcp:// only"));
    }

    /// A UDP connection keeps datagram edges: one write is one datagram, one read one datagram. A datagram to a port where nothing
    /// listens comes back as ICMP port unreachable, which Linux reports on the next send (tokio does not wake a read for a socket
    /// error), so a client sees it as a failed write, a lost connection.
    #[tokio::test]
    async fn a_udp_connection_sends_and_reads_whole_datagrams() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok(Bound::Udp(server)) = bind("udp://127.0.0.1:0").await else {
            panic!("udp:// should bind a UDP socket")
        };
        let ep = format!("udp://{}", server.local_addr().unwrap());
        let mut c = connect(&ep).await.unwrap();
        c.write_all(b"one").await.unwrap();
        c.write_all(b"two").await.unwrap();
        let mut buf = [0u8; 16];
        let (n, from) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"one");
        server.send_to(b"reply", from).await.unwrap();
        assert_eq!(c.read(&mut buf).await.unwrap(), 5);
        assert_eq!(&buf[..5], b"reply");

        let port = server.local_addr().unwrap().port();
        drop(server);
        let mut gone = connect(&format!("udp://127.0.0.1:{port}")).await.unwrap();
        gone.write_all(b"anyone?").await.unwrap();
        let mut refused = None;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Err(e) = gone.write_all(b"anyone?").await {
                refused = Some(e.kind());
                break;
            }
        }
        assert_eq!(refused, Some(io::ErrorKind::ConnectionRefused));
    }
}
