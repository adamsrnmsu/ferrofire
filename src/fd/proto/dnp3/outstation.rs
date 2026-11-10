//! The outstation as a state machine with no I/O: `Device` is what the whole outstation shares (points, events, indications),
//! `Conn` is one master's connection (link and transport state, the fragment waiting for a CONFIRM). Bytes in, bytes out, and
//! the clock is an argument, so the golden replay and the fuzz target drive it exactly as the socket does.

use super::app::{self, iin, Request, CON, FIN, FIR, MAX_APDU, RESPONSE_HEADER};
use super::command::{self, Mode, Selection, TIME_SYNC_VALID};
use super::db::{AnaPoint, BinPoint, Data, Events, Typ};
use super::link::{self, Frame, Parse};
use super::read::{self, Block};
use super::transport::{self, Reassembler};
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

/// How long the outstation waits for a CONFIRM (opendnp3 `solConfirmTimeout`).
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
/// Outstation link addresses 0xFFFD..=0xFFFF are broadcasts: acted on, never answered.
const BROADCAST: u16 = 0xFFFD;

/// Where a command goes: the datastore's write queue (and the event log).
pub trait Sink {
    fn binary(&mut self, tag: &str, index: u16, value: bool);
    fn analog(&mut self, tag: &str, index: u16, value: f32);
}

/// Where the 1 Hz refresh reads current values: `(value, provider timestamp in seconds)`.
pub trait Source {
    fn binary(&self, tag: &str) -> Option<bool>;
    fn analog(&self, tag: &str) -> Option<(f64, u64)>;
}

/// Everything the connections share.
pub struct Device {
    pub local: u16,
    pub master: u16,
    pub bin: BTreeMap<u16, BinPoint>,
    pub ana: BTreeMap<u16, AnaPoint>,
    pub events: Events,
    pub selection: Option<Selection>,
    pub restart: bool,
    need_time: bool,
    synced: Option<Duration>,
    broadcast: bool,
    tx_seq: u8,
}

impl Device {
    pub fn new(
        local: u16,
        master: u16,
        bin: BTreeMap<u16, BinPoint>,
        ana: BTreeMap<u16, AnaPoint>,
    ) -> Device {
        Device {
            local,
            master,
            bin,
            ana,
            events: Events::default(),
            selection: None,
            restart: true,
            need_time: true,
            synced: None,
            broadcast: false,
            tx_seq: 0,
        }
    }

    /// The 1 Hz sweep (bennu `Server::update`): every mapped point takes the datastore's value, binaries first, each in index
    /// order; a change on an input raises an event.
    pub fn refresh(&mut self, src: &dyn Source) {
        for (idx, p) in self.bin.iter_mut() {
            if let Some(v) = src.binary(&p.tag) {
                if p.update(v) {
                    self.events.push(*idx, Data::Bin { flags: p.flags });
                }
            }
        }
        for (idx, p) in self.ana.iter_mut() {
            if let Some((v, ts)) = src.analog(&p.tag) {
                if p.update(v) {
                    self.events.push(*idx, Data::Ana { value: v, ts });
                }
            }
        }
    }

    /// A master wrote the time: NEED_TIME clears, and comes back after `TIME_SYNC_VALID`.
    pub fn set_time(&mut self, now: Duration) {
        self.need_time = false;
        self.synced = Some(now);
    }

    /// The highest index of a type's points (0 when it has none).
    pub fn max_index(&self, typ: Typ) -> u16 {
        match typ {
            Typ::Binary => self.bin.keys().next_back().copied().unwrap_or(0),
            Typ::Analog => self.ana.keys().next_back().copied().unwrap_or(0),
        }
    }

    /// Internal indications for a response sent at `now`.
    pub fn iin(&mut self, now: Duration) -> u16 {
        if self
            .synced
            .is_some_and(|t| now.saturating_sub(t) >= TIME_SYNC_VALID)
        {
            (self.need_time, self.synced) = (true, None);
        }
        let flags = [
            (self.restart, iin::RESTART),
            (self.need_time, iin::NEED_TIME),
            (self.broadcast, iin::BROADCAST),
            (self.events.unselected(), iin::CLASS1),
            (self.events.overflowed(), iin::OVERFLOW),
        ];
        flags.iter().filter(|f| f.0).fold(0, |a, f| a | f.1)
    }
}

