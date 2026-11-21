//! Endpoints shared by every protocol module (`<endpoint>`): `tcp://ip:port`, or a bare path for a serial device.
//! No wildcard is assumed, as stock: the address in the config is the address bound.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

#[derive(Debug, PartialEq, Eq)]
pub enum Endpoint {
    Tcp(SocketAddr),
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
            "serial endpoint {p}: not supported yet (bead ww-z5w)"
        )),
    }
}

/// Bind a server endpoint. A failure is the caller's to log: the rest of the device keeps running.
pub async fn listen(ep: &str) -> Result<TcpListener, String> {
    let addr = tcp(ep)?;
    TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))
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

/// A client connection: a TCP socket, or a serial port.
#[derive(Debug)]
pub enum Conn {
    Tcp(TcpStream),
    #[cfg(feature = "proto-serial")]
    Serial(tokio_serial::SerialStream),
}

/// Forward to whichever stream `Conn` holds.
macro_rules! each {
    ($c:expr, $s:ident => $e:expr) => {
        match $c {
            Conn::Tcp($s) => $e,
            #[cfg(feature = "proto-serial")]
            Conn::Serial($s) => $e,
        }
    };
}

impl Conn {
    /// Send small writes at once (TCP; a serial port has no Nagle).
    pub fn nodelay(&self) {
        match self {
            Conn::Tcp(t) => {
                let _ = t.set_nodelay(true);
            }
            #[cfg(feature = "proto-serial")]
            Conn::Serial(_) => {}
        }
    }

    /// Write what fits right now, without waiting.
    pub fn try_write(&mut self, buf: &[u8]) -> io::Result<usize> {
        each!(self, s => s.try_write(buf))
    }
}

impl AsyncRead for Conn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        each!(&mut *self, s => Pin::new(s).poll_read(cx, buf))
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        each!(&mut *self, s => Pin::new(s).poll_write(cx, buf))
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        each!(&mut *self, s => Pin::new(s).poll_flush(cx))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        each!(&mut *self, s => Pin::new(s).poll_shutdown(cx))
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

/// Connect a client endpoint: `tcp://ip:port`, or a serial device path.
pub async fn connect(ep: &str) -> Result<Conn, String> {
    match parse(ep)? {
        Endpoint::Tcp(addr) => TcpStream::connect(addr)
            .await
            .map(Conn::Tcp)
            .map_err(|e| format!("connect {addr}: {e}")),
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
        assert!(parse("udp://1.2.3.4:5")
            .unwrap_err()
            .contains("unsupported scheme `udp`"));
        assert_eq!(parse(""), Err("endpoint is empty".into()));
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
            .contains("not supported yet"));
        assert!(listen("tcp://nope").await.is_err());
    }
}
