//! IEC 60870-5-104 slave (server) state machine, no I/O: an ASDU in, the ASDUs to send, the store writes and the event-log lines
//! out, plus the periodic report. The behaviour is stock's (bennu tag v2026.11.03) as `conformance/iec104/STOCK.md` records it.

use super::asdu::{
    cp56time2a, Asdu, Header, Item, Undecoded, ACT, ACT_CON, ACT_TERM, DEACT, DEACT_CON, PERIODIC,
    REQUEST, SPLIT_AT, SPONTANEOUS, STATION, UNKNOWN_COT, UNKNOWN_TYPE,
};
use crate::model::{Kind, Value};
use std::collections::BTreeMap;

/// `<subtype>`: binaries as single points (`M_SP_NA_1`) or double points (`M_DP_NA_1`, and `M_DP_TB_1` in the report).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subtype {
    Single,
    Double,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub addr: u16,
    pub tag: String,
    pub kind: Kind,
    pub output: bool,
}

/// The answer to one ASDU: what to send (ASDU bytes), what to write to the store, and event-log lines (level, event, message).
#[derive(Debug, Default, PartialEq)]
pub struct Reply {
    pub send: Vec<Vec<u8>>,
    pub writes: Vec<(String, Value)>,
    pub log: Vec<Line>,
}

