//! HiSLIP (IVI-6.1, tcp/4880) in front of the SCPI instrument: a synchronous channel for messages, an asynchronous one for lock, device
//! clear, status and the like, tied together by a session id. The framing, the synchronous message model (END, MessageIDs, RMT-delivered,
//! device clear) and the async replies are I/O-free (`Reader`, `Sync`, `async_reply`, `Hub`); `serve` is the sockets around them.
//! What is and is not implemented is in docs/field-device-scpi.md. Protocol version 1.0 only, synchronized (not overlapped) mode.

use super::device::{stb_of, Instrument, Scope, Session, Shared};
use crate::fd::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// The maximum message size a client is assumed to have until it says (the minimum every HiSLIP peer supports).
pub const DEFAULT_CLIENT_MAX: u64 = 272;
/// Responses held for one message (its frames may arrive for ever without an END): beyond it the session is ended.
const MAX_PENDING: usize = 16 << 20;
/// A new connection must identify itself within this long.
const INIT_WAIT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_VERSION: u32 = 0x0100;
const VENDOR: u32 = 0x4646;
const FIRST_MESSAGE_ID: u32 = 0xFFFF_FF00;

pub use crate::scpi::hislip::t;
pub use crate::scpi::hislip::{
    error, frame, Fatal, Frame, Reader, HEADER, MAX_MESSAGE, RMT_DELIVERED,
};
use t::*;

/// What a session's two channels share.
pub struct Link {
    pub status: Shared,
    /// The client's maximum message size (header included): what its responses are cut to.
    max_out: AtomicU64,
    /// An AsyncDeviceClear arrived; the synchronous channel acts on it with its next message.
    clear: AtomicBool,
    /// A response has been sent and the client has not said it read it (RMT-delivered).
    undelivered: AtomicBool,
    pub async_up: AtomicBool,
    pub closed: CancellationToken,
}

impl Link {
    fn new(status: Shared) -> Link {
        Link {
            status,
            max_out: AtomicU64::new(DEFAULT_CLIENT_MAX),
            clear: AtomicBool::new(false),
            undelivered: AtomicBool::new(false),
            async_up: AtomicBool::new(false),
            closed: CancellationToken::new(),
        }
    }
}

/// What a message is run against.
pub struct Ctx<'a> {
    pub ins: &'a Instrument,
    pub store: &'a Store,
    pub link: &'a Link,
}

/// The synchronous channel of one session: messages in, response frames out. The IEEE 488.2 conditions are exact here:
/// INTERRUPTED (-410) is a new message while the previous response has not been read (RMT-delivered clear), UNTERMINATED (-420) a
/// message started or the channel closed before the previous one had its END.
pub struct Sync {
    scpi: Session,
    /// The MessageID of the message being collected.
    partial: Option<u32>,
    out: Vec<u8>,
    /// Between AsyncDeviceClear and DeviceClearComplete: messages are dropped.
    discarding: bool,
    /// The MessageID of the last response, which an Interrupted names.
    answered: u32,
}

impl Sync {
    pub fn new(status: Shared) -> Sync {
        Sync {
            scpi: Session::sharing(status).exact(),
            partial: None,
            out: vec![],
            discarding: false,
            answered: FIRST_MESSAGE_ID,
        }
    }

    /// The frames to send for a message from the client.
    pub fn recv(&mut self, f: Frame, c: &Ctx) -> Result<Vec<Frame>, Fatal> {
        if c.link.clear.swap(false, SeqCst) {
            self.start_clear(c);
        }
        match f.ty {
            DATA | DATA_END => self.data(f, c),
            TRIGGER => {
                delivered(c.link, f.ctl);
                Ok(vec![])
            }
            DEVICE_CLEAR_COMPLETE => Ok(vec![self.clear_complete()]),
            INITIALIZE => Err(Fatal(3, "invalid initialization sequence")),
            _ => Ok(vec![error(1, "unrecognized message type")]),
        }
    }

