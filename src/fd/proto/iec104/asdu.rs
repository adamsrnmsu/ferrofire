//! IEC 60870-5-104 ASDU codec for the type ids stock (lib60870) uses: 1, 3, 13, 31, 45, 46, 50, 100. Bytes in, bytes out, no
//! I/O. Layout: type, VSQ (SQ bit 7, count), COT (cause bits 0-5, P/N bit 6, T bit 7), originator, CA (u16 LE), then objects.

/// APDU 253 less the 4-byte control field.
pub const MAX_ASDU: usize = 249;
/// Stock's `MAX_ASDU_PAYLOAD_SIZE` (`conformance/iec104/STOCK.md`, "maximum ASDU payload"): the limit in bytes of information
/// objects in one ASDU, checked before each object is added. Callers split there (60 `M_SP_NA_1`, 30 `M_ME_NC_1`), so `encode`
/// stays within `MAX_ASDU`: 6 header bytes + 240 = 246. `encode` does not split; an `Asdu` over `MAX_ASDU` is the caller's bug.
pub const SPLIT_AT: usize = 240;
const HEAD: usize = 6;

// Causes of transmission (IEC 60870-5-101 7.2.3).
pub const PERIODIC: u8 = 1;
pub const SPONTANEOUS: u8 = 3;
pub const REQUEST: u8 = 5;
pub const ACT: u8 = 6;
pub const ACT_CON: u8 = 7;
pub const DEACT: u8 = 8;
pub const DEACT_CON: u8 = 9;
pub const ACT_TERM: u8 = 10;
pub const STATION: u8 = 20;
pub const UNKNOWN_TYPE: u8 = 44;
pub const UNKNOWN_COT: u8 = 45;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub type_id: u8,
    pub sq: bool,
    pub n: u8,
    pub cot: u8,
    pub negative: bool,
    pub test: bool,
    pub originator: u8,
    pub ca: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Single {
        ioa: u32,
        on: bool,
        q: u8,
    },
    Double {
        ioa: u32,
        dp: u8,
        q: u8,
    },
    DoubleTime {
        ioa: u32,
        dp: u8,
        q: u8,
        time: [u8; 7],
    },
    Float {
        ioa: u32,
        v: f32,
        q: u8,
    },
    SingleCmd {
        ioa: u32,
        on: bool,
        select: bool,
        qu: u8,
    },
    DoubleCmd {
        ioa: u32,
        dcs: u8,
        select: bool,
        qu: u8,
    },
    SetFloat {
        ioa: u32,
        v: f32,
        select: bool,
        ql: u8,
    },
    Interrogation {
        ioa: u32,
        qoi: u8,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asdu {
    pub head: Header,
    pub items: Vec<Item>,
}

/// Not decodable. `head` is set when the 6 header bytes were readable (so an unknown type can be answered with COT 44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undecoded {
    pub head: Option<Header>,
    pub raw: Vec<u8>,
}

impl Item {
    pub fn ioa(&self) -> u32 {
        match *self {
            Item::Single { ioa, .. }
            | Item::Double { ioa, .. }
            | Item::DoubleTime { ioa, .. }
            | Item::Float { ioa, .. }
            | Item::SingleCmd { ioa, .. }
            | Item::DoubleCmd { ioa, .. }
            | Item::SetFloat { ioa, .. }
            | Item::Interrogation { ioa, .. } => ioa,
        }
    }

    /// The object bytes after the IOA.
    fn put_body(&self, o: &mut Vec<u8>) {
        let se = |select: bool| u8::from(select) << 7;
        match *self {
            Item::Single { on, q, .. } => o.push((q & 0xFE) | u8::from(on)),
            Item::Double { dp, q, .. } => o.push((q & 0xFC) | (dp & 3)),
            Item::DoubleTime { dp, q, time, .. } => {
                o.push((q & 0xFC) | (dp & 3));
                o.extend_from_slice(&time);
            }
            Item::Float { v, q, .. } => {
                o.extend_from_slice(&v.to_le_bytes());
                o.push(q);
            }
            Item::SingleCmd { on, select, qu, .. } => {
                o.push(se(select) | ((qu & 0x1F) << 2) | u8::from(on))
            }
            Item::DoubleCmd {
                dcs, select, qu, ..
            } => o.push(se(select) | ((qu & 0x1F) << 2) | (dcs & 3)),
            Item::SetFloat { v, select, ql, .. } => {
                o.extend_from_slice(&v.to_le_bytes());
                o.push(se(select) | (ql & 0x7F));
            }
            Item::Interrogation { qoi, .. } => o.push(qoi),
        }
    }
}

