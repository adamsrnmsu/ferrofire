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
pub enum Islands { Fail, Deenergize }

#[derive(Clone, Copy, Debug)]
enum F {
    Alert,
    BusV, BusA, BusGenMw, BusGenMvar, BusLoadMw, BusLoadMvar, BusShunt, BusKv, BusActive, BusSlack,
    LoadBus, LoadMw, LoadMvar, LoadActive,
    ShBus, ShActual, ShNominal, ShActive,
    GenMw, GenMvar, GenMwMax, GenMwMin, GenMvarMax, GenMvarMin, GenV, GenActive,
    BrSource, BrTarget, BrR, BrX, BrB, BrMva, BrTap, BrShift, BrActive,
}

#[derive(Debug)]
struct Tag { name: String, kind: Kind, f: F, row: usize, writable: bool }

pub struct Power {
    pub case: Case,
    pub islands: Islands,
    pub dc_fallback: bool,
    pub alert: bool,
    tags: Vec<Tag>,
    by_name: HashMap<String, usize>,
    bus_active: Vec<bool>,
    bus_slack: Vec<bool>,
    pv_type: Vec<f64>, // type a non-slack bus takes: PV if it has a generator, else PQ (or the case's own PV/PQ)
    load_on: Vec<bool>,
    shunt_on: Vec<bool>,
}

fn f2(x: f64) -> bool { x != 0.0 }

impl Power {
    /// `extra_loads` / `extra_shunts`: bus numbers that need a `load-1_bus-N` / `shunt-1_bus-N` device even though
    /// the case has none (a `power.drive` target must never be silently dropped).
    pub fn new(case: Case, islands: Islands, dc_fallback: bool, extra_loads: &[i64], extra_shunts: &[i64]) -> Result<Power, String> {
        let nb = case.bus.len();
        let rows: HashMap<i64, usize> = case.bus.iter().enumerate().map(|(i, b)| (b[BUS_I] as i64, i)).collect();
        for (what, l) in [("load", extra_loads), ("shunt", extra_shunts)] {
            for n in l { if !rows.contains_key(n) { return Err(format!("power.drive: {what}-1_bus-{n}: no bus {n} in the case")); } }
        }
        let mut has_gen = vec![false; nb];
        for g in &case.gen { if let Some(&r) = rows.get(&(g[GEN_BUS] as i64)) { has_gen[r] = true; } }
        let pv_type = (0..nb).map(|i| match case.bus[i][BUS_TYPE] {
            t if t == PV || t == PQ => t,
            _ => if has_gen[i] { PV } else { PQ },
        }).collect();
        let mut p = Power {
            islands, dc_fallback, alert: false, tags: vec![], by_name: HashMap::new(),
            bus_active: case.bus.iter().map(|b| b[BUS_TYPE] != NONE).collect(),
            bus_slack: case.bus.iter().map(|b| b[BUS_TYPE] == REF).collect(),
            pv_type,
            load_on: vec![true; nb], shunt_on: vec![true; nb],
            case,
        };
        p.build_tags(extra_loads, extra_shunts);
        p.solve();
        Ok(p)
    }

    fn add(&mut self, name: String, kind: Kind, f: F, row: usize, writable: bool) {
        self.by_name.insert(name.clone(), self.tags.len());
        self.tags.push(Tag { name, kind, f, row, writable });
    }