    /// The channel closed: a message without its END is unterminated.
    pub fn close(&mut self) {
        self.scpi.abandon();
    }

    fn data(&mut self, f: Frame, c: &Ctx) -> Result<Vec<Frame>, Fatal> {
        if self.discarding {
            return Ok(vec![]);
        }
        delivered(c.link, f.ctl);
        let mut frames = vec![];
        if self.partial.is_some_and(|id| id != f.param) {
            self.scpi.abandon();
            self.out.clear();
        }
        if self.partial.is_none() && c.link.undelivered.swap(false, SeqCst) {
            self.scpi.push_error(-410, "");
            frames.push(frame(INTERRUPTED, 0, self.answered, &[]));
        }
        self.partial = Some(f.param);
        self.out.extend(self.scpi.feed(c.ins, c.store, &f.payload));
        if self.out.len() > MAX_PENDING {
            return Err(Fatal(0, "responses held for one message are too large"));
        }
        if f.ty == DATA_END {
            self.out.extend(self.scpi.end(c.ins, c.store));
            self.partial = None;
            frames.extend(self.answer(f.param, c.link));
        }
        Ok(frames)
    }

    /// The response of message `id`, in frames no longer than the client's maximum message size; the last is the DataEND.
    fn answer(&mut self, id: u32, link: &Link) -> Vec<Frame> {
        if self.out.is_empty() {
            return vec![];
        }
        link.undelivered.store(true, SeqCst);
        self.answered = id;
        let step = (link.max_out.load(SeqCst) as usize).saturating_sub(HEADER);
        let out = std::mem::take(&mut self.out);
        let mut chunks = out.chunks(step).peekable();
        let mut frames = vec![];
        while let Some(chunk) = chunks.next() {
            let ty = if chunks.peek().is_some() {
                DATA
            } else {
                DATA_END
            };
            frames.push(frame(ty, 0, id, chunk));
        }
        frames
    }

    fn start_clear(&mut self, c: &Ctx) {
        self.scpi.clear_input();
        self.partial = None;
        self.out.clear();
        c.link.undelivered.store(false, SeqCst);
        self.discarding = true;
    }

    fn clear_complete(&mut self) -> Frame {
        match std::mem::replace(&mut self.discarding, false) {
            true => frame(DEVICE_CLEAR_ACK, 0, 0, &[]),
            false => error(0, "no device clear is in progress"),
        }
    }
}

fn delivered(link: &Link, ctl: u8) {
    if ctl & RMT_DELIVERED != 0 {
        link.undelivered.store(false, SeqCst);
    }
}

/// The reply to a message on the asynchronous channel other than the lock ones (`Hub`).
pub fn async_reply(f: &Frame, link: &Link) -> Frame {
    match f.ty {
        ASYNC_STATUS_QUERY => {
            delivered(link, f.ctl);
            let stb = stb_of(&link.status, link.undelivered.load(SeqCst));
            frame(ASYNC_STATUS_RESPONSE, stb, 0, &[])
        }
        ASYNC_MAX_SIZE => max_size(f, link),
        ASYNC_REMOTE_LOCAL => frame(ASYNC_REMOTE_LOCAL_RESPONSE, 0, 0, &[]),
        ASYNC_DEVICE_CLEAR => {
            link.clear.store(true, SeqCst);
            frame(ASYNC_DEVICE_CLEAR_ACK, 0, 0, &[])
        }
        _ => error(1, "unrecognized message type"),
    }
}

fn max_size(f: &Frame, link: &Link) -> Frame {
    match <[u8; 8]>::try_from(f.payload.as_slice()) {
        Ok(b) => {
            let size = u64::from_be_bytes(b).max(DEFAULT_CLIENT_MAX);
            link.max_out.store(size, SeqCst);
            frame(ASYNC_MAX_SIZE_RESPONSE, 0, 0, &MAX_MESSAGE.to_be_bytes())
        }
        Err(_) => error(2, "a maximum message size is 8 bytes"),
    }
}

