//! Power-flow case data and its parsers (spec section 4.4): PYPOWER `.py` (restricted literal subset), MATPOWER `.m`
//! and inline YAML. Column layout is MATPOWER's `caseformat` (bus 13, gen 21, branch 13 + 4 result columns).
//!
//! The column constants and the idea of reading a case as numeric arrays follow PYPOWER 5.1.19
//! (https://github.com/rwl/PYPOWER), BSD 3-clause, Copyright (c) 1996-2015 PSERC; see NOTICE.

use serde::Deserialize;

// bus columns
pub const BUS_I: usize = 0;
pub const BUS_TYPE: usize = 1;
pub const PD: usize = 2;
pub const QD: usize = 3;
pub const GS: usize = 4;
pub const BS: usize = 5;
pub const VM: usize = 7;
pub const VA: usize = 8;
pub const BASE_KV: usize = 9;
pub const PQ: f64 = 1.0;
pub const PV: f64 = 2.0;
pub const REF: f64 = 3.0;
pub const NONE: f64 = 4.0;
// gen columns
pub const GEN_BUS: usize = 0;
pub const PG: usize = 1;
pub const QG: usize = 2;
pub const QMAX: usize = 3;
pub const QMIN: usize = 4;
pub const VG: usize = 5;
pub const GEN_STATUS: usize = 7;
pub const PMAX: usize = 8;
pub const PMIN: usize = 9;
// branch columns
pub const F_BUS: usize = 0;
pub const T_BUS: usize = 1;
pub const BR_R: usize = 2;
pub const BR_X: usize = 3;
pub const BR_B: usize = 4;
pub const RATE_A: usize = 5;
pub const RATE_C: usize = 7;
pub const TAP: usize = 8;
pub const SHIFT: usize = 9;
pub const BR_STATUS: usize = 10;
pub const PF: usize = 13;
pub const QF: usize = 14;
pub const PT: usize = 15;
pub const QT: usize = 16;

pub type BusRow = [f64; 13];
pub type GenRow = [f64; 21];
pub type BranchRow = [f64; 17];

#[derive(Clone, Debug, Default)]
pub struct Case {
    pub base_mva: f64,
    pub bus: Vec<BusRow>,
    pub gen: Vec<GenRow>,
    pub branch: Vec<BranchRow>,
}

fn pad<const N: usize>(
    rows: Vec<Vec<f64>>,
    what: &str,
    min: usize,
) -> Result<Vec<[f64; N]>, String> {
    rows.into_iter()
        .enumerate()
        .map(|(i, r)| {
            if r.len() < min {
                return Err(format!(
                    "{what} row {}: {} columns, need at least {min}",
                    i + 1,
                    r.len()
                ));
            }
            let mut a = [0.0; N];
            for (d, s) in a.iter_mut().zip(r) {
                *d = s;
            }
            Ok(a)
        })
        .collect()
}

impl Case {
    fn from_parts(
        base: f64,
        bus: Vec<Vec<f64>>,
        gen: Vec<Vec<f64>>,
        branch: Vec<Vec<f64>>,
    ) -> Result<Case, String> {
        Ok(Case {
            base_mva: base,
            bus: pad(bus, "bus", 10)?,
            gen: pad(gen, "gen", 10)?,
            branch: pad(branch, "branch", 11)?,
        })
    }
}

// ---- shared numeric-literal reader ---------------------------------------------------------------------------

