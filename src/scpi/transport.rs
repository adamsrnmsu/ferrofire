//! What `ferrofire scpi` can connect to: a VISA-style resource string or HOST[:PORT], and a `Wire` for each protocol (raw socket lines,
//! HiSLIP, VXI-11) that sends a program message and reads one response line. The wires are blocking `std` sockets with the
//! `--timeout` on every read and write.

use super::hislip::{frame, t::*, Frame, Reader, HEADER};
use super::rpc::{ints, put, put_opaque, record, Xdr, CORE_PROG, PMAP_PROG};
use std::io::{BufRead, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 5025;
const HISLIP_PORT: u16 = 4880;

/// A program message out, a response line in: what `Link` needs of a connection.
pub trait Wire {
    fn send(&mut self, msg: &str) -> std::io::Result<()>;
    /// The next response line, without its terminator; `Ok(None)` when the instrument closed the connection.
    fn recv(&mut self) -> std::io::Result<Option<String>>;
}

impl Wire for Box<dyn Wire> {
    fn send(&mut self, msg: &str) -> std::io::Result<()> {
        (**self).send(msg)
    }
    fn recv(&mut self) -> std::io::Result<Option<String>> {
        (**self).recv()
    }
}

/// The raw socket protocol: messages and responses are lines.
pub struct Lines<R, W> {
    pub rd: R,
    pub wr: W,
}

impl<R: BufRead, W: Write> Wire for Lines<R, W> {
    fn send(&mut self, msg: &str) -> std::io::Result<()> {
        writeln!(self.wr, "{msg}").and_then(|()| self.wr.flush())
    }

    fn recv(&mut self) -> std::io::Result<Option<String>> {
        let mut line = String::new();
        let n = self.rd.read_line(&mut line)?;
        Ok((n > 0).then(|| line.trim_end_matches(['\r', '\n']).to_string()))
    }
}

/// Where to connect and with which protocol.
#[derive(Debug, PartialEq)]
pub enum Target {
    /// `HOST[:PORT]`, `TCPIP::host::port::SOCKET`
    Socket(String, u16),
    /// `TCPIP::host::hislip0[,port]::INSTR`: host, port, sub-address
    Hislip(String, u16, String),
    /// `TCPIP::host[::inst0]::INSTR`: host, device name
    Vxi11(String, String),
}

fn bad(t: &str) -> String {
    format!(
        "`{t}`: expected HOST[:PORT] or a resource TCPIP::host::INSTR (VXI-11), TCPIP::host::hislip0::INSTR (HiSLIP) or TCPIP::host::port::SOCKET"
    )
}

/// Whether `t` is a VISA TCP/IP resource: `TCPIP`, a board number, then `::`.
fn is_resource(t: &str) -> bool {
    let rest = t
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("TCPIP"))
        .map(|_| &t[5..]);
    rest.is_some_and(|r| {
        r.trim_start_matches(|c: char| c.is_ascii_digit())
            .starts_with("::")
    })
}

pub fn parse_target(t: &str) -> Result<Target, String> {
    if !is_resource(t) {
        let (host, port) = super::client::split_target(t)?;
        return Ok(Target::Socket(host, port));
    }
    let parts: Vec<&str> = t.split("::").collect();
    let kind = parts.last().map(|k| k.to_ascii_uppercase());
    match (&parts[1..parts.len() - 1], kind.as_deref()) {
        ([host], Some("INSTR")) if !host.is_empty() => {
            Ok(Target::Vxi11(host.to_string(), "inst0".into()))
        }
        ([host, name], Some("INSTR")) if !host.is_empty() => Ok(instrument(host, name, t)?),
        ([host, port], Some("SOCKET")) if !host.is_empty() => {
            let port = port.parse().map_err(|_| bad(t))?;
            Ok(Target::Socket(host.to_string(), port))
        }
        _ => Err(bad(t)),
    }
}

/// `hislip0` or `hislip0,4880` (HiSLIP), anything else (`inst0`, `gpib0,5`) the VXI-11 device name.
fn instrument(host: &str, name: &str, t: &str) -> Result<Target, String> {
    if !name.to_ascii_lowercase().starts_with("hislip") {
        return Ok(Target::Vxi11(host.into(), name.into()));
    }
    let (sub, port) = match name.split_once(',') {
        Some((sub, port)) => (sub, port.parse().map_err(|_| bad(t))?),
        None => (name, HISLIP_PORT),
    };
    Ok(Target::Hislip(host.into(), port, sub.into()))
}

