//! IEC 60870-5-104 master (client) state machine, no I/O: the GI it opens with, what the server's ASDUs do to its points, and the
//! commands a write makes. The behaviour is stock's (bennu tag v2026.11.03) as `conformance/iec104/STOCK.md`, "Client", records it.
//! `Poller` is one `fep` mode connection over a `Link`, also without I/O (docs/field-device-clients.md, "`fep` mode").

use super::apci::{Action, Link, Params, Role};
use super::asdu::{self, Asdu, Header, Item, Undecoded, ACT, ACT_CON, ACT_TERM, STATION};
use super::server::Point;
use crate::fd::proto::client::{input_polled, Fep, Mode, Out};
use crate::fd::store::{zero, Store};
use crate::model::{Kind, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// `fep` mode: a command's ACT_CON or a GI's ACT_TERM that has not come within this long means the server is gone (as the DNP3
/// FEP's `FEP_TIMEOUT`).
pub const ANSWER: Duration = Duration::from_millis(1000);
/// The stock client's originator address (`MOD/ClientConnection.cpp:36`).
const ORIGINATOR: u8 = 3;
/// The common address of everything stock sends (`MOD/ClientConnection.cpp:49`, `:136`, `:190`).
const CA: u16 = 1;
const OFF: u8 = 1;
const ON: u8 = 2;

/// One `<iec60870-5-104-connection>`.
pub struct Conn {
    pub endpoint: String,
    pub points: Vec<Point>,
    pub fep: Option<Fep>,
}

pub struct Master {
    /// Stock's address maps, one per kind, inputs and outputs together: a later point at an address takes it over
    /// (`ClientConnection.hpp:44-54`).
    bin: BTreeMap<u16, String>,
    ana: BTreeMap<u16, String>,
    /// Tag -> address, for a write.
    addr: BTreeMap<String, u16>,
    /// Q12: decode `M_SP_NA_1` and `M_DP_TB_1` too (`--strict`, fep mode); stock ignores both (`MOD/ClientConnection.cpp:377-433`).
    all_types: bool,
    fep: bool,
    /// `fep` mode: the last GI's binaries were double points, so a binary command is `C_DC_NA_1`, not `C_SC_NA_1`.
    double: bool,
}

fn command(type_id: u8, item: Item) -> Asdu {
    Asdu {
        head: Header {
            type_id,
            sq: false,
            n: 1,
            cot: ACT,
            negative: false,
            test: false,
            originator: ORIGINATOR,
            ca: CA,
        },
        items: vec![item],
    }
}

/// Stock keeps an analog in a float.
fn float32(x: f64) -> Value {
    Value::Analog(f64::from(x as f32))
}

impl Master {
    pub fn new(conn: &Conn, mode: Mode, strict: bool) -> Master {
        let map = |kind| -> BTreeMap<u16, String> {
            let of_kind = conn.points.iter().filter(|p| p.kind == kind);
            of_kind.map(|p| (p.addr, p.tag.clone())).collect()
        };
        Master {
            bin: map(Kind::Binary),
            ana: map(Kind::Analog),
            addr: conn
                .points
                .iter()
                .map(|p| (p.tag.clone(), p.addr))
                .collect(),
            all_types: strict || mode == Mode::Fep,
            fep: mode == Mode::Fep,
            double: false,
        }
    }

    /// The GI sent with STARTDT at connect; stock sends no other (`MOD/ClientConnection.cpp:46-49`).
    pub fn on_start(&mut self) -> Vec<Asdu> {
        vec![command(
            100,
            Item::Interrogation {
                ioa: 0,
                qoi: STATION,
            },
        )]
    }

    /// The tag updates one ASDU from the server makes, any COT. An IOA is cut to 16 bits (stock's `uint16_t addr`); a double
    /// point neither OFF nor ON is false. Command confirmations change nothing: stock only logs them.
    pub fn on_asdu(&mut self, a: &Asdu) -> Vec<(String, Value)> {
        match (a.head.type_id, a.head.cot) {
            (1, STATION) => self.double = false,
            (3, STATION) => self.double = true,
            _ => {}
        }
        let all = self.all_types;
        let at = |m: &BTreeMap<u16, String>, ioa: u32, v: Value| {
            m.get(&(ioa as u16)).map(|t| (t.clone(), v))
        };
        let one = |it: &Item| match *it {
            Item::Float { ioa, v, .. } => at(&self.ana, ioa, float32(f64::from(v))),
            Item::Double { ioa, dp, .. } => at(&self.bin, ioa, Value::Binary(dp == ON)),
            Item::DoubleTime { ioa, dp, .. } if all => at(&self.bin, ioa, Value::Binary(dp == ON)),
            Item::Single { ioa, on, .. } if all => at(&self.bin, ioa, Value::Binary(on)),
            _ => None,
        };
        a.items.iter().filter_map(one).collect()
    }

    /// The command a WRITE sends: `true`/`false` a `C_DC_NA_1`, a number a `C_SE_NC_1`, direct execute, to the tag's address
    /// whatever its kind (`MOD/ClientConnection.cpp:132-141`, `:186-195`). `fep` mode sends a `C_SC_NA_1` instead of the
    /// `C_DC_NA_1` until a GI has shown double points.
    pub fn command(&self, tag: &str, v: Value) -> Result<Asdu, String> {
        let ioa = u32::from(*self.addr.get(tag).ok_or("Unable to find tag")?);
        Ok(match v {
            Value::Binary(on) if self.fep && !self.double => command(
                45,
                Item::SingleCmd {
                    ioa,
                    on,
                    select: false,
                    qu: 0,
                },
            ),
            Value::Binary(on) => command(
                46,
                Item::DoubleCmd {
                    ioa,
                    dcs: if on { ON } else { OFF },
                    select: false,
                    qu: 0,
                },
            ),
            Value::Analog(x) => command(
                50,
                Item::SetFloat {
                    ioa,
                    v: x as f32,
                    select: false,
                    ql: 0,
                },
            ),
        })
    }

    /// The local update a WRITE makes at once: the point of the value's kind at the tag's address, if there is one
    /// (`updateBinary`/`updateAnalog`, `ClientConnection.hpp:56-72`), so `true` to an analog point changes nothing.
    pub fn local(&self, tag: &str, v: Value) -> Option<(String, Value)> {
        let addr = *self.addr.get(tag)?;
        let (map, v) = match v {
            Value::Binary(_) => (&self.bin, v),
            Value::Analog(x) => (&self.ana, float32(x)),
        };
        map.get(&addr).map(|t| (t.clone(), v))
    }
}

/// What the wire carries of a value: an analog as a float32. The form the echo rule compares in `fep` mode.
fn wire(v: Value) -> Value {
    match v {
        Value::Analog(x) => float32(x),
        b => b,
    }
}

/// The link's actions: the bytes to send, each ASDU delivered to `deliver`. `Err`: the link closed, or an ASDU too short for its
/// header came (stock closes then, `LIB/cs104/cs104_connection.c:658-667`). An ASDU of a type the codec lacks is ignored.
pub fn route(acts: Vec<Action>, mut deliver: impl FnMut(&Asdu)) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    for a in acts {
        match a {
            Action::Send(b) => out.extend(b),
            Action::Deliver(b) => match asdu::decode(&b) {
                Ok(a) => deliver(&a),
                Err(Undecoded { head: Some(_), .. }) => {}
                Err(_) => return Err("an ASDU too short for its header".into()),
            },
            Action::Close(why) => return Err(why),
        }
    }
    Ok(out)
}

