//! Endpoints shared by every protocol module (`<endpoint>`): `tcp://ip:port`, or a bare path for a serial device.
//! No wildcard is assumed, as stock: the address in the config is the address bound.

use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};

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
        // ponytail: serial lands with feature `proto-serial` (tokio-serial) in F6.
        Endpoint::Serial(p) => Err(format!(
            "serial endpoint {p}: not supported yet (feature proto-serial)"
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

/// Connect a client endpoint.
pub async fn connect(ep: &str) -> Result<TcpStream, String> {
    let addr = tcp(ep)?;
    TcpStream::connect(addr)
        .await
        .map_err(|e| format!("connect {addr}: {e}"))
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

    #[tokio::test]
    async fn listen_and_connect_meet() {
        let l = listen("tcp://127.0.0.1:0").await.unwrap();
        let ep = format!("tcp://{}", l.local_addr().unwrap());
        let (a, b) = tokio::join!(l.accept(), connect(&ep));
        assert!(a.is_ok() && b.is_ok());
        assert!(listen("tcp://127.0.0.1:99999").await.is_err());
        assert!(connect("/dev/ttyS0")
            .await
            .unwrap_err()
            .contains("proto-serial"));
        assert!(listen("tcp://nope").await.is_err());
    }
}