/// The connection must be closed.
#[derive(Debug, PartialEq, Eq)]
pub struct Close(pub &'static str);

/// A fragment sent with CON set, and the rest of the response that follows its confirmation.
struct Waiting {
    seq: u8,
    rest: VecDeque<Block>,
    sent: Duration,
}

/// One master connection.
pub struct Conn {
    rx: Vec<u8>,
    reasm: Reassembler,
    reset: bool,
    expect_fcb: bool,
    waiting: Option<Waiting>,
}

impl Default for Conn {
    fn default() -> Self {
        Conn::new()
    }
}

/// What a request is answered with.
enum Reply {
    /// Objects and indications to add.
    Simple(Vec<u8>, u16),
    /// A READ: the blocks to pack and the indications to add.
    Read(VecDeque<Block>, u16),
}

impl Conn {
    pub fn new() -> Conn {
        Conn {
            rx: vec![],
            reasm: Reassembler::new(MAX_APDU),
            reset: false,
            expect_fcb: true,
            waiting: None,
        }
    }

    /// Take bytes from the master; returns the bytes to send back. `Err` means a framing violation: close the connection.
    pub fn feed(
        &mut self,
        dev: &mut Device,
        sink: &mut dyn Sink,
        bytes: &[u8],
        now: Duration,
    ) -> Result<Vec<u8>, Close> {
        self.rx.extend_from_slice(bytes);
        let mut out = vec![];
        loop {
            match link::parse(&self.rx) {
                Parse::Need(_) => return Ok(out),
                Parse::Bad(why) => return Err(Close(why)),
                Parse::Frame(f, used) => {
                    self.rx.drain(..used);
                    self.frame(dev, sink, f, now, &mut out)?;
                }
            }
        }
    }

    /// Give up on a CONFIRM that did not come.
    pub fn tick(&mut self, dev: &mut Device, now: Duration) {
        if self
            .waiting
            .as_ref()
            .is_some_and(|w| now.saturating_sub(w.sent) >= CONFIRM_TIMEOUT)
        {
            self.cancel(dev);
        }
    }

    /// The connection ended: what it had selected or written goes back to the buffer.
    pub fn cancel(&mut self, dev: &mut Device) {
        self.waiting = None;
        dev.events.reset();
    }

    fn send(&self, dev: &Device, ctrl: u8, user: &[u8], out: &mut Vec<u8>) {
        out.extend(link::encode(ctrl, dev.master, dev.local, user));
    }

    /// A frame from our master to our address (or a broadcast), as a primary station.
    fn addressed(f: &Frame, dev: &Device) -> bool {
        f.ctrl & link::DIR != 0
            && f.primary()
            && (f.dst == dev.local || f.dst >= BROADCAST)
            && f.src == dev.master
    }

    fn frame(
        &mut self,
        dev: &mut Device,
        sink: &mut dyn Sink,
        f: Frame,
        now: Duration,
        out: &mut Vec<u8>,
    ) -> Result<(), Close> {
        if !Self::addressed(&f, dev) {
            return Ok(());
        }
        match f.func() {
            3 => self.confirmed_data(dev, sink, &f, now, out),
            4 => self.user_data(dev, sink, &f, now, out),
            _ => {
                self.link_control(dev, &f, out);
                Ok(())
            }
        }
    }

    /// RESET LINK STATES, TEST LINK STATES and REQUEST LINK STATUS: link management with no application data.
    fn link_control(&mut self, dev: &Device, f: &Frame, out: &mut Vec<u8>) {
        let broadcast = f.dst >= BROADCAST;
        match f.func() {
            0 => {
                (self.reset, self.expect_fcb) = (true, true);
                self.ack(dev, broadcast, out);
            }
            2 if self.fresh(f) == Some(true) => self.ack(dev, broadcast, out),
            9 => self.send(dev, 0x0B, &[], out),
            _ => {}
        }
    }

    /// Confirmed user data: acknowledged, and delivered unless it repeats the last frame.
    fn confirmed_data(
        &mut self,
        dev: &mut Device,
        sink: &mut dyn Sink,
        f: &Frame,
        now: Duration,
        out: &mut Vec<u8>,
    ) -> Result<(), Close> {
        let Some(fresh) = self.fresh(f) else {
            return Ok(());
        };
        self.ack(dev, f.dst >= BROADCAST, out);
        if fresh {
            self.user_data(dev, sink, f, now, out)?;
        }
        Ok(())
    }

    fn ack(&self, dev: &Device, broadcast: bool, out: &mut Vec<u8>) {
        if !broadcast {
            self.send(dev, 0x00, &[], out);
        }
    }

    /// Frame-count-bit check of a frame that has one: `None` when it must be ignored (link not reset, no FCV), `Some(true)`
    /// for the next expected frame, `Some(false)` for a repeat.
    fn fresh(&mut self, f: &Frame) -> Option<bool> {
        if !self.reset || f.ctrl & link::FCV == 0 {
            return None;
        }
        let fresh = (f.ctrl & link::FCB != 0) == self.expect_fcb;
        if fresh {
            self.expect_fcb = !self.expect_fcb;
        }
        Some(fresh)
    }

    fn user_data(
        &mut self,
        dev: &mut Device,
        sink: &mut dyn Sink,
        f: &Frame,
        now: Duration,
        out: &mut Vec<u8>,
    ) -> Result<(), Close> {
        match self.reasm.push(&f.user) {
            Err(_) => Err(Close("fragment too long")),
            Ok(None) => Ok(()),
            Ok(Some(frag)) => {
                self.fragment(dev, sink, &frag, f.dst >= BROADCAST, now, out);
                Ok(())
            }
        }
    }

    fn fragment(
        &mut self,
        dev: &mut Device,
        sink: &mut dyn Sink,
        frag: &[u8],
        broadcast: bool,
        now: Duration,
        out: &mut Vec<u8>,
    ) {
        let Some(req) = Request::parse(frag) else {
            return;
        };
        if !req.single() || req.unsolicited() {
            return;
        }
        if req.func == app::func::CONFIRM {
            return self.confirmed(dev, req.seq(), now, out);
        }
        self.cancel(dev);
        dev.broadcast |= broadcast;
        let reply = Self::answer(dev, sink, &req, now);
        if broadcast || req.func == app::func::DIRECT_OPERATE_NO_ACK {
            return;
        }
        match reply {
            Reply::Simple(objects, extra) => {
                let frag = app::response(req.seq(), FIR | FIN, dev.iin(now) | extra, &objects);
                dev.broadcast = false;
                self.segments(dev, &frag, out);
            }
            Reply::Read(blocks, extra) => {
                self.read_fragment(dev, req.seq(), true, blocks, extra, now, out)
            }
        }
    }

    fn control(dev: &mut Device, sink: &mut dyn Sink, req: &Request, now: Duration) -> Reply {
        use app::func::*;
        let mode = match req.func {
            SELECT => Mode::Select,
            OPERATE => Mode::Operate,
            _ => Mode::Direct,
        };
        match command::control(dev, sink, req, mode, now) {
            Ok(echo) => Reply::Simple(echo, 0),
            Err(e) => Reply::Simple(vec![], e),
        }
    }

    /// Function codes answered with a fixed reply, or refused as unsupported.
    fn plain(func: u8) -> Reply {
        use app::func::*;
        match func {
            DELAY_MEASURE => Reply::Simple(vec![52, 2, 7, 1, 0, 0], 0),
            IMMED_FREEZE | ASSIGN_CLASS | RECORD_CURRENT_TIME => Reply::Simple(vec![], 0),
            _ => Reply::Simple(vec![], iin::NO_FUNC),
        }
    }

    fn read(dev: &mut Device, req: &Request) -> Reply {
        match read::plan(dev, req.objects) {
            Ok(p) => Reply::Read(p.blocks, p.iin),
            Err(e) => Reply::Simple(vec![], e),
        }
    }

    fn answer(dev: &mut Device, sink: &mut dyn Sink, req: &Request, now: Duration) -> Reply {
        use app::func::*;
        match req.func {
            READ => Self::read(dev, req),
            WRITE => Reply::Simple(
                vec![],
                command::write(dev, req.objects, now).err().unwrap_or(0),
            ),
            SELECT | OPERATE | DIRECT_OPERATE | DIRECT_OPERATE_NO_ACK => {
                Self::control(dev, sink, req, now)
            }
            f => Self::plain(f),
        }
    }

    fn confirmed(&mut self, dev: &mut Device, seq: u8, now: Duration, out: &mut Vec<u8>) {
        if self.waiting.as_ref().is_none_or(|w| w.seq != seq) {
            return;
        }
        let Some(w) = self.waiting.take() else { return };
        dev.events.clear_written();
        if !w.rest.is_empty() {
            self.read_fragment(dev, (seq + 1) & 0x0F, false, w.rest, 0, now, out);
        }
    }

    /// Send the next fragment of a READ response: events and static data packed to the fragment limit. It asks for a
    /// CONFIRM when it carries events or is not the last.
    #[allow(clippy::too_many_arguments)]
    fn read_fragment(
        &mut self,
        dev: &mut Device,
        seq: u8,
        first: bool,
        mut blocks: VecDeque<Block>,
        extra: u16,
        now: Duration,
        out: &mut Vec<u8>,
    ) {
        let (objects, ids) = read::pack(&mut blocks, MAX_APDU - RESPONSE_HEADER);
        dev.events.mark_written(&ids);
        let fin = blocks.is_empty();
        let con = !fin || !ids.is_empty();
        let flags =
            if first { FIR } else { 0 } | if fin { FIN } else { 0 } | if con { CON } else { 0 };
        let frag = app::response(seq, flags, dev.iin(now) | extra, &objects);
        dev.broadcast = false;
        self.segments(dev, &frag, out);
        self.waiting = con.then_some(Waiting {
            seq,
            rest: blocks,
            sent: now,
        });
    }

    fn segments(&self, dev: &mut Device, frag: &[u8], out: &mut Vec<u8>) {
        for seg in transport::segment(frag, &mut dev.tx_seq) {
            self.send(dev, 0x44, &seg, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::proto::dnp3::app::iin;
    use crate::fd::proto::dnp3::link::encode;

    /// bi[2] input, bo[5] output (sbo), bs[7] output; ai[0] input, ao[1] output (sbo).
    fn device() -> Device {
        let bin = [
            (2, BinPoint::new("bi", false, true)),
            (5, BinPoint::new("bo", false, false)),
            (7, BinPoint::new("bs", true, false)),
        ];
        let ana = [
            (0, AnaPoint::new("ai", false, true)),
            (1, AnaPoint::new("ao", true, false)),
        ];
        Device::new(10, 1, bin.into_iter().collect(), ana.into_iter().collect())
    }

    #[derive(Default)]
    struct Rec(Vec<String>);
    impl Sink for Rec {
        fn binary(&mut self, tag: &str, index: u16, value: bool) {
            self.0.push(format!("{tag}@{index}={value}"));
        }
        fn analog(&mut self, tag: &str, index: u16, value: f32) {
            self.0.push(format!("{tag}@{index}={value}"));
        }
    }

    struct Vals(f64, u64);
    impl Source for Vals {
        fn binary(&self, tag: &str) -> Option<bool> {
            (tag == "bi").then_some(self.0 != 0.0)
        }
        fn analog(&self, tag: &str) -> Option<(f64, u64)> {
            (tag == "ai").then_some((self.0, self.1))
        }
    }

    struct Rig {
        dev: Device,
        conn: Conn,
        rec: Rec,
        now: Duration,
        tp: u8,
    }

    const SEC: Duration = Duration::from_secs(1);

    impl Rig {
        fn new() -> Rig {
            Rig {
                dev: device(),
                conn: Conn::new(),
                rec: Rec::default(),
                now: Duration::ZERO,
                tp: 0,
            }
        }

        fn link(&mut self, ctrl: u8, dst: u16, src: u16, user: &[u8]) -> Result<Vec<u8>, Close> {
            self.conn.feed(
                &mut self.dev,
                &mut self.rec,
                &encode(ctrl, dst, src, user),
                self.now,
            )
        }

        /// Send an application fragment as an unconfirmed-user-data frame to the outstation; returns the wire reply.
        fn to(&mut self, dst: u16, app: &[u8]) -> Vec<u8> {
            let mut user = vec![0xC0 | self.tp];
            self.tp = (self.tp + 1) & 63;
            user.extend_from_slice(app);
            self.link(0xC4, dst, 1, &user).unwrap()
        }

        /// The first reply fragment to `app` (ctl, function, objects), or empty.
        fn ask(&mut self, app: &[u8]) -> Vec<u8> {
            let wire = self.to(10, app);
            match link::parse(&wire) {
                Parse::Frame(f, _) => f.user[1..].to_vec(),
                _ => vec![],
            }
        }

        fn iin_of(reply: &[u8]) -> u16 {
            u16::from_le_bytes([reply[2], reply[3]])
        }

        fn read(&mut self, seq: u8, objects: &[u8]) -> Vec<u8> {
            let mut a = vec![0xC0 | seq, 1];
            a.extend_from_slice(objects);
            self.ask(&a)
        }

        fn confirm(&mut self, seq: u8) -> Vec<u8> {
            self.ask(&[0xC0 | seq, 0])
        }
    }

    const CLASS1: [u8; 3] = [60, 2, 6];
    fn crob(idx: u8, code: u8) -> Vec<u8> {
        let mut v = vec![12, 1, 0x17, 1, idx, code, 1, 100, 0, 0, 0, 0, 0, 0, 0, 0];
        v[15] = 0;
        v
    }
    fn req(seq: u8, func: u8, objects: &[u8]) -> Vec<u8> {
        [&[0xC0 | seq, func][..], objects].concat()
    }

    #[test]
    fn a_fresh_device_needs_time_and_a_restart_clear_and_the_time_rule_is_thirty_seconds() {
        let mut d = device();
        assert_eq!(d.iin(Duration::ZERO), iin::RESTART | iin::NEED_TIME);
        d.restart = false;
        d.set_time(10 * SEC);
        assert_eq!(d.iin(10 * SEC), 0);
        assert_eq!(d.iin(39 * SEC + Duration::from_millis(999)), 0);
        assert_eq!(d.iin(40 * SEC), iin::NEED_TIME);
        d.set_time(41 * SEC);
        assert_eq!(d.iin(60 * SEC), 0, "a new write restarts the clock");
        assert_eq!(d.iin(71 * SEC), iin::NEED_TIME);
    }

    #[test]
    fn writes_clear_the_restart_bit_and_set_the_time() {
        let mut r = Rig::new();
        let before = Rig::iin_of(&r.read(0, &[60, 1, 6]));
        assert_eq!(
            before & (iin::RESTART | iin::NEED_TIME),
            iin::RESTART | iin::NEED_TIME
        );
        let reply = r.ask(&req(1, 2, &[80, 1, 0, 7, 7, 0]));
        assert_eq!(Rig::iin_of(&reply) & iin::RESTART, 0);
        let reply = r.ask(&req(2, 2, &[50, 1, 7, 1, 0, 0, 0, 0, 0, 0]));
        assert_eq!(Rig::iin_of(&reply), 0);
        assert_eq!(
            Rig::iin_of(&r.ask(&req(3, 2, &[80, 1, 0, 3, 3, 0]))),
            iin::PARAM_ERROR
        );
        assert_eq!(
            Rig::iin_of(&r.ask(&req(4, 2, &[80, 1, 0, 7, 7, 1]))),
            iin::PARAM_ERROR
        );
        assert_eq!(
            Rig::iin_of(&r.ask(&req(5, 2, &[50, 1, 8, 1, 0, 0, 0, 0, 0, 0]))),
            iin::PARAM_ERROR
        );
        assert_eq!(Rig::iin_of(&r.ask(&req(6, 2, &[1, 2, 6]))), iin::NO_FUNC);
        assert_eq!(
            Rig::iin_of(&r.ask(&req(7, 2, &[99, 1, 6]))),
            iin::UNKNOWN_OBJECT
        );
        // two objects in one WRITE are both applied
        let both = [&[80u8, 1, 0, 7, 7, 0][..], &[50, 1, 7, 1, 0, 0, 0, 0, 0, 0]].concat();
        assert_eq!(Rig::iin_of(&r.ask(&req(8, 2, &both))), 0);
    }

    #[test]
    fn events_wait_for_their_confirm_and_a_missing_one_gives_them_back() {
        let mut r = Rig::new();
        r.dev.refresh(&Vals(0.0, 5));
        let first = r.read(0, &CLASS1);
        assert_eq!(first[0], 0xE0, "FIR FIN CON: it carries events");
        assert_eq!(
            Rig::iin_of(&first) & iin::CLASS1,
            0,
            "selected events are not 'unselected'"
        );
        r.now += 4 * SEC + Duration::from_millis(900);
        r.conn.tick(&mut r.dev, r.now);
        assert!(
            r.confirm(1).is_empty() && r.confirm(0).is_empty(),
            "a confirm has no reply"
        );
        assert_eq!(
            r.read(2, &CLASS1).len(),
            4,
            "confirmed in time: the events are gone"
        );

        r.dev.refresh(&Vals(1.0, 6));
        assert_eq!(r.read(3, &CLASS1)[0] & CON, CON);
        r.now += 5 * SEC;
        r.conn.tick(&mut r.dev, r.now);
        r.confirm(3); // too late: the outstation already gave up
        let again = r.read(4, &CLASS1);
        assert!(
            again.len() > 4,
            "the events were returned and are sent again"
        );
    }

    #[test]
    fn a_confirm_for_another_sequence_number_is_ignored() {
        let mut r = Rig::new();
        r.dev.refresh(&Vals(0.0, 5));
        assert!(r.read(5, &CLASS1).len() > 4);
        r.confirm(6);
        r.confirm(4);
        let again = r.read(7, &CLASS1);
        assert!(
            again.len() > 4,
            "nothing was confirmed, so the events come again"
        );
        r.confirm(7);
        assert_eq!(
            r.read(8, &CLASS1).len(),
            4,
            "the right sequence number confirms them"
        );
    }

    #[test]
    fn select_then_operate_needs_the_next_sequence_number_the_same_bytes_and_ten_seconds() {
        let mut r = Rig::new();
        let sel = crob(7, 3);
        assert_eq!(*r.ask(&req(2, 3, &sel)).last().unwrap(), 0);
        assert_eq!(
            *r.ask(&req(3, 4, &sel)).last().unwrap(),
            0,
            "next sequence number"
        );
        assert_eq!(r.rec.0, vec!["bs@7=true"]);
        // the selection is used up
        assert_eq!(*r.ask(&req(4, 4, &sel)).last().unwrap(), 2);
        // wrong sequence number
        r.ask(&req(5, 3, &sel));
        assert_eq!(*r.ask(&req(7, 4, &sel)).last().unwrap(), 2);
        // sequence numbers wrap
        r.ask(&req(15, 3, &sel));
        assert_eq!(*r.ask(&req(0, 4, &sel)).last().unwrap(), 0);
        // a different object
        r.ask(&req(1, 3, &sel));
        assert_eq!(*r.ask(&req(2, 4, &crob(7, 4))).last().unwrap(), 2);
        // timing: ten seconds is still in time, a little more is not
        r.ask(&req(3, 3, &sel));
        r.now += 10 * SEC;
        assert_eq!(*r.ask(&req(4, 4, &sel)).last().unwrap(), 0);
        r.ask(&req(5, 3, &sel));
        r.now += 10 * SEC + Duration::from_millis(1);
        assert_eq!(*r.ask(&req(6, 4, &sel)).last().unwrap(), 2);
        // a status octet in the OPERATE does not change what was selected
        r.ask(&req(7, 3, &sel));
        let mut with_status = sel.clone();
        *with_status.last_mut().unwrap() = 9;
        assert_eq!(*r.ask(&req(8, 4, &with_status)).last().unwrap(), 0);
    }

    #[test]
    fn a_select_that_fails_somewhere_selects_nothing() {
        let mut r = Rig::new();
        let two = [
            &crob(7, 3)[..],
            &[12, 1, 0x17, 1, 9, 3, 1, 100, 0, 0, 0, 0, 0, 0, 0, 0],
        ]
        .concat();
        let reply = r.ask(&req(0, 3, &two));
        assert_eq!(
            (reply[reply.len() - 17], reply[reply.len() - 1]),
            (0, 12),
            "statuses per object"
        );
        assert_eq!(*r.ask(&req(1, 4, &two)).last().unwrap(), 2);
    }

    #[test]
    fn direct_operate_checks_sbo_and_the_point_map() {
        let mut r = Rig::new();
        assert_eq!(
            *r.ask(&req(0, 5, &crob(7, 3))).last().unwrap(),
            2,
            "sbo point"
        );
        assert_eq!(*r.ask(&req(1, 5, &crob(5, 3))).last().unwrap(), 0);
        assert_eq!(
            *r.ask(&req(2, 5, &crob(2, 3))).last().unwrap(),
            0,
            "an input is reached through the same map"
        );
        assert_eq!(*r.ask(&req(3, 5, &crob(9, 3))).last().unwrap(), 12);
        assert_eq!(*r.ask(&req(4, 5, &crob(5, 4))).last().unwrap(), 0);
        assert_eq!(*r.ask(&req(5, 5, &crob(5, 0x43))).last().unwrap(), 0);
        assert_eq!(
            r.rec.0,
            vec!["bo@5=true", "bi@2=true", "bo@5=false", "bo@5=true"]
        );
        let ao = |idx: u8, v: f32| [&[41u8, 3, 0x17, 1, idx][..], &v.to_le_bytes(), &[0]].concat();
        assert_eq!(*r.ask(&req(6, 5, &ao(1, 1.5))).last().unwrap(), 2);
        assert_eq!(*r.ask(&req(7, 5, &ao(0, 2.5))).last().unwrap(), 0);
        assert_eq!(*r.ask(&req(8, 5, &ao(4, 2.5))).last().unwrap(), 12);
        assert_eq!(r.rec.0.last().unwrap(), "ai@0=2.5");
        // select / operate of an analog output
        assert_eq!(*r.ask(&req(9, 3, &ao(1, 3.5))).last().unwrap(), 0);
        assert_eq!(*r.ask(&req(10, 4, &ao(1, 3.5))).last().unwrap(), 0);
        assert_eq!(r.rec.0.last().unwrap(), "ao@1=3.5");
        assert_eq!(*r.ask(&req(11, 3, &ao(7, 3.5))).last().unwrap(), 12);
        // the integer and double variations are acknowledged and written nowhere
        let n = r.rec.0.len();
        assert_eq!(
            *r.ask(&req(12, 5, &[41, 1, 0x17, 1, 1, 7, 0, 0, 0, 0]))
                .last()
                .unwrap(),
            0
        );
        assert_eq!(
            *r.ask(&req(13, 5, &[41, 2, 0x17, 1, 99, 7, 0, 0]))
                .last()
                .unwrap(),
            0
        );
        assert_eq!(r.rec.0.len(), n);
    }

    #[test]
    fn refused_requests_say_why() {
        let mut r = Rig::new();
        let iin_of = |v: Vec<u8>| Rig::iin_of(&v) & 0xFF00;
        assert_eq!(
            iin_of(r.ask(&req(0, 5, &[12, 2, 0x17, 1, 5, 0]))),
            iin::UNKNOWN_OBJECT
        );
        assert_eq!(
            iin_of(r.ask(&req(1, 5, &[10, 2, 0x17, 1, 5, 0x81]))),
            iin::PARAM_ERROR
        );
        assert_eq!(
            iin_of(r.ask(&req(2, 5, &crob(5, 3)[..14]))),
            iin::PARAM_ERROR
        );
        assert_eq!(iin_of(r.ask(&req(3, 1, &[99, 1, 6]))), iin::UNKNOWN_OBJECT);
        assert_eq!(iin_of(r.ask(&req(4, 1, &[1, 2]))), iin::PARAM_ERROR);
        assert_eq!(iin_of(r.ask(&req(5, 1, &[50, 1, 6]))), iin::NO_FUNC);
        for func in [13u8, 14, 20, 21, 0x55, 129, 130] {
            assert_eq!(
                iin_of(r.ask(&req(6, func, &[]))),
                iin::NO_FUNC,
                "function {func}"
            );
        }
        for func in [7u8, 22, 24] {
            assert_eq!(iin_of(r.ask(&req(7, func, &[]))), 0, "function {func}");
        }
        assert_eq!(r.ask(&req(8, 23, &[]))[4..], [52, 2, 7, 1, 0, 0]);
    }

    #[test]
    fn a_broadcast_is_acted_on_never_answered_and_flagged_once() {
        let mut r = Rig::new();
        for dst in [0xFFFD, 0xFFFE, 0xFFFF] {
            assert!(
                r.to(dst, &req(0, 5, &crob(5, 3))).is_empty(),
                "no reply to {dst:#x}"
            );
        }
        assert_eq!(r.rec.0.len(), 3, "but it was executed");
        assert_eq!(
            Rig::iin_of(&r.read(1, &[60, 1, 6])) & iin::BROADCAST,
            iin::BROADCAST
        );
        assert_eq!(Rig::iin_of(&r.read(2, &[60, 1, 6])) & iin::BROADCAST, 0);
        // direct operate without acknowledgement: executed, no reply
        assert!(r.to(10, &req(3, 6, &crob(5, 3))).is_empty());
        assert_eq!(r.rec.0.len(), 4);
    }

    #[test]
    fn only_our_master_to_our_address_in_a_primary_frame_is_heard() {
        let mut r = Rig::new();
        let app = [0xC0, 0xC0, 1, 60, 1, 6];
        assert!(!r.link(0xC4, 10, 1, &app).unwrap().is_empty());
        assert!(
            r.link(0xC4, 11, 1, &app).unwrap().is_empty(),
            "other destination"
        );
        assert!(
            r.link(0xC4, 10, 2, &app).unwrap().is_empty(),
            "other master"
        );
        assert!(r.link(0x44, 10, 1, &app).unwrap().is_empty(), "DIR clear");
        assert!(r.link(0x84, 10, 1, &app).unwrap().is_empty(), "PRM clear");
        assert!(
            r.link(0xC5, 10, 1, &app).unwrap().is_empty(),
            "function 5 is not ours"
        );
        assert!(r.link(0xCF, 10, 1, &app).unwrap().is_empty());
    }

    #[test]
    fn link_states_follow_the_frame_count_bit() {
        let mut r = Rig::new();
        let app = [0xC0, 0xC0, 1, 60, 1, 6];
        assert_eq!(
            r.link(0xC9, 10, 1, &[]).unwrap(),
            encode(0x0B, 1, 10, &[]),
            "link status needs no reset"
        );
        assert!(
            r.link(0xF2, 10, 1, &[]).unwrap().is_empty(),
            "test link before a reset"
        );
        assert!(
            r.link(0xF3, 10, 1, &app).unwrap().is_empty(),
            "confirmed data before a reset"
        );
        assert_eq!(
            r.link(0xC0, 10, 1, &[]).unwrap(),
            encode(0x00, 1, 10, &[]),
            "reset: ACK"
        );
        assert!(
            r.link(0xD2, 10, 1, &[]).unwrap().is_empty(),
            "FCB 0 is not the expected 1"
        );
        assert!(r.link(0xC2, 10, 1, &[]).unwrap().is_empty(), "no FCV");
        assert_eq!(r.link(0xF2, 10, 1, &[]).unwrap(), encode(0x00, 1, 10, &[]));
        assert!(
            r.link(0xF2, 10, 1, &[]).unwrap().is_empty(),
            "a repeated test link is dropped"
        );
        let confirmed = r.link(0xD3, 10, 1, &app).unwrap(); // FCB 0 now expected
        assert!(
            confirmed.starts_with(&encode(0x00, 1, 10, &[])) && confirmed.len() > 10,
            "ACK then the response"
        );
        let again = r.link(0xD3, 10, 1, &app).unwrap(); // a retry: acknowledged, not processed twice
        assert_eq!(again, encode(0x00, 1, 10, &[]));
        // a broadcast needs no ACK
        assert!(r.link(0xC0, 0xFFFF, 1, &[]).unwrap().is_empty());
    }

    #[test]
    fn framing_violations_close_and_junk_requests_are_dropped() {
        let mut r = Rig::new();
        let mut bad = encode(0xC4, 10, 1, &[0xC0, 0xC0, 1, 60, 1, 6]);
        bad[12] ^= 1;
        assert_eq!(
            r.conn.feed(&mut r.dev, &mut r.rec, &bad, r.now),
            Err(Close("data CRC"))
        );
        let mut r = Rig::new();
        // a fragment longer than 2048 bytes
        let seg = |first: bool, last: bool, seq: u8| {
            let mut u = vec![seq | if first { 0x40 } else { 0 } | if last { 0x80 } else { 0 }];
            u.extend(vec![0xC0; 249]);
            u
        };
        let mut out = Ok(vec![]);
        for i in 0..10u8 {
            out = r.link(0xC4, 10, 1, &seg(i == 0, i == 9, i));
            if out.is_err() {
                break;
            }
        }
        assert_eq!(out, Err(Close("fragment too long")));
        let mut r = Rig::new();
        assert!(
            r.to(10, &[0xC0]).is_empty(),
            "too short for a function code"
        );
        assert!(r.to(10, &[0x40, 1, 60, 1, 6]).is_empty(), "not FIN");
        assert!(r.to(10, &[0x80, 1, 60, 1, 6]).is_empty(), "not FIR");
        assert!(
            r.to(10, &[0xD0, 1, 60, 1, 6]).is_empty(),
            "unsolicited bit from a master"
        );
        assert!(!r.to(10, &[0xC0, 1, 60, 1, 6]).is_empty());
        // a CONFIRM with nothing pending
        assert!(r.to(10, &[0xC0, 0]).is_empty());
    }

    #[test]
    fn the_refresh_makes_events_for_inputs_only_and_carries_the_provider_time() {
        let mut d = device();
        d.refresh(&Vals(1.0, 1_794_324_000));
        let events = d.events.select(None, 0, usize::MAX);
        assert_eq!(
            events, 2,
            "bi and ai raised one each; the outputs raised none"
        );
        let (e, _) = d
            .events
            .selected()
            .last()
            .map(|(e, v)| (e.encode(8), v))
            .unwrap();
        assert_eq!(e[..3], [0, 0, 0x01]);
        assert_eq!(e[11..], 1_794_324_000u64.to_le_bytes()[..6]);
        d.events.clear_written();
        d.refresh(&Vals(1.0, 1_794_324_001));
        assert_eq!(
            d.events.select(None, 0, usize::MAX),
            0,
            "no change, no event"
        );
        assert_eq!(d.max_index(Typ::Binary), 7);
        assert_eq!(d.max_index(Typ::Analog), 1);
        assert_eq!(
            Device::new(1, 2, Default::default(), Default::default()).max_index(Typ::Binary),
            0
        );
    }
}