/// One `fep` mode connection: a GI every `<scan-rate>`, the server's data into the tags, and once per `<cycle-time>` the output
/// tags' local changes down as direct-execute commands, one per point at a time, each answered by its ACT_CON. The learned binary
/// command type and the echo state of the outputs last across connections; the link, the commands under way and the GI do not.
pub struct Poller {
    pub name: String,
    master: Master,
    store: Store,
    status: Option<String>,
    scan: Duration,
    cycle: Duration,
    link: Link,
    outs: BTreeMap<String, Out>,
    /// Commands sent and not yet confirmed, by (type id, IOA): the tag, the value, when it went.
    waiting: BTreeMap<(u8, u32), (String, Value, Instant)>,
    /// Local changes a GI ran into: sent once the ASDUs at hand are taken.
    todo: Vec<(String, Value)>,
    /// When the GI under way went.
    gi: Option<Instant>,
    next_gi: Instant,
    next_cycle: Instant,
    up: bool,
    /// The server has ended a GI since the shell last cleared this (before each connection attempt).
    pub answered: bool,
}

impl Poller {
    pub fn new(conn: &Conn, store: Store, cycle: Duration, now: Instant) -> Poller {
        let (scan, status) = conn
            .fep
            .as_ref()
            .map_or((Duration::from_secs(1), None), |k| {
                (k.scan, k.status.clone())
            });
        let outs = conn.points.iter().filter(|p| p.output);
        Poller {
            name: conn.endpoint.clone(),
            master: Master::new(conn, Mode::Fep, false),
            store,
            status,
            scan,
            cycle,
            link: Link::new(Params::STANDARD, Role::Client),
            outs: outs
                .map(|p| (p.tag.clone(), Out::new(zero(p.kind))))
                .collect(),
            waiting: BTreeMap::new(),
            todo: vec![],
            gi: None,
            next_gi: now,
            next_cycle: now,
            up: false,
            answered: false,
        }
    }

