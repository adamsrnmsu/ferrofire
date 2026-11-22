//! The socket transport: the device binds `tcp://HOST:PORT` or `unix:///path` and the module connects. One connection at a time:
//! a new one ends the running session (the old module is sent `stop`) and starts a fresh one with a new `hello`.

use super::session::{session, End, Spec};
use super::External;
use crate::fd::config::ConfigError;
use crate::fd::proto::{backoff, BACKOFF_FIRST, BACKOFF_MAX};
use crate::fd::store::Store;
use crate::hint::show;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::io::{split, AsyncRead, AsyncWrite, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Debug, PartialEq)]
pub enum Transport {
    Stdio,
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl Transport {
    /// `stdio`, `tcp://HOST:PORT` (an IP, or `localhost`) or `unix:///path`. A host that is not loopback is a warning, an error
    /// under `--strict`: the module's session carries tag values and writes, with no authentication.
    pub fn parse(s: &str, strict: bool) -> Result<Transport, ConfigError> {
        let bad = || {
            let t = show(s);
            ConfigError(format!(
                "<transport> `{t}` is not supported (stdio, tcp://HOST:PORT, unix:///path)"
            ))
        };
        if s == "stdio" {
            return Ok(Transport::Stdio);
        }
        match s.split_once("://") {
            Some(("unix", p)) if p.starts_with('/') => Ok(Transport::Unix(p.into())),
            Some(("tcp", a)) => {
                let a = a
                    .strip_prefix("localhost:")
                    .map_or_else(|| a.to_string(), |port| format!("127.0.0.1:{port}"));
                let a: SocketAddr = a.parse().map_err(|_| bad())?;
                if !a.ip().is_loopback() {
                    let msg = format!("<transport> `{}` is not loopback: anyone who can reach it can drive the device", show(s));
                    if strict {
                        return Err(ConfigError(msg));
                    }
                    tracing::warn!("{msg}");
                }
                Ok(Transport::Tcp(a))
            }
            _ => Err(bad()),
        }
    }
}

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener, PathBuf),
}

impl Listener {
    /// A stale socket file is removed first.
    async fn bind(t: &Transport) -> std::io::Result<Listener> {
        match t {
            Transport::Tcp(a) => Ok(Listener::Tcp(TcpListener::bind(a).await?)),
            Transport::Unix(p) => {
                let _ = std::fs::remove_file(p);
                Ok(Listener::Unix(UnixListener::bind(p)?, p.clone()))
            }
            Transport::Stdio => unreachable!("stdio is not bound"),
        }
    }

    async fn accept(&self) -> std::io::Result<Box<dyn Duplex>> {
        Ok(match self {
            Listener::Tcp(l) => Box::new(l.accept().await?.0),
            Listener::Unix(l, _) => Box::new(l.accept().await?.0),
        })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Listener::Unix(_, p) = self {
            let _ = std::fs::remove_file(p);
        }
    }
}

type Running = (CancellationToken, JoinHandle<End>);

/// A session over `conn`, in its own task; it is never dropped half way, only cancelled through its token.
fn serve(conn: Box<dyn Duplex>, spec: &Spec, store: &Store, stop: &CancellationToken) -> Running {
    let (r, w) = split(conn);
    let (spec, store, token) = (spec.clone(), store.clone(), stop.child_token());
    let t = token.clone();
    (
        token,
        tokio::spawn(async move { session(BufReader::new(r), w, &spec, store, t).await }),
    )
}

/// Cancel the running session and wait until it has said `stop` and let go of its connection.
async fn end(cur: Option<Running>) {
    if let Some((token, h)) = cur {
        token.cancel();
        let _ = h.await;
    }
}

/// How the running session ended; never, while there is none.
async fn finished(cur: &mut Option<Running>) -> End {
    let Some((_, h)) = cur else {
        return std::future::pending().await;
    };
    let e = h.await.unwrap_or(End::Broken { was_ready: false });
    *cur = None;
    e
}