// ---- the sessions of one server --------------------------------------------------------------------------------------------

struct Sessions {
    next: u16,
    links: HashMap<u16, Arc<Link>>,
    /// The session holding the lock: a single exclusive lock for the instrument (shared locks are granted as exclusive ones).
    owner: Option<u16>,
}

/// The sessions of one HiSLIP server and the one lock.
pub struct Hub {
    inner: Mutex<Sessions>,
    freed: Notify,
    scope: Scope,
    global: Shared,
}

impl Hub {
    pub fn new(scope: Scope, global: Shared) -> Hub {
        Hub {
            inner: Mutex::new(Sessions {
                next: 1,
                links: HashMap::new(),
                owner: None,
            }),
            freed: Notify::new(),
            scope,
            global,
        }
    }

    fn state(&self) -> MutexGuard<'_, Sessions> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new session: its id and link; `None` when all 65535 ids are in use.
    pub fn open(&self) -> Option<(u16, Arc<Link>)> {
        let mut s = self.state();
        let id = (0..=u16::MAX)
            .map(|i| s.next.wrapping_add(i))
            .find(|id| *id != 0 && !s.links.contains_key(id))?;
        s.next = id.wrapping_add(1);
        let link = Arc::new(Link::new(self.scope.status(&self.global)));
        s.links.insert(id, link.clone());
        Some((id, link))
    }

    /// The link of session `id` for its asynchronous channel, once.
    pub fn attach(&self, id: u16) -> Option<Arc<Link>> {
        let link = self.state().links.get(&id)?.clone();
        (!link.async_up.swap(true, SeqCst)).then_some(link)
    }

    /// The session is over: its lock is released and its channels told to end.
    pub fn close(&self, id: u16) {
        let link = {
            let mut s = self.state();
            if s.owner == Some(id) {
                s.owner = None;
            }
            s.links.remove(&id)
        };
        if let Some(l) = link {
            l.closed.cancel();
        }
        self.freed.notify_waiters();
    }

    /// Take the lock for `id` if it is free or already `id`'s.
    pub fn try_lock(&self, id: u16) -> bool {
        let mut s = self.state();
        *s.owner.get_or_insert(id) == id
    }

    /// Release `id`'s lock; false when it holds none.
    pub fn unlock(&self, id: u16) -> bool {
        let released = {
            let mut s = self.state();
            (s.owner == Some(id)).then(|| s.owner = None).is_some()
        };
        self.freed.notify_waiters();
        released
    }

    /// Another session holds the lock.
    fn blocked(&self, id: u16) -> bool {
        self.state().owner.is_some_and(|o| o != id)
    }

    /// AsyncLockInfoResponse: control 1 when a lock is held, the number of locks held as the parameter.
    pub fn lock_info(&self) -> Frame {
        let held = u8::from(self.state().owner.is_some());
        frame(ASYNC_LOCK_INFO_RESPONSE, held, u32::from(held), &[])
    }

    /// Wait until no other session holds the lock.
    async fn wait_unblocked(&self, id: u16) {
        loop {
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if !self.blocked(id) {
                return;
            }
            freed.await;
        }
    }

    /// AsyncLock: a request waits up to the timeout (the message parameter, in ms) for the lock; a release names no one else's.
    pub async fn lock(&self, id: u16, f: &Frame) -> Frame {
        let code = match f.ctl {
            0 => 3 - 2 * u8::from(self.unlock(id)),
            _ => u8::from(
                self.request(id, Duration::from_millis(f.param.into()))
                    .await,
            ),
        };
        frame(ASYNC_LOCK_RESPONSE, code, 0, &[])
    }