    /// How often `tick` is wanted.
    pub fn period(&self, tick: Duration) -> Duration {
        tick.min(self.cycle).min(self.scan)
    }

    /// A new connection: STARTDT, then the newest value of each output that changed while there was none, then the GI.
    pub fn connect(&mut self, now: Instant) -> Vec<u8> {
        self.link = Link::new(Params::STANDARD, Role::Client);
        let mut out = route(self.link.start(now), |_| {}).unwrap_or_default();
        self.next_cycle = now;
        self.write_down(now, &mut out);
        self.poll(now, &mut out);
        out
    }

    /// Bytes from the server: what to send back, or why the connection must close.
    pub fn on_bytes(&mut self, b: &[u8], now: Instant) -> Result<Vec<u8>, String> {
        let mut asdus = vec![];
        let routed = route(self.link.on_bytes(b, now), |a| asdus.push(a.clone()));
        // the ASDUs before a close too: a confirmation among them must not leave its command due again
        for a in asdus {
            self.take(&a);
        }
        let mut out = routed?;
        for (tag, v) in std::mem::take(&mut self.todo) {
            self.send_down(&tag, v, now, &mut out);
        }
        Ok(out)
    }

    /// The link's timers, an answer overdue, the cycle's write-down and the GI that is due.
    pub fn tick(&mut self, now: Instant) -> Result<Vec<u8>, String> {
        let mut out = route(self.link.tick(now), |_| {})?;
        let sent = self
            .gi
            .into_iter()
            .chain(self.waiting.values().map(|w| w.2));
        if sent.min().is_some_and(|t| now >= t + ANSWER) {
            return Err(format!("no answer within {ANSWER:?}"));
        }
        self.write_down(now, &mut out);
        if self.gi.is_none() && now >= self.next_gi {
            self.poll(now, &mut out);
        }
        Ok(out)
    }

    /// The connection is gone: the status says so, and the commands under way are due again.
    pub fn lost(&mut self) {
        self.set_up(false);
        self.gi = None;
        let waiting = std::mem::take(&mut self.waiting).into_values().map(|w| w.0);
        let tags: Vec<_> = waiting.chain(self.todo.drain(..).map(|t| t.0)).collect();
        for tag in tags {
            self.settle(&tag, None);
        }
    }

    fn set_up(&mut self, up: bool) {
        if self.up != up {
            self.up = up;
            if let Some(tag) = &self.status {
                let _ = self.store.write(tag, Value::Binary(up));
            }
        }
    }

    /// Hand an ASDU to the link; `false` when the k window is full.
    fn send(&mut self, a: &Asdu, now: Instant, out: &mut Vec<u8>) -> bool {
        let Some(acts) = self.link.send(a.encode(), now) else {
            return false;
        };
        out.extend(route(acts, |_| {}).unwrap_or_default());
        true
    }

    fn poll(&mut self, now: Instant, out: &mut Vec<u8>) {
        for gi in self.master.on_start() {
            if self.send(&gi, now, out) {
                (self.gi, self.next_gi) = (Some(now), now + self.scan);
            }
        }
    }

    /// Once per cycle: every output tag's local change.
    fn write_down(&mut self, now: Instant, out: &mut Vec<u8>) {
        if now < self.next_cycle {
            return;
        }
        self.next_cycle = now + self.cycle;
        let store = &self.store;
        let due: Vec<_> = (self.outs.iter_mut())
            .filter_map(|(tag, o)| o.due(store, tag, wire).map(|v| (tag.clone(), v)))
            .collect();
        for (tag, v) in due {
            self.send_down(&tag, v, now, out);
        }
    }

