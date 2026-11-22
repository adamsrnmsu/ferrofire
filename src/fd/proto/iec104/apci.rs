//! IEC 60870-5-104 APCI layer: framing, 15-bit sequence numbers, the k/w windows and the t1/t2/t3 timers. Sans-IO like
//! `dnp3::link`: bytes and `Instant`s go in, `Action`s come out, and the caller owns the socket and the clock. Behaviour
//! follows `conformance/iec104/STOCK.md` (lib60870); where it is silent this follows IEC 60870-5-104.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Longest APDU body (control field plus ASDU) the length byte may announce.
pub const MAX_APDU: usize = 253;
const START: u8 = 0x68;
const SEQ_MASK: u16 = 0x7FFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum U {
    StartAct,
    StartCon,
    StopAct,
    StopCon,
    TestAct,
    TestCon,
}

const U_CODES: [(U, u8); 6] = [
    (U::StartAct, 0x07),
    (U::StartCon, 0x0B),
    (U::StopAct, 0x13),
    (U::StopCon, 0x23),
    (U::TestAct, 0x43),
    (U::TestCon, 0x83),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    I { ns: u16, nr: u16, asdu: Vec<u8> },
    S { nr: u16 },
    U(U),
}

pub fn encode(f: &Frame) -> Vec<u8> {
    let seq = |n: u16| [(n << 1) as u8, (n >> 7) as u8];
    let (ctl, asdu): ([u8; 4], &[u8]) = match f {
        Frame::I { ns, nr, asdu } => {
            let (a, b) = (seq(*ns), seq(*nr));
            ([a[0], a[1], b[0], b[1]], asdu)
        }
        Frame::S { nr } => ([0x01, 0, (nr << 1) as u8, (nr >> 7) as u8], &[]),
        Frame::U(u) => (
            [
                U_CODES.iter().find(|(x, _)| x == u).map_or(0, |c| c.1),
                0,
                0,
                0,
            ],
            &[],
        ),
    };
    debug_assert!(asdu.len() <= MAX_APDU - 4);
    let mut v = vec![START, (4 + asdu.len()) as u8];
    v.extend_from_slice(&ctl);
    v.extend_from_slice(asdu);
    v
}

/// Stream decoder. An `Err` means close the connection; it drops what was buffered and yields nothing more.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    failed: bool,
}

impl Decoder {
    pub fn push(&mut self, b: &[u8]) {
        if !self.failed {
            self.buf.extend_from_slice(b);
        }
    }

    /// Bytes held back (a partial frame, or a pushed chunk not yet drained).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    // Named `next` by the plan; it returns a Result, so it is not an Iterator.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Frame>, String> {
        match self.take() {
            Err(e) => {
                self.failed = true;
                self.buf.clear();
                Err(e)
            }
            ok => ok,
        }
    }

    fn take(&mut self) -> Result<Option<Frame>, String> {
        let Some(&first) = self.buf.first() else {
            return Ok(None);
        };
        if first != START {
            return Err(format!("bad start byte {first:#04x}"));
        }
        let Some(&len) = self.buf.get(1) else {
            return Ok(None);
        };
        let len = len as usize;
        if !(4..=MAX_APDU).contains(&len) {
            return Err(format!("bad APDU length {len}"));
        }
        if self.buf.len() < 2 + len {
            return Ok(None);
        }
        let body: Vec<u8> = self.buf.drain(..2 + len).skip(2).collect();
        parse(&body).map(Some)
    }
}

fn parse(body: &[u8]) -> Result<Frame, String> {
    let c = [body[0], body[1], body[2], body[3]];
    if c[0] & 1 == 1 {
        return parse_s_or_u(c, body.len());
    }
    if body.len() == 4 || c[2] & 1 != 0 {
        return Err("bad I-frame control field".into());
    }
    let seq = |lo: u8, hi: u8| u16::from_le_bytes([lo, hi]) >> 1;
    Ok(Frame::I {
        ns: seq(c[0], c[1]),
        nr: seq(c[2], c[3]),
        asdu: body[4..].to_vec(),
    })
}

