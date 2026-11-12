//! The SCPI server over VXI-11 on real sockets: ONC RPC calls built by hand (XDR, record marking), the portmapper over TCP and UDP
//! finding the core channel, a link's life (create, write, read, readstb, clear, destroy), links dying with their connection, and the
//! status system shared with the raw socket.

mod common;

use common::scpi::{free_port, load};
use ferrofire::fd::proto::scpi::vxi11::{record, CORE_PROG, PMAP_PROG};
use ferrofire::fd::proto::Module;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

struct Device {
    stop: CancellationToken,
    pm: u16,
    core: u16,
    raw: u16,
}

fn start(inner: &str) -> Device {
    let (pm, core, raw) = (free_port(), free_port(), free_port());
    let (server, store) = load(&format!(
        "<endpoint>tcp://127.0.0.1:{raw}</endpoint><hislip-endpoint>none</hislip-endpoint>
         <vxi11-endpoint>tcp://127.0.0.1:{pm}</vxi11-endpoint><vxi11-core-endpoint>tcp://127.0.0.1:{core}</vxi11-core-endpoint>
         <event-logging>/dev/null</event-logging>{inner}"
    ));
    let stop = CancellationToken::new();
    tokio::spawn(server.run(store, stop.clone()));
    Device {
        stop,
        pm,
        core,
        raw,
    }
}

async fn connect(port: u16) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("nothing listens on {port}");
}

fn words(xs: &[u32]) -> Vec<u8> {
    xs.iter().flat_map(|x| x.to_be_bytes()).collect()
}

fn opaque(d: &[u8]) -> Vec<u8> {
    let mut o = words(&[d.len() as u32]);
    o.extend(d);
    o.extend(vec![0; d.len().wrapping_neg() & 3]);
    o
}

/// An RPC call body: xid, CALL, RPC 2, program, version, procedure, AUTH_NONE credentials and verifier, then `args`.
fn call(prog: u32, vers: u32, proc: u32, args: &[u8]) -> Vec<u8> {
    let mut c = words(&[1, 0, 2, prog, vers, proc, 0, 0, 0, 0]);
    c.extend(args);
    c
}

/// The results of the accepted reply to a call (the 24 bytes before them are checked).
fn results(reply: &[u8]) -> Vec<u32> {
    assert_eq!(reply[..24], words(&[1, 1, 0, 0, 0, 0])[..], "{reply:?}");
    reply[24..]
        .chunks(4)
        .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
        .collect()
}

struct Rpc(TcpStream);

impl Rpc {
    async fn raw(&mut self, body: &[u8]) -> Vec<u8> {
        self.0.write_all(&record(body)).await.unwrap();
        let mut head = [0u8; 4];
        timeout(Duration::from_secs(3), self.0.read_exact(&mut head))
            .await
            .expect("a reply")
            .unwrap();
        let len = (u32::from_be_bytes(head) & 0x7FFF_FFFF) as usize;
        let mut body = vec![0; len];
        self.0.read_exact(&mut body).await.unwrap();
        body
    }

    async fn core(&mut self, proc: u32, args: &[u8]) -> Vec<u32> {
        results(&self.raw(&call(CORE_PROG, 1, proc, args)).await)
    }

    async fn create(&mut self) -> u32 {
        let r = self
            .core(10, &[words(&[7, 0, 0]), opaque(b"inst0")].concat())
            .await;
        assert_eq!(r[0], 0, "{r:?}");
        r[1]
    }

    async fn write(&mut self, link: u32, flags: u32, data: &[u8]) -> Vec<u32> {
        self.core(11, &[words(&[link, 0, 0, flags]), opaque(data)].concat())
            .await
    }

    /// (error, reason, data) of a device_read.
    async fn read(&mut self, link: u32, size: u32, flags: u32, term: u32) -> (u32, u32, Vec<u8>) {
        let r = self
            .raw(&call(
                CORE_PROG,
                1,
                12,
                &words(&[link, size, 0, 0, flags, term]),
            ))
            .await;
        let w = results(&r);
        (w[0], w[1], r[36..36 + w[2] as usize].to_vec())
    }

    /// Write a message with END and read its response.
    async fn ask(&mut self, link: u32, text: &str) -> String {
        assert_eq!(self.write(link, 8, text.as_bytes()).await[0], 0);
        let (err, reason, data) = self.read(link, 4096, 0, 0).await;
        assert_eq!((err, reason & 4), (0, 4), "{text}");
        String::from_utf8(data).unwrap()
    }
}

const IDN: &str = concat!("ferrofire,rtu-1,0,", env!("CARGO_PKG_VERSION"), "\n");

#[tokio::test]
async fn the_portmapper_over_tcp_and_udp_finds_the_core_channel() {
    let d = start("");
    let mut pm = Rpc(connect(d.pm).await);
    let get = call(PMAP_PROG, 2, 3, &words(&[CORE_PROG, 1, 6, 0]));
    assert_eq!(results(&pm.raw(&get).await), [u32::from(d.core)]);
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&get, ("127.0.0.1", d.pm)).await.unwrap();
    let mut buf = [0u8; 512];
    let (n, _) = timeout(Duration::from_secs(3), sock.recv_from(&mut buf))
        .await
        .expect("a UDP reply")
        .unwrap();
    assert_eq!(results(&buf[..n]), [u32::from(d.core)]);
    // rpcbind (version 3) is asked first by libtirpc clients and told we speak version 2
    let reply = pm.raw(&call(PMAP_PROG, 3, 3, &[])).await;
    assert_eq!(reply[20..], words(&[2, 2, 2])[..]);
    let mut core = Rpc(connect(d.core).await);
    let link = core.create().await;
    assert_eq!(core.ask(link, "*IDN?").await, IDN);
    d.stop.cancel();
}