    async fn request(&self, id: u16, wait: Duration) -> bool {
        let got = tokio::time::timeout(wait, async {
            loop {
                let freed = self.freed.notified();
                tokio::pin!(freed);
                freed.as_mut().enable();
                if self.try_lock(id) {
                    return;
                }
                freed.await;
            }
        });
        got.await.is_ok()
    }
}

// ---- the sockets ------------------------------------------------------------------------------------------------------------

/// Read until a whole frame is in `rd`; `None` when the peer goes, `wait` passes or the read fails. A malformed header is a
/// `Fatal` for the caller to send.
async fn next(rd: &mut Reader, s: &mut TcpStream, wait: Duration) -> Result<Option<Frame>, Fatal> {
    let mut buf = [0u8; 4096];
    loop {
        if let Some(f) = rd.take()? {
            return Ok(Some(f));
        }
        match tokio::time::timeout(wait, s.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => rd.push(&buf[..n]),
            _ => return Ok(None),
        }
    }
}

async fn send(s: &mut TcpStream, frames: &[Frame]) -> bool {
    let bytes: Vec<u8> = frames.iter().flat_map(Frame::encode).collect();
    matches!(
        tokio::time::timeout(WRITE_TIMEOUT, s.write_all(&bytes)).await,
        Ok(Ok(()))
    )
}

/// What a connection needs: the instrument, its store, the sessions and when to give up on a silent client.
pub struct Server {
    pub ins: Arc<Instrument>,
    pub store: Store,
    pub hub: Hub,
    pub idle: Duration,
    pub stop: CancellationToken,
}

impl Server {
    /// One connection, whichever channel it turns out to be. The socket closes when this returns.
    pub async fn serve(&self, mut s: TcpStream) {
        crate::fd::net::keepalive(&s);
        let mut rd = Reader::default();
        let first = match next(&mut rd, &mut s, INIT_WAIT).await {
            Ok(Some(f)) => f,
            Ok(None) => return,
            Err(e) => {
                send(&mut s, &[e.frame()]).await;
                return;
            }
        };
        let done = match first.ty {
            INITIALIZE => self.sync_channel(&mut s, rd, &first).await,
            ASYNC_INITIALIZE => self.async_channel(&mut s, rd, &first).await,
            _ => Some(Fatal(3, "invalid initialization sequence")),
        };
        if let Some(e) = done {
            send(&mut s, &[e.frame()]).await;
        }
    }

    /// Initialize: a session is born; then its messages until the client or the instrument ends it.
    async fn sync_channel(&self, s: &mut TcpStream, mut rd: Reader, init: &Frame) -> Option<Fatal> {
        if !matches!(init.payload.as_slice(), b"" | b"hislip0") {
            return Some(Fatal(0, "no such HiSLIP instrument"));
        }
        let Some((id, link)) = self.hub.open() else {
            return Some(Fatal(4, "too many sessions"));
        };
        let reply = frame(
            INITIALIZE_RESPONSE,
            0,
            SERVER_VERSION << 16 | u32::from(id),
            &[],
        );
        let fatal = match send(s, &[reply]).await {
            true => self.messages(s, &mut rd, id, &link).await,
            false => None,
        };
        self.hub.close(id);
        fatal
    }

    async fn messages(
        &self,
        s: &mut TcpStream,
        rd: &mut Reader,
        id: u16,
        link: &Arc<Link>,
    ) -> Option<Fatal> {
        let mut sync = Sync::new(link.status.clone());
        let c = Ctx {
            ins: &self.ins,
            store: &self.store,
            link,
        };
        let fatal = loop {
            let f = tokio::select! {
                _ = self.stop.cancelled() => break None,
                _ = link.closed.cancelled() => break None,
                f = next(rd, s, self.idle) => match f { Ok(Some(f)) => f, Ok(None) => break None, Err(e) => break Some(e) },
            };
            if !link.async_up.load(SeqCst) {
                break Some(Fatal(2, "the asynchronous channel is not established"));
            }
            self.hub.wait_unblocked(id).await;
            match sync.recv(f, &c) {
                Ok(out) if send(s, &out).await => {}
                Ok(_) => break None,
                Err(e) => break Some(e),
            }
        };
        sync.close();
        fatal
    }