impl External {
    /// Bind, then serve connections while the optional `<command>` is kept running; returns when the device stops (or the module
    /// said `error`), the socket file gone.
    pub(super) async fn run_socket(&self, store: Store, stop: CancellationToken) {
        match Listener::bind(&self.transport).await {
            Ok(l) => {
                tokio::join!(self.listen(l, &store, &stop), self.supervise(&stop));
            }
            Err(e) => {
                let msg = format!(
                    "module {}: cannot listen on {}: {e}",
                    self.spec.name, self.addr
                );
                tracing::error!("{msg}");
                if self.spec.strict {
                    super::fatal(msg, &stop);
                }
            }
        }
    }

    async fn listen(&self, l: Listener, store: &Store, stop: &CancellationToken) {
        let mut cur: Option<Running> = None;
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                c = l.accept() => match c {
                    Ok(conn) => {
                        end(cur.take()).await;
                        cur = Some(serve(conn, &self.spec, store, stop));
                    }
                    Err(e) => {
                        tracing::error!("module {}: accept: {e}", self.spec.name);
                        sleep(BACKOFF_FIRST).await;
                    }
                },
                e = finished(&mut cur) => {
                    if let End::Refused(msg) = e {
                        return self.refused(msg, stop);
                    }
                }
            }
        }
        end(cur).await;
    }

    /// Keep the `<command>` (if there is one) running: restarted on the backoff, which starts over once it has lived long enough;
    /// at the device's stop it is signalled as in the stdio transport.
    async fn supervise(&self, stop: &CancellationToken) {
        if self.argv.is_empty() {
            return;
        }
        let name = &self.spec.name;
        let mut pause = BACKOFF_FIRST;
        loop {
            let started = Instant::now();
            tracing::info!("module {name}: starting {:?}", self.argv);
            match self.spawn(false) {
                Ok(mut child) => {
                    super::stderr(name, child.stderr.take().expect("piped"));
                    tokio::select! {
                        r = child.wait() => tracing::warn!("module {name}: exited ({r:?})"),
                        _ = stop.cancelled() => return super::reap(name, child, Instant::now()).await,
                    }
                }
                Err(e) => tracing::error!("module {name}: cannot start: {e}"),
            }
            if started.elapsed() > BACKOFF_MAX {
                pause = BACKOFF_FIRST;
            }
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = sleep(pause) => {}
            }
            pause = backoff(pause, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::config::{Opts, Tags};
    use crate::fd::proto::Module;
    use crate::fd::store::Layout;
    use crate::model::Kind;
    use std::time::Duration;
    use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, Lines};
    use tokio::net::{TcpStream, UnixStream};
    use tokio::time::timeout;

    const WAIT: Duration = Duration::from_secs(5);

    fn module(transport: &str, strict: bool) -> Result<External, ConfigError> {
        let xml = format!(
            "<external-module><name>m</name><transport>{transport}</transport></external-module>"
        );
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let opts = Opts {
            strict,
            ..Opts::default()
        };
        External::parse(doc.root_element(), &mut Tags::default(), &opts)
    }

    /// Run a module over `transport` until the returned token is cancelled.
    fn run(transport: &str) -> (CancellationToken, JoinHandle<()>) {
        let store = Store::new(Layout {
            points: vec![("a".into(), Kind::Binary, None)],
            external: vec![("a".into(), "a".into())],
            internal: vec![],
        });
        let stop = CancellationToken::new();
        let m = module(transport, false).unwrap();
        (stop.clone(), tokio::spawn(m.run(store, stop)))
    }

    fn free_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    /// Connect once the device is listening.
    async fn retry<T, F: std::future::Future<Output = std::io::Result<T>>>(f: impl Fn() -> F) -> T {
        for _ in 0..200 {
            if let Ok(c) = f().await {
                return c;
            }
            sleep(Duration::from_millis(25)).await;
        }
        panic!("the device never listened");
    }

    async fn line<R: AsyncBufRead + Unpin>(l: &mut Lines<R>) -> String {
        timeout(WAIT, l.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }

    fn lines<S: AsyncRead + Unpin>(s: S) -> Lines<BufReader<S>> {
        BufReader::new(s).lines()
    }

    #[tokio::test]
    async fn tcp_module_connects_and_gets_hello() {
        let port = free_port();
        let (stop, task) = run(&format!("tcp://127.0.0.1:{port}"));
        let c = retry(|| TcpStream::connect(("127.0.0.1", port))).await;
        let hello: serde_json::Value = serde_json::from_str(&line(&mut lines(c)).await).unwrap();
        assert_eq!(hello["t"], "hello");
        stop.cancel();
        timeout(WAIT, task).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_second_connection_replaces_the_first() {
        let port = free_port();
        let (stop, task) = run(&format!("tcp://127.0.0.1:{port}"));
        let mut one = lines(retry(|| TcpStream::connect(("127.0.0.1", port))).await);
        assert!(line(&mut one).await.contains("hello"));
        let mut two = lines(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        assert!(line(&mut two).await.contains("hello"));
        // the first is told to stop, then sees the end of the stream
        assert!(line(&mut one).await.contains("stop"));
        assert!(timeout(WAIT, one.next_line())
            .await
            .unwrap()
            .unwrap()
            .is_none());
        stop.cancel();
        timeout(WAIT, task).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unix_socket_round_trip() {
        let path = std::env::temp_dir().join(format!("ff-ext-{}.sock", std::process::id()));
        // a file left over from an earlier run does not stop the bind
        std::fs::write(&path, "stale").unwrap();
        let (stop, task) = run(&format!("unix://{}", path.display()));
        let (r, mut w) = retry(|| UnixStream::connect(&path)).await.into_split();
        let mut r = lines(r);
        assert!(line(&mut r).await.contains("\"t\":\"hello\""));
        let ready =
            r#"{"t":"ready","role":"server","uses":[{"tag":"a","kind":"binary","dir":"in"}]}"#;
        w.write_all(format!("{ready}\n").as_bytes()).await.unwrap();
        w.write_all(b"{\"t\":\"write\",\"id\":1,\"tag\":\"a\",\"value\":true}\n")
            .await
            .unwrap();
        let ack: serde_json::Value = serde_json::from_str(&line(&mut r).await).unwrap();
        assert_eq!(ack, serde_json::json!({"t":"ack","id":1}));
        stop.cancel();
        assert!(line(&mut r).await.contains("stop"));
        timeout(WAIT, task).await.unwrap().unwrap();
        assert!(!path.exists(), "the socket file is left behind");
    }

    #[test]
    fn non_loopback_is_refused_under_strict() {
        assert!(module("tcp://127.0.0.1:1", true).is_ok());
        assert!(module("tcp://localhost:1", true).is_ok());
        assert!(module("tcp://[::1]:1", true).is_ok());
        let e = module("tcp://0.0.0.0:1", true).err().unwrap().0;
        assert!(e.contains("not loopback"), "{e}");
        // not strict: a warning only
        assert!(module("tcp://0.0.0.0:1", false).is_ok());
        assert!(module("unix://relative", false).is_err());
    }

    #[tokio::test]
    async fn the_command_is_started_with_the_address_and_stopped() {
        let dir = std::env::temp_dir().join(format!("ff-ext-{}-cmd", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (out, port) = (dir.join("addr"), free_port());
        let tr = format!("tcp://127.0.0.1:{port}");
        let script = dir.join("m.sh");
        std::fs::write(
            &script,
            format!(
                "echo $FERROFIRE_MODULE_ADDR > {}\nexec sleep 30\n",
                out.display()
            ),
        )
        .unwrap();
        let xml = format!("<external-module><name>m</name><transport>{tr}</transport><command>sh {}</command></external-module>", script.display());
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let m =
            External::parse(doc.root_element(), &mut Tags::default(), &Opts::default()).unwrap();
        let store = Store::new(Layout {
            points: vec![],
            external: vec![],
            internal: vec![],
        });
        let stop = CancellationToken::new();
        let task = tokio::spawn(m.run(store, stop.clone()));
        retry(|| async { std::fs::read_to_string(&out).map_err(std::io::Error::other) }).await;
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), tr);
        stop.cancel();
        timeout(WAIT, task).await.unwrap().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
