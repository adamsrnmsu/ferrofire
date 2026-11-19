//! Shared by the client tests (modbus_client, dnp3_master, fd_fep): whole devices loaded as `ferrofire field-device` loads them,
//! a command-interface client, and the transcript format of conformance/stock/probe_cmdif.py.
#![allow(dead_code)]

use ferrofire::fd::config::{Config, Opts};
use ferrofire::fd::proto::REGISTRY;
use ferrofire::fd::store::Store;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use zeromq::{Socket, SocketRecv, SocketSend};

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The comms modules of a config, running; dropped or `stop`ped, they end.
pub struct Modules {
    pub store: Store,
    pub stop: CancellationToken,
}

impl Drop for Modules {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Start the `<comms>` modules of `xml` on the current runtime. No scan cycle runs: queued writes stay queued.
pub fn modules(xml: &str) -> Modules {
    let cfg = Config::parse(xml, REGISTRY, &Opts::default()).expect("config");
    let stop = CancellationToken::new();
    for (_, start) in cfg.modules {
        tokio::spawn(start(cfg.store.clone(), stop.clone()));
    }
    Modules {
        store: cfg.store,
        stop,
    }
}

/// A whole field device (modules, logic, scan cycle) on its own thread, as `ferrofire field-device` runs it.
pub struct Device {
    pub store: Store,
    pub stop: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Device {
    pub fn stop(&mut self) {
        self.stop.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn device(xml: &str) -> Device {
    let cfg = Config::parse(xml, REGISTRY, &Opts::default()).expect("config");
    let (store, stop) = (cfg.store.clone(), CancellationToken::new());
    let s2 = stop.clone();
    let thread = std::thread::spawn(move || ferrofire::fd::run(cfg, s2, false).unwrap());
    Device {
        store,
        stop,
        thread: Some(thread),
    }
}

/// Poll `ok` every 10 ms until it holds; panics with `what` after three times `within`. `within` is the time the step takes on
/// an idle host; the slack is for the llvm-cov build that `make crap` runs with every test binary at once, where 2-3 s waits
/// timed out (ww-a12, ww-ke0). A passing step returns as soon as `ok` holds, so the slack only lengthens a failure.
pub async fn until(what: &str, within: Duration, mut ok: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + within * 3;
    while !ok() {
        assert!(
            tokio::time::Instant::now() < end,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait until something listens on (or, `up == false`, nothing listens on) 127.0.0.1:`port`.
pub async fn listening(port: u16, up: bool) {
    for _ in 0..500 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
            == up
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "port {port} never became {}",
        if up { "open" } else { "closed" }
    );
}

/// A command-interface client, as bennu-probe: one REQ socket per request, the text plus a NUL.
pub async fn command(port: u16, text: &str) -> Vec<u8> {
    let mut req = zeromq::ReqSocket::new();
    let ep = format!("tcp://127.0.0.1:{port}");
    for _ in 0..200 {
        if req.connect(&ep).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut msg = text.as_bytes().to_vec();
    msg.push(0);
    req.send(msg.into()).await.unwrap();
    let m = timeout(Duration::from_secs(10), req.recv())
        .await
        .unwrap_or_else(|_| panic!("no reply to {text:?}"))
        .unwrap();
    m.get(0).unwrap().to_vec()
}

/// A reply as the golden transcripts show it: printable ASCII as is, the rest `\xNN`, and the text after a failure masked
/// (stock's is a dangling pointer).
pub fn shown(reply: &[u8]) -> String {
    let s: String = reply
        .iter()
        .map(|&c| match c {
            32..=126 if c != b'\\' => char::from(c).to_string(),
            _ => format!("\\x{c:02x}"),
        })
        .collect();
    for head in ["ERR=Failed reading tag '", "ERR=Failed writing tag '"] {
        if let (true, Some(at)) = (s.starts_with(head), s.find("': ")) {
            return format!("{}<text>\\x00", &s[..at + 3]);
        }
    }
    s
}

pub enum Line<'a> {
    /// `# text`: the harness did something.
    Mark(&'a str),
    /// `request<TAB>reply`.
    Command(&'a str, &'a str),
    /// `> ...` or `< ...`: what the capture saw.
    Wire(&'a str),
}

pub fn transcript(golden: &str) -> impl Iterator<Item = Line<'_>> {
    golden.lines().map(|l| {
        if let Some(m) = l.strip_prefix("# ") {
            Line::Mark(m)
        } else if l.starts_with("> ") || l.starts_with("< ") {
            Line::Wire(l)
        } else {
            let mut f = l.split('\t');
            Line::Command(f.next().unwrap(), f.next().expect("request<TAB>reply"))
        }
    })
}

/// A TCP relay in front of 127.0.0.1:`target` that keeps what the client side sent, chunk by chunk. With nothing listening on
/// `target` an accepted connection is closed at once: to its client the device is down.
pub struct Tap {
    pub port: u16,
    pub sent: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    pub accepted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

pub async fn tap(target: u16) -> Tap {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let t = Tap {
        port: l.local_addr().unwrap().port(),
        sent: Default::default(),
        accepted: Default::default(),
    };
    let (sent, accepted) = (t.sent.clone(), t.accepted.clone());
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = l.accept().await else {
                return;
            };
            accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let Ok(server) = tokio::net::TcpStream::connect(("127.0.0.1", target)).await else {
                continue;
            };
            let sent = sent.clone();
            tokio::spawn(async move {
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                let up = async {
                    let mut buf = [0u8; 4096];
                    while let Ok(n @ 1..) = cr.read(&mut buf).await {
                        sent.lock().unwrap().push(buf[..n].to_vec());
                        if sw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                };
                let down = async {
                    let mut buf = [0u8; 4096];
                    while let Ok(n @ 1..) = sr.read(&mut buf).await {
                        if cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                };
                // either side closing ends both
                tokio::select! { _ = up => {}, _ = down => {} }
            });
        }
    });
    t
}

impl Tap {
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.sent.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }
}

/// A serial port in front of `tap_port`: the path of a pty slave to use as a client's `<endpoint>`, a bridge from its master side
/// to the tap (which keeps what the client wrote). The pair stays open until the runtime ends.
#[cfg(feature = "proto-serial")]
pub fn pty_to(tap_port: u16) -> String {
    let (mut master, slave) = tokio_serial::SerialStream::pair().expect("pty pair");
    let path = tokio_serial::SerialPort::name(&slave).expect("pty name");
    tokio::spawn(async move {
        let _slave = slave;
        let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", tap_port))
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut master, &mut tcp).await;
    });
    path
}