/// Body size after the IOA for a type id, `None` when unknown.
fn body_len(t: u8) -> Option<usize> {
    Some(match t {
        1 | 3 | 45 | 46 | 100 => 1,
        13 | 50 => 5,
        31 => 8,
        _ => return None,
    })
}

fn get_body(t: u8, ioa: u32, b: &[u8]) -> Item {
    let f = || f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    match t {
        1 => Item::Single {
            ioa,
            on: b[0] & 1 == 1,
            q: b[0] & 0xFE,
        },
        3 => Item::Double {
            ioa,
            dp: b[0] & 3,
            q: b[0] & 0xFC,
        },
        31 => Item::DoubleTime {
            ioa,
            dp: b[0] & 3,
            q: b[0] & 0xFC,
            time: b[1..8].try_into().unwrap(),
        },
        13 => Item::Float {
            ioa,
            v: f(),
            q: b[4],
        },
        45 => Item::SingleCmd {
            ioa,
            on: b[0] & 1 == 1,
            select: b[0] & 0x80 != 0,
            qu: (b[0] >> 2) & 0x1F,
        },
        46 => Item::DoubleCmd {
            ioa,
            dcs: b[0] & 3,
            select: b[0] & 0x80 != 0,
            qu: (b[0] >> 2) & 0x1F,
        },
        50 => Item::SetFloat {
            ioa,
            v: f(),
            select: b[4] & 0x80 != 0,
            ql: b[4] & 0x7F,
        },
        _ => Item::Interrogation { ioa, qoi: b[0] },
    }
}

impl Asdu {
    /// The wire bytes. The VSQ count is `items.len()` (low 7 bits). With `head.sq` only the first object carries an IOA.
    /// Callers keep the objects within `SPLIT_AT` bytes; the result is then at most `MAX_ASDU`.
    pub fn encode(&self) -> Vec<u8> {
        debug_assert!(self.items.len() <= 127, "VSQ counts 7 bits");
        let h = &self.head;
        let mut o = vec![
            h.type_id,
            (u8::from(h.sq) << 7) | (self.items.len() & 0x7F) as u8,
            (h.cot & 0x3F) | (u8::from(h.negative) << 6) | (u8::from(h.test) << 7),
            h.originator,
        ];
        o.extend_from_slice(&h.ca.to_le_bytes());
        for (i, it) in self.items.iter().enumerate() {
            if !h.sq || i == 0 {
                o.extend_from_slice(&it.ioa().to_le_bytes()[..3]);
            }
            it.put_body(&mut o);
        }
        o
    }
}

fn head_of(b: &[u8]) -> Option<Header> {
    let b = b.get(..HEAD)?;
    Some(Header {
        type_id: b[0],
        sq: b[1] & 0x80 != 0,
        n: b[1] & 0x7F,
        cot: b[2] & 0x3F,
        negative: b[2] & 0x40 != 0,
        test: b[2] & 0x80 != 0,
        originator: b[3],
        ca: u16::from_le_bytes([b[4], b[5]]),
    })
}

/// Decode accepts SQ=0 (stock) and SQ=1 (one IOA, then consecutive addresses). Bytes after the last object are ignored.
pub fn decode(b: &[u8]) -> Result<Asdu, Undecoded> {
    let bad = |head| Undecoded {
        head,
        raw: b.to_vec(),
    };
    let head = head_of(b);
    let Some(h) = head.clone() else {
        return Err(bad(None));
    };
    let Some(blen) = body_len(h.type_id) else {
        return Err(bad(head));
    };
    let ioa_at = |p: usize| {
        b.get(p..p + 3)
            .map(|x| u32::from_le_bytes([x[0], x[1], x[2], 0]))
    };
    let mut items = Vec::with_capacity(h.n as usize);
    let mut p = HEAD;
    let mut first = 0;
    for i in 0..h.n as u32 {
        let ioa = if !h.sq || i == 0 {
            let Some(a) = ioa_at(p) else {
                return Err(bad(head));
            };
            p += 3;
            first = a;
            a
        } else {
            first + i
        };
        let Some(body) = b.get(p..p + blen) else {
            return Err(bad(head));
        };
        p += blen;
        items.push(get_body(h.type_id, ioa, body));
    }
    Ok(Asdu { head: h, items })
}