/// An event-log line: level, event, message.
pub type Line = (&'static str, String, String);
/// A direct execute's store write and its two log lines.
type Execute = ((String, Value), [Line; 2]);

/// A tag's value in the store, `None` when the store lacks it.
type Read<'a> = &'a dyn Fn(&str) -> Option<Value>;

/// Address -> (tag, output), one map per kind: address order, one address in both kinds, and the later of two points at one
/// address of one kind wins, as in stock's `std::map` (Server.cpp:978-1013).
type Points = BTreeMap<u16, (String, bool)>;

pub struct Slave {
    binaries: Points,
    analogs: Points,
    subtype: Subtype,
}

impl Slave {
    pub fn new(points: Vec<Point>, subtype: Subtype) -> Slave {
        let mut s = Slave {
            binaries: Points::new(),
            analogs: Points::new(),
            subtype,
        };
        for p in points {
            let m = match p.kind {
                Kind::Binary => &mut s.binaries,
                Kind::Analog => &mut s.analogs,
            };
            m.insert(p.addr, (p.tag, p.output));
        }
        s
    }

    /// The answer to one ASDU from the master. Replies are the received ASDU with its COT byte changed; the CA is not checked.
    pub fn on_asdu(&self, a: Result<Asdu, Undecoded>, read: Read) -> Reply {
        let mut r = Reply::default();
        match a {
            Ok(a) => {
                let raw = a.encode();
                match a.head.type_id {
                    100 => self.interrogation(&a, &raw, read, &mut r.send),
                    45 | 46 | 50 => self.command(&a, &raw, &mut r),
                    // A monitor type from the master: no handler (cs104_slave.c:2031-2036).
                    _ => r.send.push(echo(&raw, UNKNOWN_TYPE, true)),
                }
            }
            Err(Undecoded { head: Some(h), raw }) => r.send.extend(undecoded(&h, &raw)),
            // Under 6 bytes: no answer; stock closes the connection (`closes`).
            Err(_) => {}
        }
        r
    }

    /// C_IC_NA_1 (Server.cpp:298-364): the COT is checked before the QOI (cs104_slave.c:1815), and COT 8 is answered as COT 6.
    fn interrogation(&self, a: &Asdu, raw: &[u8], read: Read, send: &mut Vec<Vec<u8>>) {
        if a.head.cot != ACT && a.head.cot != DEACT {
            send.extend([echo(raw, UNKNOWN_COT, true), echo(raw, UNKNOWN_TYPE, true)]);
            return;
        }
        // A GI with no object gets no answer; stock closes the connection when the payload is short (`closes`).
        let Some(Item::Interrogation { qoi, .. }) = a.items.first() else {
            return;
        };
        if *qoi != STATION {
            send.push(echo(raw, ACT_CON, true));
            return;
        }
        let bin = match self.subtype {
            Subtype::Single => 1,
            Subtype::Double => 3,
        };
        send.push(echo(raw, ACT_CON, false));
        send.extend(self.data(bin, STATION, read).iter().map(Asdu::encode));
        send.push(echo(raw, ACT_TERM, false));
    }

    /// C_SC_NA_1, C_DC_NA_1, C_SE_NC_1 with select-before-operate off (Server.cpp:447-702): a direct execute at an output of the
    /// command's kind gets ACT_CON, the write and ACT_TERM; anything else one negative reply.
    fn command(&self, a: &Asdu, raw: &[u8], r: &mut Reply) {
        let refuse = |cot| vec![echo(raw, cot, true)];
        r.send = match (a.head.cot, a.items.first()) {
            // No selection is ever held, so a cancel always fails.
            (DEACT, _) => refuse(DEACT_CON),
            (ACT, Some(item)) => match self.execute(item) {
                None => refuse(ACT_CON),
                Some((write, log)) => {
                    r.writes.push(write);
                    r.log.extend(log);
                    vec![echo(raw, ACT_CON, false), echo(raw, ACT_TERM, false)]
                }
            },
            // No object: the handler returns false and the library answers (Server.cpp:476-480).
            (ACT, None) => refuse(UNKNOWN_TYPE),
            _ => refuse(UNKNOWN_COT),
        };
    }

    /// The store write of a direct execute and its log lines (Server.cpp:752-830); `None` when stock refuses it: the select bit,
    /// a double state other than OFF(1) or ON(2), or no output of the command's kind at the IOA (Server.cpp:832-845).
    fn execute(&self, item: &Item) -> Option<Execute> {
        let (ioa, select, value, shown) = order(item)?;
        let (points, kind) = match value {
            Value::Binary(_) => (&self.binaries, ["binary", "Binary"]),
            Value::Analog(_) => (&self.analogs, ["analog", "Analog"]),
        };
        // Stock keeps the low 16 bits of the IOA (`uint16_t addr`, Server.cpp:483, :566, :656): IOA 65536 is address 0.
        let addr = ioa as u16;
        let (tag, output) = points.get(&addr)?;
        if select || !output {
            return None;
        }
        let log = [
            (
                "info",
                format!("iec60870-5-104 Server write{}", kind[1]),
                format!(
                    "{} point command at address {addr} with value {shown}.",
                    kind[1]
                ),
            ),
            (
                "info",
                format!("write {}", kind[0]),
                "Data successfully written.".into(),
            ),
        ];
        Some(((tag.clone(), value), log))
    }

    /// The binaries (type `bin`, 1 or 3) then the analogs, each split at stock's payload limit.
    fn data(&self, bin: u8, cot: u8, read: Read) -> Vec<Asdu> {
        let bins = values(&self.binaries, read).map(|(ioa, v)| match bin {
            1 => Item::Single {
                ioa,
                on: on(v),
                q: 0,
            },
            _ => Item::Double {
                ioa,
                dp: dp(on(v)),
                q: 0,
            },
        });
        [split(bin, cot, bins.collect(), 4), self.floats(cot, read)].concat()
    }

    fn floats(&self, cot: u8, read: Read) -> Vec<Asdu> {
        let items = values(&self.analogs, read).map(|(ioa, v)| Item::Float {
            ioa,
            v: float(v),
            q: 0,
        });
        split(13, cot, items.collect(), 8)
    }

    /// The report every `rpoll-rate` s (Server.cpp:155-274). Single: as the GI, COT 1. Double: each binary its own `M_DP_TB_1`,
    /// COT 3, stamped `unix_ms` as UTC, then the analogs, COT 1.
    pub fn periodic(&self, read: Read, unix_ms: i64) -> Vec<Asdu> {
        if self.subtype == Subtype::Single {
            return self.data(1, PERIODIC, read);
        }
        let time = cp56time2a(unix_ms, 0);
        let stamped = values(&self.binaries, read).map(|(ioa, v)| {
            let item = Item::DoubleTime {
                ioa,
                dp: dp(on(v)),
                q: 0,
                time,
            };
            asdu(31, SPONTANEOUS, vec![item])
        });
        stamped.chain(self.floats(PERIODIC, read)).collect()
    }
}

/// A command's IOA, select bit, value and value as stock logs it (0/1 for 45, the DCS for 46); `None` for a DCS other than
/// OFF(1) or ON(2) (Server.cpp:571-572).
fn order(item: &Item) -> Option<(u32, bool, Value, String)> {
    Some(match *item {
        Item::SingleCmd {
            ioa, on, select, ..
        } => (ioa, select, Value::Binary(on), u8::from(on).to_string()),
        Item::DoubleCmd {
            ioa,
            dcs: dcs @ (1 | 2),
            select,
            ..
        } => (ioa, select, Value::Binary(dcs == 2), dcs.to_string()),
        Item::SetFloat { ioa, v, select, .. } => {
            (ioa, select, Value::Analog(f64::from(v)), v.to_string())
        }
        _ => return None,
    })
}

/// The answer to an ASDU the codec cannot decode. 104 and 107 the library answers (cs104_slave.c:1934-1949, :2005-2021). For 100
/// to 106 it checks the COT first and sends negative 45 when it is wrong (cs104_slave.c:1815-2001). With no handler set, negative 44
/// follows either way (:2031-2036). Other types get negative 44 alone.
fn undecoded(h: &Header, raw: &[u8]) -> Vec<Vec<u8>> {
    match h.type_id {
        104 | 107 if h.cot == ACT => vec![echo(raw, ACT_CON, h.negative)],
        104 | 107 => vec![echo(raw, UNKNOWN_COT, true)],
        // A GI with no object gets no answer; stock closes the connection (`closes`).
        100 if accepted(100, h.cot) => vec![],
        t => {
            let wrong = (!accepted(t, h.cot)).then(|| echo(raw, UNKNOWN_COT, true));
            wrong
                .into_iter()
                .chain([echo(raw, UNKNOWN_TYPE, true)])
                .collect()
        }
    }
}

/// Whether the library takes `cot` for `type_id` (cs104_slave.c:1815, :1841, :1867, :1892, :1955, :1982). Types it does not check
/// take any COT.
fn accepted(type_id: u8, cot: u8) -> bool {
    let cots: &[u8] = match type_id {
        100 | 101 => &[ACT, DEACT],
        102 => &[REQUEST],
        103 | 105 => &[ACT],
        106 => &[ACT, SPONTANEOUS],
        _ => return true,
    };
    cots.contains(&cot)
}

/// Whether stock closes the connection on this ASDU instead of answering it (STOCK.md): one under the 6-byte header
/// (`cs101_asdu.c:157-160`, `cs104_slave.c:2270-2272`), or a GI at COT 6 or 8 with less than one object of payload
/// (`cs104_slave.c:1827-1828`, read whatever the VSQ count says). The slave answers neither; the connection's owner closes.
pub fn closes(asdu: &[u8]) -> bool {
    asdu.len() < 6 || (asdu[0] == 100 && matches!(asdu[2] & 0x3F, ACT | DEACT) && asdu.len() < 10)
}

/// The received ASDU with only its COT and P/N changed; the test bit stays (`CS101_ASDU_setCOT`, cs101_asdu.c:319-325).
fn echo(raw: &[u8], cot: u8, negative: bool) -> Vec<u8> {
    let mut b = raw.to_vec();
    b[2] = (b[2] & 0x80) | (u8::from(negative) << 6) | cot;
    b
}

/// The points whose tag the store has, in address order (stock skips the others: `hasTag`, Server.cpp:321).
fn values<'a>(points: &'a Points, read: Read<'a>) -> impl Iterator<Item = (u32, Value)> + 'a {
    points
        .iter()
        .filter_map(move |(&addr, (tag, _))| Some((u32::from(addr), read(tag)?)))
}

fn on(v: Value) -> bool {
    v == Value::Binary(true)
}

fn float(v: Value) -> f32 {
    match v {
        Value::Analog(x) => x as f32,
        Value::Binary(b) => f32::from(u8::from(b)),
    }
}

/// False is OFF (1), true is ON (2) (Server.cpp:110-116).
fn dp(on: bool) -> u8 {
    1 + u8::from(on)
}

/// ASDUs of `type_id` with at most `SPLIT_AT` object bytes each (`size` bytes an object, IOA included). No objects still give one
/// empty ASDU (Server.cpp:329, :354).
fn split(type_id: u8, cot: u8, items: Vec<Item>, size: usize) -> Vec<Asdu> {
    let mut parts: Vec<Vec<Item>> = items
        .chunks(SPLIT_AT / size)
        .map(<[Item]>::to_vec)
        .collect();
    if parts.is_empty() {
        parts.push(vec![]);
    }
    parts
        .into_iter()
        .map(|items| asdu(type_id, cot, items))
        .collect()
}

/// A data ASDU as stock creates one: originator 0, CA 1 (`CS101_ASDU_create`, Server.cpp:164).
fn asdu(type_id: u8, cot: u8, items: Vec<Item>) -> Asdu {
    let head = Header {
        type_id,
        sq: false,
        n: items.len() as u8,
        cot,
        negative: false,
        test: false,
        originator: 0,
        ca: 1,
    };
    Asdu { head, items }
}

#[cfg(test)]
mod tests {
    use super::super::asdu::{cp56time2a, decode};
    use super::*;
    use std::collections::HashMap;

    const G: &str = include_str!("../../../../conformance/stock/iec104-server.golden");
    const GD: &str = include_str!("../../../../conformance/stock/iec104-server-double.golden");
    const GB: &str = include_str!("../../../../conformance/stock/iec104-server-big.golden");

    /// A request ASDU as `probe_104x.py`'s `a()` builds it: VSQ 1, originator 0, CA 1.
    fn a(typ: u8, cot: u8, ioa: u32, body: &[u8]) -> Vec<u8> {
        let i = ioa.to_le_bytes();
        [&[typ, 1, cot, 0, 1, 0, i[0], i[1], i[2]], body].concat()
    }

    fn f32b(v: f32, qual: u8) -> Vec<u8> {
        [&v.to_le_bytes()[..], &[qual]].concat()
    }

    /// ASDU bytes as the probe prints them: hex, with the CP56Time2a of an `M_DP_TB_1` masked `__`.
    fn text(b: &[u8]) -> String {
        let mut out: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
        if b[0] == 31 {
            out.iter_mut()
                .skip(10)
                .take(7)
                .for_each(|s| *s = "__".into());
        }
        out.join(" ")
    }

    /// What stock sent for the request on golden line `n` (1-based): the `< I` lines right after it.
    fn answer(g: &str, n: usize) -> Vec<&str> {
        assert!(
            g.lines().nth(n - 1).unwrap().starts_with("> "),
            "line {n} is not a request"
        );
        g.lines()
            .skip(n)
            .map_while(|l| l.strip_prefix("< I "))
            .collect()
    }

    /// The periodic ASDUs listed under the `periodic ...:` line `n` (sorted text, as the probe prints them).
    fn periodic_set(g: &str, n: usize) -> Vec<&str> {
        assert!(g.lines().nth(n - 1).unwrap().starts_with("periodic "));
        g.lines()
            .skip(n)
            .map_while(|l| l.strip_prefix("  I "))
            .collect()
    }

    fn sent(r: &Reply) -> Vec<String> {
        r.send.iter().map(|b| text(b)).collect()
    }

    fn sorted(v: &[Asdu]) -> Vec<String> {
        let mut t: Vec<String> = v.iter().map(|a| text(&a.encode())).collect();
        t.sort();
        t
    }

    fn pt(addr: u16, tag: &str, kind: Kind, output: bool) -> Point {
        Point {
            addr,
            tag: tag.into(),
            kind,
            output,
        }
    }

    /// The points of `conformance/stock/iec104-server.xml`, in its order.
    fn sample() -> Vec<Point> {
        vec![
            pt(100, "I_generator-1_bus-1.active", Kind::Binary, false),
            pt(101, "I_load-1_bus-1.active", Kind::Binary, false),
            pt(102, "I_bus-1.gen_mw", Kind::Analog, false),
            pt(103, "I_load-1_bus-1.mw", Kind::Analog, false),
            pt(0, "O_generator-1_bus-1.active", Kind::Binary, true),
            pt(1, "O_load-1_bus-1.active", Kind::Binary, true),
            pt(2, "O_bus-1.gen_mw", Kind::Analog, true),
            pt(3, "O_load-1_bus-1.mw", Kind::Analog, true),
        ]
    }

    /// The store values behind G:15-16 (300.7 is `9a 59 96 43`, 250.5 is `00 80 7a 43`).
    fn store() -> HashMap<&'static str, Value> {
        HashMap::from([
            ("I_generator-1_bus-1.active", Value::Binary(true)),
            ("I_load-1_bus-1.active", Value::Binary(false)),
            ("O_generator-1_bus-1.active", Value::Binary(true)),
            ("O_load-1_bus-1.active", Value::Binary(false)),
            ("I_bus-1.gen_mw", Value::Analog(300.7)),
            ("O_bus-1.gen_mw", Value::Analog(300.7)),
            ("I_load-1_bus-1.mw", Value::Analog(250.5)),
            ("O_load-1_bus-1.mw", Value::Analog(250.5)),
        ])
    }

    /// One request through a sample slave of `subtype`.
    fn ask(subtype: Subtype, req: &[u8]) -> Reply {
        let s = store();
        Slave::new(sample(), subtype).on_asdu(decode(req), &|t| s.get(t).copied())
    }

    const GI: [u8; 1] = [20];

    fn bin(tag: &str, on: bool) -> (String, Value) {
        (tag.into(), Value::Binary(on))
    }

    #[test]
    fn gi_single_mode_equals_stock() {
        // G:13 GI; G:23 COT 8 is a full GI too; G:31 CA 2: ACT_CON/ACT_TERM carry CA 2, the data ASDUs CA 1.
        let r = ask(Subtype::Single, &a(100, 6, 0, &GI));
        assert_eq!(sent(&r), answer(G, 13));
        assert!(r.writes.is_empty() && r.log.is_empty());
        assert_eq!(
            sent(&ask(Subtype::Single, &a(100, 8, 0, &GI))),
            answer(G, 23)
        );
        let mut ca2 = a(100, 6, 0, &GI);
        ca2[4] = 2;
        assert_eq!(sent(&ask(Subtype::Single, &ca2)), answer(G, 31));
    }

    #[test]
    fn gi_double_mode_equals_stock() {
        // GD:13 (binaries M_DP_NA_1: false -> 1 OFF, true -> 2 ON), GD:27 COT 8, GD:35 CA 2.
        assert_eq!(
            sent(&ask(Subtype::Double, &a(100, 6, 0, &GI))),
            answer(GD, 13)
        );
        assert_eq!(
            sent(&ask(Subtype::Double, &a(100, 8, 0, &GI))),
            answer(GD, 27)
        );
        let mut ca2 = a(100, 6, 0, &GI);
        ca2[4] = 2;
        assert_eq!(sent(&ask(Subtype::Double, &ca2)), answer(GD, 35));
        // The mapping at IOA 0 (true -> 2 ON) and IOA 1 (false -> 1 OFF), against the single-mode bytes of G:15.
        let bins = &sent(&ask(Subtype::Double, &a(100, 6, 0, &GI)))[1];
        assert!(bins.starts_with("03 04 14 00 01 00 00 00 00 02 01 00 00 01"));
        assert!(answer(G, 13)[1].starts_with("01 04 14 00 01 00 00 00 00 01 01 00 00 00"));
    }

    #[test]
    fn other_qoi_gets_one_negative_act_con() {
        // G:21 QOI 21: one negative ACT_CON. G:28 COT 3: negative COT 45, then negative COT 44.
        assert_eq!(
            sent(&ask(Subtype::Single, &a(100, 6, 0, &[21]))),
            answer(G, 21)
        );
        assert_eq!(
            sent(&ask(Subtype::Single, &a(100, 3, 0, &GI))),
            answer(G, 28)
        );
        // COT is checked before QOI (cs104_slave.c:1815): COT 3 with QOI 21 is still the two frames.
        assert_eq!(ask(Subtype::Single, &a(100, 3, 0, &[21])).send.len(), 2);
    }

    #[test]
    fn single_command_writes_and_confirms_as_stock() {
        // G:37 and G:40: ACT_CON then ACT_TERM, the request with only the COT changed; G:180-181 the writes.
        let r = ask(Subtype::Single, &a(45, 6, 0, &[1]));
        assert_eq!(sent(&r), answer(G, 37));
        assert_eq!(r.writes, [bin("O_generator-1_bus-1.active", true)]);
        assert_eq!(
            r.log,
            [
                (
                    "info",
                    "iec60870-5-104 Server writeBinary".into(),
                    "Binary point command at address 0 with value 1.".into()
                ),
                (
                    "info",
                    "write binary".into(),
                    "Data successfully written.".into()
                ),
            ]
        );
        let r = ask(Subtype::Single, &a(45, 6, 1, &[0]));
        assert_eq!(sent(&r), answer(G, 40));
        assert_eq!(r.writes, [bin("O_load-1_bus-1.active", false)]);
        assert_eq!(
            r.log[0].2,
            "Binary point command at address 1 with value 0."
        );
        // G:92: the CA is not checked, and is echoed (G:186 the write).
        let mut ca2 = a(45, 6, 0, &[0]);
        ca2[4] = 2;
        let r = ask(Subtype::Single, &ca2);
        assert_eq!(sent(&r), answer(G, 92));
        assert_eq!(r.writes, [bin("O_generator-1_bus-1.active", false)]);
        // An IOA over 65535 keeps its low 16 bits (`uint16_t addr`, Server.cpp:483): IOA 65536 executes at address 0. No golden
        // has this.
        let r = ask(Subtype::Single, &a(45, 6, 65536, &[1]));
        assert_eq!(
            sent(&r),
            [
                "2d 01 07 00 01 00 00 00 01 01",
                "2d 01 0a 00 01 00 00 00 01 01"
            ]
        );
        assert_eq!(r.writes, [bin("O_generator-1_bus-1.active", true)]);
        assert_eq!(
            r.log[0].2,
            "Binary point command at address 0 with value 1."
        );
        // The originator is echoed (GC:32: the stock client's OA 3 comes back).
        let mut oa3 = a(45, 6, 0, &[1]);
        oa3[3] = 3;
        assert!(ask(Subtype::Single, &oa3).send.iter().all(|b| b[3] == 3));
        // G:55: the GI after the commands shows the store's values, not the commands'.
        assert_eq!(
            sent(&ask(Subtype::Single, &a(100, 6, 0, &GI))),
            answer(G, 55)
        );
        // GD:41: the same in double mode.
        assert_eq!(
            sent(&ask(Subtype::Double, &a(45, 6, 0, &[1]))),
            answer(GD, 41)
        );
    }

    #[test]
    fn double_command_writes_and_confirms_as_stock() {
        // G:43 OFF(1) -> false, G:46 ON(2) -> true (G:182-183).
        let r = ask(Subtype::Single, &a(46, 6, 0, &[1]));
        assert_eq!(sent(&r), answer(G, 43));
        assert_eq!(r.writes, [bin("O_generator-1_bus-1.active", false)]);
        assert_eq!(
            r.log[0].2,
            "Binary point command at address 0 with value 1."
        );
        let r = ask(Subtype::Single, &a(46, 6, 1, &[2]));
        assert_eq!(sent(&r), answer(G, 46));
        assert_eq!(r.writes, [bin("O_load-1_bus-1.active", true)]);
        assert_eq!(
            r.log[0].2,
            "Binary point command at address 1 with value 2."
        );
        // GD:47, GD:50: the same in double mode.
        assert_eq!(
            sent(&ask(Subtype::Double, &a(46, 6, 0, &[1]))),
            answer(GD, 47)
        );
        assert_eq!(
            sent(&ask(Subtype::Double, &a(46, 6, 1, &[2]))),
            answer(GD, 50)
        );
        // G:70, G:72: state 0 and 3 are refused, no write, no log.
        for (n, dcs) in [(70, 0), (72, 3)] {
            let r = ask(Subtype::Single, &a(46, 6, 0, &[dcs]));
            assert_eq!(sent(&r), answer(G, n));
            assert!(r.writes.is_empty() && r.log.is_empty());
        }
    }

    #[test]
    fn setpoint_float_writes_and_confirms_as_stock() {
        // G:49 and G:52; G:184-185 the writes.
        let r = ask(Subtype::Single, &a(50, 6, 2, &f32b(77.5, 0)));
        assert_eq!(sent(&r), answer(G, 49));
        assert_eq!(r.writes, [("O_bus-1.gen_mw".into(), Value::Analog(77.5))]);
        assert_eq!(
            r.log,
            [
                (
                    "info",
                    "iec60870-5-104 Server writeAnalog".into(),
                    "Analog point command at address 2 with value 77.5.".into()
                ),
                (
                    "info",
                    "write analog".into(),
                    "Data successfully written.".into()
                ),
            ]
        );
        let r = ask(Subtype::Single, &a(50, 6, 3, &f32b(-2.5, 0)));
        assert_eq!(sent(&r), answer(G, 52));
        assert_eq!(
            r.writes,
            [("O_load-1_bus-1.mw".into(), Value::Analog(-2.5))]
        );
        // GD:53: the same in double mode.
        let r = ask(Subtype::Double, &a(50, 6, 2, &f32b(77.5, 0)));
        assert_eq!(sent(&r), answer(GD, 53));
        assert_eq!(r.writes.len(), 1);
    }

    #[test]
    fn select_bit_is_refused() {
        // Q7 no longer holds at the tag: G:78, G:80, G:82, S/E 1 at a valid output: one negative ACT_CON, no write.
        for (n, req) in [
            (78, a(45, 6, 0, &[0x81])),
            (80, a(46, 6, 0, &[0x82])),
            (82, a(50, 6, 2, &f32b(5.0, 0x80))),
        ] {
            let r = ask(Subtype::Single, &req);
            assert_eq!(sent(&r), answer(G, n), "G:{n}");
            assert!(r.writes.is_empty() && r.log.is_empty());
        }
    }

    #[test]
    fn unknown_ioa_and_unknown_types_answer_as_stock() {
        let one = f32b(1.0, 0);
        let five = f32b(5.0, 0);
        let t0 = [0u8; 7];
        let ts = [&[0xaa, 0x55][..], &t0].concat();
        for (g, n, req) in [
            // Refused: no point (G:64), an input (G:66, G:76), the wrong kind of output (G:68, G:74).
            (G, 64, a(45, 6, 50, &[1])),
            (G, 66, a(45, 6, 100, &[1])),
            (G, 68, a(45, 6, 2, &[1])),
            (G, 74, a(50, 6, 0, &one)),
            (G, 76, a(50, 6, 102, &one)),
            // COT 8: negative COT 9 (G:84-88); another COT: negative COT 45 (G:90).
            (G, 84, a(45, 8, 0, &[1])),
            (G, 86, a(46, 8, 0, &[2])),
            (G, 88, a(50, 8, 2, &five)),
            (G, 90, a(45, 3, 0, &[1])),
            // Other type ids (G:96-112): negative COT 44, but 104 and 107 at COT 6 get COT 7, 107 at COT 3 negative 45.
            (G, 96, a(101, 6, 0, &[5])),
            (G, 98, a(102, 5, 100, &[])),
            (G, 100, a(103, 6, 0, &t0)),
            (G, 102, a(104, 6, 0, &[0xaa, 0x55])),
            (G, 104, a(105, 6, 0, &[1])),
            (G, 106, a(107, 6, 0, &ts)),
            (G, 108, a(107, 3, 0, &ts)),
            (G, 110, a(58, 6, 0, &[&[1][..], &t0].concat())),
            (G, 112, a(1, 3, 0, &[1])),
            // GD:119: the same in double mode.
            (GD, 119, a(1, 3, 0, &[1])),
        ] {
            let r = ask(Subtype::Single, &req);
            assert_eq!(sent(&r), answer(g, n), "line {n}");
            assert!(r.writes.is_empty() && r.log.is_empty(), "line {n}");
        }
        // A command whose ASDU holds no object (VSQ 0) at COT 6: not handled, negative COT 44 (Server.cpp:476-480).
        let r = ask(Subtype::Single, &[45, 0, 6, 0, 1, 0]);
        assert_eq!(sent(&r), ["2d 00 6c 00 01 00"]);
        // A wrong COT on 101, 102, 103, 105, 106: negative COT 45, then negative COT 44 (cs104_slave.c:1859, :1884, :1930, :1974,
        // :2001, then :2031-2036). No golden has these. The accepted COTs of 101 (8) and 106 (3) get 44 alone.
        for (typ, cot, body) in [
            (101, 3, &[5][..]),
            (102, 6, &[]),
            (103, 3, &t0),
            (105, 8, &[1]),
            (106, 5, &[0, 0]),
        ] {
            let req = a(typ, cot, 0, body);
            let want = [45, 44].map(|c| text(&[&[typ, 1, 0x40 | c], &req[3..]].concat()));
            assert_eq!(sent(&ask(Subtype::Single, &req)), want, "type {typ}");
        }
        // 104 at a wrong COT: one negative COT 45, as 107 (G:108).
        let r = ask(Subtype::Single, &a(104, 3, 0, &[0xaa, 0x55]));
        assert_eq!(sent(&r), ["68 01 6d 00 01 00 00 00 00 aa 55"]);
        for (typ, cot, body) in [(101, 8, &[5][..]), (106, 3, &[0, 0])] {
            assert_eq!(
                ask(Subtype::Single, &a(typ, cot, 0, body)).send.len(),
                1,
                "type {typ}"
            );
        }
        // A GI with no object: no answer at COT 6 (no payload), stock closes (cs104_slave.c:1827-1828); at a
        // wrong COT negative 45 then 44 (:1833, :2031-2036).
        assert!(ask(Subtype::Single, &[100, 0, 6, 0, 1, 0]).send.is_empty());
        assert!(ask(Subtype::Single, &[100, 1, 6, 0, 1, 0]).send.is_empty());
        assert_eq!(
            sent(&ask(Subtype::Single, &[100, 1, 3, 0, 1, 0])),
            ["64 01 6d 00 01 00", "64 01 6c 00 01 00"]
        );
        // Under 6 bytes: no answer (stock closes the connection: `closes`).
        assert_eq!(ask(Subtype::Single, &[45, 1, 6]), Reply::default());
        // The test bit is kept in an echo (setCOT keeps the top two bits, cs101_asdu.c:319-325).
        let r = ask(Subtype::Single, &a(45, 0x86, 50, &[1]));
        assert_eq!(r.send[0][2], 0xC7);
    }

    #[test]
    fn stock_closes_on_a_short_asdu_or_a_gi_without_an_object() {
        assert!(closes(&[45, 1, 6]));
        assert!(closes(&[100, 0, 6, 0, 1, 0]));
        assert!(closes(&[100, 1, 0x48, 0, 1, 0, 0, 0, 0]), "COT 8, P/N set");
        assert!(!closes(&[100, 1, 3, 0, 1, 0]), "another COT is answered");
        assert!(!closes(&a(100, 6, 0, &GI)));
        assert!(
            !closes(&[100, 0, 6, 0, 1, 0, 0, 0, 0, 20]),
            "an object with VSQ 0 is read"
        );
        assert!(!closes(&[45, 1, 6, 0, 1, 0]));
    }

    #[test]
    fn periodic_single_and_double_mode() {
        let s = store();
        let read = |t: &str| s.get(t).copied();
        // G:18-20 (and G:60-62 after the commands): one M_SP_NA_1 and one M_ME_NC_1, COT 1, binaries first (Server.cpp:155-211).
        let p = Slave::new(sample(), Subtype::Single).periodic(&read, 0);
        assert_eq!(sorted(&p), periodic_set(G, 18));
        assert_eq!(sorted(&p), periodic_set(G, 60));
        assert_eq!((p[0].head.type_id, p[1].head.type_id), (1, 13));
        // GD:19-24: each binary its own M_DP_TB_1, VSQ 1, COT 3, then the analogs' M_ME_NC_1 COT 1. The golden lists the frames
        // sorted (probe_104x.py:121), so the order is the source's (Server.cpp:236-265): binaries first, in address order.
        let ms = 1_795_000_000_123;
        let p = Slave::new(sample(), Subtype::Double).periodic(&read, ms);
        assert_eq!(sorted(&p), periodic_set(GD, 19));
        let order: Vec<(u8, usize)> = p
            .iter()
            .map(|a| (a.head.type_id, a.encode()[6] as usize))
            .collect();
        assert_eq!(order, [(31, 0), (31, 1), (31, 100), (31, 101), (13, 2)]);
        // GD:18: the CP56Time2a is UTC.
        assert!(p[..4]
            .iter()
            .all(|a| a.encode()[10..17] == cp56time2a(ms, 0)));
    }

    /// The points of `conformance/iec104/server-big.xml`: the sample plus 130 binary and 70 analog inputs.
    fn big() -> Vec<Point> {
        let mut v = sample();
        v.extend((1000..1130).map(|a| pt(a, "I_generator-1_bus-1.active", Kind::Binary, false)));
        v.extend((2000..2070).map(|a| pt(a, "I_bus-1.gen_mw", Kind::Analog, false)));
        v
    }

    #[test]
    fn periodic_splits_at_the_stock_payload_limit() {
        // GB:13-21: 134 binaries as 60 + 60 + 14, 74 analogs as 30 + 30 + 14, in address order; GB:22-28 the same in the report.
        let s = store();
        let read = |t: &str| s.get(t).copied();
        let slave = Slave::new(big(), Subtype::Single);
        let r = slave.on_asdu(decode(&a(100, 6, 0, &GI)), &read);
        assert_eq!(sent(&r), answer(GB, 13));
        assert!(r
            .send
            .iter()
            .all(|b| b.len() <= super::super::asdu::MAX_ASDU));
        assert_eq!(sorted(&slave.periodic(&read, 0)), periodic_set(GB, 22));
    }

    #[test]
    fn binary_and_analog_at_one_address_both_appear() {
        // Review Focus 4: one address in both kinds, and NaN and inf go out as their float32 bits. An empty kind still gets its
        // ASDU (Server.cpp:329, :354).
        let points = vec![
            pt(5, "b", Kind::Binary, true),
            pt(5, "x", Kind::Analog, true),
            pt(6, "y", Kind::Analog, false),
        ];
        let s = HashMap::from([
            ("b", Value::Binary(true)),
            ("x", Value::Analog(f64::NAN)),
            ("y", Value::Analog(f64::INFINITY)),
        ]);
        let read = |t: &str| s.get(t).copied();
        let slave = Slave::new(points, Subtype::Single);
        let r = slave.on_asdu(decode(&a(100, 6, 0, &GI)), &read);
        let nan = text(&f32::NAN.to_le_bytes());
        let inf = text(&f32::INFINITY.to_le_bytes());
        assert_eq!(sent(&r)[1], "01 01 14 00 01 00 05 00 00 01");
        assert_eq!(
            sent(&r)[2],
            format!("0d 02 14 00 01 00 05 00 00 {nan} 00 06 00 00 {inf} 00")
        );
        // A 45 at 5 writes the binary, a 50 at 5 the analog.
        let w = |req: Vec<u8>| slave.on_asdu(decode(&req), &read).writes;
        assert_eq!(w(a(45, 6, 5, &[0])), [bin("b", false)]);
        assert_eq!(
            w(a(50, 6, 5, &f32b(1.5, 0))),
            [("x".into(), Value::Analog(1.5))]
        );
        // No binaries: the GI still has an empty M_SP_NA_1, and the report too.
        let slave = Slave::new(vec![pt(6, "y", Kind::Analog, false)], Subtype::Single);
        let r = slave.on_asdu(decode(&a(100, 6, 0, &GI)), &read);
        assert_eq!(sent(&r)[1], "01 00 14 00 01 00");
        assert_eq!(
            text(&slave.periodic(&read, 0)[0].encode()),
            "01 00 01 00 01 00"
        );
        // No analogs: an empty M_ME_NC_1 in both.
        let slave = Slave::new(vec![pt(5, "b", Kind::Binary, true)], Subtype::Single);
        let r = slave.on_asdu(decode(&a(100, 6, 0, &GI)), &read);
        assert_eq!(sent(&r)[2], "0d 00 14 00 01 00");
        assert_eq!(
            text(&slave.periodic(&read, 0)[1].encode()),
            "0d 00 01 00 01 00"
        );
    }
}
