//! The socket transport: the device binds `tcp://HOST:PORT` or `unix:///path` and the module connects. One connection at a time:
//! a new one ends the running session (the old module is sent `stop`) and starts a fresh one with a new `hello`.

use super::session::{session, End, Spec};
use super::External;
use crate::fd::config::ConfigError;
use crate::fd::proto::{backoff, BACKOFF_FIRST};
use crate::fd::store::Store;
use crate::hint::show;
use std::net::SocketAddr;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{split, AsyncRead, AsyncWrite, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{sleep, timeout, Instant};
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

/// How long the supervisor gives a session's end to be noticed after its module's process has exited.
const END_GRACE: Duration = Duration::from_millis(100);

enum Listener {
    Tcp(TcpListener),
    /// The path, and the (device, inode) of the socket file this listener made there.
    Unix(UnixListener, PathBuf, (u64, u64)),
}

fn file_id(p: &Path) -> std::io::Result<(u64, u64)> {
    let m = std::fs::symlink_metadata(p)?;
    Ok((m.dev(), m.ino()))
}

/// Remove a stale socket file at `p`; anything else there is an error.
fn clear_stale(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(p),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the path exists and is not a socket",
        )),
        Err(_) => Ok(()),
    }
}

impl Listener {
    async fn bind(t: &Transport) -> std::io::Result<Listener> {
        match t {
            Transport::Tcp(a) => Ok(Listener::Tcp(TcpListener::bind(a).await?)),
            Transport::Unix(p) => {
                clear_stale(p)?;
                let l = UnixListener::bind(p)?;
                Ok(Listener::Unix(l, p.clone(), file_id(p)?))
            }
            Transport::Stdio => unreachable!("stdio is not bound"),
        }
    }

    async fn accept(&self) -> std::io::Result<Box<dyn Duplex>> {
        Ok(match self {
            Listener::Tcp(l) => Box::new(l.accept().await?.0),
            Listener::Unix(l, ..) => Box::new(l.accept().await?.0),
        })
    }
}

impl Drop for Listener {
    /// The socket file goes, unless another instance has bound the path since.
    fn drop(&mut self) {
        if let Listener::Unix(_, p, id) = self {
            if file_id(p).is_ok_and(|now| now == *id) {
                let _ = std::fs::remove_file(p);
            }
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
                // `up`: a session that had come up has ended; the command is stopped with the listener
                let (up, local) = (watch::channel(false).0, stop.child_token());
                tokio::join!(
                    self.listen(l, &store, &stop, &local, &up),
                    self.supervise(&local, &up)
                );
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

    async fn listen(
        &self,
        l: Listener,
        store: &Store,
        stop: &CancellationToken,
        local: &CancellationToken,
        up: &watch::Sender<bool>,
    ) {
        let (mut cur, mut old, mut refused): (Option<Running>, JoinSet<()>, _) =
            (None, JoinSet::new(), None);
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                c = l.accept() => match c {
                    Ok(conn) => {
                        // the old peer may be slow to take `stop`: the new one does not wait for it
                        old.spawn(end(cur.take()));
                        cur = Some(serve(conn, &self.spec, store, stop));
                    }
                    Err(e) => {
                        tracing::error!("module {}: accept: {e}", self.spec.name);
                        tokio::select! { _ = stop.cancelled() => {}, _ = sleep(BACKOFF_FIRST) => {} }
                    }
                },
                e = finished(&mut cur) => match e {
                    End::Refused(msg) => {
                        refused = Some(msg);
                        break;
                    }
                    End::Broken { was_ready: true } => {
                        up.send_replace(true);
                    }
                    _ => {}
                }
            }
        }
        end(cur).await;
        while old.join_next().await.is_some() {}
        if let Some(msg) = refused {
            local.cancel();
            self.refused(msg, stop);
        }
    }

    /// Keep the `<command>` (if there is one) running: restarted on the backoff, which starts over after a module whose session
    /// came up; at `stop` it is signalled as in the stdio transport.
    async fn supervise(&self, stop: &CancellationToken, up: &watch::Sender<bool>) {
        if self.argv.is_empty() {
            return;
        }
        let name = &self.spec.name;
        let mut pause = BACKOFF_FIRST;
        loop {
            tracing::info!("module {name}: starting {:?}", self.argv);
            let mut seen = up.subscribe();
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
            // its session ends as it does: give the end a moment to be noticed
            let was_up = timeout(END_GRACE, seen.wait_for(|v| *v)).await;
            if was_up.is_ok_and(|r| r.is_ok()) {
                pause = BACKOFF_FIRST;
            }
            up.send_replace(false);
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
        // a socket file left over from an earlier run does not stop the bind
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
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

    #[tokio::test]
    async fn a_path_that_is_not_a_socket_is_refused_and_kept() {
        let path = std::env::temp_dir().join(format!("ff-ext-{}-file", std::process::id()));
        std::fs::write(&path, "mine").unwrap();
        let (_stop, task) = run(&format!("unix://{}", path.display()));
        timeout(WAIT, task).await.unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn a_listener_leaves_a_socket_that_another_one_has_bound() {
        let path = std::env::temp_dir().join(format!("ff-ext-{}-rebind", std::process::id()));
        let t = Transport::Unix(path.clone());
        let first = Listener::bind(&t).await.unwrap();
        let second = Listener::bind(&t).await.unwrap();
        drop(first);
        assert!(
            path.exists(),
            "the first listener removed the second's socket"
        );
        drop(second);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_new_connection_does_not_wait_for_a_peer_that_never_reads() {
        let port = free_port();
        let (stop, task) = run(&format!("tcp://127.0.0.1:{port}"));
        // held open, never read from
        let _deaf = retry(|| TcpStream::connect(("127.0.0.1", port))).await;
        sleep(Duration::from_millis(100)).await;
        let mut two = lines(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        assert!(line(&mut two).await.contains("hello"));
        let t = Instant::now();
        stop.cancel();
        timeout(WAIT, task).await.unwrap().unwrap();
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    /// A module that reached `ready` and exited is restarted after the first pause each time: in 5.5 s that is 5 starts (0, 1.3,
    /// 2.6, 3.9, 5.2 s), where a pause that doubled gives 3.
    #[tokio::test]
    async fn a_command_whose_session_was_ready_restarts_after_the_first_pause() {
        let dir = std::env::temp_dir().join(format!("ff-ext-{}-ready", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (starts, script, port) = (dir.join("starts"), dir.join("m.sh"), free_port());
        std::fs::write(
            &script,
            format!("echo start >> {}\nsleep 0.3\n", starts.display()),
        )
        .unwrap();
        let xml = format!(
            "<external-module><name>m</name><transport>tcp://127.0.0.1:{port}</transport><command>sh {}</command></external-module>",
            script.display()
        );
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
        let count = || std::fs::read_to_string(&starts).map_or(0, |s| s.lines().count());
        let (end, mut seen) = (Instant::now() + Duration::from_millis(5500), 0);
        while Instant::now() < end {
            // as a module: connect after each start, say `ready`, and hang up
            if count() > seen {
                seen = count();
                if let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)).await {
                    let ready = b"{\"t\":\"ready\",\"role\":\"server\",\"uses\":[]}\n";
                    c.write_all(ready).await.unwrap();
                    sleep(Duration::from_millis(50)).await;
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
        assert!(count() >= 4, "{} starts in 5.5 s", count());
        stop.cancel();
        timeout(WAIT, task).await.unwrap().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
