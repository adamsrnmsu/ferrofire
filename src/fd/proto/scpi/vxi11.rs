//! VXI-11 (VMEbus Extensions for Instrumentation, TCP/IP instrument protocol) in front of the SCPI instrument: ONC RPC (RFC 5531) with
//! XDR (RFC 4506), a portmapper (program 100000 version 2) that tells a client where the core channel is, and the core channel
//! (program 0x0607AF) with the calls an SCPI client makes. Everything but the sockets is I/O-free: `Records` cuts the TCP stream into RPC
//! records, `Vxi11::call` turns a record into the reply record. The abort program is answered on the core port (a link is aborted by
//! clearing it); the interrupt channel, locks and DOCMD are refused with error 8, "operation not supported".

use super::device::{stb_of, Instrument, Scope, Session, Shared};
use crate::fd::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio_util::sync::CancellationToken;

pub use crate::scpi::rpc::{
    put, put_opaque, record, Records, Xdr, ABORT_PROG, CORE_PROG, PMAP_PROG,
};
const IPPROTO_TCP: u32 = 6;
/// `max_recv_size` of create_link: the most one device_write may carry.
pub const MAX_RECV: u32 = 1 << 20;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

// VXI-11 error codes (VXI-11 table B.2) and read reasons.
const OK: i32 = 0;
const NOT_ACCESSIBLE: i32 = 3;
const BAD_LINK: i32 = 4;
const NOT_SUPPORTED: i32 = 8;
const OUT_OF_RESOURCES: i32 = 9;
const NO_LOCK_HELD: i32 = 12;
const IO_TIMEOUT: i32 = 15;
const OP_END: i32 = 8;
const OP_TERMCHAR_SET: i32 = 128;
const RX_REQCNT: i32 = 1;
const RX_CHR: i32 = 2;
const RX_END: i32 = 4;

// RPC accept_stat values.
const PROG_UNAVAIL: u32 = 1;
const PROG_MISMATCH: u32 = 2;
const PROC_UNAVAIL: u32 = 3;
const GARBAGE_ARGS: u32 = 4;

// ---- RPC replies ----------------------------------------------------------------------------------------------------

fn reply_head(xid: u32, rest: &[u32]) -> Vec<u8> {
    let mut out = vec![];
    [xid, 1].iter().chain(rest).for_each(|x| put(&mut out, *x));
    out
}

/// An accepted reply: AUTH_NONE verifier, the accept_stat and, for a success, the results.
fn accepted(xid: u32, stat: u32, body: &[u8]) -> Vec<u8> {
    let mut out = reply_head(xid, &[0, 0, 0, stat]);
    out.extend(body);
    out
}

fn mismatch(xid: u32, low: u32, high: u32) -> Vec<u8> {
    let mut body = vec![];
    put(&mut body, low);
    put(&mut body, high);
    accepted(xid, PROG_MISMATCH, &body)
}

/// A reply denied for the RPC version: only version 2 exists.
fn denied(xid: u32) -> Vec<u8> {
    reply_head(xid, &[1, 0, 2, 2])
}

fn int(x: i32) -> Vec<u8> {
    x.to_be_bytes().to_vec()
}

fn ints(xs: &[u32]) -> Vec<u8> {
    let mut out = vec![];
    xs.iter().for_each(|x| put(&mut out, *x));
    out
}

type Results = Result<Vec<u8>, u32>;

/// One link of the core channel: an SCPI session, the response not yet read, and whether a message is half written.
struct Link {
    session: Session,
    out: Vec<u8>,
    /// Written without END: the message is not over.
    open: bool,
    conn: u64,
}

struct Links {
    map: HashMap<i32, Link>,
    next: i32,
}

/// The VXI-11 service: the portmapper and the core channel over the same instrument.
pub struct Vxi11 {
    ins: std::sync::Arc<Instrument>,
    store: Store,
    scope: Scope,
    global: Shared,
    core_port: u16,
    max_links: usize,
    links: Mutex<Links>,
    conns: AtomicU64,
}

impl Vxi11 {
    pub fn new(
        ins: std::sync::Arc<Instrument>,
        store: Store,
        (scope, global): (Scope, Shared),
        core_port: u16,
        max_links: usize,
    ) -> Vxi11 {
        Vxi11 {
            ins,
            store,
            scope,
            global,
            core_port,
            max_links,
            links: Mutex::new(Links {
                map: HashMap::new(),
                next: 1,
            }),
            conns: AtomicU64::new(0),
        }
    }

