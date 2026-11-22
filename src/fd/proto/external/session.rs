//! One ffmod/1 session over any line stream: `hello`, `ready`, then a `values` each scan cycle and the module's writes, until
//! stop, end of stream or the module's `error`. The transports (stdio, socket, bennu) run this over their streams.

use super::msg::{decode, encode, Dir, FromModule, Role, ToModule, Use, MAX_LINE};
use crate::fd::cmdif;
use crate::fd::proto::client::{input_polled, Out, TagPoints};
use crate::fd::store::{kind_of, same, zero, Store, WriteError};
use crate::hint::show;
use crate::model::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::{CancellationToken, DropGuard};

/// How long a module has to answer `hello` with `ready`.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// Answers to writes waiting for a module that is slow to read them; one more is dropped.
const ANSWERS: usize = 64;
/// How long the writer has to put `stop` on the wire once the session is over, before it is cut off.
const STOP_GRACE: Duration = Duration::from_secs(1);
/// How long a client's output write stays under way without the module's `sent` before it counts as failed.
const SENT_TIMEOUT: Duration = Duration::from_secs(10);

/// What a session needs to know about its module and the device.
#[derive(Clone, Debug)]
pub struct Spec {
    /// The module's name, in every log line about it.
    pub name: String,
    pub device: String,
    pub strict: bool,
    /// The protocol element, passed on in `hello`.
    pub config: String,
    /// The device scan cycle: how often the store is looked at for changes.
    pub cycle: Duration,
    /// A client's `<command-interface>`, served over its tags.
    pub cmdif: Option<String>,
}

/// How a session ended.
#[derive(Clone, Debug, PartialEq)]
pub enum End {
    /// The device is stopping.
    Stopped,
    /// End of stream, or no `ready` in time: restart. `was_ready`: the module's `ready` had been taken.
    Broken { was_ready: bool },
    /// The module sent `error`, or `--strict` refused its tags: no restart.
    Refused(String),
}

/// Run one session: `r` carries the module's lines, `w` takes the device's. Whatever ends it, the module is sent `stop` (given
/// `STOP_GRACE` to take it) and `w` is dropped.
pub async fn session<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin + Send + 'static>(
    r: R,
    w: W,
    spec: &Spec,
    store: Store,
    stop: CancellationToken,
) -> End {
    let points = store.snapshot();
    let hello = ToModule::Hello {
        device: spec.device.clone(),
        strict: spec.strict,
        config: spec.config.clone(),
        points: points.clone(),
    };
    let (pending, changes) = watch::channel(Pending::default());
    let (answers, queue) = mpsc::channel(ANSWERS);
    let mut writing = tokio::spawn(writer(w, hello, changes, queue));
    let mut s = Session {
        spec,
        store,
        lines: Lines {
            r,
            buf: vec![],
            over: false,
        },
        pending,
        answers,
        last: points,
        client: false,
        ins: BTreeSet::new(),
        outs: BTreeMap::new(),
        inflight: BTreeMap::new(),
        stalled: false,
    };
    let end = s.run(&stop).await;
    drop(s); // closes the writer's channels: it sends `stop` and ends
    if tokio::time::timeout(STOP_GRACE, &mut writing)
        .await
        .is_err()
    {
        writing.abort();
    }
    end
}

/// Changes not written yet: the latest value of each tag, numbered in the order they came, so that a module slow to read gets
/// the newest value of every tag that changed and nothing in between is lost.
#[derive(Default)]
struct Pending {
    seq: u64,
    tags: BTreeMap<String, (u64, Value)>,
}

/// The changes numbered after `seen`, which moves on past them.
fn fresh(p: &Pending, seen: &mut u64) -> Vec<(String, Value)> {
    let new = p.tags.iter().filter(|(_, (n, _))| *n > *seen);
    let out = new.map(|(t, (_, v))| (t.clone(), *v)).collect();
    *seen = p.seq;
    out
}

/// The points of snapshot `now` that differ from `last`, the snapshot before it. The same tags in the same order every time: the
/// store's layout is fixed at load.
pub(super) fn moved<'a>(
    now: &'a [(String, Value)],
    last: &'a [(String, Value)],
) -> impl Iterator<Item = &'a (String, Value)> {
    now.iter()
        .zip(last)
        .filter(|(n, o)| !same(&n.1, &o.1))
        .map(|(n, _)| n)
}