    /// AsyncInitialize: the second channel of a session; then its requests until the session ends.
    async fn async_channel(
        &self,
        s: &mut TcpStream,
        mut rd: Reader,
        init: &Frame,
    ) -> Option<Fatal> {
        let id = (init.param & 0xFFFF) as u16;
        let Some(link) = self.hub.attach(id) else {
            return Some(Fatal(
                3,
                "no such session, or its asynchronous channel is up",
            ));
        };
        if !send(s, &[frame(ASYNC_INITIALIZE_RESPONSE, 0, VENDOR, &[])]).await {
            self.hub.close(id);
            return None;
        }
        let fatal = loop {
            let f = tokio::select! {
                _ = self.stop.cancelled() => break None,
                _ = link.closed.cancelled() => break None,
                f = next(&mut rd, s, Duration::MAX) => match f { Ok(Some(f)) => f, Ok(None) => break None, Err(e) => break Some(e) },
            };
            let reply = match f.ty {
                ASYNC_LOCK => self.hub.lock(id, &f).await,
                ASYNC_LOCK_INFO => self.hub.lock_info(),
                _ => async_reply(&f, &link),
            };
            if !send(s, &[reply]).await {
                break None;
            }
        };
        self.hub.close(id);
        fatal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::proto::point::Point;
    use crate::fd::proto::scpi::device::{shared, Route};
    use crate::fd::store::Layout;
    use crate::model::{Kind, Value};

    fn ins() -> Instrument {
        Instrument {
            idn: "ferrofire,rtu,0,1".into(),
            routes: vec![Route {
                point: Point {
                    tag: "lvl".into(),
                    kind: Kind::Analog,
                    writable: true,
                    min: None,
                    max: None,
                },
                read: vec!["MEASure".into(), "SCALar".into(), "LVL".into()],
                write: Some(vec!["SOURce".into(), "LVL".into()]),
                default: Some(5.0),
            }],
            max_line: 1024,
        }
    }

    struct Rig {
        ins: Instrument,
        store: Store,
        link: Link,
        sync: Sync,
    }

    fn rig() -> Rig {
        let st = shared();
        Rig {
            ins: ins(),
            store: Store::new(Layout {
                points: vec![],
                external: vec![],
                internal: vec![("lvl".into(), Value::Analog(2.5))],
            }),
            link: Link::new(st.clone()),
            sync: Sync::new(st),
        }
    }

    impl Rig {
        fn recv(&mut self, f: Frame) -> Vec<Frame> {
            let c = Ctx {
                ins: &self.ins,
                store: &self.store,
                link: &self.link,
            };
            self.sync.recv(f, &c).unwrap()
        }
        /// A DataEND with the RMT-delivered bit as a client that reads everything sets it.
        fn query(&mut self, id: u32, text: &str) -> Vec<Frame> {
            self.recv(frame(DATA_END, RMT_DELIVERED, id, text.as_bytes()))
        }
        fn errors(&mut self) -> String {
            let out = self.query(1, "SYST:ERR?");
            String::from_utf8(out[0].payload.clone()).unwrap()
        }
    }

    const ID: u32 = FIRST_MESSAGE_ID;

    #[test]
    fn a_query_is_answered_in_one_dataend_with_its_message_id() {
        let mut r = rig();
        assert_eq!(
            r.query(ID, "*IDN?"),
            [frame(DATA_END, 0, ID, b"ferrofire,rtu,0,1\n")]
        );
        assert_eq!(r.query(ID + 2, "*CLS"), [], "a command has no response");
        assert_eq!(r.recv(frame(TRIGGER, 1, ID + 4, &[])), []);
    }

    #[test]
    fn a_message_in_several_data_frames_is_run_at_its_end() {
        let mut r = rig();
        assert_eq!(r.recv(frame(DATA, 0, ID, b"*ID")), []);
        assert_eq!(r.recv(frame(DATA, 0, ID, b"N?;*OPC")), []);
        assert_eq!(
            r.recv(frame(DATA_END, 0, ID, b"?")),
            [frame(DATA_END, 0, ID, b"ferrofire,rtu,0,1;1\n")]
        );
        assert_eq!(
            r.errors(),
            "0,\"No error\"\n",
            "END ended the last line: no -420"
        );
    }

    #[test]
    fn responses_are_cut_to_the_clients_maximum_message_size() {
        let mut r = rig();
        let big = ["*IDN?"; 80].join(";");
        let out = r.query(ID, &big);
        let want = ["ferrofire,rtu,0,1"; 80].join(";") + "\n";
        let whole: Vec<u8> = out.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(whole, want.as_bytes());
        assert!(out.len() > 4 && out.iter().all(|f| f.payload.len() <= 256));
        assert!(out[..out.len() - 1]
            .iter()
            .all(|f| f.ty == DATA && f.param == ID));
        assert_eq!(out.last().unwrap().ty, DATA_END);
        let a = async_reply(
            &frame(ASYNC_MAX_SIZE, 0, 0, &1000u64.to_be_bytes()),
            &r.link,
        );
        assert_eq!(
            a,
            frame(ASYNC_MAX_SIZE_RESPONSE, 0, 0, &MAX_MESSAGE.to_be_bytes())
        );
        let out = r.query(ID + 2, &big);
        assert_eq!(
            out.len(),
            2,
            "{} bytes in frames of at most 984",
            whole.len()
        );
        assert_eq!(out[0].payload.len(), 984);
        let small = async_reply(&frame(ASYNC_MAX_SIZE, 0, 0, &1u64.to_be_bytes()), &r.link);
        assert_eq!(small.ty, ASYNC_MAX_SIZE_RESPONSE);
        assert_eq!(
            r.link.max_out.load(SeqCst),
            DEFAULT_CLIENT_MAX,
            "never below the minimum"
        );
        let bad = async_reply(&frame(ASYNC_MAX_SIZE, 0, 0, b"123"), &r.link);
        assert_eq!(bad.ty, ERROR);
        assert_eq!(bad.ctl, 2);
    }

    #[test]
    fn a_new_message_before_the_response_was_read_interrupts_it() {
        let mut r = rig();
        r.recv(frame(DATA_END, 0, ID, b"*IDN?"));
        let out = r.recv(frame(DATA_END, 0, ID + 2, b"*OPC?"));
        assert_eq!(
            out,
            [
                frame(INTERRUPTED, 0, ID, &[]),
                frame(DATA_END, 0, ID + 2, b"1\n")
            ]
        );
        assert_eq!(r.errors(), "-410,\"Query INTERRUPTED\"\n");
        let out = r.recv(frame(DATA_END, RMT_DELIVERED, ID + 4, b"*OPC?"));
        assert_eq!(
            out,
            [frame(DATA_END, 0, ID + 4, b"1\n")],
            "read first: no interruption"
        );
        let out = r.recv(frame(DATA_END, 0, ID + 6, b"*CLS"));
        assert_eq!(
            out,
            [frame(INTERRUPTED, 0, ID + 4, &[])],
            "the unread response was the one of ID+4"
        );
        let out = r.recv(frame(DATA_END, 0, ID + 8, b"*ESR?"));
        assert_eq!(
            out,
            [frame(DATA_END, 0, ID + 8, b"0\n")],
            "an interruption reads the response away"
        );
    }

    #[test]
    fn a_status_query_with_rmt_delivered_counts_as_reading() {
        let mut r = rig();
        r.recv(frame(DATA_END, 0, ID, b"*IDN?"));
        let s = async_reply(&frame(ASYNC_STATUS_QUERY, 0, ID, &[]), &r.link);
        assert_eq!(
            (s.ty, s.ctl),
            (ASYNC_STATUS_RESPONSE, 16),
            "MAV: a response is waiting"
        );
        let s = async_reply(&frame(ASYNC_STATUS_QUERY, 1, ID, &[]), &r.link);
        assert_eq!(s.ctl, 0, "delivered: nothing waiting");
        let out = r.recv(frame(DATA_END, 0, ID + 2, b"*OPC?"));
        assert_eq!(out.len(), 1, "so the next message interrupts nothing");
        r.recv(frame(DATA_END, 1, ID + 4, b"*FOO"));
        let s = async_reply(&frame(ASYNC_STATUS_QUERY, 0, ID, &[]), &r.link);
        assert_eq!(s.ctl, 4, "the error queue: EAV");
    }

    #[test]
    fn a_message_abandoned_for_another_is_unterminated() {
        let mut r = rig();
        r.recv(frame(DATA, 1, ID, b"*IDN?"));
        r.recv(frame(DATA_END, 1, ID + 2, b"*OPC?"));
        assert_eq!(r.errors(), "-420,\"Query UNTERMINATED\"\n");
        r.recv(frame(DATA, 1, ID + 4, b"*ESE 3"));
        r.sync.close();
        assert_eq!(r.errors(), "0,\"No error\"\n", "a command is not a query");
        r.recv(frame(DATA, 1, ID + 6, b"*IDN?"));
        r.sync.close();
        assert_eq!(
            r.errors(),
            "-420,\"Query UNTERMINATED\"\n",
            "nor is the channel closing"
        );
        r.recv(frame(DATA, 1, ID + 8, b"*ESE 3\n*ES"));
        assert_eq!(
            r.recv(frame(DATA_END, 1, ID + 8, b"E?")),
            [frame(DATA_END, 0, ID + 8, b"3\n")]
        );
    }

    #[test]
    fn a_device_clear_drops_the_message_in_progress_and_ends_with_complete() {
        let mut r = rig();
        r.recv(frame(DATA, 1, ID, b"*IDN?"));
        let a = async_reply(&frame(ASYNC_DEVICE_CLEAR, 0, 0, &[]), &r.link);
        assert_eq!(a, frame(ASYNC_DEVICE_CLEAR_ACK, 0, 0, &[]));
        assert_eq!(
            r.recv(frame(DATA_END, 1, ID + 4, b"*IDN?")),
            [],
            "dropped until DeviceClearComplete"
        );
        assert_eq!(
            r.recv(frame(DEVICE_CLEAR_COMPLETE, 0, 0, &[])),
            [frame(DEVICE_CLEAR_ACK, 0, 0, &[])]
        );
        assert_eq!(r.recv(frame(DEVICE_CLEAR_COMPLETE, 0, 0, &[]))[0].ty, ERROR);
        assert_eq!(
            r.query(FIRST_MESSAGE_ID, "*OPC?"),
            [frame(DATA_END, 0, FIRST_MESSAGE_ID, b"1\n")],
            "no -410, no -420: a clear is not an error"
        );
        assert_eq!(r.errors(), "0,\"No error\"\n");
    }

    #[test]
    fn other_messages_get_errors_and_a_second_initialize_is_fatal() {
        let mut r = rig();
        assert_eq!(
            r.recv(frame(ASYNC_LOCK, 0, 0, &[])),
            [error(1, "unrecognized message type")]
        );
        let c = Ctx {
            ins: &r.ins,
            store: &r.store,
            link: &r.link,
        };
        assert_eq!(
            r.sync.recv(frame(INITIALIZE, 0, 0, &[]), &c),
            Err(Fatal(3, "invalid initialization sequence"))
        );
        assert_eq!(async_reply(&frame(DATA, 0, 0, &[]), &r.link).ty, ERROR);
        assert_eq!(
            async_reply(&frame(ASYNC_REMOTE_LOCAL, 1, 0, &[]), &r.link),
            frame(ASYNC_REMOTE_LOCAL_RESPONSE, 0, 0, &[])
        );
    }

    #[test]
    fn responses_held_for_a_message_without_an_end_are_bounded() {
        let mut r = rig();
        let chunk = "*IDN?\n".repeat(60_000);
        let c = Ctx {
            ins: &r.ins,
            store: &r.store,
            link: &r.link,
        };
        let mut fatal = None;
        for _ in 0..20 {
            if let Err(e) = r.sync.recv(frame(DATA, 0, ID, chunk.as_bytes()), &c) {
                fatal = Some(e);
                break;
            }
        }
        assert_eq!(fatal.map(|f| f.0), Some(0));
    }

    #[tokio::test]
    async fn the_hub_hands_out_ids_attaches_once_and_holds_one_lock() {
        let hub = Hub::new(Scope::Instrument, shared());
        let ((a, la), (b, lb)) = (hub.open().unwrap(), hub.open().unwrap());
        assert_ne!(a, b);
        assert!(
            Arc::ptr_eq(&la.status, &lb.status),
            "instrument scope: one status system"
        );
        assert!(hub.attach(a).is_some() && hub.attach(a).is_none());
        assert!(hub.attach(60000).is_none());
        assert_eq!(hub.lock_info(), frame(ASYNC_LOCK_INFO_RESPONSE, 0, 0, &[]));
        let req = |ms| frame(ASYNC_LOCK, 1, ms, &[]);
        assert_eq!(hub.lock(a, &req(0)).await.ctl, 1);
        assert_eq!(hub.lock(a, &req(0)).await.ctl, 1, "again: still yours");
        assert_eq!(
            hub.lock(b, &req(30)).await.ctl,
            0,
            "held: the request times out"
        );
        assert_eq!(hub.lock_info(), frame(ASYNC_LOCK_INFO_RESPONSE, 1, 1, &[]));
        assert!(hub.blocked(b) && !hub.blocked(a));
        assert_eq!(
            hub.lock(b, &frame(ASYNC_LOCK, 0, 0, &[])).await.ctl,
            3,
            "b has none to release"
        );
        let waiter = {
            let hub = &hub;
            async move { hub.lock(b, &req(5000)).await.ctl }
        };
        let releaser = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            hub.lock(a, &frame(ASYNC_LOCK, 0, 0, &[])).await.ctl
        };
        assert_eq!(
            tokio::join!(waiter, releaser),
            (1, 1),
            "released: the waiting request gets it"
        );
        tokio::time::timeout(Duration::from_secs(1), hub.wait_unblocked(a))
            .await
            .expect_err("b holds it now");
        hub.close(b);
        tokio::time::timeout(Duration::from_secs(1), hub.wait_unblocked(a))
            .await
            .expect("closing releases the lock");
        assert!(lb.closed.is_cancelled() && !la.closed.is_cancelled());
        let per = Hub::new(Scope::Connection, shared());
        let ((_, x), (_, y)) = (per.open().unwrap(), per.open().unwrap());
        assert!(
            !Arc::ptr_eq(&x.status, &y.status),
            "connection scope: one each"
        );
    }

    #[test]
    fn session_ids_skip_zero_and_those_in_use() {
        let hub = Hub::new(Scope::Instrument, shared());
        hub.state().next = u16::MAX;
        let (a, _) = hub.open().unwrap();
        let (b, _) = hub.open().unwrap();
        assert_eq!((a, b), (u16::MAX, 1), "0 is skipped when wrapping");
        hub.state().next = 1;
        assert_eq!(hub.open().unwrap().0, 2, "1 is in use");
        for _ in 0..u16::MAX - 3 {
            hub.open().unwrap();
        }
        assert!(hub.open().is_none(), "all 65535 in use");
    }
}
