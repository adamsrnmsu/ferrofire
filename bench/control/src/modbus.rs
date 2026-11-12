//! A minimal Modbus/TCP client: FC5 write single coil, FC2 read discrete inputs, FC4 read input registers. One request in
//! flight per connection; every operation has a timeout (the connection's read/write timeout).
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Mb {
    s: TcpStream,
    tid: u16,
}

/// MBAP header (transaction, protocol 0, length, unit 1) followed by the PDU.
pub fn frame(tid: u16, pdu: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(7 + pdu.len());
    f.extend_from_slice(&tid.to_be_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&(pdu.len() as u16 + 1).to_be_bytes());
    f.push(1);
    f.extend_from_slice(pdu);
    f
}

/// Bit `i` of a read-bits payload (LSB first within each byte).
pub fn bit(bytes: &[u8], i: usize) -> bool {
    bytes[i / 8] >> (i % 8) & 1 == 1
}

fn bad(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}

impl Mb {
    pub fn connect(host: &str, port: u16) -> io::Result<Mb> {
        let s = TcpStream::connect((host, port))?;
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Mb { s, tid: 0 })
    }

    /// One request, one response PDU (an exception response is an error).
    fn call(&mut self, pdu: &[u8]) -> io::Result<Vec<u8>> {
        self.tid = self.tid.wrapping_add(1);
        self.s.write_all(&frame(self.tid, pdu))?;
        let mut h = [0u8; 7];
        self.s.read_exact(&mut h)?;
        if u16::from_be_bytes([h[0], h[1]]) != self.tid {
            return Err(bad("transaction id mismatch"));
        }
        let n = u16::from_be_bytes([h[4], h[5]]) as usize;
        if n < 2 {
            return Err(bad("short response"));
        }
        let mut p = vec![0u8; n - 1];
        self.s.read_exact(&mut p)?;
        match p[0] {
            f if f == pdu[0] => Ok(p),
            f if f == pdu[0] | 0x80 => Err(bad(&format!("exception {:?}", p.get(1)))),
            _ => Err(bad("function mismatch")),
        }
    }

    pub fn write_coil(&mut self, addr: u16, on: bool) -> io::Result<()> {
        let v: u16 = if on { 0xFF00 } else { 0 };
        let r = self.call(&[&[5u8][..], &addr.to_be_bytes(), &v.to_be_bytes()].concat())?;
        match r[1..] == [&addr.to_be_bytes()[..], &v.to_be_bytes()].concat()[..] {
            true => Ok(()),
            false => Err(bad("write echo differs")),
        }
    }

    /// FC2: `n` discrete inputs from `addr`, as the raw bit payload (read with [`bit`]).
    pub fn read_bits(&mut self, addr: u16, n: u16) -> io::Result<Vec<u8>> {
        let r = self.call(&[&[2u8][..], &addr.to_be_bytes(), &n.to_be_bytes()].concat())?;
        match r.get(1).map(|&c| c as usize) == Some(r.len() - 2)
            && r.len() - 2 >= (n as usize).div_ceil(8)
        {
            true => Ok(r[2..].to_vec()),
            false => Err(bad("bad bit count")),
        }
    }

    /// FC4: `n` input registers from `addr`.
    pub fn read_regs(&mut self, addr: u16, n: u16) -> io::Result<Vec<u16>> {
        let r = self.call(&[&[4u8][..], &addr.to_be_bytes(), &n.to_be_bytes()].concat())?;
        match r.get(1).map(|&c| c as usize) == Some(r.len() - 2) && r.len() - 2 == 2 * n as usize {
            true => Ok(r[2..]
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect()),
            false => Err(bad("bad register count")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn frame_is_mbap_plus_pdu() {
        assert_eq!(
            frame(0x0102, &[5, 0, 3, 0xFF, 0]),
            [1, 2, 0, 0, 0, 6, 1, 5, 0, 3, 0xFF, 0]
        );
    }

    #[test]
    fn bits_are_lsb_first() {
        assert_eq!(
            [
                bit(&[0b0000_0101], 0),
                bit(&[0b0000_0101], 1),
                bit(&[0b0000_0101], 2),
                bit(&[0, 1], 8)
            ],
            [true, false, true, true]
        );
    }

    /// A one-shot server answering FC5 with the echo, FC2 with 3 bits `101`, FC4 with two registers.
    #[test]
    fn a_client_talks_to_a_server() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            for _ in 0..3 {
                let mut h = [0u8; 7];
                s.read_exact(&mut h).unwrap();
                let mut p = vec![0u8; u16::from_be_bytes([h[4], h[5]]) as usize - 1];
                s.read_exact(&mut p).unwrap();
                let body: Vec<u8> = match p[0] {
                    5 => p.clone(),
                    2 => vec![2, 1, 0b101],
                    _ => vec![4, 4, 0x40, 0x20, 0, 1],
                };
                s.write_all(&frame(u16::from_be_bytes([h[0], h[1]]), &body))
                    .unwrap();
            }
        });
        let mut m = Mb::connect("127.0.0.1", port).unwrap();
        m.write_coil(7, true).unwrap();
        let b = m.read_bits(0, 3).unwrap();
        assert_eq!([bit(&b, 0), bit(&b, 1), bit(&b, 2)], [true, false, true]);
        assert_eq!(m.read_regs(30000, 2).unwrap(), [0x4020, 1]);
        t.join().unwrap();
    }
}
