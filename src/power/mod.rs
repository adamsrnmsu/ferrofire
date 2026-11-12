//! Quasi-static AC power flow for model files (spec sections 3.4, 4). `Power` owns a case plus the PYPOWER tag
//! vocabulary (names and fields as pybennu's PyPower provider publishes them) and re-solves on demand.
//!
//! Deviations from pybennu, on purpose (spec decisions 8 and 9):
//! - load/shunt tags and bus `load_mw`/`shunt_mvar` writes do affect the solve (`load-1_bus-N.active=false` removes the load);
//! - a bus keeps its PV/PQ type when `active`/`slack` are written (pybennu turns every non-slack bus into PQ);
//! - `islands: deenergize` publishes VM = 0 for buses no reference generator can reach.

pub mod case;
pub mod pf;

use crate::model::Kind;
use case::*;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Islands {
    Fail,
    Deenergize,
}

/// Which case array a tag's row indexes.
#[derive(Clone, Copy, Debug)]
enum Tab {
    Bus,
    Gen,
    Branch,
}

/// Boolean fields: set from any nonzero, read back as 0/1.
#[derive(Clone, Copy, Debug)]
enum Flag {
    Alert,
    BusActive,
    BusSlack,
    LoadOn,
    ShuntOn,
    GenOn,    // gen status > 0
    BranchOn, // branch status != 0
}

/// How a tag reads and writes the case.
#[derive(Clone, Copy, Debug)]
enum F {
    /// One numeric column of its row.
    Cell(Tab, usize),
    /// Sum of a gen column over the generators on its bus (read-only).
    GenSum(usize),
    Flag(Flag),
    /// Bus BS while the shunt is in service, else 0 (writes go to BS).
    ShActual,
    /// Reads RATE_C, writes all three rating columns, as pybennu does.
    BrMva,
}

#[derive(Debug)]
struct Tag {
    name: String,
    kind: Kind,
    f: F,
    row: usize,
    writable: bool,
}