    fn links(&self) -> MutexGuard<'_, Links> {
        self.links.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A connection number for `call`; its links are destroyed by `close`.
    pub fn connection(&self) -> u64 {
        self.conns.fetch_add(1, SeqCst)
    }

    /// The connection is gone: so are the links it made.
    pub fn close(&self, conn: u64) {
        self.links().map.retain(|_, l| l.conn != conn);
    }

    /// The reply record to a call record; `None` for bytes that are not a call at all (nothing is said back).
    pub fn call(&self, conn: u64, rec: &[u8]) -> Option<Vec<u8>> {
        let mut x = Xdr(rec);
        let (xid, kind, rpcvers) = (x.u32()?, x.u32()?, x.u32()?);
        if kind != 0 {
            return None;
        }
        let (prog, vers, proc) = (x.u32()?, x.u32()?, x.u32()?);
        // credentials and verifier: flavor and body, which AUTH_NONE (and AUTH_SYS) callers send and we do not look at
        for _ in 0..2 {
            x.u32()?;
            x.opaque(400)?;
        }
        if rpcvers != 2 {
            return Some(denied(xid));
        }
        let done = match (prog, vers) {
            (PMAP_PROG, 2) => self.portmap(proc, x),
            (CORE_PROG | ABORT_PROG, 1) => self.core(conn, prog, proc, x),
            (PMAP_PROG, _) => return Some(mismatch(xid, 2, 2)),
            (CORE_PROG | ABORT_PROG, _) => return Some(mismatch(xid, 1, 1)),
            _ => Err(PROG_UNAVAIL),
        };
        Some(match done {
            Ok(body) => accepted(xid, 0, &body),
            Err(stat) => accepted(xid, stat, &[]),
        })
    }

    /// The portmapper: GETPORT and DUMP tell where the core channel is, SET and UNSET are refused (false).
    fn portmap(&self, proc: u32, mut x: Xdr) -> Results {
        let core = u32::from(self.core_port);
        match proc {
            0 => Ok(vec![]),
            1 | 2 => Ok(ints(&[0])),
            3 => {
                let (prog, vers, prot) = (x.u32(), x.u32(), x.u32());
                let (Some(prog), Some(vers), Some(prot), Some(_)) = (prog, vers, prot, x.u32())
                else {
                    return Err(GARBAGE_ARGS);
                };
                let ours =
                    matches!(prog, CORE_PROG | ABORT_PROG) && vers == 1 && prot == IPPROTO_TCP;
                Ok(ints(&[if ours { core } else { 0 }]))
            }
            4 => Ok(ints(&[
                1,
                CORE_PROG,
                1,
                IPPROTO_TCP,
                core,
                1,
                ABORT_PROG,
                1,
                IPPROTO_TCP,
                core,
                0,
            ])),
            _ => Err(PROC_UNAVAIL),
        }
    }

    /// The core (and abort) program: `proc` per VXI-11 table B.1. Arguments that do not decode are GARBAGE_ARGS.
    fn core(&self, conn: u64, prog: u32, proc: u32, mut x: Xdr) -> Results {
        if proc == 0 {
            return Ok(vec![]);
        }
        let bad = || GARBAGE_ARGS;
        let mut links = self.links();
        if prog == ABORT_PROG {
            // device_abort(link): the nearest a link has to an abort is a clear
            return match proc {
                1 => with_link(&mut links, (x.i32().ok_or_else(bad)?, proc), |l| {
                    l.clear();
                    Ok(int(OK))
                }),
                _ => Err(PROC_UNAVAIL),
            };
        }
        match proc {
            10 => self.create_link(&mut links, conn, x),
            23 => {
                let id = x.i32().ok_or_else(bad)?;
                Ok(int(if links.map.remove(&id).is_some() {
                    OK
                } else {
                    BAD_LINK
                }))
            }
            11..=22 => {
                let id = x.i32().ok_or_else(bad)?;
                with_link(&mut links, (id, proc), |l| self.on_link(l, proc, x))
            }
            25 | 26 => Ok(int(NOT_SUPPORTED)),
            _ => Err(PROC_UNAVAIL),
        }
    }