    /// Command output `tag` to `v`. A NaN is never sent; a command that cannot go now (the k window full, or one to the same
    /// address under way) is due again next cycle.
    fn send_down(&mut self, tag: &str, v: Value, now: Instant, out: &mut Vec<u8>) {
        if matches!(v, Value::Analog(x) if x.is_nan()) {
            tracing::warn!(
                "iec60870-5-104-client: {}: tag `{tag}` is NaN; not sent",
                self.name
            );
            return self.settle(tag, Some(v));
        }
        let Ok(cmd) = self.master.command(tag, v) else {
            return self.settle(tag, None);
        };
        let key = (cmd.head.type_id, cmd.items[0].ioa());
        if self.waiting.contains_key(&key) || !self.send(&cmd, now, out) {
            return self.settle(tag, None);
        }
        self.waiting.insert(key, (tag.to_string(), v, now));
    }

    /// End the write under way for `tag`: `sent` is the value that went, `None` that it did not.
    fn settle(&mut self, tag: &str, sent: Option<Value>) {
        if let Some(o) = self.outs.get_mut(tag) {
            match sent {
                Some(v) => o.echo.sent_ok(v),
                None => o.echo.sent_failed(),
            }
        }
    }

    /// One ASDU from the server: the GI's end or refusal, a command's confirmation or refusal (a negative reply at any COT, as
    /// 44-47 from an RTU that is not stock), or data for the tags (any COT).
    fn take(&mut self, a: &Asdu) {
        match (a.head.type_id, a.head.cot, a.head.negative) {
            (100, ACT_TERM, false) => {
                self.gi = None;
                self.answered = true;
                self.set_up(true);
            }
            (100, cot, true) => {
                tracing::warn!(
                    "iec60870-5-104-client: {}: the server refused the interrogation (negative COT {cot})",
                    self.name
                );
                self.gi = None;
                self.set_up(false);
            }
            (45 | 46 | 50, cot, refused) if cot == ACT_CON || refused => {
                for it in &a.items {
                    self.confirmed((a.head.type_id, it.ioa()), refused);
                }
            }
            _ => {
                for (tag, v) in self.master.on_asdu(a) {
                    self.polled(tag, v);
                }
            }
        }
    }

    /// A command was answered. One the server refuses is not repeated: the next GI shows what it holds.
    fn confirmed(&mut self, key: (u8, u32), refused: bool) {
        let Some((tag, v, _)) = self.waiting.remove(&key) else {
            return;
        };
        if refused {
            tracing::warn!(
                "iec60870-5-104-client: {}: tag `{tag}` = {v:?} was refused by the server",
                self.name
            );
        }
        self.settle(&tag, Some(v));
    }