struct Cur<'a> {
    s: &'a str,
    i: usize,
}
impl<'a> Cur<'a> {
    fn line(&self) -> usize {
        self.s[..self.i.min(self.s.len())].matches('\n').count() + 1
    }
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        let l = self.s.lines().nth(self.line() - 1).unwrap_or("").trim();
        Err(format!("line {}: {msg}: `{l}`", self.line()))
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && (self.s.as_bytes()[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.as_bytes().get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn num(&mut self) -> Result<f64, String> {
        self.ws();
        let b = self.s.as_bytes();
        let st = self.i;
        while self.i < b.len()
            && (b[self.i].is_ascii_digit() || matches!(b[self.i], b'+' | b'-' | b'.' | b'e' | b'E'))
        {
            self.i += 1;
        }
        match self.s[st..self.i].parse::<f64>() {
            Ok(x) => Ok(x),
            Err(_) => {
                self.i = st;
                self.err("expected a numeric literal")
            }
        }
    }
}

/// Drop `#` (python) or `%` (matlab) comments to end of line.
fn strip_comments(src: &str, c: char) -> String {
    src.lines()
        .map(|l| l.split(c).next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- PYPOWER .py ---------------------------------------------------------------------------------------------

/// Position just after `ppc["key"] =` (either quote), or None.
fn py_key(src: &str, key: &str) -> Option<usize> {
    for q in ['"', '\''] {
        let pat = format!("[{q}{key}{q}]");
        if let Some(p) = src.find(&pat) {
            let rest = &src[p + pat.len()..];
            let eq = rest.find('=')?;
            if rest[..eq].trim().is_empty() {
                return Some(p + pat.len() + eq + 1);
            }
        }
    }
    None
}

/// Restricted literal parser: `ppc["bus"] = array([[1, 3, 0.5], ...])` or a bare nested list.
pub fn parse_py(src: &str) -> Result<Case, String> {
    let src = strip_comments(src, '#');
    let mat = |key: &str| -> Result<Vec<Vec<f64>>, String> {
        let at = py_key(&src, key).ok_or(format!("case: no ppc[\"{key}\"] assignment"))?;
        let mut c = Cur { s: &src, i: at };
        let wrapped = if c.peek() == Some(b'[') {
            false
        } else {
            let st = c.i;
            if !src[st..].starts_with("array(") {
                return c.err("only literal arrays (`array([...])` or `[...]`) are supported");
            }
            c.i += "array(".len();
            true
        };
        if !c.eat(b'[') {
            return c.err("expected `[`");
        }
        let mut rows = vec![];
        while !c.eat(b']') {
            if !c.eat(b'[') {
                return c.err("expected `[` starting a row");
            }
            let mut row = vec![];
            while !c.eat(b']') {
                row.push(c.num()?);
                c.eat(b',');
            }
            rows.push(row);
            c.eat(b',');
        }
        if wrapped && !c.eat(b')') {
            return c.err("expected `)`");
        }
        Ok(rows)
    };
    let at = py_key(&src, "baseMVA").ok_or("case: no ppc[\"baseMVA\"] assignment")?;
    let base = Cur { s: &src, i: at }.num()?;
    Case::from_parts(base, mat("bus")?, mat("gen")?, mat("branch")?)
}

// ---- MATPOWER .m ---------------------------------------------------------------------------------------------

pub fn parse_m(src: &str) -> Result<Case, String> {
    let src = strip_comments(src, '%');
    let find = |key: &str| -> Result<usize, String> {
        let pat = format!("mpc.{key}");
        let p = src
            .match_indices(&pat)
            .map(|(i, _)| i)
            .find(|i| !src[i + pat.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_'))
            .ok_or(format!("case: no mpc.{key}"))?;
        let eq = src[p..]
            .find('=')
            .ok_or(format!("case: mpc.{key} has no `=`"))?;
        Ok(p + eq + 1)
    };
    let mat = |key: &str| -> Result<Vec<Vec<f64>>, String> {
        let mut c = Cur {
            s: &src,
            i: find(key)?,
        };
        if !c.eat(b'[') {
            return c.err("expected `[`");
        }
        let end = src[c.i..].find(']').ok_or("case: unterminated `[`")? + c.i;
        src[c.i..end]
            .split([';', '\n'])
            .filter(|r| !r.trim().is_empty())
            .map(|r| {
                r.split(|ch: char| ch == ',' || ch.is_whitespace())
                    .filter(|t| !t.is_empty())
                    .map(|t| {
                        t.parse::<f64>()
                            .map_err(|_| format!("case: bad number `{t}` in mpc.{key}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<_, _>>()
    };
    let base = Cur {
        s: &src,
        i: find("baseMVA")?,
    }
    .num()?;
    Case::from_parts(base, mat("bus")?, mat("gen")?, mat("branch")?)
}

// ---- inline YAML ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BusType {
    N(f64),
    S(String),
}

fn d_one() -> f64 {
    1.0
}
fn d_base() -> f64 {
    100.0
}
fn d_vmax() -> f64 {
    1.1
}
fn d_vmin() -> f64 {
    0.9
}
fn d_ninf() -> f64 {
    -360.0
}
fn d_pinf() -> f64 {
    360.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IBus {
    id: f64,
    #[serde(rename = "type")]
    ty: BusType,
    #[serde(default = "d_one")]
    vm: f64,
    #[serde(default)]
    va: f64,
    #[serde(default)]
    pd: f64,
    #[serde(default)]
    qd: f64,
    #[serde(default)]
    gs: f64,
    #[serde(default)]
    bs: f64,
    #[serde(default)]
    base_kv: f64,
    #[serde(default = "d_one")]
    area: f64,
    #[serde(default = "d_one")]
    zone: f64,
    #[serde(default = "d_vmax")]
    vmax: f64,
    #[serde(default = "d_vmin")]
    vmin: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IBranch {
    from: f64,
    to: f64,
    r: f64,
    x: f64,
    #[serde(default)]
    b: f64,
    #[serde(default)]
    rate: f64,
    #[serde(default)]
    tap: f64,
    #[serde(default)]
    shift: f64,
    #[serde(default = "d_one")]
    status: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IGen {
    bus: f64,
    #[serde(default)]
    pg: f64,
    #[serde(default)]
    qg: f64,
    #[serde(default = "d_one")]
    vg: f64,
    #[serde(default)]
    qmax: f64,
    #[serde(default)]
    qmin: f64,
    #[serde(default)]
    pmax: f64,
    #[serde(default)]
    pmin: f64,
    #[serde(default = "d_one")]
    status: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inline {
    #[serde(rename = "baseMVA", default = "d_base")]
    base_mva: f64,
    buses: Vec<IBus>,
    #[serde(default)]
    branches: Vec<IBranch>,
    #[serde(default)]
    gens: Vec<IGen>,
}

impl Inline {
    pub fn to_case(&self) -> Result<Case, String> {
        let mut c = Case {
            base_mva: self.base_mva,
            ..Default::default()
        };
        for b in &self.buses {
            let ty = match &b.ty {
                BusType::N(n) => *n,
                BusType::S(s) => match s.as_str() {
                    "slack" | "ref" => REF,
                    "pv" => PV,
                    "pq" => PQ,
                    "isolated" | "none" => NONE,
                    o => return Err(format!("inline bus {}: unknown type `{o}`", b.id)),
                },
            };
            c.bus.push([
                b.id, ty, b.pd, b.qd, b.gs, b.bs, b.area, b.vm, b.va, b.base_kv, b.zone, b.vmax,
                b.vmin,
            ]);
        }
        for g in &self.gens {
            let mut r = [0.0; 21];
            r[..10].copy_from_slice(&[
                g.bus,
                g.pg,
                g.qg,
                g.qmax,
                g.qmin,
                g.vg,
                self.base_mva,
                g.status,
                g.pmax,
                g.pmin,
            ]);
            c.gen.push(r);
        }
        for b in &self.branches {
            let mut r = [0.0; 17];
            r[..13].copy_from_slice(&[
                b.from,
                b.to,
                b.r,
                b.x,
                b.b,
                b.rate,
                b.rate,
                b.rate,
                b.tap,
                b.shift,
                b.status,
                d_ninf(),
                d_pinf(),
            ]);
            c.branch.push(r);
        }
        Ok(c)
    }
}

/// Load by extension: `.py` or `.m`.
pub fn load_file(path: &std::path::Path) -> Result<Case, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("case {}: {e}", path.display()))?;
    let r = match path.extension().and_then(|e| e.to_str()) {
        Some("py") => parse_py(&src),
        Some("m") => parse_m(&src),
        _ => Err("only .py and .m cases are supported (not .mat)".to_string()),
    };
    r.map_err(|e| format!("case {}: {e}", path.display()))
}