    /// create_link(clientId, lockDevice, lock_timeout, device): the device is `inst0`, there are no locks to take.
    fn create_link(&self, links: &mut Links, conn: u64, mut x: Xdr) -> Results {
        let (_client, lock, _timeout, device) = (x.i32(), x.u32(), x.u32(), x.opaque(255));
        let (Some(_), Some(lock), Some(_), Some(device)) = (_client, lock, _timeout, device) else {
            return Err(GARBAGE_ARGS);
        };
        let reply = |e, id| ints(&[e as u32, id, u32::from(self.core_port), MAX_RECV]);
        if !device.eq_ignore_ascii_case(b"inst0") {
            return Ok(reply(NOT_ACCESSIBLE, 0));
        }
        if lock != 0 {
            return Ok(reply(NOT_SUPPORTED, 0));
        }
        if links.map.len() >= self.max_links {
            return Ok(reply(OUT_OF_RESOURCES, 0));
        }
        let id = links.next;
        links.next = id.wrapping_add(1).max(1);
        let session = Session::sharing(self.scope.status(&self.global)).exact();
        let link = Link {
            session,
            out: vec![],
            open: false,
            conn,
        };
        links.map.insert(id, link);
        Ok(reply(OK, id as u32))
    }

    /// The calls on a link, `proc` 11 to 22, with the arguments after the link id.
    fn on_link(&self, l: &mut Link, proc: u32, mut x: Xdr) -> Results {
        match proc {
            11 => self.write(l, x),
            12 => read(l, x),
            13 => Ok(ints(&[
                0,
                u32::from(stb_of(l.session.status(), !l.out.is_empty())),
            ])),
            14 | 16 | 17 => Ok(int(OK)),
            15 => {
                l.clear();
                Ok(int(OK))
            }
            19 => Ok(int(NO_LOCK_HELD)),
            22 => Ok(ints(&[NOT_SUPPORTED as u32, 0])),
            _ => {
                x.u32();
                Ok(int(NOT_SUPPORTED))
            }
        }
    }

    /// device_write(link, io_timeout, lock_timeout, flags, data). A write that starts a message while the response to the last is
    /// unread INTERRUPTS it (-410): the response is dropped. END ends the message and runs its last line.
    fn write(&self, l: &mut Link, mut x: Xdr) -> Results {
        let (_, _, flags, data) = (x.u32(), x.u32(), x.i32(), x.opaque(MAX_RECV as usize));
        let (Some(flags), Some(data)) = (flags, data) else {
            return Err(GARBAGE_ARGS);
        };
        if !l.open && !l.out.is_empty() {
            l.out.clear();
            l.session.push_error(-410, "");
        }
        l.out.extend(l.session.feed(&self.ins, &self.store, data));
        l.open = flags & OP_END == 0;
        if !l.open {
            l.out.extend(l.session.end(&self.ins, &self.store));
        }
        Ok(ints(&[0, data.len() as u32]))
    }
}

/// Run `f` on the link `id`; an unknown id is error 4, in the shape of the reply `proc` has (error, then the zero results).
fn with_link(
    links: &mut Links,
    (id, proc): (i32, u32),
    f: impl FnOnce(&mut Link) -> Results,
) -> Results {
    let results = match proc {
        12 => 2,
        11 | 13 | 22 => 1,
        _ => 0,
    };
    match links.map.get_mut(&id) {
        Some(l) => f(l),
        None => Ok(ints(&[&[BAD_LINK as u32][..], &[0; 2][..results]].concat())),
    }
}

impl Link {
    fn clear(&mut self) {
        self.session.clear_input();
        self.out.clear();
        self.open = false;
    }
}

/// device_read(link, requestSize, io_timeout, lock_timeout, flags, termChar): up to `requestSize` bytes of the response, ending at the
/// termination character when the flags ask. The reason says why it stopped: the size (1), the character (2), the end of the
/// response (4, which is every response's last byte). Reading while a message is half written is UNTERMINATED (-420): the message is
/// dropped and there is nothing to read. Nothing to read is io_timeout (the instrument does not make a client wait).
fn read(l: &mut Link, mut x: Xdr) -> Results {
    let (size, _, _, flags, term) = (x.u32(), x.u32(), x.u32(), x.i32(), x.i32());
    let (Some(size), Some(flags), Some(term)) = (size, flags, term) else {
        return Err(GARBAGE_ARGS);
    };
    if std::mem::take(&mut l.open) {
        l.session.abandon();
    }
    if l.out.is_empty() {
        return Ok(ints(&[IO_TIMEOUT as u32, 0, 0]));
    }
    let want = (size as usize).min(l.out.len());
    let stop = (flags & OP_TERMCHAR_SET != 0)
        .then(|| {
            l.out[..want]
                .iter()
                .position(|b| i32::from(*b) == term & 0xFF)
        })
        .flatten();
    let n = stop.map_or(want, |i| i + 1);
    let data: Vec<u8> = l.out.drain(..n).collect();
    let reason = [
        (stop.is_some(), RX_CHR),
        (l.out.is_empty(), RX_END),
        (stop.is_none() && n == size as usize, RX_REQCNT),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0, |acc, (_, bit)| acc | bit);
    let mut out = ints(&[0, reason as u32]);
    put_opaque(&mut out, &data);
    Ok(out)
}