pub fn connect_to(host: &str, port: u16, wait: Duration) -> Result<TcpStream, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .next()
        .ok_or_else(|| format!("{host}: no address"))?;
    let s = TcpStream::connect_timeout(&addr, wait).map_err(|e| format!("connect {addr}: {e}"))?;
    let set = s
        .set_read_timeout(Some(wait))
        .and_then(|()| s.set_write_timeout(Some(wait)))
        .and_then(|()| s.set_nodelay(true));
    set.map_err(|e| format!("{addr}: {e}"))?;
    Ok(s)
}

/// Connect to `target`; `portmapper` is the port to ask for a VXI-11 instrument's core channel.
pub fn open(target: &Target, wait: Duration, portmapper: u16) -> Result<Box<dyn Wire>, String> {
    Ok(match target {
        Target::Socket(host, port) => {
            let s = connect_to(host, *port, wait)?;
            let rd = std::io::BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
            Box::new(Lines { rd, wr: s })
        }
        Target::Hislip(host, port, sub) => Box::new(Hislip::open(host, *port, sub, wait)?),
        Target::Vxi11(host, device) => Box::new(Vxi11::open(host, (portmapper, device), wait)?),
    })
}

fn other(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

// ---- HiSLIP ------------------------------------------------------------------------------------------------------------------

/// A HiSLIP session: the synchronous channel carries messages, the asynchronous one only the handshake here.
pub struct Hislip<S = TcpStream> {
    sync: S,
    rd: Reader,
    id: u32,
    /// The last response was read to its end (the RMT-delivered bit of the next message).
    rmt: u8,
    /// The most the instrument takes in one frame, header included.
    max: u64,
    _asy: Option<TcpStream>,
}

impl Hislip {
    pub fn open(host: &str, port: u16, sub: &str, wait: Duration) -> Result<Hislip, String> {
        let fail = |what: &str, e: std::io::Error| format!("HiSLIP {what}: {e}");
        let mut sync = connect_to(host, port, wait)?;
        let mut rd = Reader::default();
        let init = frame(INITIALIZE, 0, 0x0100_4646, sub.as_bytes());
        let r = exchange(&mut sync, &mut rd, &init).map_err(|e| fail("initialize", e))?;
        if r.ty != INITIALIZE_RESPONSE {
            return Err(format!("HiSLIP initialize: {}", describe(&r)));
        }
        let mut asy = connect_to(host, port, wait)?;
        let mut ard = Reader::default();
        let open = frame(ASYNC_INITIALIZE, 0, r.param & 0xFFFF, &[]);
        let a = exchange(&mut asy, &mut ard, &open).map_err(|e| fail("async initialize", e))?;
        if a.ty != ASYNC_INITIALIZE_RESPONSE {
            return Err(format!("HiSLIP async initialize: {}", describe(&a)));
        }
        let size = frame(ASYNC_MAX_SIZE, 0, 0, &(1u64 << 20).to_be_bytes());
        let s = exchange(&mut asy, &mut ard, &size).map_err(|e| fail("maximum message size", e))?;
        let max = <[u8; 8]>::try_from(s.payload.as_slice()).map_or(272, u64::from_be_bytes);
        Ok(Hislip {
            sync,
            rd,
            id: 0xFFFF_FF00,
            rmt: u8::default(),
            max,
            _asy: Some(asy),
        })
    }
}

fn describe(f: &Frame) -> String {
    match f.ty {
        FATAL_ERROR | ERROR => format!("{} (code {})", String::from_utf8_lossy(&f.payload), f.ctl),
        _ => format!("unexpected message type {}", f.ty),
    }
}

/// Send `f` and read the one frame that answers it.
fn exchange(s: &mut (impl Read + Write), rd: &mut Reader, f: &Frame) -> std::io::Result<Frame> {
    s.write_all(&f.encode())?;
    next(s, rd)
}

fn next(s: &mut impl Read, rd: &mut Reader) -> std::io::Result<Frame> {
    let mut buf = [0u8; 4096];
    loop {
        if let Some(f) = rd.take().map_err(|e| other(e.1))? {
            return Ok(f);
        }
        match s.read(&mut buf)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => rd.push(&buf[..n]),
        }
    }
}

impl<S: Read + Write> Wire for Hislip<S> {
    fn send(&mut self, msg: &str) -> std::io::Result<()> {
        let data = format!("{msg}\n");
        let step = (self.max as usize).saturating_sub(HEADER).max(1);
        let mut chunks = data.as_bytes().chunks(step).peekable();
        while let Some(c) = chunks.next() {
            let ty = if chunks.peek().is_some() {
                DATA
            } else {
                DATA_END
            };
            self.sync
                .write_all(&frame(ty, self.rmt, self.id, c).encode())?;
        }
        self.id = self.id.wrapping_add(2);
        Ok(())
    }