/// (field name, wire type, accessor, writable)
type Field = (&'static str, Kind, F, bool);

const AN: Kind = Kind::Analog;
const BI: Kind = Kind::Binary;
const fn bus(c: usize) -> F {
    F::Cell(Tab::Bus, c)
}
const fn gen(c: usize) -> F {
    F::Cell(Tab::Gen, c)
}
const fn br(c: usize) -> F {
    F::Cell(Tab::Branch, c)
}

const BUS_FIELDS: [Field; 10] = [
    ("voltage", AN, bus(VM), true),
    ("angle", AN, bus(VA), true),
    ("gen_mw", AN, F::GenSum(PG), false),
    ("gen_mvar", AN, F::GenSum(QG), false),
    ("load_mw", AN, bus(PD), true),
    ("load_mvar", AN, bus(QD), true),
    ("shunt_mvar", AN, bus(BS), true),
    ("base_kv", AN, bus(BASE_KV), true),
    ("active", BI, F::Flag(Flag::BusActive), true),
    ("slack", BI, F::Flag(Flag::BusSlack), true),
];
const LOAD_FIELDS: [Field; 4] = [
    ("bus", AN, bus(BUS_I), false),
    ("mw", AN, bus(PD), true),
    ("mvar", AN, bus(QD), true),
    ("active", BI, F::Flag(Flag::LoadOn), true),
];
const SHUNT_FIELDS: [Field; 4] = [
    ("bus", AN, bus(BUS_I), false),
    ("actual_mvar", AN, F::ShActual, true),
    ("nominal_mvar", AN, bus(BS), false),
    ("active", BI, F::Flag(Flag::ShuntOn), true),
];
const GEN_FIELDS: [Field; 8] = [
    ("mw", AN, gen(PG), true),
    ("mvar", AN, gen(QG), true),
    ("mw_max", AN, gen(PMAX), true),
    ("mw_min", AN, gen(PMIN), true),
    ("mvar_max", AN, gen(QMAX), true),
    ("mvar_min", AN, gen(QMIN), true),
    ("voltage", AN, gen(VG), true),
    ("active", BI, F::Flag(Flag::GenOn), true),
];
const BRANCH_FIELDS: [Field; 9] = [
    ("source", AN, br(F_BUS), false),
    ("target", AN, br(T_BUS), false),
    ("resistance", AN, br(BR_R), true),
    ("reactance", AN, br(BR_X), true),
    ("charging", AN, br(BR_B), true),
    ("mva_max", AN, F::BrMva, true),
    ("turns_ratio", AN, br(TAP), true),
    ("phase_angle", AN, br(SHIFT), true),
    ("active", BI, F::Flag(Flag::BranchOn), true),
];

pub struct Power {
    pub case: Case,
    pub islands: Islands,
    pub dc_fallback: bool,
    /// PYPOWER's ENFORCE_Q_LIMS (off in sceptre's PyPower, hence off unless the model asks).
    pub enforce_q_lims: bool,
    pub alert: bool,
    tags: Vec<Tag>,
    by_name: HashMap<String, usize>,
    bus_active: Vec<bool>,
    bus_slack: Vec<bool>,
    pv_type: Vec<f64>, // type a non-slack bus takes: PV if it has a generator, else PQ (or the case's own PV/PQ)
    load_on: Vec<bool>,
    shunt_on: Vec<bool>,
}

fn f2(x: f64) -> bool {
    x != 0.0
}

fn bit(b: bool) -> f64 {
    b as u8 as f64
}

/// Bus number -> row.
fn bus_rows(case: &Case) -> HashMap<i64, usize> {
    (case.bus.iter().enumerate())
        .map(|(i, b)| (b[BUS_I] as i64, i))
        .collect()
}

/// A `power.drive` target must never be silently dropped: its bus has to exist.
fn check_extras(
    rows: &HashMap<i64, usize>,
    extra_loads: &[i64],
    extra_shunts: &[i64],
) -> Result<(), String> {
    for (what, l) in [("load", extra_loads), ("shunt", extra_shunts)] {
        if let Some(n) = l.iter().find(|n| !rows.contains_key(n)) {
            return Err(format!(
                "power.drive: {what}-1_bus-{n}: no bus {n} in the case"
            ));
        }
    }
    Ok(())
}

/// The type a non-slack bus takes: the case's own PV/PQ, else PV when it has a generator and PQ when not.
fn pv_types(case: &Case, rows: &HashMap<i64, usize>) -> Vec<f64> {
    let mut has_gen = vec![false; case.bus.len()];
    for r in case
        .gen
        .iter()
        .filter_map(|g| rows.get(&(g[GEN_BUS] as i64)))
    {
        has_gen[*r] = true;
    }
    (case.bus.iter().zip(has_gen))
        .map(|(b, g)| match b[BUS_TYPE] {
            t if t == PV || t == PQ => t,
            _ if g => PV,
            _ => PQ,
        })
        .collect()
}

impl Power {
    /// `extra_loads` / `extra_shunts`: bus numbers that need a `load-1_bus-N` / `shunt-1_bus-N` device even though
    /// the case has none (a `power.drive` target must never be silently dropped).
    pub fn new(
        case: Case,
        islands: Islands,
        dc_fallback: bool,
        extra_loads: &[i64],
        extra_shunts: &[i64],
    ) -> Result<Power, String> {
        Self::new_with(
            case,
            islands,
            (dc_fallback, false),
            extra_loads,
            extra_shunts,
        )
    }

    /// `new` with the options as `(dc_fallback, enforce_q_lims)`.
    pub fn new_with(
        case: Case,
        islands: Islands,
        (dc_fallback, enforce_q_lims): (bool, bool),
        extra_loads: &[i64],
        extra_shunts: &[i64],
    ) -> Result<Power, String> {
        let nb = case.bus.len();
        let rows = bus_rows(&case);
        check_extras(&rows, extra_loads, extra_shunts)?;
        let mut p = Power {
            islands,
            dc_fallback,
            enforce_q_lims,
            alert: false,
            tags: vec![],
            by_name: HashMap::new(),
            bus_active: case.bus.iter().map(|b| b[BUS_TYPE] != NONE).collect(),
            bus_slack: case.bus.iter().map(|b| b[BUS_TYPE] == REF).collect(),
            pv_type: pv_types(&case, &rows),
            load_on: vec![true; nb],
            shunt_on: vec![true; nb],
            case,
        };
        p.build_tags(extra_loads, extra_shunts);
        p.solve();
        Ok(p)
    }

    fn add(&mut self, name: String, kind: Kind, f: F, row: usize, writable: bool) {
        self.by_name.insert(name.clone(), self.tags.len());
        self.tags.push(Tag {
            name,
            kind,
            f,
            row,
            writable,
        });
    }

    fn add_device(&mut self, dev: &str, row: usize, fields: &[Field]) {
        for &(fld, kind, f, writable) in fields {
            self.add(format!("{dev}.{fld}"), kind, f, row, writable);
        }
    }

    /// Same device/field order as pybennu's `elements` dict (bus incl. bus-alert, load, shunt, gen, branch).
    fn build_tags(&mut self, extra_loads: &[i64], extra_shunts: &[i64]) {
        self.add(
            "bus-alert.active".into(),
            BI,
            F::Flag(Flag::Alert),
            0,
            false,
        );
        for i in 0..self.case.bus.len() {
            let dev = format!("bus-{}", self.case.bus[i][BUS_I] as i64);
            self.add_device(&dev, i, &BUS_FIELDS);
        }
        let has_load = |b: &BusRow| b[PD] != 0.0 || b[QD] != 0.0;
        self.add_bus_devices("load", extra_loads, &LOAD_FIELDS, has_load);
        self.add_bus_devices("shunt", extra_shunts, &SHUNT_FIELDS, |b| b[BS] != 0.0);
        self.add_gens();
        self.add_branches();
    }

    /// `load-1_bus-N` / `shunt-1_bus-N`: on buses that carry one (or were asked for).
    fn add_bus_devices(
        &mut self,
        kind: &str,
        extra: &[i64],
        fields: &[Field],
        present: impl Fn(&BusRow) -> bool,
    ) {
        for i in 0..self.case.bus.len() {
            let b = self.case.bus[i];
            let id = b[BUS_I] as i64;
            if present(&b) || extra.contains(&id) {
                self.add_device(&format!("{kind}-1_bus-{id}"), i, fields);
            }
        }
    }

    fn add_gens(&mut self) {
        let mut cnt: HashMap<i64, usize> = HashMap::new();
        for r in 0..self.case.gen.len() {
            let bus = self.case.gen[r][GEN_BUS] as i64;
            let c = cnt.entry(bus).or_insert(0);
            *c += 1;
            let dev = format!("generator-{}_bus-{}", c, bus);
            self.add_device(&dev, r, &GEN_FIELDS);
        }
    }

    fn add_branches(&mut self) {
        let mut cnt: HashMap<(i64, i64), usize> = HashMap::new();
        for r in 0..self.case.branch.len() {
            let (f, t) = (
                self.case.branch[r][F_BUS] as i64,
                self.case.branch[r][T_BUS] as i64,
            );
            let c = cnt.entry((f, t)).or_insert(0);
            *c += 1;
            let dev = format!("branch-{}_{}-{}", c, f, t);
            self.add_device(&dev, r, &BRANCH_FIELDS);
        }
    }

    pub fn n_tags(&self) -> usize {
        self.tags.len()
    }
    pub fn tag_name(&self, i: usize) -> &str {
        &self.tags[i].name
    }
    pub fn tag_kind(&self, i: usize) -> Kind {
        self.tags[i].kind
    }
    pub fn tag_index(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }
    pub fn writable(&self, i: usize) -> bool {
        self.tags[i].writable
    }

    fn bus_gen_sum(&self, row: usize, col: usize) -> f64 {
        let id = self.case.bus[row][BUS_I];
        self.case
            .gen
            .iter()
            .filter(|g| g[GEN_BUS] == id)
            .map(|g| g[col])
            .sum()
    }

    fn cell(&self, tab: Tab, r: usize, col: usize) -> f64 {
        match tab {
            Tab::Bus => self.case.bus[r][col],
            Tab::Gen => self.case.gen[r][col],
            Tab::Branch => self.case.branch[r][col],
        }
    }

    fn cell_mut(&mut self, tab: Tab, r: usize, col: usize) -> &mut f64 {
        match tab {
            Tab::Bus => &mut self.case.bus[r][col],
            Tab::Gen => &mut self.case.gen[r][col],
            Tab::Branch => &mut self.case.branch[r][col],
        }
    }

    fn flag(&self, fl: Flag, r: usize) -> bool {
        match fl {
            Flag::Alert => self.alert,
            Flag::BusActive => self.bus_active[r],
            Flag::BusSlack => self.bus_slack[r],
            Flag::LoadOn => self.load_on[r],
            Flag::ShuntOn => self.shunt_on[r],
            Flag::GenOn => self.case.gen[r][GEN_STATUS] > 0.0,
            Flag::BranchOn => self.case.branch[r][BR_STATUS] != 0.0,
        }
    }

    fn set_flag(&mut self, fl: Flag, r: usize, x: f64) {
        match fl {
            Flag::Alert => {}
            Flag::BusActive => self.bus_active[r] = f2(x),
            Flag::BusSlack => self.bus_slack[r] = f2(x),
            Flag::LoadOn => self.load_on[r] = f2(x),
            Flag::ShuntOn => self.shunt_on[r] = f2(x),
            Flag::GenOn => self.case.gen[r][GEN_STATUS] = bit(f2(x)),
            Flag::BranchOn => self.case.branch[r][BR_STATUS] = bit(f2(x)),
        }
    }

    pub fn get(&self, i: usize) -> f64 {
        let t = &self.tags[i];
        let r = t.row;
        match t.f {
            F::Cell(tab, col) => self.cell(tab, r, col),
            F::GenSum(col) => self.bus_gen_sum(r, col),
            F::Flag(fl) => bit(self.flag(fl, r)),
            F::ShActual if self.shunt_on[r] => self.case.bus[r][BS],
            F::ShActual => 0.0,
            F::BrMva => self.case.branch[r][RATE_C],
        }
    }

    /// Edit the case. Read-only tags are ignored. Binary fields take any nonzero as true.
    pub fn set(&mut self, i: usize, x: f64) {
        let (f, r, writable) = (self.tags[i].f, self.tags[i].row, self.tags[i].writable);
        if !writable {
            return;
        }
        match f {
            F::Cell(tab, col) => *self.cell_mut(tab, r, col) = x,
            F::Flag(fl) => self.set_flag(fl, r, x),
            F::ShActual => self.case.bus[r][BS] = x,
            F::BrMva => {
                for col in RATE_A..=RATE_C {
                    self.case.branch[r][col] = x;
                }
            } // pybennu writes all three rating columns
            F::GenSum(_) => {}
        }
    }

    /// The case as the solver sees it: bus types from active/slack, de-activated loads and shunts zeroed.
    fn effective(&self) -> Case {
        let mut c = self.case.clone();
        for (i, b) in c.bus.iter_mut().enumerate() {
            b[BUS_TYPE] = if !self.bus_active[i] {
                NONE
            } else if self.bus_slack[i] {
                REF
            } else {
                self.pv_type[i]
            };
            if !self.load_on[i] {
                b[PD] = 0.0;
                b[QD] = 0.0;
            }
            if !self.shunt_on[i] {
                b[BS] = 0.0;
            }
        }
        c
    }

    fn apply(&mut self, s: &Case) {
        for (b, r) in self.case.bus.iter_mut().zip(&s.bus) {
            b[VM] = r[VM];
            b[VA] = r[VA];
        }
        for (g, r) in self.case.gen.iter_mut().zip(&s.gen) {
            g[PG] = r[PG];
            g[QG] = r[QG];
        }
        for (b, r) in self.case.branch.iter_mut().zip(&s.branch) {
            b[PF] = r[PF];
            b[QF] = r[QF];
            b[PT] = r[PT];
            b[QT] = r[QT];
        }
    }

    /// Solve from the stored (warm) voltages. On failure keep the last good results, raise `bus-alert`
    /// (publishing the DC solution instead when `dc_fallback`).
    pub fn solve(&mut self) {
        let eff = self.effective();
        let o = pf::Opts {
            deenergize: self.islands == Islands::Deenergize,
            enforce_q_lims: self.enforce_q_lims,
            ..Default::default()
        };
        let r = pf::runpf(&eff, &o);
        let why = match r {
            Ok(s) if s.success => {
                self.apply(&s.case);
                self.alert = false;
                return;
            }
            Ok(_) => "did not converge".to_string(),
            Err(e) => e,
        };
        if !self.alert {
            tracing::warn!(
                "power flow failed ({why}); keeping the last solution, bus-alert raised"
            );
        }
        self.alert = true;
        if self.dc_fallback {
            if let Ok(s) = pf::runpf(&eff, &pf::Opts { dc: true, ..o }) {
                self.apply(&s.case);
                for g in self.case.gen.iter_mut() {
                    g[QG] = 0.0;
                }
            }
        }
    }
}