/// CP56Time2a for `unix_ms`. Stock sends UTC (`conformance/iec104/STOCK.md`, CP56Time2a line), so pass offset 0 for stock; a
/// nonzero `utc_offset_s` shifts to that local time. Day-of-week 0, IV 0, SU 0.
pub fn cp56time2a(unix_ms: i64, utc_offset_s: i32) -> [u8; 7] {
    let ms = unix_ms + i64::from(utc_offset_s) * 1000;
    let days = ms.div_euclid(86_400_000);
    let day_ms = ms.rem_euclid(86_400_000);
    // Civil date from days since 1970-01-01 (Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let sec_ms = (day_ms % 60_000) as u16;
    let s = sec_ms.to_le_bytes();
    [
        s[0],
        s[1],
        (day_ms / 60_000 % 60) as u8,
        (day_ms / 3_600_000) as u8,
        day as u8,
        month as u8,
        (year - 2000).rem_euclid(100) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(type_id: u8, n: u8, cot: u8) -> Header {
        Header {
            type_id,
            sq: false,
            n,
            cot,
            negative: false,
            test: false,
            originator: 0,
            ca: 1,
        }
    }

    #[test]
    fn gi_act_con_bytes() {
        // Spec 4.3: `64 01 07 00 01 00 000000 14`.
        let a = Asdu {
            head: head(100, 1, 7),
            items: vec![Item::Interrogation { ioa: 0, qoi: 20 }],
        };
        let want = [0x64, 1, 7, 0, 1, 0, 0, 0, 0, 0x14];
        assert_eq!(a.encode(), want);
        assert_eq!(decode(&want).unwrap(), a);
    }

    #[test]
    fn single_points_vsq4_bytes() {
        // Spec 4.3: `01 04 14 00 01 00 | 000000 00 | 010000 00 | 640000 00 | 650000 00`; goldens G (VSQ 04) show the same.
        let items = [0, 1, 100, 101]
            .map(|ioa| Item::Single {
                ioa,
                on: false,
                q: 0,
            })
            .to_vec();
        let a = Asdu {
            head: head(1, 4, 20),
            items,
        };
        let want = [
            1, 4, 0x14, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 100, 0, 0, 0, 101, 0, 0, 0,
        ];
        assert_eq!(a.encode(), want);
        assert_eq!(decode(&want).unwrap(), a);
    }

    #[test]
    fn float_points_bytes() {
        // Spec 4.3: type 13, IOAs 2, 3, 102, 103, float32 LE + QDS 00.
        let items = [(2, 1.5f32), (3, -2.0), (102, 0.0), (103, 100.25)]
            .map(|(ioa, v)| Item::Float { ioa, v, q: 0 })
            .to_vec();
        let a = Asdu {
            head: head(13, 4, 20),
            items,
        };
        let mut want = vec![13, 4, 0x14, 0, 1, 0];
        for (ioa, v) in [(2u8, 1.5f32), (3, -2.0), (102, 0.0), (103, 100.25)] {
            want.extend_from_slice(&[ioa, 0, 0]);
            want.extend_from_slice(&v.to_le_bytes());
            want.push(0);
        }
        assert_eq!(a.encode(), want);
        assert_eq!(decode(&want).unwrap(), a);
    }

    #[test]
    fn double_point_on_off() {
        // STOCK.md double mode: false is OFF (1), true is ON (2).
        for (dp, byte) in [(1u8, 1u8), (2, 2)] {
            let a = Asdu {
                head: head(3, 1, 20),
                items: vec![Item::Double { ioa: 5, dp, q: 0 }],
            };
            assert_eq!(a.encode()[9], byte);
            assert_eq!(decode(&a.encode()).unwrap(), a);
        }
    }

    /// One of each type, IOAs starting at `a`.
    fn every_item(a: u32) -> Vec<(u8, Item)> {
        let t = [1, 2, 3, 4, 5, 6, 7];
        vec![
            (
                1,
                Item::Single {
                    ioa: a,
                    on: true,
                    q: 0x10,
                },
            ),
            (
                3,
                Item::Double {
                    ioa: a,
                    dp: 2,
                    q: 0x80,
                },
            ),
            (
                31,
                Item::DoubleTime {
                    ioa: a,
                    dp: 1,
                    q: 0,
                    time: t,
                },
            ),
            (
                13,
                Item::Float {
                    ioa: a,
                    v: -3.5,
                    q: 0x10,
                },
            ),
            (
                45,
                Item::SingleCmd {
                    ioa: a,
                    on: true,
                    select: true,
                    qu: 3,
                },
            ),
            (
                46,
                Item::DoubleCmd {
                    ioa: a,
                    dcs: 2,
                    select: false,
                    qu: 31,
                },
            ),
            (
                50,
                Item::SetFloat {
                    ioa: a,
                    v: 12.25,
                    select: true,
                    ql: 5,
                },
            ),
            (100, Item::Interrogation { ioa: a, qoi: 20 }),
        ]
    }

    #[test]
    fn every_type_round_trips() {
        for ((t, one), (_, two)) in every_item(0x01_02_03)
            .into_iter()
            .zip(every_item(0x01_02_04))
        {
            for sq in [false, true] {
                // Consecutive IOAs, so SQ=1 reproduces them.
                let h = Header {
                    sq,
                    negative: true,
                    test: true,
                    originator: 9,
                    ca: 0x1234,
                    ..head(t, 2, 6)
                };
                let a = Asdu {
                    head: h,
                    items: vec![one.clone(), two.clone()],
                };
                assert_eq!(decode(&a.encode()).unwrap(), a, "type {t} sq {sq}");
            }
        }
    }

    #[test]
    fn nan_and_inf_encode_as_float32_bits() {
        for bits in [
            f32::NAN.to_bits(),
            f32::INFINITY.to_bits(),
            f32::NEG_INFINITY.to_bits(),
            0x7FC0_0001,
        ] {
            let a = Asdu {
                head: head(13, 1, 20),
                items: vec![Item::Float {
                    ioa: 1,
                    v: f32::from_bits(bits),
                    q: 0,
                }],
            };
            let b = a.encode();
            assert_eq!(&b[9..13], bits.to_le_bytes());
            let Item::Float { v, .. } = decode(&b).unwrap().items[0] else {
                panic!()
            };
            assert_eq!(v.to_bits(), bits);
        }
    }

    #[test]
    fn unknown_type_keeps_the_header() {
        // STOCK.md: an unknown type is answered with negative COT 44; the server needs the header for that.
        let raw = [107, 1, 6, 0, 1, 0, 0, 0, 0, 1, 2, 3];
        let e = decode(&raw).unwrap_err();
        let h = e.head.unwrap();
        assert_eq!((h.type_id, h.cot, h.ca, h.n), (107, 6, 1, 1));
        assert_eq!(e.raw, raw);
    }

    #[test]
    fn truncated_asdus_are_errors_not_panics() {
        for (t, it) in every_item(1) {
            let full = Asdu {
                head: head(t, 1, 6),
                items: vec![it],
            }
            .encode();
            for cut in 0..full.len() {
                let e = decode(&full[..cut]).unwrap_err();
                assert_eq!(e.head.is_some(), cut >= HEAD, "type {t} cut {cut}");
            }
            assert!(decode(&full).is_ok());
        }
    }

    #[test]
    fn max_split_fits_in_max_asdu() {
        let items = (0..SPLIT_AT as u32 / 4)
            .map(|ioa| Item::Single {
                ioa,
                on: true,
                q: 0,
            })
            .collect();
        let a = Asdu {
            head: head(1, 60, 20),
            items,
        };
        assert!(a.encode().len() <= MAX_ASDU);
        let items = (0..SPLIT_AT as u32 / 8)
            .map(|ioa| Item::Float { ioa, v: 1.0, q: 0 })
            .collect();
        assert!(
            Asdu {
                head: head(13, 30, 20),
                items
            }
            .encode()
            .len()
                <= MAX_ASDU
        );
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
        let types = [1u8, 3, 13, 31, 45, 46, 50, 100];
        for _ in 0..20_000 {
            let n = (rnd() % 300) as usize;
            let mut buf: Vec<u8> = (0..n).map(|_| rnd() as u8).collect();
            if n > 0 && rnd() % 2 == 0 {
                buf[0] = types[(rnd() % 8) as usize]; // reach the object loop
            }
            if let Ok(a) = decode(&buf) {
                assert_eq!(a.items.len(), a.head.n as usize);
            }
        }
    }

    #[test]
    fn cp56time2a_matches_a_stock_transcript_timestamp() {
        // The golden masks CP56Time2a (`__`), so the bytes are computed by hand from the layout for 2026-11-22 13:45:07.250 UTC:
        // ms LE u16 incl. seconds (7250 = 0x1C52), minute 45, hour 13, day 22 with DOW bits 0, month 11, year 26.
        // STOCK.md: the time is UTC (not the DUT's TZ), day-of-week 0, IV 0, SU 0.
        let unix_ms = 1_795_355_107_250; // 2026-11-22T13:45:07.250Z
        assert_eq!(cp56time2a(unix_ms, 0), [0x52, 0x1C, 45, 13, 22, 11, 26]);
        assert_eq!(cp56time2a(unix_ms, 3600)[3], 14);
        assert_eq!(cp56time2a(0, 0), [0, 0, 0, 0, 1, 1, 70]);
    }
}