// ---- the sockets -----------------------------------------------------------------------------------------------------------

/// One TCP connection: RPC records in, replies out, until the client goes, goes quiet or the device stops. Its links die with it.
pub async fn serve_tcp(mut s: TcpStream, svc: &Vxi11, idle: Duration, stop: &CancellationToken) {
    crate::fd::net::keepalive(&s);
    let (conn, mut recs, mut buf) = (svc.connection(), Records::default(), [0u8; 4096]);
    'conn: loop {
        loop {
            match recs.take() {
                Ok(Some(rec)) => {
                    let sent = match svc.call(conn, &rec) {
                        Some(reply) => send(&mut s, &record(&reply)).await,
                        None => true,
                    };
                    if !sent {
                        break 'conn;
                    }
                }
                Ok(None) => break,
                Err(_) => break 'conn,
            }
        }
        let read = tokio::select! {
            _ = stop.cancelled() => break,
            r = tokio::time::timeout(idle, s.read(&mut buf)) => r,
        };
        match read {
            Ok(Ok(n)) if n > 0 => recs.push(&buf[..n]),
            _ => break,
        }
    }
    svc.close(conn);
}

async fn send(s: &mut TcpStream, bytes: &[u8]) -> bool {
    matches!(
        tokio::time::timeout(WRITE_TIMEOUT, s.write_all(bytes)).await,
        Ok(Ok(()))
    )
}

/// The portmapper over UDP: a call datagram in, a reply datagram out. Calls are connection 0's, and make no links to clean up.
pub async fn serve_udp(sock: UdpSocket, svc: &Vxi11, stop: &CancellationToken) {
    let mut buf = vec![0u8; 8192];
    loop {
        let got =
            tokio::select! { _ = stop.cancelled() => return, r = sock.recv_from(&mut buf) => r };
        let Ok((n, from)) = got else { continue };
        // only the portmapper speaks UDP: a link made here would never be closed
        if let Some(reply) = svc.call_udp(&buf[..n]) {
            let _ = sock.send_to(&reply, from).await;
        }
    }
}