    fn recv(&mut self) -> std::io::Result<Option<String>> {
        let mut text = vec![];
        loop {
            let f = match next(&mut self.sync, &mut self.rd) {
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                f => f?,
            };
            if absorb(&f, &mut text)? {
                self.rmt = 1;
                return Ok(Some(line(&text)));
            }
        }
    }
}

/// Take the data of a frame of a response; true when it was the last (DataEND). An `Interrupted` or an error is the end of the query.
fn absorb(f: &Frame, text: &mut Vec<u8>) -> std::io::Result<bool> {
    match f.ty {
        DATA | DATA_END => text.extend(&f.payload),
        INTERRUPTED => return Err(other("the instrument interrupted the query (-410)")),
        FATAL_ERROR | ERROR => return Err(other(describe(f))),
        _ => {}
    }
    Ok(f.ty == DATA_END)
}

/// The response line: the text without its terminator.
fn line(text: &[u8]) -> String {
    String::from_utf8_lossy(text)
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

// ---- VXI-11 ------------------------------------------------------------------------------------------------------------------

/// A VXI-11 link on the core channel. The link is destroyed when this is dropped.
pub struct Vxi11<S: Read + Write = TcpStream> {
    s: S,
    xid: u32,
    link: u32,
    /// The most one device_write may carry.
    max: usize,
    wait_ms: u32,
}

/// The port of the core channel, from the portmapper at `portmapper` (GETPORT, TCP).
fn core_port(host: &str, portmapper: u16, wait: Duration) -> Result<u16, String> {
    let mut pm = connect_to(host, portmapper, wait)?;
    let get = ints(&[CORE_PROG, 1, 6, 0]);
    let r =
        rpc(&mut pm, 1, (PMAP_PROG, 2, 3), &get).map_err(|e| format!("VXI-11 portmapper: {e}"))?;
    core_port_in(&r)
}

/// The port in a GETPORT reply; 0 is the portmapper saying it does not know the program.
fn core_port_in(reply: &[u8]) -> Result<u16, String> {
    let port = Xdr(reply).u32().and_then(|p| u16::try_from(p).ok());
    port.filter(|p| *p != 0)
        .ok_or_else(|| "VXI-11: the portmapper does not know the core channel".into())
}

/// The link and the largest write in a create_link reply.
fn created(reply: &[u8], device: &str) -> Result<(u32, usize), String> {
    let [error, link, _abort, max] = Xdr(reply)
        .words()
        .ok_or("VXI-11 create_link: short reply")?;
    match error {
        0 => Ok((link, max.max(1) as usize)),
        e => Err(format!(
            "VXI-11 create_link: error {e} (3 is no such device `{device}`)"
        )),
    }
}

/// A timeout in whole milliseconds, as far as a `u32` goes.
fn millis(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// One RPC call and the results of its reply.
fn rpc(
    s: &mut (impl Read + Write),
    xid: u32,
    (prog, vers, proc): (u32, u32, u32),
    args: &[u8],
) -> std::io::Result<Vec<u8>> {
    let mut call = vec![];
    [xid, 0, 2, prog, vers, proc, 0, 0, 0, 0]
        .iter()
        .for_each(|w| put(&mut call, *w));
    call.extend(args);
    s.write_all(&record(&call))?;
    let reply = read_record(s)?;
    let mut x = Xdr(&reply);
    let head = (x.u32(), x.u32(), x.u32(), x.u32(), x.opaque(400), x.u32());
    match head {
        (Some(id), ..) if id != xid => Err(other("an RPC reply to another call")),
        (_, Some(1), Some(0), Some(_), Some(_), Some(0)) => Ok(x.0.to_vec()),
        (_, Some(1), Some(0), Some(_), Some(_), Some(stat)) => {
            Err(other(format!("RPC call {prog:#x}/{proc} refused ({stat})")))
        }
        _ => Err(other("not an RPC reply")),
    }
}

fn read_record(s: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut rec = vec![];
    loop {
        let mut head = [0u8; 4];
        s.read_exact(&mut head)?;
        let head = u32::from_be_bytes(head);
        let mut frag = vec![u8::default(); (head & 0x7FFF_FFFF) as usize];
        s.read_exact(&mut frag)?;
        rec.extend(frag);
        if head >> 31 == 1 || rec.len() > super::rpc::MAX_RECORD {
            return Ok(rec);
        }
    }
}

impl Vxi11 {
    pub fn open(
        host: &str,
        (portmapper, device): (u16, &str),
        wait: Duration,
    ) -> Result<Vxi11, String> {
        let port = core_port(host, portmapper, wait)?;
        let mut s = connect_to(host, port, wait)?;
        let mut args = ints(&[1, 0, 0]);
        put_opaque(&mut args, device.as_bytes());
        let r = rpc(&mut s, 1, (CORE_PROG, 1, 10), &args)
            .map_err(|e| format!("VXI-11 create_link: {e}"))?;
        let (link, max) = created(&r, device)?;
        Ok(Vxi11 {
            s,
            xid: 2,
            link,
            max,
            wait_ms: millis(wait),
        })
    }
}

impl<S: Read + Write> Vxi11<S> {
    fn core(&mut self, proc: u32, args: &[u8]) -> std::io::Result<Vec<u8>> {
        self.xid += 1;
        rpc(&mut self.s, self.xid, (CORE_PROG, 1, proc), args)
    }
}

impl<S: Read + Write> Wire for Vxi11<S> {
    fn send(&mut self, msg: &str) -> std::io::Result<()> {
        let data = format!("{msg}\n");
        let mut chunks = data.as_bytes().chunks(self.max).peekable();
        while let Some(c) = chunks.next() {
            let flags = if chunks.peek().is_some() { 0 } else { 8 };
            let mut args = ints(&[self.link, self.wait_ms, self.wait_ms, flags]);
            put_opaque(&mut args, c);
            match Xdr(&self.core(11, &args)?).u32() {
                Some(0) => {}
                e => return Err(other(format!("device_write: error {e:?}"))),
            }
        }
        Ok(())
    }

    fn recv(&mut self) -> std::io::Result<Option<String>> {
        let mut text = vec![];
        loop {
            let args = ints(&[self.link, 1 << 16, self.wait_ms, self.wait_ms, 0, 0]);
            let r = self.core(12, &args)?;
            let mut x = Xdr(&r);
            let (error, reason) = (x.u32(), x.u32().unwrap_or_default());
            match error {
                Some(0) => {}
                Some(15) => return Err(std::io::ErrorKind::TimedOut.into()),
                e => return Err(other(format!("device_read: error {e:?}"))),
            }
            text.extend(x.opaque(usize::MAX).unwrap_or_default());
            if reason & 4 != 0 {
                return Ok(Some(line(&text)));
            }
        }
    }
}

impl<S: Read + Write> Drop for Vxi11<S> {
    fn drop(&mut self) {
        let _ = self.core(23, &ints(&[self.link]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scpi::rpc::Records;

    fn sock(h: &str, p: u16) -> Result<Target, String> {
        Ok(Target::Socket(h.into(), p))
    }

    #[test]
    fn resource_strings_and_plain_targets_name_a_protocol() {
        assert_eq!(parse_target("lab"), sock("lab", 5025));
        assert_eq!(parse_target("10.0.0.5:9"), sock("10.0.0.5", 9));
        assert_eq!(parse_target("[::1]:7"), sock("::1", 7));
        assert_eq!(parse_target("fe80::1"), sock("fe80::1", 5025));
        assert_eq!(parse_target("TCPIP::lab::5025::SOCKET"), sock("lab", 5025));
        assert_eq!(
            parse_target("tcpip0::10.0.0.5::9999::socket"),
            sock("10.0.0.5", 9999)
        );
        assert_eq!(
            parse_target("TCPIP::lab::INSTR"),
            Ok(Target::Vxi11("lab".into(), "inst0".into()))
        );
        assert_eq!(
            parse_target("TCPIP0::lab::inst0::INSTR"),
            Ok(Target::Vxi11("lab".into(), "inst0".into()))
        );
        assert_eq!(
            parse_target("TCPIP12::lab::gpib0,5::INSTR"),
            Ok(Target::Vxi11("lab".into(), "gpib0,5".into()))
        );
        assert_eq!(
            parse_target("TCPIP::lab::hislip0::INSTR"),
            Ok(Target::Hislip("lab".into(), 4880, "hislip0".into()))
        );
        assert_eq!(
            parse_target("TCPIP::lab::HiSLIP1,4881::instr"),
            Ok(Target::Hislip("lab".into(), 4881, "HiSLIP1".into()))
        );
        for bad in [
            "TCPIP::lab",
            "TCPIP::INSTR",
            "TCPIP::::INSTR",
            "TCPIP::lab::SOCKET",
            "TCPIP::lab::x::SOCKET",
            "TCPIP::lab::70000::SOCKET",
            "TCPIP::lab::hislip0,x::INSTR",
            "TCPIP::lab::a::b::INSTR",
            "TCPIP::lab::5025::GPIB",
        ] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
        assert!(parse_target("TCPIP::lab")
            .unwrap_err()
            .contains("TCPIP::host::hislip0::INSTR"));
    }

    // ---- the wires over a scripted stream ----

    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    type Out = Rc<RefCell<Vec<u8>>>;

    /// A stream that reads from a script `step` bytes at a time and records what is written.
    struct Pipe {
        input: Cursor<Vec<u8>>,
        step: usize,
        out: Out,
    }

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.step);
            self.input.read(&mut buf[..n])
        }
    }

    impl Write for Pipe {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.out.borrow_mut().extend(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn pipe(input: Vec<u8>, step: usize) -> (Pipe, Out) {
        let out = Out::default();
        let p = Pipe {
            input: Cursor::new(input),
            step,
            out: out.clone(),
        };
        (p, out)
    }

    const ID: u32 = 0xFFFF_FF00;

    fn hislip(frames: &[Frame], step: usize, max: u64) -> (Hislip<Pipe>, Out) {
        let (sync, out) = pipe(frames.iter().flat_map(Frame::encode).collect(), step);
        let h = Hislip {
            sync,
            rd: Reader::default(),
            id: ID,
            rmt: 0,
            max,
            _asy: None,
        };
        (h, out)
    }

    /// The frames in what was written.
    fn frames_in(out: &Out) -> Vec<Frame> {
        let mut r = Reader::default();
        r.push(&out.borrow());
        std::iter::from_fn(|| r.take().unwrap()).collect()
    }

    #[test]
    fn a_hislip_message_is_one_dataend_with_the_next_message_id_each_time() {
        let (mut h, out) = hislip(&[], 1, 272);
        h.send("*IDN?").unwrap();
        h.send("*CLS").unwrap();
        assert_eq!(
            frames_in(&out),
            [
                frame(DATA_END, 0, ID, b"*IDN?\n"),
                frame(DATA_END, 0, ID + 2, b"*CLS\n")
            ]
        );
    }

    #[test]
    fn a_long_hislip_message_is_cut_to_the_maximum_message_size_under_one_message_id() {
        let (mut h, out) = hislip(&[], 1, 20);
        h.send("abcdefghi").unwrap();
        h.send("x").unwrap();
        assert_eq!(
            frames_in(&out),
            [
                frame(DATA, 0, ID, b"abcd"),
                frame(DATA, 0, ID, b"efgh"),
                frame(DATA_END, 0, ID, b"i\n"),
                frame(DATA_END, 0, ID + 2, b"x\n"),
            ]
        );
        let (mut h, out) = hislip(&[], 1, 0);
        h.send("a").unwrap();
        assert_eq!(
            frames_in(&out),
            [frame(DATA, 0, ID, b"a"), frame(DATA_END, 0, ID, b"\n")],
            "a size that leaves no room still moves one byte at a time"
        );
    }

    #[test]
    fn a_hislip_response_is_read_to_its_dataend_and_then_acknowledged_by_the_next_message() {
        let replies = [
            frame(DATA, 0, ID, b"ab"),
            frame(ASYNC_LOCK_RESPONSE, 0, 0, b""),
            frame(DATA_END, 0, ID, b"c\r\n"),
        ];
        let (mut h, out) = hislip(&replies, 1, 272);
        assert_eq!(h.recv().unwrap(), Some("abc".to_string()));
        h.send("*OPC?").unwrap();
        assert_eq!(
            frames_in(&out)[0].ctl,
            1,
            "RMT-delivered once a response was read"
        );
        assert_eq!(
            h.recv().unwrap(),
            None,
            "the end of the stream is the instrument closing"
        );
    }

    #[test]
    fn hislip_errors_end_the_query_with_their_text() {
        let err = |f: Frame| {
            let (mut h, _) = hislip(&[f], 4096, 272);
            h.recv().unwrap_err().to_string()
        };
        assert!(err(frame(INTERRUPTED, 0, ID, b"")).contains("interrupted the query (-410)"));
        assert!(err(frame(ERROR, 3, 0, b"too big")).contains("too big (code 3)"));
        assert!(err(frame(FATAL_ERROR, 1, 0, b"bad")).contains("bad (code 1)"));
        let (mut h, _) = hislip(&[], 4096, 272);
        h.sync = pipe(b"GET / HTTP/1.1\r\n\r\n".to_vec(), 4096).0;
        assert!(h.recv().unwrap_err().to_string().contains("poorly formed"));
        assert_eq!(
            describe(&frame(DATA, 0, 0, b"")),
            "unexpected message type 6"
        );
    }

    #[test]
    fn frames_are_read_from_a_stream_that_gives_one_byte_at_a_time() {
        let (mut p, _) = pipe(frame(DATA_END, 1, 7, b"xyz").encode(), 1);
        let mut rd = Reader::default();
        assert_eq!(
            next(&mut p, &mut rd).unwrap(),
            frame(DATA_END, 1, 7, b"xyz")
        );
        let e = next(&mut p, &mut rd).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// The reply record of the call with this xid: accepted, the results.
    fn answer(xid: u32, results: &[u8]) -> Vec<u8> {
        record(&[ints(&[xid, 1, 0, 0, 0, 0]), results.to_vec()].concat())
    }

    fn vxi(replies: &[Vec<u8>], step: usize, max: usize) -> (Vxi11<Pipe>, Out) {
        let (s, out) = pipe(replies.concat(), step);
        let v = Vxi11 {
            s,
            xid: 2,
            link: 7,
            max,
            wait_ms: 1500,
        };
        (v, out)
    }

    /// (procedure, arguments) of every call in what was written.
    fn calls_in(out: &Out) -> Vec<(u32, Vec<u8>)> {
        let mut r = Records::default();
        r.push(&out.borrow());
        std::iter::from_fn(|| r.take().unwrap())
            .map(|c| {
                let mut x = Xdr(&c);
                let [_, _, _, _, _, proc] = x.words().unwrap();
                x.skip_auth().unwrap();
                x.skip_auth().unwrap();
                (proc, x.0.to_vec())
            })
            .collect()
    }

    fn opaque(data: &[u8]) -> Vec<u8> {
        let mut o = vec![];
        put_opaque(&mut o, data);
        o
    }

    #[test]
    fn a_vxi11_message_is_written_in_pieces_the_last_with_end() {
        let ok = |n: u32| answer(3 + n, &ints(&[0, 4]));
        let (mut v, out) = vxi(&[ok(0), ok(1)], 1, 4);
        v.send("abcdef").unwrap();
        let calls = calls_in(&out);
        let want = |flags, data: &[u8]| [ints(&[7, 1500, 1500, flags]), opaque(data)].concat();
        assert_eq!(calls[0], (11, want(0, b"abcd")));
        assert_eq!(calls[1], (11, want(8, b"ef\n")));
        let (mut v, _) = vxi(&[answer(3, &ints(&[17, 0]))], 4096, 4);
        let e = v.send("a").unwrap_err().to_string();
        assert!(e.contains("device_write: error Some(17)"), "{e}");
    }

    #[test]
    fn a_vxi11_response_is_read_until_the_end_reason() {
        let data = |xid, reason, d: &[u8]| answer(xid, &[ints(&[0, reason]), opaque(d)].concat());
        let (mut v, out) = vxi(&[data(3, 1, b"ab"), data(4, 4, b"c\r\n")], 1, 100);
        assert_eq!(v.recv().unwrap(), Some("abc".to_string()));
        let calls = calls_in(&out);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (12, ints(&[7, 1 << 16, 1500, 1500, 0, 0])));
        let (mut v, _) = vxi(&[answer(3, &ints(&[15, 0, 0]))], 4096, 100);
        assert_eq!(v.recv().unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        let (mut v, _) = vxi(&[answer(3, &ints(&[4, 0, 0]))], 4096, 100);
        let e = v.recv().unwrap_err();
        assert_ne!(e.kind(), std::io::ErrorKind::TimedOut);
        assert!(e.to_string().contains("device_read: error Some(4)"));
    }

    #[test]
    fn a_dropped_vxi11_link_is_destroyed() {
        let (v, out) = vxi(&[answer(3, &ints(&[0]))], 4096, 100);
        drop(v);
        assert_eq!(calls_in(&out), [(23, ints(&[7]))]);
    }

    #[test]
    fn rpc_replies_are_checked_and_records_read_whole() {
        let call = |reply: Vec<u8>| {
            let (mut p, _) = pipe(reply, 1);
            rpc(&mut p, 9, (CORE_PROG, 1, 0), &[])
        };
        let msg = |r: Vec<u8>| call(r).unwrap_err().to_string();
        assert_eq!(call(answer(9, &ints(&[5]))).unwrap(), ints(&[5]));
        assert!(msg(record(&ints(&[9, 1, 0, 0, 0, 3]))).contains("refused (3)"));
        assert!(
            msg(answer(8, &[])).contains("to another call"),
            "another xid"
        );
        assert!(
            msg(record(&ints(&[9, 0, 0, 0, 0, 0]))).contains("not an RPC reply"),
            "not a reply"
        );
        assert!(
            msg(record(&ints(&[9, 1, 1, 0, 2, 2]))).contains("not an RPC reply"),
            "denied"
        );
        let mut two = vec![0, 0, 0, 2, 1, 2];
        two.extend(record(&[3, 4]));
        let (mut p, _) = pipe(two, 1);
        assert_eq!(read_record(&mut p).unwrap(), [1, 2, 3, 4]);
        let (mut p, _) = pipe(vec![0, 0, 0, 5, 1], 4096);
        assert_eq!(
            read_record(&mut p).unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        let half = crate::scpi::rpc::MAX_RECORD / 2;
        let fragment = |n: usize| [&(n as u32).to_be_bytes()[..], &vec![0; n]].concat();
        let (mut p, _) = pipe([fragment(half + 1), fragment(half + 1)].concat(), 4096);
        assert_eq!(
            read_record(&mut p).unwrap().len(),
            2 * half + 2,
            "a record that grows past the limit is handed over, not read for ever"
        );
        let (mut p, _) = pipe([fragment(half), fragment(half)].concat(), 4096);
        assert_eq!(
            read_record(&mut p).unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof,
            "exactly the limit is still within it: the next fragment is awaited"
        );
    }

    #[test]
    fn the_portmapper_and_create_link_replies_are_read() {
        assert_eq!(core_port_in(&ints(&[4881])), Ok(4881));
        for bad in [ints(&[0]), ints(&[70000]), vec![]] {
            assert!(core_port_in(&bad)
                .unwrap_err()
                .contains("does not know the core channel"));
        }
        assert_eq!(core_port_in(&ints(&[65535])), Ok(65535));
        assert_eq!(
            created(&ints(&[0, 7, 4881, 1 << 20]), "inst0"),
            Ok((7, 1 << 20))
        );
        assert_eq!(
            created(&ints(&[0, 7, 4881, 0]), "inst0"),
            Ok((7, 1)),
            "a size of 0 still moves a byte"
        );
        assert!(created(&ints(&[3, 0, 0, 0]), "gpib0")
            .unwrap_err()
            .contains("error 3 (3 is no such device `gpib0`)"));
        assert!(created(&ints(&[0, 7]), "inst0")
            .unwrap_err()
            .contains("short reply"));
        assert_eq!(millis(Duration::from_millis(1234)), 1234);
        assert_eq!(millis(Duration::from_secs(1 << 40)), u32::MAX);
    }

    // ---- the handshakes against a scripted peer on a real socket ----

    use std::net::TcpListener;

    fn listener() -> (TcpListener, u16) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        (l, port)
    }

    const WAIT: Duration = Duration::from_secs(5);

    /// A HiSLIP peer: answers the handshake of both channels with `replies` (InitializeResponse, AsyncInitializeResponse, the maximum
    /// message size response, as many as the client gets to ask for) and hands back the frames it was sent.
    fn hislip_peer(replies: Vec<Frame>) -> (u16, std::thread::JoinHandle<Vec<Frame>>) {
        let (l, port) = listener();
        let t = std::thread::spawn(move || {
            let (mut sent, mut replies) = (vec![], replies.into_iter());
            let (mut sync, _) = l.accept().unwrap();
            let mut r = Reader::default();
            sent.push(next(&mut sync, &mut r).unwrap());
            let Some(first) = replies.next() else {
                return sent;
            };
            sync.write_all(&first.encode()).unwrap();
            let Ok((mut asy, _)) = l.accept() else {
                return sent;
            };
            let mut ar = Reader::default();
            for reply in replies {
                sent.push(next(&mut asy, &mut ar).unwrap());
                asy.write_all(&reply.encode()).unwrap();
            }
            sent
        });
        (port, t)
    }

    #[test]
    fn hislip_opens_the_sync_channel_then_the_async_one_and_asks_for_the_maximum_size() {
        let (port, peer) = hislip_peer(vec![
            frame(INITIALIZE_RESPONSE, 0, 0x0100_0007, b""),
            frame(ASYNC_INITIALIZE_RESPONSE, 0, 0x4646, b""),
            frame(ASYNC_MAX_SIZE_RESPONSE, 0, 0, &4096u64.to_be_bytes()),
        ]);
        let h = Hislip::open("127.0.0.1", port, "hislip0", WAIT).unwrap();
        assert_eq!((h.max, h.id, h.rmt), (4096, ID, 0));
        let sent = peer.join().unwrap();
        assert_eq!(
            sent,
            [
                frame(INITIALIZE, 0, 0x0100_4646, b"hislip0"),
                frame(ASYNC_INITIALIZE, 0, 7, b""),
                frame(ASYNC_MAX_SIZE, 0, 0, &(1u64 << 20).to_be_bytes()),
            ]
        );
        let (port, peer) = hislip_peer(vec![
            frame(INITIALIZE_RESPONSE, 0, 0x0100_0007, b""),
            frame(ASYNC_INITIALIZE_RESPONSE, 0, 0, b""),
            frame(ASYNC_MAX_SIZE_RESPONSE, 0, 0, b"short"),
        ]);
        assert_eq!(
            Hislip::open("127.0.0.1", port, "hislip0", WAIT)
                .unwrap()
                .max,
            272
        );
        peer.join().unwrap();
    }

    #[test]
    fn hislip_open_reports_what_the_peer_refused() {
        let open = |replies: Vec<Frame>| {
            // the peer thread may be left waiting for a second connection that a refused client never makes
            let (port, _peer) = hislip_peer(replies);
            Hislip::open("127.0.0.1", port, "hislip7", WAIT)
                .err()
                .unwrap()
        };
        let e = open(vec![frame(FATAL_ERROR, 0, 0, b"no such HiSLIP instrument")]);
        assert_eq!(e, "HiSLIP initialize: no such HiSLIP instrument (code 0)");
        let e = open(vec![
            frame(INITIALIZE_RESPONSE, 0, 0x0100_0007, b""),
            frame(FATAL_ERROR, 3, 0, b"nope"),
        ]);
        assert_eq!(e, "HiSLIP async initialize: nope (code 3)");
        let e = open(vec![
            frame(INITIALIZE_RESPONSE, 0, 0x0100_0007, b""),
            frame(ASYNC_INITIALIZE_RESPONSE, 0, 0, b""),
        ]);
        assert!(e.starts_with("HiSLIP maximum message size:"), "{e}");
    }

    /// A VXI-11 portmapper that answers one call with `results`.
    fn portmapper(results: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
        let (l, port) = listener();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            read_record(&mut s).unwrap();
            s.write_all(&answer(1, &results)).unwrap();
        });
        (port, t)
    }

    #[test]
    fn vxi11_asks_the_portmapper_then_creates_a_link_on_the_core_channel() {
        // the portmapper answers with the core channel's port, known only once both listeners exist
        let ((pml, pm_port), (corel, core_port)) = (listener(), listener());
        let peer = std::thread::spawn(move || {
            let mut sent = vec![];
            let (mut s, _) = pml.accept().unwrap();
            sent.push(read_record(&mut s).unwrap());
            s.write_all(&answer(1, &ints(&[u32::from(core_port)])))
                .unwrap();
            let (mut s, _) = corel.accept().unwrap();
            sent.push(read_record(&mut s).unwrap());
            let created = ints(&[0, 9, u32::from(core_port), 4096]);
            s.write_all(&answer(1, &created)).unwrap();
            sent
        });
        let v = Vxi11::open("127.0.0.1", (pm_port, "inst0"), WAIT).unwrap();
        assert_eq!((v.link, v.max, v.wait_ms, v.xid), (9, 4096, 5000, 2));
        let sent = peer.join().unwrap();
        let call = |prog, vers, proc, args: &[u8]| {
            record(
                &[
                    ints(&[1, 0, 2, prog, vers, proc, 0, 0, 0, 0]),
                    args.to_vec(),
                ]
                .concat(),
            )[4..]
                .to_vec()
        };
        assert_eq!(sent[0], call(PMAP_PROG, 2, 3, &ints(&[CORE_PROG, 1, 6, 0])));
        assert_eq!(
            sent[1],
            call(
                CORE_PROG,
                1,
                10,
                &[ints(&[1, 0, 0]), opaque(b"inst0")].concat()
            )
        );
        drop(v);
    }

    #[test]
    fn vxi11_open_reports_what_the_portmapper_and_the_device_refused() {
        let (pm_port, peer) = portmapper(ints(&[0]));
        let e = Vxi11::open("127.0.0.1", (pm_port, "inst0"), WAIT)
            .err()
            .unwrap();
        assert!(e.contains("does not know the core channel"), "{e}");
        peer.join().unwrap();
        let (l, port) = listener();
        drop(l);
        let e = Vxi11::open("127.0.0.1", (port, "inst0"), WAIT)
            .err()
            .unwrap();
        assert!(e.contains("connect 127.0.0.1"), "{e}");
    }

    #[test]
    fn a_raw_socket_response_may_be_an_empty_line_and_its_end_is_the_instrument_closing() {
        let mut l = Lines {
            rd: Cursor::new(b"\nab\r\n".to_vec()),
            wr: vec![],
        };
        l.send("*IDN?").unwrap();
        assert_eq!(l.wr, b"*IDN?\n");
        assert_eq!(l.recv().unwrap(), Some(String::new()));
        assert_eq!(l.recv().unwrap(), Some("ab".to_string()));
        assert_eq!(l.recv().unwrap(), None);
    }
}