    /// Same device/field order as pybennu's `elements` dict (bus incl. bus-alert, load, shunt, gen, branch).
    fn build_tags(&mut self, extra_loads: &[i64], extra_shunts: &[i64]) {
        use F::*;
        let (an, bi) = (Kind::Analog, Kind::Binary);
        self.add("bus-alert.active".into(), bi, Alert, 0, false);
        let id = |b: &BusRow| b[BUS_I] as i64;
        for i in 0..self.case.bus.len() {
            let n = format!("bus-{}", id(&self.case.bus[i]));
            for (fld, k, f, w) in [("voltage", an, BusV, true), ("angle", an, BusA, true), ("gen_mw", an, BusGenMw, false),
                ("gen_mvar", an, BusGenMvar, false), ("load_mw", an, BusLoadMw, true), ("load_mvar", an, BusLoadMvar, true),
                ("shunt_mvar", an, BusShunt, true), ("base_kv", an, BusKv, true), ("active", bi, BusActive, true), ("slack", bi, BusSlack, true)] {
                self.add(format!("{n}.{fld}"), k, f, i, w);
            }
        }
        for i in 0..self.case.bus.len() {
            let b = self.case.bus[i];
            if b[PD] != 0.0 || b[QD] != 0.0 || extra_loads.contains(&id(&b)) {
                let n = format!("load-1_bus-{}", id(&b));
                for (fld, k, f, w) in [("bus", an, LoadBus, false), ("mw", an, LoadMw, true), ("mvar", an, LoadMvar, true), ("active", bi, LoadActive, true)] {
                    self.add(format!("{n}.{fld}"), k, f, i, w);
                }
            }
        }
        for i in 0..self.case.bus.len() {
            let b = self.case.bus[i];
            if b[BS] != 0.0 || extra_shunts.contains(&id(&b)) {
                let n = format!("shunt-1_bus-{}", id(&b));
                for (fld, k, f, w) in [("bus", an, ShBus, false), ("actual_mvar", an, ShActual, true), ("nominal_mvar", an, ShNominal, false), ("active", bi, ShActive, true)] {
                    self.add(format!("{n}.{fld}"), k, f, i, w);
                }
            }
        }
        let mut cnt: HashMap<i64, usize> = HashMap::new();
        for r in 0..self.case.gen.len() {
            let bus = self.case.gen[r][GEN_BUS] as i64;
            let c = cnt.entry(bus).or_insert(0);
            *c += 1;
            let n = format!("generator-{}_bus-{}", c, bus);
            for (fld, k, f) in [("mw", an, GenMw), ("mvar", an, GenMvar), ("mw_max", an, GenMwMax), ("mw_min", an, GenMwMin),
                ("mvar_max", an, GenMvarMax), ("mvar_min", an, GenMvarMin), ("voltage", an, GenV), ("active", bi, GenActive)] {
                self.add(format!("{n}.{fld}"), k, f, r, true);
            }
        }
        let mut cnt: HashMap<(i64, i64), usize> = HashMap::new();
        for r in 0..self.case.branch.len() {
            let (f, t) = (self.case.branch[r][F_BUS] as i64, self.case.branch[r][T_BUS] as i64);
            let c = cnt.entry((f, t)).or_insert(0);
            *c += 1;
            let n = format!("branch-{}_{}-{}", c, f, t);
            for (fld, k, fl, w) in [("source", an, BrSource, false), ("target", an, BrTarget, false), ("resistance", an, BrR, true),
                ("reactance", an, BrX, true), ("charging", an, BrB, true), ("mva_max", an, BrMva, true), ("turns_ratio", an, BrTap, true),
                ("phase_angle", an, BrShift, true), ("active", bi, BrActive, true)] {
                self.add(format!("{n}.{fld}"), k, fl, r, w);
            }
        }
    }

    pub fn n_tags(&self) -> usize { self.tags.len() }
    pub fn tag_name(&self, i: usize) -> &str { &self.tags[i].name }
    pub fn tag_kind(&self, i: usize) -> Kind { self.tags[i].kind }
    pub fn tag_index(&self, name: &str) -> Option<usize> { self.by_name.get(name).copied() }
    pub fn writable(&self, i: usize) -> bool { self.tags[i].writable }

    fn bus_gen_sum(&self, row: usize, col: usize) -> f64 {
        let id = self.case.bus[row][BUS_I];
        self.case.gen.iter().filter(|g| g[GEN_BUS] == id).map(|g| g[col]).sum()
    }

    pub fn get(&self, i: usize) -> f64 {
        use F::*;
        let t = &self.tags[i];
        let (c, r) = (&self.case, t.row);
        match t.f {
            Alert => self.alert as u8 as f64,
            BusV => c.bus[r][VM], BusA => c.bus[r][VA], BusGenMw => self.bus_gen_sum(r, PG), BusGenMvar => self.bus_gen_sum(r, QG),
            BusLoadMw => c.bus[r][PD], BusLoadMvar => c.bus[r][QD], BusShunt => c.bus[r][BS], BusKv => c.bus[r][BASE_KV],
            BusActive => self.bus_active[r] as u8 as f64, BusSlack => self.bus_slack[r] as u8 as f64,
            LoadBus | ShBus => c.bus[r][BUS_I], LoadMw => c.bus[r][PD], LoadMvar => c.bus[r][QD], LoadActive => self.load_on[r] as u8 as f64,
            ShActual => if self.shunt_on[r] { c.bus[r][BS] } else { 0.0 }, ShNominal => c.bus[r][BS], ShActive => self.shunt_on[r] as u8 as f64,
            GenMw => c.gen[r][PG], GenMvar => c.gen[r][QG], GenMwMax => c.gen[r][PMAX], GenMwMin => c.gen[r][PMIN],
            GenMvarMax => c.gen[r][QMAX], GenMvarMin => c.gen[r][QMIN], GenV => c.gen[r][VG], GenActive => (c.gen[r][GEN_STATUS] > 0.0) as u8 as f64,
            BrSource => c.branch[r][F_BUS], BrTarget => c.branch[r][T_BUS], BrR => c.branch[r][BR_R], BrX => c.branch[r][BR_X],
            BrB => c.branch[r][BR_B], BrMva => c.branch[r][RATE_C], BrTap => c.branch[r][TAP], BrShift => c.branch[r][SHIFT],
            BrActive => (c.branch[r][BR_STATUS] != 0.0) as u8 as f64,
        }
    }