    fn polled(&mut self, tag: String, v: Value) {
        match self.outs.get_mut(&tag) {
            Some(o) => {
                let local = o.polled(&self.store, &tag, v, wire(v), wire);
                self.todo.extend(local.map(|l| (tag, l)));
            }
            None => input_polled(&self.store, &tag, v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::proto::iec104::apci::{encode, Decoder, Frame, U};
    use crate::fd::store::Layout;
    use Value::{Analog, Binary};

    fn point(addr: u16, tag: &str, kind: Kind, output: bool) -> Point {
        let tag = tag.into();
        Point {
            addr,
            tag,
            kind,
            output,
        }
    }

    /// A poller over `pump` (binary output 0), `sp` (analog output 2) and `lvl` (analog input 102), a GI every second.
    fn poller(t: Instant) -> Poller {
        let conn = Conn {
            endpoint: "tcp://127.0.0.1:1".into(),
            points: vec![
                point(0, "pump", Kind::Binary, true),
                point(2, "sp", Kind::Analog, true),
                point(102, "lvl", Kind::Analog, false),
            ],
            fep: Some(Fep {
                scan: Duration::from_secs(1),
                status: Some("up".into()),
            }),
        };
        let store = Store::new(Layout {
            internal: [
                ("pump", Binary(false)),
                ("sp", Analog(0.0)),
                ("lvl", Analog(0.0)),
                ("up", Binary(false)),
            ]
            .iter()
            .map(|(t, v)| (t.to_string(), *v))
            .collect(),
            ..Default::default()
        });
        Poller::new(&conn, store, Duration::from_millis(100), t)
    }

    /// The frames in `b`: a U-frame as its code, an I-frame as its ASDU.
    fn frames(b: &[u8]) -> Vec<Result<U, Asdu>> {
        let mut d = Decoder::default();
        d.push(b);
        let mut out = vec![];
        while let Ok(Some(f)) = d.next() {
            match f {
                Frame::U(u) => out.push(Ok(u)),
                Frame::I { asdu, .. } => out.push(Err(asdu::decode(&asdu).unwrap())),
                Frame::S { .. } => {}
            }
        }
        out
    }

    fn types(b: &[u8]) -> Vec<u8> {
        frames(b)
            .into_iter()
            .filter_map(|f| f.err())
            .map(|a| a.head.type_id)
            .collect()
    }

    /// The server's I-frames, numbered from `ns`, acknowledging nothing.
    fn server(ns: u16, asdus: &[&[u8]]) -> Vec<u8> {
        let i = |(n, a): (usize, &&[u8])| {
            encode(&Frame::I {
                ns: ns + n as u16,
                nr: 0,
                asdu: a.to_vec(),
            })
        };
        asdus.iter().enumerate().flat_map(i).collect()
    }

    const GI_END: &[u8] = &[100, 1, 10, 3, 1, 0, 0, 0, 0, 20];

    #[test]
    fn a_connection_opens_with_startdt_the_changed_outputs_then_the_gi() {
        let t = Instant::now();
        let mut p = poller(t);
        p.store.write("sp", Analog(5.0)).unwrap();
        p.store.apply_internal();
        let f = frames(&p.connect(t));
        assert_eq!(f[0], Ok(U::StartAct));
        let Err(cmd) = &f[1] else { panic!("{f:?}") };
        let want = Item::SetFloat {
            ioa: 2,
            v: 5.0,
            select: false,
            ql: 0,
        };
        assert_eq!((cmd.head.type_id, &cmd.items[..]), (50, &[want][..]));
        assert_eq!(types(&p.connect(t))[..], [100], "nothing new to send");
    }

    #[test]
    fn the_gi_feeds_the_tags_and_its_end_sets_the_status() {
        let t = Instant::now();
        let mut p = poller(t);
        p.connect(t);
        let data: [&[u8]; 3] = [
            &[1, 1, 20, 0, 1, 0, 0, 0, 0, 1],
            &[13, 1, 20, 0, 1, 0, 102, 0, 0, 0, 0, 0x60, 0x40, 0],
            GI_END,
        ];
        assert!(p.on_bytes(&server(0, &data[..2]), t).is_ok());
        assert_eq!(p.store.queued("up"), None, "not before the GI's end");
        assert_eq!(p.store.queued("lvl"), Some(Analog(3.5)));
        assert_eq!(p.store.queued("pump"), Some(Binary(true)));
        p.on_bytes(&server(2, &data[2..]), t).unwrap();
        assert!(p.answered);
        assert_eq!(p.store.queued("up"), Some(Binary(true)));
        // the next GI goes a scan later, not while one is under way
        assert_eq!(types(&p.tick(t).unwrap()), Vec::<u8>::new());
        assert_eq!(types(&p.tick(t + Duration::from_secs(1)).unwrap()), [100]);
        assert_eq!(
            types(&p.tick(t + Duration::from_millis(1500)).unwrap()),
            Vec::<u8>::new()
        );
        p.lost();
        assert_eq!(p.store.queued("up"), Some(Binary(false)));
    }

    #[test]
    fn an_answer_overdue_loses_the_connection_and_the_write_goes_again() {
        let t = Instant::now();
        let mut p = poller(t);
        p.connect(t);
        p.on_bytes(&server(0, &[GI_END]), t).unwrap();
        p.store.write("pump", Binary(true)).unwrap();
        p.store.apply_internal();
        let later = t + Duration::from_millis(200);
        assert_eq!(
            types(&p.tick(later).unwrap()),
            [45],
            "C_SC_NA_1 after a single-point GI"
        );
        assert!(p.tick(later + ANSWER / 2).is_ok());
        assert!(p.tick(later + ANSWER).unwrap_err().contains("no answer"));
        p.lost();
        assert_eq!(p.store.queued("up"), Some(Binary(false)));
        assert_eq!(types(&p.connect(later + ANSWER))[..1], [45], "due again");
    }

    #[test]
    fn a_confirmed_or_refused_command_is_not_sent_again() {
        let t = Instant::now();
        let mut p = poller(t);
        p.connect(t);
        p.on_bytes(&server(0, &[GI_END]), t).unwrap();
        for (n, (x, cot)) in [(5.0f32, 7u8), (6.0, 0x47)].into_iter().enumerate() {
            p.store.write("sp", Analog(f64::from(x))).unwrap();
            p.store.apply_internal();
            let at = t + Duration::from_millis(200 * (n as u64 + 1));
            assert_eq!(types(&p.tick(at).unwrap()), [50]);
            let mut con = vec![50, 1, cot, 3, 1, 0, 2, 0, 0];
            con.extend(x.to_le_bytes());
            con.push(0);
            p.on_bytes(&server(n as u16 + 1, &[&con]), at).unwrap();
            assert_eq!(
                types(&p.tick(at + Duration::from_millis(100)).unwrap()),
                Vec::<u8>::new()
            );
        }
        // a NaN is never sent, and not tried again either
        p.store.write("sp", Analog(f64::NAN)).unwrap();
        p.store.apply_internal();
        let at = t + Duration::from_secs(2);
        assert_eq!(types(&p.tick(at).unwrap()), [100]);
        assert_eq!(
            types(&p.tick(at + Duration::from_millis(100)).unwrap()),
            Vec::<u8>::new()
        );
    }

    /// A negative reply at any COT settles a command as refused (a third-party RTU may answer COT 44-47, not stock's negative
    /// 7), and a refused GI ends it, not answered: neither waits for `ANSWER` and drops the connection.
    #[test]
    fn a_negative_reply_at_any_cot_settles_the_command_or_gi() {
        let t = Instant::now();
        let mut p = poller(t);
        p.connect(t);
        p.on_bytes(&server(0, &[GI_END]), t).unwrap();
        p.store.write("sp", Analog(7.0)).unwrap();
        p.store.apply_internal();
        let at = t + Duration::from_millis(200);
        assert_eq!(types(&p.tick(at).unwrap()), [50]);
        // negative 47, unknown IOA
        let mut neg = vec![50, 1, 0x6F, 3, 1, 0, 2, 0, 0];
        neg.extend(7.0f32.to_le_bytes());
        neg.push(0);
        p.on_bytes(&server(1, &[&neg]), at).unwrap();
        assert!(p.waiting.is_empty(), "settled");
        let gi_at = t + Duration::from_secs(1);
        assert_eq!(types(&p.tick(gi_at).unwrap()), [100], "not sent again");
        p.on_bytes(&server(2, &[&[100, 1, 0x47, 3, 1, 0, 0, 0, 0, 20]]), gi_at)
            .unwrap();
        assert!(p.gi.is_none());
        assert_eq!(p.store.queued("up"), Some(Binary(false)));
        assert!(p.tick(gi_at + ANSWER).is_ok(), "no answer timeout");
    }

    /// The ASDUs of a read that ends in a protocol error are taken before the close: a confirmed command is not sent again.
    #[test]
    fn the_asdus_before_a_close_in_one_read_are_taken() {
        let t = Instant::now();
        let mut p = poller(t);
        p.connect(t);
        p.on_bytes(&server(0, &[GI_END]), t).unwrap();
        p.store.write("sp", Analog(5.0)).unwrap();
        p.store.apply_internal();
        let at = t + Duration::from_millis(200);
        assert_eq!(types(&p.tick(at).unwrap()), [50]);
        let mut con = vec![50, 1, 7, 3, 1, 0, 2, 0, 0];
        con.extend(5.0f32.to_le_bytes());
        con.push(0);
        let read = [server(1, &[&con]), vec![0x69, 4, 1, 0, 0, 0]].concat();
        assert!(p.on_bytes(&read, at).is_err());
        assert!(p.waiting.is_empty());
        p.lost();
        assert_eq!(types(&p.connect(at)), [100], "the 50 is not sent again");
    }

    #[test]
    fn a_full_window_leaves_the_write_due() {
        let t = Instant::now();
        let mut p = poller(t);
        p.connect(t);
        // the GI and 11 more I-frames fill k = 12
        let gi = p.master.on_start().remove(0);
        for _ in 0..11 {
            assert!(p.send(&gi, t, &mut vec![]));
        }
        assert!(!p.send(&gi, t, &mut vec![]));
        p.store.write("sp", Analog(1.0)).unwrap();
        p.store.apply_internal();
        assert_eq!(
            types(&p.tick(t + Duration::from_millis(100)).unwrap()),
            Vec::<u8>::new()
        );
        assert!(p.waiting.is_empty());
        // the server acknowledges them all: the write goes
        p.on_bytes(&encode(&Frame::S { nr: 12 }), t).unwrap();
        assert_eq!(
            types(&p.tick(t + Duration::from_millis(200)).unwrap()),
            [50]
        );
    }
}
