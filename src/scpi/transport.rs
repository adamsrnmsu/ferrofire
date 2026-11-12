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
pub struct Hislip {
    sync: TcpStream,
    rd: Reader,
    id: u32,
    /// The last response was read to its end (the RMT-delivered bit of the next message).
    rmt: u8,
    /// The most the instrument takes in one frame, header included.
    max: u64,
    _asy: TcpStream,
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
            rmt: 0,
            max,
            _asy: asy,
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
fn exchange(s: &mut TcpStream, rd: &mut Reader, f: &Frame) -> std::io::Result<Frame> {
    s.write_all(&f.encode())?;
    next(s, rd)
}

fn next(s: &mut TcpStream, rd: &mut Reader) -> std::io::Result<Frame> {
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

impl Wire for Hislip {
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
pub struct Vxi11 {
    s: TcpStream,
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
    let port = Xdr(&r).u32().and_then(|p| u16::try_from(p).ok());
    port.filter(|p| *p != 0)
        .ok_or_else(|| "VXI-11: the portmapper does not know the core channel".into())
}

/// One RPC call and the results of its reply.
fn rpc(
    s: &mut TcpStream,
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
        (Some(id), Some(1), Some(0), Some(_), Some(_), Some(0)) if id == xid => Ok(x.0.to_vec()),
        (_, _, _, _, _, Some(stat)) => {
            Err(other(format!("RPC call {prog:#x}/{proc} refused ({stat})")))
        }
        _ => Err(other("not an RPC reply")),
    }
}

fn read_record(s: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut rec = vec![];
    loop {
        let mut head = [0u8; 4];
        s.read_exact(&mut head)?;
        let head = u32::from_be_bytes(head);
        let mut frag = vec![0; (head & 0x7FFF_FFFF) as usize];
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
        let [error, link, _abort, max] =
            Xdr(&r).words().ok_or("VXI-11 create_link: short reply")?;
        match error {
            0 => Ok(Vxi11 {
                s,
                xid: 2,
                link,
                max: max.max(1) as usize,
                wait_ms: wait.as_millis().min(u128::from(u32::MAX)) as u32,
            }),
            e => Err(format!(
                "VXI-11 create_link: error {e} (3 is no such device `{device}`)"
            )),
        }
    }

    fn core(&mut self, proc: u32, args: &[u8]) -> std::io::Result<Vec<u8>> {
        self.xid += 1;
        rpc(&mut self.s, self.xid, (CORE_PROG, 1, proc), args)
    }
}

impl Wire for Vxi11 {
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
            let (error, reason) = (x.u32(), x.u32().unwrap_or(0));
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

impl Drop for Vxi11 {
    fn drop(&mut self) {
        let _ = self.core(23, &ints(&[self.link]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