    /// Edit the case. Caller checks `writable`. Binary fields take any nonzero as true.
    pub fn set(&mut self, i: usize, x: f64) {
        use F::*;
        let (f, r) = (self.tags[i].f, self.tags[i].row);
        let c = &mut self.case;
        match f {
            BusV => c.bus[r][VM] = x, BusA => c.bus[r][VA] = x,
            BusLoadMw | LoadMw => c.bus[r][PD] = x, BusLoadMvar | LoadMvar => c.bus[r][QD] = x,
            BusShunt | ShActual => c.bus[r][BS] = x, BusKv => c.bus[r][BASE_KV] = x,
            BusActive => self.bus_active[r] = f2(x), BusSlack => self.bus_slack[r] = f2(x),
            LoadActive => self.load_on[r] = f2(x), ShActive => self.shunt_on[r] = f2(x),
            GenMw => c.gen[r][PG] = x, GenMvar => c.gen[r][QG] = x, GenMwMax => c.gen[r][PMAX] = x, GenMwMin => c.gen[r][PMIN] = x,
            GenMvarMax => c.gen[r][QMAX] = x, GenMvarMin => c.gen[r][QMIN] = x, GenV => c.gen[r][VG] = x, GenActive => c.gen[r][GEN_STATUS] = f2(x) as u8 as f64,
            BrR => c.branch[r][BR_R] = x, BrX => c.branch[r][BR_X] = x, BrB => c.branch[r][BR_B] = x,
            BrMva => for col in RATE_A..=RATE_C { c.branch[r][col] = x }, // pybennu writes all three rating columns
            BrTap => c.branch[r][TAP] = x, BrShift => c.branch[r][SHIFT] = x, BrActive => c.branch[r][BR_STATUS] = f2(x) as u8 as f64,
            Alert | LoadBus | ShBus | ShNominal | BusGenMw | BusGenMvar | BrSource | BrTarget => {}
        }
    }

    /// The case as the solver sees it: bus types from active/slack, de-activated loads and shunts zeroed.
    fn effective(&self) -> Case {
        let mut c = self.case.clone();
        for (i, b) in c.bus.iter_mut().enumerate() {
            b[BUS_TYPE] = if !self.bus_active[i] { NONE } else if self.bus_slack[i] { REF } else { self.pv_type[i] };
            if !self.load_on[i] { b[PD] = 0.0; b[QD] = 0.0; }
            if !self.shunt_on[i] { b[BS] = 0.0; }
        }
        c
    }

    fn apply(&mut self, s: &Case) {
        for (b, r) in self.case.bus.iter_mut().zip(&s.bus) { b[VM] = r[VM]; b[VA] = r[VA]; }
        for (g, r) in self.case.gen.iter_mut().zip(&s.gen) { g[PG] = r[PG]; g[QG] = r[QG]; }
        for (b, r) in self.case.branch.iter_mut().zip(&s.branch) { b[PF] = r[PF]; b[QF] = r[QF]; b[PT] = r[PT]; b[QT] = r[QT]; }
    }

    /// Solve from the stored (warm) voltages. On failure keep the last good results, raise `bus-alert`
    /// (publishing the DC solution instead when `dc_fallback`).
    pub fn solve(&mut self) {
        let eff = self.effective();
        let o = pf::Opts { deenergize: self.islands == Islands::Deenergize, ..Default::default() };
        let r = pf::runpf(&eff, &o);
        let why = match r {
            Ok(s) if s.success => { self.apply(&s.case); self.alert = false; return; }
            Ok(_) => "did not converge".to_string(),
            Err(e) => e,
        };
        if !self.alert { eprintln!("rustbelt: power flow failed ({why}); keeping the last solution, bus-alert raised"); }
        self.alert = true;
        if self.dc_fallback {
            if let Ok(s) = pf::runpf(&eff, &pf::Opts { dc: true, ..o }) {
                self.apply(&s.case);
                for g in self.case.gen.iter_mut() { g[QG] = 0.0; }
            }
        }
    }
}