async fn send<W: AsyncWrite + Unpin>(w: &mut W, m: &ToModule) -> std::io::Result<()> {
    w.write_all(encode(m).as_bytes()).await?;
    w.flush().await
}

/// Everything the device sends a module: `hello`, then answers as they come and the pending changes whenever the module can
/// take them, then `stop` once both channels close. A module that does not read stalls only this task.
async fn writer<W: AsyncWrite + Unpin>(
    mut w: W,
    hello: ToModule,
    mut changes: watch::Receiver<Pending>,
    mut answers: mpsc::Receiver<ToModule>,
) {
    let mut seen = 0;
    if send(&mut w, &hello).await.is_err() {
        return;
    }
    loop {
        let m = tokio::select! {
            biased;
            Some(m) = answers.recv() => m,
            Ok(()) = changes.changed() => ToModule::Values(fresh(&changes.borrow_and_update(), &mut seen)),
            else => break,
        };
        if send(&mut w, &m).await.is_err() {
            return;
        }
    }
    let _ = send(&mut w, &ToModule::Stop).await;
    let _ = w.shutdown().await;
}

/// The module's lines, each at most `MAX_LINE` bytes. State lives here between calls, so `next` can be raced in a `select!`
/// without losing a byte.
struct Lines<R> {
    r: R,
    buf: Vec<u8>,
    /// The line under way is over `MAX_LINE`: the rest of it is skipped.
    over: bool,
}

impl<R: AsyncBufRead + Unpin> Lines<R> {
    /// The next line without its `\n`, or why it was dropped; `None` at end of stream (a partial last line is dropped).
    async fn next(&mut self) -> Option<Result<String, String>> {
        loop {
            let b = self.r.fill_buf().await.ok()?;
            if b.is_empty() {
                return None;
            }
            let nl = b.iter().position(|&c| c == b'\n');
            let part = &b[..nl.unwrap_or(b.len())];
            if !self.over {
                self.buf.extend_from_slice(part);
            }
            let used = part.len() + usize::from(nl.is_some());
            self.r.consume(used);
            // once over, nothing more is kept and `take` drops what was
            self.over |= self.buf.len() > MAX_LINE;
            if nl.is_some() {
                return Some(self.take());
            }
        }
    }

    fn take(&mut self) -> Result<String, String> {
        let line = std::mem::take(&mut self.buf);
        if std::mem::take(&mut self.over) {
            return Err(format!("line over {MAX_LINE} bytes"));
        }
        String::from_utf8(line).map_err(|e| e.to_string())
    }
}

/// A `log` line from the module, at its level (`info` for a level tracing does not have).
fn log(name: &str, level: &str, msg: &str) {
    let msg = msg.escape_debug();
    match level {
        "error" => tracing::error!("module {name}: {msg}"),
        "warn" => tracing::warn!("module {name}: {msg}"),
        "debug" => tracing::debug!("module {name}: {msg}"),
        "trace" => tracing::trace!("module {name}: {msg}"),
        _ => tracing::info!("module {name}: {msg}"),
    }
}

fn refusal(e: WriteError) -> &'static str {
    match e {
        WriteError::UnknownTag => "unknown tag",
        WriteError::WrongKind => "wrong kind",
    }
}

struct Session<'a, R> {
    spec: &'a Spec,
    store: Store,
    lines: Lines<R>,
    pending: watch::Sender<Pending>,
    answers: mpsc::Sender<ToModule>,
    /// The snapshot the last changes were taken from.
    last: Vec<(String, Value)>,
    /// `role: client`: writes go through the client rules, and `outs` through their echo rule.
    client: bool,
    ins: BTreeSet<String>,
    outs: BTreeMap<String, Out>,
    /// Outputs whose write went out in `values` and waits for the module's `sent`, until when.
    inflight: BTreeMap<String, Instant>,
    /// The last answer was dropped: the module is not reading (warned once per stall).
    stalled: bool,
}