fn parse_s_or_u(c: [u8; 4], len: usize) -> Result<Frame, String> {
    if len != 4 {
        return Err("S or U frame with an ASDU".into());
    }
    if c[0] == 0x01 && c[1] == 0 && c[2] & 1 == 0 {
        return Ok(Frame::S {
            nr: u16::from_le_bytes([c[2], c[3]]) >> 1,
        });
    }
    match U_CODES.iter().find(|(_, code)| *code == c[0]) {
        Some((u, _)) if c[1..] == [0, 0, 0] => Ok(Frame::U(*u)),
        _ => Err(format!("bad control field {c:02x?}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub k: u16,
    pub w: u16,
    pub t1: Duration,
    pub t2: Duration,
    pub t3: Duration,
}

impl Params {
    /// The IEC 104 defaults; also what stock lib60870 measured (STOCK.md).
    pub const STANDARD: Params = Params {
        k: 12,
        w: 8,
        t1: Duration::from_secs(15),
        t2: Duration::from_secs(10),
        t3: Duration::from_secs(20),
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send(Vec<u8>),
    /// An ASDU for the layer above.
    Deliver(Vec<u8>),
    Close(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Server,
    Client,
}

pub struct Link {
    p: Params,
    role: Role,
    dec: Decoder,
    started: bool,
    closed: bool,
    /// Next N(S) to send, next N(S) expected, and the oldest N(S) the peer has not acked.
    vs: u16,
    vr: u16,
    acked: u16,
    /// Send times of the unacked I-frames, oldest first (t1 runs from the front).
    unacked: VecDeque<Instant>,
    /// I-frames received and not yet acked by us, and when the t2 clock for them runs out.
    owed: u16,
    t2_at: Option<Instant>,
    last_rx: Option<Instant>,
    /// When our STARTDT or TESTFR act went out and is not yet confirmed (t1 applies).
    act_at: Option<Instant>,
}

impl Link {
    pub fn new(p: Params, role: Role) -> Link {
        Link {
            p,
            role,
            dec: Decoder::default(),
            started: false,
            closed: false,
            vs: 0,
            vr: 0,
            acked: 0,
            unacked: VecDeque::new(),
            owed: 0,
            t2_at: None,
            last_rx: None,
            act_at: None,
        }
    }

    pub fn started(&self) -> bool {
        self.started
    }

    /// Server, single redundancy group: another connection's STARTDT stops this one without a word (lib60870
    /// `MasterConnection_deactivate`, STOCK.md "two connections"), so its next I- or S-frame closes it.
    pub fn deactivate(&mut self) {
        self.started = false;
    }

    /// Client: send STARTDT act. The link counts as started at once because the stock client sends its interrogation before
    /// the STARTDT con arrives (STOCK.md).
    pub fn start(&mut self, now: Instant) -> Vec<Action> {
        self.started = true;
        self.act_at = Some(now);
        vec![send(&Frame::U(U::StartAct))]
    }

    /// Client, at the close: an S-frame for the I-frames received and not yet acknowledged, if any. lib60870 sends it before it
    /// closes (`cs104_connection.c:897-900`); to a server that has gone, it draws the RST in `client-iec104.golden` (GC:65).
    pub fn confirm(&mut self) -> Option<Vec<u8>> {
        (self.owed > 0).then(|| encode(&self.ack_frame()))
    }

    /// `None` when stopped, closed or k frames are unacked; the caller keeps the ASDU.
    pub fn send(&mut self, asdu: Vec<u8>, now: Instant) -> Option<Vec<Action>> {
        if !self.started || self.closed || self.unacked.len() >= self.p.k as usize {
            return None;
        }
        let f = Frame::I {
            ns: self.vs,
            nr: self.vr,
            asdu,
        };
        self.vs = (self.vs + 1) & SEQ_MASK;
        self.unacked.push_back(now);
        self.owed = 0;
        self.t2_at = None;
        Some(vec![send(&f)])
    }

    pub fn on_bytes(&mut self, b: &[u8], now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        if self.closed {
            return out;
        }
        self.last_rx.get_or_insert(now);
        self.dec.push(b);
        loop {
            match self.dec.next() {
                Ok(Some(f)) => {
                    self.last_rx = Some(now);
                    self.frame(f, now, &mut out);
                }
                Ok(None) => break,
                Err(e) => out.push(self.close(e)),
            }
            if self.closed {
                break;
            }
        }
        out
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        if self.closed {
            return vec![];
        }
        let last_rx = *self.last_rx.get_or_insert(now);
        let expired = |t: Instant, d: Duration| now >= t + d;
        if self.unacked.front().is_some_and(|&t| expired(t, self.p.t1)) {
            return vec![self.close("t1: I-frame not acknowledged".into())];
        }
        if let Some(t) = self.act_at {
            if expired(t, self.p.t1) {
                return vec![self.close("t1: STARTDT or TESTFR not confirmed".into())];
            }
        } else if expired(last_rx, self.p.t3) {
            self.act_at = Some(now);
            return vec![send(&Frame::U(U::TestAct))];
        }
        if self.t2_at.is_some_and(|t| now >= t) {
            return vec![self.ack_now()];
        }
        vec![]
    }

    fn close(&mut self, why: String) -> Action {
        self.closed = true;
        Action::Close(why)
    }

    fn ack_now(&mut self) -> Action {
        send(&self.ack_frame())
    }

    fn ack_frame(&mut self) -> Frame {
        self.owed = 0;
        self.t2_at = None;
        Frame::S { nr: self.vr }
    }

    /// Apply the peer's N(R). An ack for frames we never sent is a protocol violation (IEC 104; STOCK.md is silent).
    fn ack(&mut self, nr: u16) -> Result<(), String> {
        let n = nr.wrapping_sub(self.acked) & SEQ_MASK;
        if n as usize > self.unacked.len() {
            return Err(format!("N(R) {nr} acknowledges frames not sent"));
        }
        self.unacked.drain(..n as usize);
        self.acked = nr;
        Ok(())
    }

    fn frame(&mut self, f: Frame, now: Instant, out: &mut Vec<Action>) {
        // A server only accepts I and S frames while started (STOCK.md); a client's `started` is set by `start`.
        let result = match f {
            Frame::I { .. } | Frame::S { .. } if !self.started && self.role == Role::Server => {
                Err("I or S frame before STARTDT".into())
            }
            Frame::I { ns, nr, asdu } => self.on_i(ns, nr, asdu, now, out),
            Frame::S { nr } => self.ack(nr),
            Frame::U(u) => {
                self.on_u(u, out);
                Ok(())
            }
        };
        if let Err(e) = result {
            out.push(self.close(e));
        }
    }

    fn on_i(
        &mut self,
        ns: u16,
        nr: u16,
        asdu: Vec<u8>,
        now: Instant,
        out: &mut Vec<Action>,
    ) -> Result<(), String> {
        if ns != self.vr {
            return Err(format!("N(S) {ns}, expected {}", self.vr));
        }
        self.ack(nr)?;
        self.vr = (self.vr + 1) & SEQ_MASK;
        self.owed += 1;
        out.push(Action::Deliver(asdu));
        if self.owed >= self.p.w {
            out.push(self.ack_now());
        } else {
            self.t2_at.get_or_insert(now + self.p.t2);
        }
        Ok(())
    }

    fn on_u(&mut self, u: U, out: &mut Vec<Action>) {
        match u {
            // lib60870's client answers a STARTDT act too (`cs104_connection.c:690-700`); its started is set by `start`
            U::StartAct => {
                self.started = true;
                out.push(send(&Frame::U(U::StartCon)));
            }
            U::StopAct if self.role == Role::Server => {
                // S-frame first so the master sees its I-frames acked, then the con (STOCK.md).
                out.push(self.ack_now());
                self.started = false;
                out.push(send(&Frame::U(U::StopCon)));
            }
            U::TestAct => out.push(send(&Frame::U(U::TestCon))),
            // a server ignores both, as stock's: no mask of `cs104_slave.c:2281-2338` matches them
            U::StartCon | U::StopCon if self.role == Role::Server => {}
            U::StartCon | U::TestCon => self.act_at = None,
            U::StopCon => self.started = false,
            U::StopAct => {}
        }
    }
}

fn send(f: &Frame) -> Action {
    Action::Send(encode(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: [u8; 6] = [0x68, 0x04, 0x07, 0, 0, 0];

    fn sends(a: &[Action]) -> Vec<Vec<u8>> {
        a.iter()
            .filter_map(|x| {
                if let Action::Send(b) = x {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    fn closed(a: &[Action]) -> bool {
        a.iter().any(|x| matches!(x, Action::Close(_)))
    }
    fn i_frame(ns: u16, nr: u16) -> Vec<u8> {
        encode(&Frame::I {
            ns,
            nr,
            asdu: vec![1, 2, 3],
        })
    }
    fn s_frame(nr: u16) -> Vec<u8> {
        encode(&Frame::S { nr })
    }
    fn server(now: Instant) -> Link {
        let mut l = Link::new(Params::STANDARD, Role::Server);
        l.on_bytes(&START, now);
        assert!(l.started());
        l
    }

    #[test]
    fn startdt_act_gets_con() {
        let mut l = Link::new(Params::STANDARD, Role::Server);
        let a = l.on_bytes(&START, Instant::now());
        assert_eq!(a, vec![Action::Send(vec![0x68, 0x04, 0x0B, 0, 0, 0])]);
        assert!(l.started());
    }

    #[test]
    fn i_frame_round_trips_with_sequence_numbers() {
        let f = Frame::I {
            ns: 0x1234,
            nr: 0x7ABC,
            asdu: vec![9, 8, 7],
        };
        let mut d = Decoder::default();
        d.push(&encode(&f));
        assert_eq!(d.next(), Ok(Some(f)));
        assert_eq!(d.next(), Ok(None));
        let now = Instant::now();
        let mut l = server(now);
        let a = l.on_bytes(&i_frame(0, 0), now);
        assert_eq!(a, vec![Action::Deliver(vec![1, 2, 3])]);
        let a = l.send(vec![5], now).unwrap();
        assert_eq!(
            sends(&a),
            vec![encode(&Frame::I {
                ns: 0,
                nr: 1,
                asdu: vec![5]
            })]
        );
    }

    #[test]
    fn sequence_numbers_wrap_at_32768() {
        let now = Instant::now();
        let mut l = server(now);
        for i in 0..32770u32 {
            let a = l.on_bytes(&i_frame((i & 0x7FFF) as u16, 0), now);
            assert!(!closed(&a), "receive {i}");
        }
        for i in 0..32770u32 {
            let a = l.send(vec![1], now).unwrap();
            let want = encode(&Frame::I {
                ns: (i & 0x7FFF) as u16,
                nr: 2,
                asdu: vec![1],
            });
            assert_eq!(sends(&a), vec![want]);
            let a = l.on_bytes(&s_frame(((i + 1) & 0x7FFF) as u16), now);
            assert!(!closed(&a), "ack {i}");
        }
    }

    #[test]
    fn an_s_frame_follows_w_received_i_frames() {
        let now = Instant::now();
        let mut l = server(now);
        for n in 0..7 {
            assert_eq!(
                sends(&l.on_bytes(&i_frame(n, 0), now)),
                Vec::<Vec<u8>>::new()
            );
        }
        assert_eq!(sends(&l.on_bytes(&i_frame(7, 0), now)), vec![s_frame(8)]);
    }

    #[test]
    fn an_s_frame_follows_t2_after_an_unacked_i_frame() {
        let t0 = Instant::now();
        let mut l = server(t0);
        l.on_bytes(&i_frame(0, 0), t0);
        assert!(sends(&l.tick(t0 + Duration::from_secs(9))).is_empty());
        assert_eq!(
            sends(&l.tick(t0 + Duration::from_secs(10))),
            vec![s_frame(1)]
        );
        assert!(sends(&l.tick(t0 + Duration::from_secs(11))).is_empty());
    }

    #[test]
    fn send_returns_none_at_k_unacked_and_t1_closes() {
        let t0 = Instant::now();
        let mut l = server(t0);
        for i in 0..12 {
            assert!(l.send(vec![0], t0 + Duration::from_secs(i)).is_some());
        }
        assert!(l.send(vec![0], t0 + Duration::from_secs(12)).is_none());
        // t1 runs from the oldest unacked frame (sent at t0), not the newest.
        assert!(!closed(&l.tick(t0 + Duration::from_secs(14))));
        assert!(closed(&l.tick(t0 + Duration::from_secs(15))));
        // An ack reopens the window.
        let mut l = server(t0);
        for _ in 0..12 {
            l.send(vec![0], t0).unwrap();
        }
        l.on_bytes(&s_frame(1), t0);
        assert!(l.send(vec![0], t0).is_some());
        assert!(l.send(vec![0], t0).is_none());
    }

    #[test]
    fn testfr_act_gets_con() {
        let mut l = Link::new(Params::STANDARD, Role::Server);
        let a = l.on_bytes(&[0x68, 0x04, 0x43, 0, 0, 0], Instant::now());
        assert_eq!(a, vec![Action::Send(vec![0x68, 0x04, 0x83, 0, 0, 0])]);
        assert!(!l.started());
    }

    #[test]
    fn t3_idle_sends_testfr_act() {
        let t0 = Instant::now();
        let mut l = server(t0);
        assert!(l.tick(t0 + Duration::from_secs(19)).is_empty());
        let t3 = t0 + Duration::from_secs(20);
        assert_eq!(
            l.tick(t3),
            vec![Action::Send(vec![0x68, 0x04, 0x43, 0, 0, 0])]
        );
        assert!(l.tick(t3 + Duration::from_secs(14)).is_empty());
        assert!(closed(&l.tick(t3 + Duration::from_secs(15))));
        // Answered in time: no close, and the idle clock restarts.
        let mut l = server(t0);
        l.tick(t3);
        let t4 = t3 + Duration::from_secs(5);
        l.on_bytes(&[0x68, 0x04, 0x83, 0, 0, 0], t4);
        assert!(l.tick(t4 + Duration::from_secs(19)).is_empty());
        assert_eq!(sends(&l.tick(t4 + Duration::from_secs(20))).len(), 1);
    }

    #[test]
    fn bad_start_byte_or_length_closes() {
        for bad in [
            vec![0x69, 0x04, 0x07, 0, 0, 0],
            vec![0x68, 0x03, 0x07, 0, 0, 0],
            vec![0x68, 0xFE, 0x07, 0, 0, 0],
            vec![0x68, 0x04, 0x09, 0, 0, 0],
            vec![0x68, 0x04, 0x07, 1, 0, 0],
            vec![0x68, 0x04, 0x00, 0, 0, 0],
        ] {
            let mut d = Decoder::default();
            d.push(&bad);
            assert!(d.next().is_err(), "{bad:02x?}");
            let mut l = Link::new(Params::STANDARD, Role::Server);
            assert!(closed(&l.on_bytes(&bad, Instant::now())), "{bad:02x?}");
        }
    }

    #[test]
    fn i_and_s_frames_before_startdt_close_the_server() {
        for f in [i_frame(0, 0), s_frame(0)] {
            let mut l = Link::new(Params::STANDARD, Role::Server);
            let a = l.on_bytes(&f, Instant::now());
            assert_eq!(a.len(), 1);
            assert!(closed(&a));
        }
    }

    #[test]
    fn a_deactivated_server_answers_testfr_but_closes_on_an_i_frame() {
        let t = Instant::now();
        let mut l = server(t);
        l.deactivate();
        assert!(!l.started());
        assert_eq!(l.send(vec![1], t), None);
        let test = l.on_bytes(&[0x68, 4, 0x43, 0, 0, 0], t);
        assert_eq!(sends(&test), [vec![0x68, 4, 0x83, 0, 0, 0]]);
        assert!(closed(&l.on_bytes(&i_frame(0, 0), t)));
    }

    /// Stock's server classifies by mask and ignores what matches none (`cs104_slave.c:2340-2343`): a STOPDT or STARTDT con from
    /// the master neither stops it nor ends its TESTFR wait (only a TESTFR con does, `:2321-2327`).
    #[test]
    fn a_server_ignores_stopdt_and_startdt_con() {
        let t0 = Instant::now();
        let mut l = server(t0);
        for con in [0x23, 0x0B] {
            assert_eq!(l.on_bytes(&[0x68, 4, con, 0, 0, 0], t0), vec![]);
            assert!(l.started(), "{con:#04x}");
        }
        let t3 = t0 + Duration::from_secs(20);
        assert_eq!(sends(&l.tick(t3)).len(), 1, "TESTFR act");
        l.on_bytes(&[0x68, 4, 0x0B, 0, 0, 0], t3);
        assert!(closed(&l.tick(t3 + Duration::from_secs(15))));
    }

    #[test]
    fn stopdt_acks_then_confirms_and_stops_sending() {
        let now = Instant::now();
        let mut l = server(now);
        l.on_bytes(&i_frame(0, 0), now);
        let a = l.on_bytes(&[0x68, 0x04, 0x13, 0, 0, 0], now);
        assert_eq!(sends(&a), vec![s_frame(1), vec![0x68, 0x04, 0x23, 0, 0, 0]]);
        assert!(!l.started());
        assert!(l.send(vec![0], now).is_none());
        assert!(closed(&l.on_bytes(&i_frame(1, 0), now)));
    }

    #[test]
    fn an_ack_for_frames_never_sent_closes() {
        let now = Instant::now();
        let mut l = server(now);
        assert!(closed(&l.on_bytes(&s_frame(1), now)));
        let mut l = server(now);
        l.send(vec![0], now).unwrap();
        assert!(!closed(&l.on_bytes(&s_frame(1), now)));
        assert!(closed(&l.on_bytes(&i_frame(0, 5), now)));
    }

    #[test]
    fn an_out_of_order_i_frame_closes() {
        let now = Instant::now();
        let mut l = server(now);
        assert!(closed(&l.on_bytes(&i_frame(1, 0), now)));
    }

    #[test]
    fn client_start_sends_startdt_and_may_send_at_once() {
        let now = Instant::now();
        let mut l = Link::new(Params::STANDARD, Role::Client);
        assert_eq!(sends(&l.start(now)), vec![START.to_vec()]);
        assert!(l.send(vec![7], now).is_some());
        assert!(l.on_bytes(&[0x68, 0x04, 0x0B, 0, 0, 0], now).is_empty());
        // No STARTDT con within t1: close.
        let mut l = Link::new(Params::STANDARD, Role::Client);
        l.start(now);
        assert!(closed(&l.tick(now + Duration::from_secs(15))));
    }

    #[test]
    fn a_client_confirms_what_it_owes_at_the_close() {
        let now = Instant::now();
        let mut l = Link::new(Params::STANDARD, Role::Client);
        l.start(now);
        assert_eq!(l.confirm(), None);
        l.on_bytes(&[i_frame(0, 0), i_frame(1, 0)].concat(), now);
        assert_eq!(l.confirm(), Some(s_frame(2)));
        assert_eq!(l.confirm(), None);
        // a STARTDT act from the server is confirmed, a STOPDT act ignored, as lib60870's client does
        assert_eq!(
            sends(&l.on_bytes(&START, now)),
            vec![[0x68, 4, 0x0B, 0, 0, 0].to_vec()]
        );
        assert!(l.on_bytes(&[0x68, 4, 0x13, 0, 0, 0], now).is_empty());
        assert!(l.started());
    }

    #[test]
    fn a_stream_split_anywhere_decodes_the_same() {
        let frames = [i_frame(0, 0), s_frame(3), START.to_vec(), i_frame(1, 2)];
        let all: Vec<u8> = frames.concat();
        let whole = {
            let mut d = Decoder::default();
            d.push(&all);
            let mut v = vec![];
            while let Some(f) = d.next().unwrap() {
                v.push(f);
            }
            v
        };
        assert_eq!(whole.len(), 4);
        for cut in 0..=all.len() {
            let mut d = Decoder::default();
            let mut v = vec![];
            for part in [&all[..cut], &all[cut..]] {
                d.push(part);
                while let Some(f) = d.next().unwrap() {
                    v.push(f);
                }
            }
            assert_eq!(v, whole, "cut at {cut}");
        }
        let mut d = Decoder::default();
        let mut v = vec![];
        for b in &all {
            d.push(&[*b]);
            while let Some(f) = d.next().unwrap() {
                v.push(f);
            }
        }
        assert_eq!(v, whole);
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let now = Instant::now();
        for _ in 0..10_000 {
            let n = (rnd() % 600) as usize;
            // Half the buffers start like a frame so the length and control paths are reached.
            let mut buf: Vec<u8> = (0..n).map(|_| rnd() as u8).collect();
            if rnd() % 2 == 0 && n > 1 {
                buf[0] = 0x68;
            }
            let mut d = Decoder::default();
            d.push(&buf);
            while let Ok(Some(_)) = d.next() {}
            assert!(d.buffered() < MAX_APDU + 2, "buffered {}", d.buffered());
            let mut l = Link::new(Params::STANDARD, Role::Server);
            l.on_bytes(&START, now);
            l.on_bytes(&buf, now);
            l.tick(now + Duration::from_secs(40));
        }
    }
}