#[tokio::test]
async fn a_link_writes_reads_reads_the_status_byte_clears_and_is_destroyed() {
    let d = start("");
    let mut c = Rpc(connect(d.core).await);
    let link = c.create().await;
    assert_eq!(c.write(link, 8, b"*IDN?\n").await, [0, 6]);
    assert_eq!(c.core(13, &words(&[link, 0, 0, 0])).await, [0, 16], "MAV");
    assert_eq!(
        c.read(link, 100, 0, 0).await,
        (0, 4, IDN.as_bytes().to_vec())
    );
    assert_eq!(
        c.read(link, 100, 0, 0).await.0,
        15,
        "nothing to read: io_timeout"
    );
    assert_eq!(c.ask(link, "MEAS:SCAL:LEVEL?").await, "12.5\n");
    assert_eq!(c.write(link, 8, b"SOUR:SETPOINT 7000").await[0], 0);
    assert_eq!(
        c.core(13, &words(&[link, 0, 0, 0])).await,
        [0, 4],
        "the error queue is not empty"
    );
    assert_eq!(
        c.ask(link, "SYST:ERR?").await,
        "-420,\"Query UNTERMINATED\"\n",
        "the read of nothing above"
    );
    assert!(c.ask(link, "SYST:ERR?").await.starts_with("-222,"));
    c.write(link, 8, b"*IDN?").await;
    assert_eq!(c.read(link, 7, 0, 0).await, (0, 1, b"ferrofi".to_vec()));
    let (_, reason, data) = c.read(link, 100, 128, u32::from(b',')).await;
    assert_eq!((reason, data), (2, b"re,".to_vec()));
    assert_eq!(c.core(15, &words(&[link, 0, 0, 0])).await, [0]);
    assert_eq!(
        c.read(link, 100, 0, 0).await.0,
        15,
        "a clear empties the output"
    );
    for p in [14, 16, 17] {
        assert_eq!(c.core(p, &words(&[link, 0, 0, 0])).await, [0], "proc {p}");
    }
    assert_eq!(c.core(23, &words(&[link])).await, [0]);
    assert_eq!(c.core(23, &words(&[link])).await, [4], "no such link now");
    assert_eq!(c.core(13, &words(&[link, 0, 0, 0])).await, [4, 0]);
    let wrong = [words(&[7, 0, 0]), opaque(b"gpib0,2")].concat();
    assert_eq!(c.core(10, &wrong).await[0], 3);
    d.stop.cancel();
}

#[tokio::test]
async fn links_die_with_their_connection_and_share_the_status_with_the_raw_socket() {
    let d = start("");
    let mut a = Rpc(connect(d.core).await);
    let link = a.create().await;
    let mut b = Rpc(connect(d.core).await);
    assert_eq!(
        b.core(15, &words(&[link, 0, 0, 0])).await,
        [0],
        "links are the server's, not the connection's"
    );
    drop(a);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        b.core(15, &words(&[link, 0, 0, 0])).await,
        [4],
        "the link went with its connection"
    );
    let lb = b.create().await;
    let mut raw = connect(d.raw).await;
    raw.write_all(b"*ESE 32\nFOO\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(b.ask(lb, "*ESE?").await, "32\n");
    assert_eq!(
        b.ask(lb, "SYST:ERR?").await,
        "-113,\"Undefined header;FOO\"\n"
    );
    d.stop.cancel();
    let d = start("<status-scope>connection</status-scope>");
    let mut c = Rpc(connect(d.core).await);
    let (x, y) = (c.create().await, c.create().await);
    c.write(x, 8, b"*FOO").await;
    assert_eq!(c.ask(y, "SYST:ERR:COUN?").await, "0\n");
    assert_eq!(c.ask(x, "SYST:ERR:COUN?").await, "1\n");
    d.stop.cancel();
}

#[tokio::test]
async fn garbage_and_odd_calls_do_not_disturb_the_service() {
    let d = start("");
    let mut s = connect(d.core).await;
    s.write_all(&record(b"not a call at all")).await.unwrap();
    s.write_all(&0xFFFF_FFFFu32.to_be_bytes()).await.unwrap();
    let mut buf = [0u8; 16];
    assert!(
        matches!(
            timeout(Duration::from_secs(3), s.read(&mut buf)).await,
            Ok(Ok(0) | Err(_))
        ),
        "a record over the limit ends the connection"
    );
    let mut c = Rpc(connect(d.core).await);
    let reply = c.raw(&call(999, 1, 0, &[])).await;
    assert_eq!(reply[20..24], words(&[1])[..], "PROG_UNAVAIL");
    let reply = c.raw(&call(CORE_PROG, 1, 77, &[])).await;
    assert_eq!(reply[20..24], words(&[3])[..], "PROC_UNAVAIL");
    let link = c.create().await;
    assert_eq!(c.ask(link, "*OPC?").await, "1\n");
    d.stop.cancel();
}