impl<R: AsyncBufRead + Unpin> Session<'_, R> {
    async fn run(&mut self, stop: &CancellationToken) -> End {
        // closed once the writer has ended: the module's input is gone
        let gone = self.answers.clone();
        let ready = tokio::select! {
            _ = stop.cancelled() => return End::Stopped,
            _ = gone.closed() => return End::Broken { was_ready: false },
            r = tokio::time::timeout(READY_TIMEOUT, self.ready()) => r,
        };
        let (role, uses) = match ready {
            Ok(Ok(ready)) => ready,
            Ok(Err(end)) => return end,
            Err(_) => return self.late(),
        };
        let uses = match self.checked(uses) {
            Ok(uses) => uses,
            Err(end) => return end,
        };
        self.take(role, &uses);
        let _cmdif = self.command_interface(role, &uses, stop);
        self.serve(stop, &gone).await
    }

    /// After `ready`: scan cycles and module messages until something ends the session.
    async fn serve(&mut self, stop: &CancellationToken, gone: &mpsc::Sender<ToModule>) -> End {
        let mut tick = tokio::time::interval(self.spec.cycle.max(Duration::from_millis(1)));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let end = tokio::select! {
                _ = stop.cancelled() => Some(End::Stopped),
                _ = gone.closed() => Some(End::Broken { was_ready: true }),
                _ = tick.tick() => {
                    self.on_tick();
                    None
                }
                m = self.next() => self.on_msg(m),
            };
            if let Some(end) = end {
                return end;
            }
        }
    }

    fn late(&self) -> End {
        tracing::warn!(
            "module {}: no ready within {READY_TIMEOUT:?}",
            self.spec.name
        );
        End::Broken { was_ready: false }
    }

    /// Wait for `ready`; `log` and `error` are taken on the way.
    async fn ready(&mut self) -> Result<(Role, Vec<Use>), End> {
        loop {
            match self.next().await {
                Some(FromModule::Ready { role, uses }) => return Ok((role, uses)),
                Some(m) => {
                    if let Some(end) = self.aside(m) {
                        return Err(end);
                    }
                }
                None => return Err(End::Broken { was_ready: false }),
            }
        }
    }

    /// The next message that decodes; `None` at end of stream. A line that does not decode is logged and dropped.
    async fn next(&mut self) -> Option<FromModule> {
        loop {
            match self.lines.next().await?.and_then(|l| decode(&l)) {
                Ok(m) => return Some(m),
                Err(e) => tracing::warn!("module {}: line dropped: {}", self.spec.name, show(&e)),
            }
        }
    }

    /// A message other than `write` after `ready`, or other than `ready` before: `Some` ends the session.
    fn aside(&self, m: FromModule) -> Option<End> {
        let name = &self.spec.name;
        match m {
            FromModule::Log { level, msg } => log(name, &level, &msg),
            FromModule::Error(msg) => {
                tracing::error!("module {name}: {}", msg.escape_debug());
                return Some(End::Refused(msg));
            }
            other => tracing::warn!("module {name}: {other:?} ignored"),
        }
        None
    }

    /// The `uses` the store has with that kind. Another is a warning and skipped, or under `--strict` the end of the session.
    fn checked(&self, uses: Vec<Use>) -> Result<Vec<Use>, End> {
        let mut ok = vec![];
        for u in uses {
            let why = match self.store.kind(&u.tag) {
                Some(k) if k == u.kind => {
                    ok.push(u);
                    continue;
                }
                Some(_) => "wrong kind",
                None => "unknown tag",
            };
            let msg = format!("tag `{}`: {why}", show(&u.tag));
            if self.spec.strict {
                tracing::error!("module {}: {msg}", self.spec.name);
                return Err(End::Refused(msg));
            }
            tracing::warn!("module {}: {msg}, skipped", self.spec.name);
        }
        Ok(ok)
    }

    /// Set up the role's rules. An output starts from the value `hello` gave the module.
    fn take(&mut self, role: Role, uses: &[Use]) {
        self.client = role == Role::Client;
        if !self.client {
            return;
        }
        for u in uses {
            let Dir::Out = u.dir else {
                self.ins.insert(u.tag.clone());
                continue;
            };
            let was = self.last.iter().find(|(t, _)| *t == u.tag);
            let out = Out::new(was.map_or(zero(u.kind), |(_, v)| *v));
            self.outs.insert(u.tag.clone(), out);
        }
    }

    /// A client's `<command-interface>` over its tags, served until the session ends.
    fn command_interface(
        &self,
        role: Role,
        uses: &[Use],
        stop: &CancellationToken,
    ) -> Option<DropGuard> {
        let ep = self.spec.cmdif.clone().filter(|_| role == Role::Client)?;
        let token = stop.child_token();
        let tags = uses.iter().map(|u| (u.tag.clone(), u.dir == Dir::Out));
        let points = TagPoints::new(self.store.clone(), tags);
        let serving = token.clone();
        tokio::spawn(async move { cmdif::serve(&ep, points, serving).await });
        Some(token.drop_guard())
    }

    /// One scan cycle: what changed in the store since the last, an output's change as its echo rule decides.
    fn on_tick(&mut self) {
        let now = self.store.snapshot();
        let mut ch: Vec<_> = moved(&now, &self.last)
            .filter(|(t, _)| !self.outs.contains_key(t))
            .cloned()
            .collect();
        self.last = now;
        self.expire();
        let until = Instant::now() + SENT_TIMEOUT;
        for (tag, out) in &mut self.outs {
            if let Some(v) = out.due(&self.store, tag, |v| v) {
                self.inflight.insert(tag.clone(), until);
                ch.push((tag.clone(), v));
            }
        }
        self.queue(ch);
    }

    /// Output writes with no `sent` in time count as failed, so they are due again.
    fn expire(&mut self) {
        let (now, outs, name) = (Instant::now(), &mut self.outs, &self.spec.name);
        self.inflight.retain(|tag, until| {
            let late = *until <= now;
            if let Some(out) = outs.get_mut(tag).filter(|_| late) {
                tracing::warn!("module {name}: no sent for `{tag}` within {SENT_TIMEOUT:?}");
                out.echo.sent_failed();
            }
            !late
        });
    }

    /// The module's peer answered the write of an output: the echo rule takes it as sent, or as due again.
    fn on_sent(&mut self, tag: &str, v: Value, ok: bool) {
        let out = self.inflight.remove(tag).and(self.outs.get_mut(tag));
        match (out, ok) {
            (Some(out), true) => out.echo.sent_ok(v),
            (Some(out), false) => out.echo.sent_failed(),
            (None, _) => tracing::warn!(
                "module {}: sent for `{}`, which has no write under way, ignored",
                self.spec.name,
                show(tag)
            ),
        }
    }

    fn queue(&self, ch: Vec<(String, Value)>) {
        self.pending.send_if_modified(|p| {
            for (t, v) in &ch {
                p.seq += 1;
                p.tags.insert(t.clone(), (p.seq, *v));
            }
            !ch.is_empty()
        });
    }

    fn on_msg(&mut self, m: Option<FromModule>) -> Option<End> {
        match m {
            None => Some(End::Broken { was_ready: true }),
            Some(FromModule::Write { id, tag, value }) => {
                self.on_write(id, &tag, value);
                None
            }
            Some(FromModule::Sent { tag, value, ok }) => {
                self.on_sent(&tag, value, ok);
                None
            }
            Some(m) => self.aside(m),
        }
    }

    /// A write answered with `ack` or `reject`. A module that does not read its answers loses them, not the session.
    fn on_write(&mut self, id: u64, tag: &str, v: Value) {
        let done = match self.client {
            true => self.client_write(tag, v),
            false => self.store.write(tag, v).map_err(refusal),
        };
        let answer = match done {
            Ok(()) => ToModule::Ack(id),
            Err(why) => ToModule::Reject(id, why.into()),
        };
        let full = self.answers.try_send(answer).is_err();
        if full && !self.stalled {
            tracing::warn!(
                "module {}: not reading, answers dropped until it does",
                self.spec.name
            );
        }
        self.stalled = full;
    }

    /// A client's polled value: an input into its tag, an output through its echo rule (a local change it runs into is sent).
    fn client_write(&mut self, tag: &str, v: Value) -> Result<(), &'static str> {
        if !self.ins.contains(tag) && !self.outs.contains_key(tag) {
            return Err("unknown tag");
        }
        if self.store.kind(tag) != Some(kind_of(v)) {
            return Err("wrong kind");
        }
        let Some(out) = self.outs.get_mut(tag) else {
            input_polled(&self.store, tag, v);
            return Ok(());
        };
        if let Some(local) = out.polled(&self.store, tag, v, v, |v| v) {
            self.inflight
                .insert(tag.into(), Instant::now() + SENT_TIMEOUT);
            self.queue(vec![(tag.into(), local)]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::msg::MAX_LINE;
    use super::*;
    use crate::fd::store::Layout;
    use crate::model::{Kind, Value};
    use serde_json::{json, Value as J};
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    const CYCLE: Duration = Duration::from_millis(10);

    /// `a` binary and `b` analog, each a tag over the point of the same name.
    fn store() -> Store {
        Store::new(Layout {
            points: vec![
                ("a".into(), Kind::Binary, None),
                ("b".into(), Kind::Analog, None),
            ],
            external: vec![("a".into(), "a".into()), ("b".into(), "b".into())],
            internal: vec![],
        })
    }

    fn spec(strict: bool) -> Spec {
        Spec {
            name: "m".into(),
            device: "rtu-1".into(),
            strict,
            config: "<x/>".into(),
            cycle: CYCLE,
            cmdif: None,
        }
    }

    /// The module's end of a running session.
    struct Module {
        lines: Lines<BufReader<DuplexStream>>,
        w: DuplexStream,
        stop: CancellationToken,
        end: JoinHandle<End>,
    }

    fn start(spec: Spec, store: Store) -> Module {
        let (dev_r, mod_w) = duplex(1 << 20);
        let (mod_r, dev_w) = duplex(1 << 20);
        let stop = CancellationToken::new();
        let token = stop.clone();
        let end = tokio::spawn(async move {
            session(BufReader::new(dev_r), dev_w, &spec, store, token).await
        });
        Module {
            lines: BufReader::new(mod_r).lines(),
            w: mod_w,
            stop,
            end,
        }
    }

    impl Module {
        async fn send(&mut self, line: &str) {
            self.w.write_all(line.as_bytes()).await.unwrap();
            self.w.write_all(b"\n").await.unwrap();
        }

        async fn recv(&mut self) -> J {
            let line = timeout(Duration::from_secs(5), self.lines.next_line()).await;
            serde_json::from_str(&line.unwrap().unwrap().unwrap()).unwrap()
        }

        /// Nothing arrives for three cycles.
        async fn quiet(&mut self) {
            let line = timeout(3 * CYCLE, self.lines.next_line()).await;
            assert!(line.is_err(), "unexpected line {line:?}");
        }

        /// Read `hello`, answer `ready` with `uses`; returns the `hello`.
        async fn ready(&mut self, role: &str, uses: &str) -> J {
            let hello = self.recv().await;
            self.send(&format!(
                r#"{{"t":"ready","role":"{role}","uses":[{uses}]}}"#
            ))
            .await;
            hello
        }

        /// The output `b` changes to 3.0 locally, and the `values` carrying it arrives.
        async fn local_change(&mut self, s: &Store) {
            s.publish("b", 3.0, 1);
            assert_eq!(
                self.recv().await,
                json!({"t":"values","changes":[["b",3.0]]})
            );
        }

        /// The peer answered the write of `b` = 3.0.
        async fn sent(&mut self, ok: bool) {
            self.send(&json!({"t":"sent","tag":"b","value":3.0,"ok":ok}).to_string())
                .await;
        }

        /// A `write` and the device's answer to it.
        async fn write(&mut self, id: u64, tag: &str, value: J) -> J {
            self.send(&json!({"t":"write","id":id,"tag":tag,"value":value}).to_string())
                .await;
            self.recv().await
        }
    }

    /// A client session with `a` an input and `b` an output.
    async fn client(s: &Store) -> Module {
        let mut m = start(spec(false), s.clone());
        let uses =
            r#"{"tag":"a","kind":"binary","dir":"in"},{"tag":"b","kind":"analog","dir":"out"}"#;
        m.ready("client", uses).await;
        m
    }

    fn reject(id: u64, err: &str) -> J {
        json!({"t":"reject","id":id,"err":err})
    }

    #[tokio::test]
    async fn hello_then_values_on_change() {
        let s = store();
        let mut m = start(spec(false), s.clone());
        let hello = m.ready("server", "").await;
        assert_eq!(
            hello,
            json!({"t":"hello","v":1,"device":"rtu-1","opts":{"strict":false},"config":"<x/>",
                "points":[{"tag":"a","kind":"binary","value":false},{"tag":"b","kind":"analog","value":0.0}]})
        );
        s.publish("b", 3.0, 1);
        assert_eq!(m.recv().await, json!({"t":"values","changes":[["b",3.0]]}));
        m.quiet().await;
        m.stop.cancel();
        assert_eq!(m.recv().await, json!({"t":"stop"}));
        assert_eq!(m.end.await.unwrap(), End::Stopped);
    }

    #[tokio::test]
    async fn server_write_is_acked_or_rejected() {
        let s = store();
        let mut m = start(spec(false), s.clone());
        m.ready("server", "").await;
        assert_eq!(m.write(1, "b", json!(4.0)).await, json!({"t":"ack","id":1}));
        assert_eq!(s.queued("b"), Some(Value::Analog(4.0)));
        assert_eq!(m.write(2, "zz", json!(1.0)).await, reject(2, "unknown tag"));
        assert_eq!(m.write(3, "a", json!(1.5)).await, reject(3, "wrong kind"));
        drop(m.w);
        assert_eq!(m.end.await.unwrap(), End::Broken { was_ready: true });
    }

    #[tokio::test]
    async fn strict_refuses_unknown_tags() {
        let zz = r#"{"tag":"zz","kind":"analog","dir":"in"}"#;
        let mut m = start(spec(true), store());
        m.ready("server", zz).await;
        assert_eq!(m.recv().await, json!({"t":"stop"}));
        assert!(matches!(m.end.await.unwrap(), End::Refused(why) if why.contains("zz")));

        let mut m = start(spec(false), store());
        m.ready("server", zz).await;
        assert_eq!(m.write(1, "b", json!(1.0)).await, json!({"t":"ack","id":1}));
        m.send(r#"{"t":"error","msg":"bind"}"#).await;
        assert_eq!(m.end.await.unwrap(), End::Refused("bind".into()));
    }

    #[tokio::test(start_paused = true)]
    async fn no_ready_in_time_is_broken() {
        let mut m = start(spec(false), store());
        let t0 = tokio::time::Instant::now();
        assert_eq!(m.recv().await["t"], "hello");
        assert_eq!(m.end.await.unwrap(), End::Broken { was_ready: false });
        assert!(t0.elapsed() >= READY_TIMEOUT);
    }

    /// Before `ready`: a `log` is taken and the wait goes on, an `error` or the end of the stream ends it.
    #[tokio::test]
    async fn before_ready_log_error_and_end_of_stream() {
        let mut m = start(spec(false), store());
        m.recv().await;
        m.send(r#"{"t":"log","level":"info","msg":"starting"}"#)
            .await;
        m.send(r#"{"t":"error","msg":"bind"}"#).await;
        assert_eq!(m.end.await.unwrap(), End::Refused("bind".into()));

        let mut m = start(spec(false), store());
        m.recv().await;
        drop(m.w);
        assert_eq!(m.end.await.unwrap(), End::Broken { was_ready: false });
    }

    #[tokio::test]
    async fn client_role_uses_the_client_rules() {
        let s = store();
        let mut m = client(&s).await;
        // a poll that brings what the tag holds queues nothing
        assert_eq!(
            m.write(1, "a", json!(false)).await,
            json!({"t":"ack","id":1})
        );
        assert!(!s.is_queued("a"));
        assert_eq!(
            m.write(2, "a", json!(true)).await,
            json!({"t":"ack","id":2})
        );
        assert_eq!(s.queued("a"), Some(Value::Binary(true)));
        // a local change of an output goes down once; the poll that shows it is not sent back
        m.local_change(&s).await;
        m.quiet().await;
        m.sent(true).await;
        assert_eq!(m.write(3, "b", json!(3.0)).await, json!({"t":"ack","id":3}));
        m.quiet().await;
        assert!(!s.is_queued("b"));
        // the peer moves on: its value goes into the tag, and is not sent back once it lands
        assert_eq!(m.write(4, "b", json!(7.0)).await, json!({"t":"ack","id":4}));
        assert_eq!(s.queued("b"), Some(Value::Analog(7.0)));
        s.ack("b", Value::Analog(7.0));
        assert_eq!(s.get("b"), Some(Value::Analog(7.0)));
        m.quiet().await;
        // a tag the module did not list
        assert_eq!(m.write(5, "zz", json!(1.0)).await, reject(5, "unknown tag"));
        assert_eq!(m.write(6, "b", json!(true)).await, reject(6, "wrong kind"));
    }

    /// Fix round 1: polls that still show the old value while the write is under way do not revert the local change.
    #[tokio::test]
    async fn a_stale_poll_does_not_revert_a_write_under_way() {
        let s = store();
        let mut m = client(&s).await;
        m.local_change(&s).await;
        assert_eq!(m.write(1, "b", json!(0.0)).await, json!({"t":"ack","id":1}));
        assert_eq!(m.write(2, "b", json!(0.0)).await, json!({"t":"ack","id":2}));
        assert_ne!(s.queued("b"), Some(Value::Analog(0.0)));
        m.quiet().await;
    }

    /// After `sent`, one poll still showing the old value is let pass; the poll that shows the write settles it.
    #[tokio::test]
    async fn sent_ok_then_poll_settles() {
        let s = store();
        let mut m = client(&s).await;
        m.local_change(&s).await;
        m.sent(true).await;
        assert_eq!(m.write(1, "b", json!(0.0)).await, json!({"t":"ack","id":1}));
        assert!(!s.is_queued("b"));
        assert_eq!(m.write(2, "b", json!(3.0)).await, json!({"t":"ack","id":2}));
        assert!(!s.is_queued("b"));
        m.quiet().await;
    }

    /// No `sent` within `SENT_TIMEOUT`, or `ok: false`: the write failed and goes down again.
    #[tokio::test(start_paused = true)]
    async fn no_sent_in_time_counts_as_failed() {
        let s = store();
        let mut m = client(&s).await;
        m.local_change(&s).await;
        let t0 = tokio::time::Instant::now();
        let again = timeout(2 * SENT_TIMEOUT, m.lines.next_line()).await;
        let again: J = serde_json::from_str(&again.unwrap().unwrap().unwrap()).unwrap();
        assert_eq!(again, json!({"t":"values","changes":[["b",3.0]]}));
        assert!(t0.elapsed() >= SENT_TIMEOUT);
        m.sent(false).await;
        assert_eq!(m.recv().await, json!({"t":"values","changes":[["b",3.0]]}));
        m.sent(true).await;
        m.quiet().await;
    }

    /// A module whose input is gone (the writer cannot write) ends the session.
    #[tokio::test]
    async fn writer_gone_is_broken() {
        let s = store();
        let mut m = start(spec(false), s.clone());
        m.ready("server", "").await;
        drop(m.lines);
        s.publish("b", 3.0, 1);
        assert_eq!(m.end.await.unwrap(), End::Broken { was_ready: true });
    }

    /// Review Focus 1: a line that does not decode, or is over `MAX_LINE`, is dropped and the session goes on.
    #[tokio::test]
    async fn garbage_and_long_lines_are_dropped() {
        let s = store();
        let mut m = start(spec(false), s.clone());
        m.ready("server", "").await;
        for level in ["error", "warn", "info", "debug", "trace", "other"] {
            m.send(&json!({"t":"log","level":level,"msg":"hi\nthere"}).to_string())
                .await;
        }
        m.send("not json").await;
        m.send(r#"{"t":"nope"}"#).await;
        m.send(r#"{"t":"sent","tag":"b","value":1.0,"ok":true}"#)
            .await;
        m.send(r#"{"t":"ready","role":"server","uses":[]}"#).await;
        // a write padded to `len` bytes, newline excluded
        let padded = |id: u64, len: usize| {
            let head = format!(r#"{{"t":"write","id":{id},"tag":"b","value":{id}.0,"pad":""#);
            format!("{head}{}\"}}", "x".repeat(len - head.len() - 2))
        };
        m.send(&padded(7, MAX_LINE)).await;
        assert_eq!(m.recv().await, json!({"t":"ack","id":7}));
        m.send(&padded(8, MAX_LINE + 1)).await;
        m.send(&padded(8, 70_000)).await;
        assert_eq!(m.write(9, "b", json!(2.0)).await, json!({"t":"ack","id":9}));
        assert_eq!(s.queued("b"), Some(Value::Analog(2.0)));
    }
}