impl Vxi11 {
    /// A call that arrived by UDP: the portmapper answers, the core and abort programs are not offered (PROG_UNAVAIL).
    pub fn call_udp(&self, rec: &[u8]) -> Option<Vec<u8>> {
        let mut x = Xdr(rec);
        let (xid, _, _, prog) = (x.u32()?, x.u32()?, x.u32()?, x.u32()?);
        match prog {
            PMAP_PROG => self.call(0, rec),
            _ => Some(accepted(xid, PROG_UNAVAIL, &[])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::proto::point::Point;
    use crate::fd::proto::scpi::device::{shared, Route};
    use crate::fd::store::Layout;
    use crate::model::{Kind, Value};
    use std::sync::Arc;

    fn svc(scope: Scope, max_links: usize) -> Vxi11 {
        let ins = Instrument {
            idn: "ferrofire,rtu,0,1".into(),
            routes: vec![Route {
                point: Point {
                    tag: "lvl".into(),
                    kind: Kind::Analog,
                    writable: false,
                    min: None,
                    max: None,
                },
                read: vec!["MEASure".into(), "SCALar".into(), "LVL".into()],
                write: None,
                default: None,
            }],
            max_line: 64,
        };
        let store = Store::new(Layout {
            points: vec![],
            external: vec![],
            internal: vec![("lvl".into(), Value::Analog(2.5))],
        });
        Vxi11::new(Arc::new(ins), store, (scope, shared()), 4881, max_links)
    }

    fn call_bytes(xid: u32, prog: u32, vers: u32, proc: u32, args: &[u8]) -> Vec<u8> {
        let mut c = ints(&[xid, 0, 2, prog, vers, proc, 0, 0, 0, 0]);
        c.extend(args);
        c
    }

    /// The results of an accepted success reply to xid 7.
    fn ok(reply: Option<Vec<u8>>) -> Vec<u8> {
        let r = reply.expect("a reply");
        assert_eq!(r[..24], ints(&[7, 1, 0, 0, 0, 0])[..], "{r:?}");
        r[24..].to_vec()
    }

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect()
    }

    fn core(s: &Vxi11, proc: u32, args: &[u8]) -> Vec<u8> {
        ok(s.call(1, &call_bytes(7, CORE_PROG, 1, proc, args)))
    }

    fn create(s: &Vxi11) -> u32 {
        let mut a = ints(&[99, 0, 0]);
        put_opaque(&mut a, b"inst0");
        let r = words(&core(s, 10, &a));
        assert_eq!((r[0], r[2], r[3]), (0, 4881, MAX_RECV), "{r:?}");
        r[1]
    }

    fn write(s: &Vxi11, link: u32, flags: u32, data: &[u8]) -> Vec<u32> {
        let mut a = ints(&[link, 0, 0, flags]);
        put_opaque(&mut a, data);
        words(&core(s, 11, &a))
    }

    fn read_reply(s: &Vxi11, link: u32, size: u32, flags: u32, term: u32) -> (Vec<u32>, Vec<u8>) {
        let r = core(s, 12, &ints(&[link, size, 0, 0, flags, term]));
        let len = words(&r)[2] as usize;
        (words(&r[..8]), r[12..12 + len].to_vec())
    }

    #[test]
    fn the_portmapper_says_where_the_core_channel_is() {
        let s = svc(Scope::Instrument, 4);
        let get = |prog, vers, prot| {
            words(&ok(s.call(
                0,
                &call_bytes(7, PMAP_PROG, 2, 3, &ints(&[prog, vers, prot, 0])),
            )))
        };
        assert_eq!(get(CORE_PROG, 1, 6), [4881]);
        assert_eq!(get(ABORT_PROG, 1, 6), [4881]);
        assert_eq!(get(CORE_PROG, 1, 17), [0], "no UDP core channel");
        assert_eq!(get(CORE_PROG, 2, 6), [0]);
        assert_eq!(get(12345, 1, 6), [0]);
        assert!(ok(s.call(0, &call_bytes(7, PMAP_PROG, 2, 0, &[]))).is_empty());
        assert_eq!(
            words(&ok(s.call(0, &call_bytes(7, PMAP_PROG, 2, 1, &[])))),
            [0]
        );
        let dump = words(&ok(s.call(0, &call_bytes(7, PMAP_PROG, 2, 4, &[]))));
        assert_eq!(dump.len(), 11);
        assert_eq!(&dump[..5], &[1, CORE_PROG, 1, 6, 4881]);
        assert_eq!(dump[10], 0, "the list ends");
        assert_eq!(
            words(
                &s.call(0, &call_bytes(7, PMAP_PROG, 2, 3, &ints(&[1])))
                    .unwrap()[20..]
            ),
            [GARBAGE_ARGS]
        );
        assert_eq!(
            words(&s.call(0, &call_bytes(7, PMAP_PROG, 2, 5, &[])).unwrap()[20..]),
            [PROC_UNAVAIL]
        );
    }

    #[test]
    fn rpc_errors_are_the_ones_the_rfc_names() {
        let s = svc(Scope::Instrument, 4);
        let r = s.call(0, &call_bytes(7, PMAP_PROG, 3, 0, &[])).unwrap();
        assert_eq!(
            words(&r),
            [7, 1, 0, 0, 0, PROG_MISMATCH, 2, 2],
            "rpcbind v3 and v4: we are v2"
        );
        let r = s.call(0, &call_bytes(7, CORE_PROG, 2, 0, &[])).unwrap();
        assert_eq!(words(&r)[5..], [PROG_MISMATCH, 1, 1]);
        let r = s.call(0, &call_bytes(7, 999, 1, 0, &[])).unwrap();
        assert_eq!(words(&r)[5], PROG_UNAVAIL);
        let mut c = call_bytes(7, CORE_PROG, 1, 0, &[]);
        c[11] = 3;
        assert_eq!(
            words(&s.call(0, &c).unwrap()),
            [7, 1, 1, 0, 2, 2],
            "RPC version 3: denied"
        );
        assert_eq!(
            s.call(0, &call_bytes(7, CORE_PROG, 1, 99, &[]))
                .map(|r| words(&r)[5]),
            Some(PROC_UNAVAIL)
        );
        let mut reply = call_bytes(7, CORE_PROG, 1, 0, &[]);
        reply[7] = 1;
        assert_eq!(s.call(0, &reply), None, "a reply is not answered");
        assert_eq!(s.call(0, &[0; 11]), None, "nor garbage");
        assert_eq!(
            s.call(0, &call_bytes(7, CORE_PROG, 1, 0, &[])[..30]),
            None,
            "a cut call header"
        );
        let mut c = call_bytes(7, CORE_PROG, 1, 0, &[]);
        c[36..40].copy_from_slice(&1000u32.to_be_bytes());
        assert_eq!(s.call(0, &c), None, "a verifier longer than 400 bytes");
        let a = ok(s.call(0, &call_bytes(7, ABORT_PROG, 1, 0, &[])));
        assert!(a.is_empty());
        assert_eq!(
            s.call(0, &call_bytes(7, ABORT_PROG, 1, 2, &[]))
                .map(|r| words(&r)[5]),
            Some(PROC_UNAVAIL)
        );
    }

    #[test]
    fn a_link_is_created_for_inst0_only_and_destroyed() {
        let s = svc(Scope::Instrument, 2);
        let new = |name: &[u8], lock: u32| {
            let mut a = ints(&[1, lock, 0]);
            put_opaque(&mut a, name);
            words(&core(&s, 10, &a))
        };
        assert_eq!(new(b"inst0", 0)[..2], [0, 1]);
        assert_eq!(
            new(b"INST0", 0)[..2],
            [0, 2],
            "the name is not case sensitive"
        );
        assert_eq!(
            new(b"inst0", 0)[0],
            OUT_OF_RESOURCES as u32,
            "two links at most"
        );
        assert_eq!(new(b"gpib0,3", 0)[0], NOT_ACCESSIBLE as u32);
        assert_eq!(words(&core(&s, 23, &ints(&[1]))), [0]);
        assert_eq!(
            words(&core(&s, 23, &ints(&[1]))),
            [BAD_LINK as u32],
            "already destroyed"
        );
        assert_eq!(new(b"inst0", 1)[0], NOT_SUPPORTED as u32, "no locks");
        assert_eq!(new(b"inst0", 0)[..2], [0, 3]);
        let t = svc(Scope::Instrument, 1);
        assert_eq!(
            t.call(0, &call_bytes(7, CORE_PROG, 1, 10, &ints(&[1])))
                .map(|r| words(&r)[5]),
            Some(GARBAGE_ARGS)
        );
        assert_eq!(
            t.call(0, &call_bytes(7, CORE_PROG, 1, 23, &[]))
                .map(|r| words(&r)[5]),
            Some(GARBAGE_ARGS)
        );
        assert_eq!(
            words(&core(&t, 20, &ints(&[1]))),
            [BAD_LINK as u32],
            "unknown link"
        );
    }

    #[test]
    fn write_then_read_returns_the_response_with_its_reason() {
        let s = svc(Scope::Instrument, 4);
        let l = create(&s);
        assert_eq!(write(&s, l, 8, b"*IDN?\n"), [0, 6]);
        let (head, data) = read_reply(&s, l, 100, 0, 0);
        assert_eq!(
            (head, data),
            (vec![0, RX_END as u32], b"ferrofire,rtu,0,1\n".to_vec())
        );
        let (head, _) = read_reply(&s, l, 100, 0, 0);
        assert_eq!(head, [IO_TIMEOUT as u32, 0], "nothing more to read");
        assert_eq!(
            write(&s, l, 8, b"MEAS:SCAL:LVL?")[1],
            14,
            "END ends the last line"
        );
        let (head, data) = read_reply(&s, l, 100, 0, 0);
        assert_eq!((head[0], data), (0, b"2.5\n".to_vec()));
        assert_eq!(write(&s, l, 0, b"*OPC")[0], 0);
        assert_eq!(write(&s, l, 8, b"?")[0], 0);
        assert_eq!(read_reply(&s, l, 100, 0, 0).1, b"1\n");
        assert_eq!(
            s.call(1, &call_bytes(7, CORE_PROG, 1, 11, &ints(&[l, 0, 0])))
                .map(|r| words(&r)[5]),
            Some(GARBAGE_ARGS)
        );
        assert_eq!(
            words(&core(&s, 11, &{
                let mut a = ints(&[77, 0, 0, 8]);
                put_opaque(&mut a, b"x");
                a
            })),
            [BAD_LINK as u32, 0]
        );
    }

    #[test]
    fn a_read_stops_at_the_size_or_the_termination_character() {
        let s = svc(Scope::Instrument, 4);
        let l = create(&s);
        write(&s, l, 8, b"*IDN?");
        let (head, data) = read_reply(&s, l, 5, 0, 0);
        assert_eq!((head, data), (vec![0, RX_REQCNT as u32], b"ferro".to_vec()));
        let (head, data) = read_reply(&s, l, 100, OP_TERMCHAR_SET as u32, u32::from(b','));
        assert_eq!(
            (head, data),
            (vec![0, RX_CHR as u32], b"fire,".to_vec()),
            "the character is included"
        );
        let (head, data) = read_reply(&s, l, 4, OP_TERMCHAR_SET as u32, u32::from(b'x'));
        assert_eq!(
            (head, data),
            (vec![0, RX_REQCNT as u32], b"rtu,".to_vec()),
            "not found: the size"
        );
        let (head, data) = read_reply(&s, l, 100, OP_TERMCHAR_SET as u32, u32::from(b'\n'));
        assert_eq!(
            (head, data),
            (vec![0, (RX_CHR | RX_END) as u32], b"0,1\n".to_vec()),
            "the last byte: both"
        );
        write(&s, l, 8, b"*IDN?");
        let (head, data) = read_reply(&s, l, 18, 0, 0);
        assert_eq!(
            (head, data.len()),
            (vec![0, (RX_END | RX_REQCNT) as u32], 18),
            "exactly the size, and the end"
        );
        write(&s, l, 8, b"*IDN?");
        let (head, _) = read_reply(&s, l, 100, OP_TERMCHAR_SET as u32, 0x10A);
        assert_eq!(
            head[1],
            (RX_CHR | RX_END) as u32,
            "only the low byte of the character counts"
        );
        assert_eq!(
            s.call(1, &call_bytes(7, CORE_PROG, 1, 12, &ints(&[l, 1])))
                .map(|r| words(&r)[5]),
            Some(GARBAGE_ARGS)
        );
    }

    #[test]
    fn the_488_2_query_errors_are_exact() {
        let s = svc(Scope::Instrument, 4);
        let l = create(&s);
        write(&s, l, 8, b"*IDN?");
        write(&s, l, 8, b"*OPC?");
        assert_eq!(
            read_reply(&s, l, 100, 0, 0).1,
            b"1\n",
            "the unread response was dropped"
        );
        write(&s, l, 8, b"SYST:ERR?");
        assert_eq!(
            read_reply(&s, l, 100, 0, 0).1,
            b"-410,\"Query INTERRUPTED\"\n"
        );
        write(&s, l, 0, b"*IDN?");
        assert_eq!(
            read_reply(&s, l, 100, 0, 0).0[0],
            IO_TIMEOUT as u32,
            "reading an unterminated query: nothing"
        );
        write(&s, l, 8, b"SYST:ERR?");
        assert_eq!(
            read_reply(&s, l, 100, 0, 0).1,
            b"-420,\"Query UNTERMINATED\"\n"
        );
        write(&s, l, 0, b"*ESE 5");
        read_reply(&s, l, 100, 0, 0);
        write(&s, l, 8, b"*ESE?");
        assert_eq!(
            read_reply(&s, l, 100, 0, 0).1,
            b"0\n",
            "a half-written command is dropped by a read, not run"
        );
        write(&s, l, 8, b"SYST:ERR:COUN?");
        assert_eq!(read_reply(&s, l, 100, 0, 0).1, b"0\n", "and is no error");
    }

    #[test]
    fn readstb_clear_trigger_local_remote_and_the_refused_calls() {
        let s = svc(Scope::Instrument, 4);
        let l = create(&s);
        let stb = |l: u32| words(&core(&s, 13, &ints(&[l, 0, 0, 0])));
        assert_eq!(stb(l), [0, 0]);
        write(&s, l, 8, b"*IDN?;*FOO");
        assert_eq!(stb(l), [0, 4 | 16], "EAV and MAV");
        read_reply(&s, l, 100, 0, 0);
        assert_eq!(stb(l), [0, 4]);
        assert_eq!(stb(55), [BAD_LINK as u32, 0]);
        write(&s, l, 8, b"*IDN?");
        write(&s, l, 0, b"*OP");
        assert_eq!(words(&core(&s, 15, &ints(&[l, 0, 0, 0]))), [0]);
        assert_eq!(
            read_reply(&s, l, 100, 0, 0).0[0],
            IO_TIMEOUT as u32,
            "a clear empties both buffers"
        );
        write(&s, l, 8, b"SYST:ERR:COUN?");
        assert_eq!(
            read_reply(&s, l, 100, 0, 0).1,
            b"2\n",
            "-113 and the -410 of the write over the unread response; the clear raised none"
        );
        for p in [14, 16, 17] {
            assert_eq!(words(&core(&s, p, &ints(&[l, 0, 0, 0]))), [0], "proc {p}");
        }
        for p in [18, 20, 25, 26] {
            assert_eq!(
                words(&core(&s, p, &ints(&[l, 0, 0, 0]))),
                [NOT_SUPPORTED as u32],
                "proc {p}"
            );
        }
        assert_eq!(words(&core(&s, 22, &ints(&[l]))), [NOT_SUPPORTED as u32, 0]);
        assert_eq!(words(&core(&s, 22, &ints(&[88]))), [BAD_LINK as u32, 0]);
        assert_eq!(
            words(&core(&s, 12, &ints(&[88, 1, 0, 0, 0, 0]))),
            [BAD_LINK as u32, 0, 0]
        );
        assert_eq!(words(&core(&s, 19, &ints(&[l]))), [NO_LOCK_HELD as u32]);
        assert_eq!(
            words(&core(&s, 15, &ints(&[88, 0, 0, 0]))),
            [BAD_LINK as u32]
        );
        assert_eq!(
            words(&ok(s.call(0, &call_bytes(7, ABORT_PROG, 1, 1, &ints(&[l]))))),
            [0]
        );
        assert_eq!(
            words(&ok(
                s.call(0, &call_bytes(7, ABORT_PROG, 1, 1, &ints(&[88])))
            )),
            [BAD_LINK as u32]
        );
        assert_eq!(
            words(&ok(
                s.call(0, &call_bytes(7, ABORT_PROG, 1, 1, &ints(&[88])))
            )),
            [BAD_LINK as u32]
        );
        assert_eq!(
            s.call(0, &call_bytes(7, ABORT_PROG, 1, 1, &[]))
                .map(|r| words(&r)[5]),
            Some(GARBAGE_ARGS)
        );
        assert_eq!(
            s.call(0, &call_bytes(7, CORE_PROG, 1, 26, &[]))
                .map(|r| words(&r)[5]),
            Some(0)
        );
    }

    #[test]
    fn links_share_the_instrument_status_or_have_their_own_and_die_with_their_connection() {
        let s = svc(Scope::Instrument, 4);
        let (a, b) = (create(&s), create(&s));
        write(&s, a, 8, b"*FOO");
        write(&s, b, 8, b"SYST:ERR?");
        assert!(
            read_reply(&s, b, 100, 0, 0).1.starts_with(b"-113"),
            "a's error, read by b"
        );
        let own = svc(Scope::Connection, 4);
        let (a, b) = (create(&own), create(&own));
        write(&own, a, 8, b"*FOO");
        write(&own, b, 8, b"SYST:ERR:COUN?");
        assert_eq!(read_reply(&own, b, 100, 0, 0).1, b"0\n");
        let c = s.connection();
        assert_ne!(c, s.connection());
        let mut args = ints(&[1, 0, 0]);
        put_opaque(&mut args, b"inst0");
        s.call(c, &call_bytes(7, CORE_PROG, 1, 10, &args)).unwrap();
        assert_eq!(s.links().map.len(), 3);
        s.close(c);
        assert_eq!(
            s.links().map.len(),
            2,
            "only the link of that connection went"
        );
    }

    #[test]
    fn udp_answers_the_portmapper_and_nothing_else() {
        let s = svc(Scope::Instrument, 4);
        let get = call_bytes(7, PMAP_PROG, 2, 3, &ints(&[CORE_PROG, 1, 6, 0]));
        assert_eq!(words(&ok(s.call_udp(&get))), [4881]);
        let r = s.call_udp(&call_bytes(7, CORE_PROG, 1, 10, &[])).unwrap();
        assert_eq!(words(&r)[5], PROG_UNAVAIL);
        assert_eq!(s.call_udp(&[1, 2]), None);
    }
}
