//! Engine core: YAML model -> stepper. No clock (spec section 2); the only IO is reading a `power.case` file at load.
//! Inputs, params, states (`ode` via RK4, `toward`), 1-D bodies, outputs, and an optional `power:` block (AC power
//! flow re-solved when an input or a `drive` coupling changes).
//!
//! Every named quantity lives in one `Vec<f64>` of slots; expressions are compiled against it.
//! Outputs with an `expr` own a slot; an output without one republishes the input/state/body field of that name.

use crate::expr::{compile, Node};
use crate::power::{case, Islands, Power};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Analog(f64),
    Binary(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Analog,
    Binary,
}

// ---- YAML schema ---------------------------------------------------------------------------------------------

/// Mapping that keeps file order (publish order = declaration order).
#[derive(Debug)]
struct Ordered<T>(Vec<(String, T)>);
impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Ordered(vec![])
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Ordered<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Ordered<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a mapping")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut v = vec![];
                while let Some(kv) = m.next_entry::<String, T>()? {
                    v.push(kv);
                }
                Ok(Ordered(v))
            }
        }
        d.deserialize_map(V(Default::default()))
    }
}

/// A number or an expression string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Src {
    N(f64),
    S(String),
}
impl Src {
    fn text(&self) -> String {
        match self {
            Src::N(n) => format!("{n:?}"),
            Src::S(s) => s.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Default_ {
    B(bool),
    N(f64),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputDef {
    #[serde(rename = "type")]
    ty: Kind,
    #[serde(rename = "bool", default)]
    is_bool: bool,
    #[serde(default)]
    default: Option<Default_>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Toward {
    target: Src,
    rate: Src,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateDef {
    ode: Option<Src>,
    toward: Option<Toward>,
    init: Src,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Barrier {
    at: f64,
    open: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BodyInit {
    x: f64,
    dir: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BodyDef {
    speed: Src,
    init: BodyInit,
    #[serde(default)]
    barriers: Vec<Barrier>,
    bounce: Option<[f64; 2]>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutDef {
    #[serde(rename = "type")]
    ty: Kind,
    expr: Option<Src>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PowerDef {
    case: Option<String>,
    tags: Option<String>,
    resolve: Option<String>,
    max_interval: Option<f64>,
    #[serde(default)]
    dc_fallback: bool,
    islands: Option<String>,
    #[allow(dead_code)]
    noise: Option<f64>, // pypower-compat mode only (phase 3)
    #[serde(default)]
    drive: Ordered<Src>,
    inline: Option<case::Inline>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    version: u32,
    dt: f64,
    #[serde(default = "one")]
    substeps: usize,
    #[allow(dead_code)]
    publish_rate: Option<f64>,
    #[serde(default)]
    params: Ordered<f64>,
    #[serde(default)]
    inputs: Ordered<InputDef>,
    #[serde(default)]
    states: Ordered<StateDef>,
    #[serde(default)]
    bodies: Ordered<BodyDef>,
    #[serde(default)]
    outputs: Ordered<OutDef>,
    power: Option<PowerDef>,
    publish_order: Option<Vec<String>>,
}
fn one() -> usize {
    1
}

// ---- compiled model ------------------------------------------------------------------------------------------

struct Input {
    slot: usize,
    is_bool: bool,
}
struct Ode {
    slot: usize,
    f: Node,
}
struct TowardState {
    slot: usize,
    target: Node,
    rate: Node,
}
struct Bound {
    slot: usize,
    min: f64,
    max: f64,
}
/// Power block at run time: `slots[i]` is the value slot of power tag `i`.
struct PowerRt {
    pw: Power,
    slots: Vec<usize>,
    drive: Vec<(usize, Node)>,
    max_interval: f64,
    last_solve: f64,
    dirty: bool,
}
impl PowerRt {
    fn sync(&self, v: &mut [f64]) {
        for (i, &s) in self.slots.iter().enumerate() {
            v[s] = self.pw.get(i);
        }
    }
}

struct Body {
    x: usize,
    dir: usize,
    speed: Node,
    barriers: Vec<(f64, Node)>,
    bounce: Option<[f64; 2]>,
}

pub struct Model {
    pub dt: f64,
    pub publish_rate: Option<f64>,
    pub time: f64,
    substeps: usize,
    v: Vec<f64>,
    inputs: HashMap<String, Input>,
    odes: Vec<Ode>,
    towards: Vec<TowardState>,
    bounds: Vec<Bound>,
    bodies: Vec<Body>,
    eval_order: Vec<(usize, Node)>, // output slots with their expression, dependency order
    published: Vec<(String, Kind, usize)>,
    power: Option<PowerRt>,
}

/// Standoff when a body moving down is held at a closed barrier (keeps it in the zone it came from).
const EPS: f64 = 1e-3;

fn move_toward(cur: f64, target: f64, max: f64) -> f64 {
    let d = target - cur;
    if d.abs() <= max {
        target
    } else {
        cur + max * d.signum()
    }
}

#[derive(Default)]
struct Syms {
    map: HashMap<String, usize>,
    n: usize,
}
impl Syms {
    fn add(&mut self, name: &str, what: &str) -> Result<usize, String> {
        if self.map.insert(name.to_string(), self.n).is_some() {
            return Err(format!("{what} `{name}` is already defined"));
        }
        self.n += 1;
        Ok(self.n - 1)
    }
}

impl Model {
    pub fn load(yaml: &str) -> Result<Model, String> {
        Model::load_in(yaml, None)
    }

    /// `dir`: where a relative `power.case` path is resolved (the model file's directory).
    pub fn load_in(yaml: &str, dir: Option<&Path>) -> Result<Model, String> {
        let doc: Doc = serde_saphyr::from_str(yaml).map_err(|e| format!("model: {e}"))?;
        if doc.version != 1 {
            return Err(format!("model: unsupported version {}", doc.version));
        }
        if !(doc.dt > 0.0) || doc.substeps == 0 {
            return Err("model: dt must be > 0 and substeps >= 1".into());
        }

        // slots: params, inputs, states, body fields, then expression outputs
        let mut syms = Syms::default();
        let mut init: Vec<(usize, f64)> = vec![];
        for (n, val) in &doc.params.0 {
            let s = syms.add(n, "param")?;
            init.push((s, *val));
        }
        let mut inputs = HashMap::new();
        for (n, d) in &doc.inputs.0 {
            let slot = syms.add(n, "input")?;
            let dv = match d.default {
                Some(Default_::B(b)) => b as u8 as f64,
                Some(Default_::N(x)) => x,
                None => 0.0,
            };
            init.push((
                slot,
                if d.is_bool || d.ty == Kind::Binary {
                    (dv != 0.0) as u8 as f64
                } else {
                    dv
                },
            ));
            inputs.insert(
                n.clone(),
                Input {
                    slot,
                    is_bool: d.is_bool || d.ty == Kind::Binary,
                },
            );
        }
        let state_slots: Vec<usize> = doc
            .states
            .0
            .iter()
            .map(|(n, _)| syms.add(n, "state"))
            .collect::<Result<_, _>>()?;
        let mut body_slots = vec![];
        for (n, _) in &doc.bodies.0 {
            body_slots.push((
                syms.add(&format!("{n}.x"), "body field")?,
                syms.add(&format!("{n}.dir"), "body field")?,
            ));
        }
        let out_slots: Vec<Option<usize>> = doc
            .outputs
            .0
            .iter()
            .map(|(n, o)| {
                if o.expr.is_some() {
                    syms.add(n, "output").map(Some)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<_, _>>()?;

        // power: build the case + tag vocabulary; every power tag gets a slot so expressions can read it
        let mut pw = None;
        let mut pslots = vec![];
        if let Some(pd) = &doc.power {
            if pd.tags.as_deref().is_some_and(|t| t != "pypower") {
                return Err("power.tags: only `pypower` is supported".into());
            }
            if pd.resolve.as_deref().is_some_and(|t| t != "on_change") {
                return Err("power.resolve: only `on_change` is supported".into());
            }
            let islands = match pd.islands.as_deref() {
                None | Some("deenergize") => Islands::Deenergize,
                Some("fail") => Islands::Fail,
                Some(o) => {
                    return Err(format!("power.islands: `{o}` (expected fail | deenergize)"))
                }
            };
            let cs = match (&pd.case, &pd.inline) {
                (Some(f), None) => {
                    case::load_file(&dir.map(|d| d.join(f)).unwrap_or_else(|| f.into()))?
                }
                (None, Some(i)) => i.to_case().map_err(|e| format!("power.inline: {e}"))?,
                _ => return Err("power: exactly one of `case` or `inline` is required".into()),
            };
            let (mut loads, mut shunts) = (vec![], vec![]);
            for (k, _) in &pd.drive.0 {
                let dev = k.rsplit_once('.').map(|x| x.0).unwrap_or(k);
                let num = |p: &str| dev.strip_prefix(p).and_then(|n| n.parse::<i64>().ok());
                if let Some(n) = num("load-1_bus-") {
                    loads.push(n);
                } else if let Some(n) = num("shunt-1_bus-") {
                    shunts.push(n);
                }
            }
            let p = Power::new(cs, islands, pd.dc_fallback, &loads, &shunts)
                .map_err(|e| format!("power: {e}"))?;
            for i in 0..p.n_tags() {
                pslots.push(syms.add(p.tag_name(i), "power tag")?);
            }
            pw = Some(p);
        }

        let mut v = vec![0.0; syms.n];
        for (s, x) in init {
            v[s] = x;
        }
        let c = |src: &str, ctx: &str| {
            compile(src, &syms.map).map_err(|e| format!("{ctx}: `{src}` {e}"))
        };

        let (mut odes, mut towards, mut bounds) = (vec![], vec![], vec![]);
        for ((n, d), &slot) in doc.states.0.iter().zip(&state_slots) {
            v[slot] = c(&d.init.text(), &format!("state {n} init"))?.eval(&v); // may use params
            match (&d.ode, &d.toward) {
                (Some(f), None) => odes.push(Ode {
                    slot,
                    f: c(&f.text(), &format!("state {n} ode"))?,
                }),
                (None, Some(t)) => towards.push(TowardState {
                    slot,
                    target: c(&t.target.text(), &format!("state {n} toward.target"))?,
                    rate: c(&t.rate.text(), &format!("state {n} toward.rate"))?,
                }),
                _ => {
                    return Err(format!(
                        "state {n}: exactly one of `ode` or `toward` is required"
                    ))
                }
            }
            if d.min.is_some() || d.max.is_some() {
                bounds.push(Bound {
                    slot,
                    min: d.min.unwrap_or(f64::NEG_INFINITY),
                    max: d.max.unwrap_or(f64::INFINITY),
                });
            }
        }

        let mut bodies = vec![];
        for ((n, d), &(x, dir)) in doc.bodies.0.iter().zip(&body_slots) {
            v[x] = d.init.x;
            v[dir] = d.init.dir;
            let barriers = d
                .barriers
                .iter()
                .map(|b| Ok((b.at, c(&b.open, &format!("body {n} barrier"))?)))
                .collect::<Result<_, String>>()?;
            bodies.push(Body {
                x,
                dir,
                speed: c(&d.speed.text(), &format!("body {n} speed"))?,
                barriers,
                bounce: d.bounce,
            });
        }

        // outputs: compile, then order by dependency among expression outputs
        let mut exprs: Vec<(usize, Node)> = vec![];
        for ((n, o), slot) in doc.outputs.0.iter().zip(&out_slots) {
            if let (Some(e), Some(s)) = (&o.expr, slot) {
                exprs.push((*s, c(&e.text(), &format!("output {n}"))?));
            }
        }
        let eval_order = order_outputs(exprs, &syms)?;

        // power drive: `target = expr`, evaluated in declaration order
        let mut prt = None;
        if let (Some(p), Some(pd)) = (pw, &doc.power) {
            let mut drive = vec![];
            for (k, e) in &pd.drive.0 {
                let ti = p
                    .tag_index(k)
                    .ok_or(format!("power.drive: `{k}` is not a power tag"))?;
                if !p.writable(ti) {
                    return Err(format!("power.drive: `{k}` is read-only"));
                }
                drive.push((ti, c(&e.text(), &format!("power.drive {k}"))?));
            }
            let rt = PowerRt {
                pw: p,
                slots: pslots,
                drive,
                max_interval: pd.max_interval.unwrap_or(1.0),
                last_solve: 0.0,
                dirty: true,
            };
            rt.sync(&mut v);
            prt = Some(rt);
        }

        let mut published = vec![];
        for ((n, o), slot) in doc.outputs.0.iter().zip(&out_slots) {
            let s = match slot {
                Some(s) => *s,
                None => *syms.map.get(n).ok_or(format!(
                    "output {n}: no expr and no input/state/param of that name"
                ))?,
            };
            published.push((n.clone(), o.ty, s));
        }
        if let Some(p) = &prt {
            for (i, &s) in p.slots.iter().enumerate() {
                published.push((p.pw.tag_name(i).to_string(), p.pw.tag_kind(i), s));
            }
        }
        if let Some(order) = &doc.publish_order {
            published = order
                .iter()
                .map(|t| {
                    published
                        .iter()
                        .find(|p| &p.0 == t)
                        .cloned()
                        .ok_or(format!("publish_order: unknown output `{t}`"))
                })
                .collect::<Result<_, _>>()?;
        }

        let mut m = Model {
            dt: doc.dt,
            publish_rate: doc.publish_rate,
            time: 0.0,
            substeps: doc.substeps,
            v,
            inputs,
            odes,
            towards,
            bounds,
            bodies,
            eval_order,
            published,
            power: prt,
        };
        m.eval();
        m.power_update();
        m.eval();
        Ok(m)
    }

    /// Published tags in publish order with their wire type.
    pub fn tags(&self) -> impl Iterator<Item = (&str, Kind)> {
        self.published.iter().map(|(n, k, _)| (n.as_str(), *k))
    }

    fn eval(&mut self) {
        for (slot, f) in &self.eval_order {
            self.v[*slot] = f.eval(&self.v);
        }
    }

    fn wire(&self, k: Kind, x: f64) -> Value {
        if k == Kind::Binary {
            Value::Binary(x != 0.0)
        } else {
            Value::Analog(x)
        }
    }

    /// Latch writes into inputs. Returns the tags that are not inputs (they are ignored). Boolean inputs latch 0/1.
    pub fn apply_writes(&mut self, writes: &[(String, Value)]) -> Vec<String> {
        let mut rejected = vec![];
        for (tag, val) in writes {
            match self.inputs.get(tag) {
                Some(i) => {
                    let x = match *val {
                        Value::Analog(x) => x,
                        Value::Binary(b) => b as u8 as f64,
                    };
                    self.v[i.slot] = if i.is_bool {
                        (x != 0.0) as u8 as f64
                    } else {
                        x
                    };
                }
                None => {
                    let x = match *val {
                        Value::Analog(x) => x,
                        Value::Binary(b) => b as u8 as f64,
                    };
                    match self.power.as_mut() {
                        Some(p) => match p.pw.tag_index(tag).filter(|&i| p.pw.writable(i)) {
                            Some(i) => {
                                p.pw.set(i, x);
                                p.dirty = true;
                                p.sync(&mut self.v);
                            }
                            None => rejected.push(tag.clone()),
                        },
                        None => rejected.push(tag.clone()),
                    }
                }
            }
        }
        self.eval();
        rejected
    }

    /// True when a write to `tag` would be accepted (a model input, or a writable power tag).
    pub fn writable(&self, tag: &str) -> bool {
        self.inputs.contains_key(tag)
            || self
                .power
                .as_ref()
                .is_some_and(|p| p.pw.tag_index(tag).is_some_and(|i| p.pw.writable(i)))
    }

    pub fn get(&self, tag: &str) -> Option<Value> {
        self.published
            .iter()
            .find(|p| p.0 == tag)
            .map(|(_, k, s)| self.wire(*k, self.v[*s]))
    }

    /// All published tags at the current instant.
    pub fn snapshot(&self) -> Vec<(&str, Value)> {
        self.published
            .iter()
            .map(|(n, k, s)| (n.as_str(), self.wire(*k, self.v[*s])))
            .collect()
    }

    /// ponytail: drive targets are compared and re-set every step (exact compare, 1e-9 abs), so an expression that
    /// changes continuously re-solves every step; add a drive tolerance if a big case is driven by a ramp.
    fn power_update(&mut self) {
        let Some(p) = self.power.as_mut() else { return };
        for (ti, f) in &p.drive {
            let x = f.eval(&self.v);
            if (p.pw.get(*ti) - x).abs() > 1e-9 {
                p.pw.set(*ti, x);
                p.dirty = true;
                p.sync(&mut self.v); // later drive entries may read this one (`load-1_bus-3`.mw)
            }
        }
        if p.dirty || self.time - p.last_solve >= p.max_interval - 1e-9 {
            p.pw.solve();
            p.dirty = false;
            p.last_solve = self.time;
            p.sync(&mut self.v);
        }
    }

    pub fn step(&mut self, dt: f64) {
        self.eval();
        self.power_update();
        // toward states and bodies read the instant-t values; compute first, commit after the ODE advance
        let tw: Vec<f64> = self
            .towards
            .iter()
            .map(|t| {
                let cur = self.v[t.slot];
                move_toward(cur, t.target.eval(&self.v), t.rate.eval(&self.v) * dt)
            })
            .collect();
        let bd: Vec<(f64, f64)> = self.bodies.iter().map(|b| self.body_step(b, dt)).collect();

        let h = dt / self.substeps as f64;
        for _ in 0..self.substeps {
            self.rk4(h);
        }

        for (t, x) in self.towards.iter().zip(tw) {
            self.v[t.slot] = x;
        }
        for (b, (x, d)) in self.bodies.iter().zip(bd) {
            self.v[b.x] = x;
            self.v[b.dir] = d;
        }
        for b in &self.bounds {
            self.v[b.slot] = self.v[b.slot].clamp(b.min, b.max);
        }
        self.time += dt;
        self.eval();
    }

    fn body_step(&self, b: &Body, dt: f64) -> (f64, f64) {
        let (x, mut dir) = (self.v[b.x], self.v[b.dir]);
        let mut nx = x + dir * b.speed.eval(&self.v) * dt;
        for (at, open) in &b.barriers {
            if open.eval(&self.v) != 0.0 {
                continue;
            }
            if dir > 0.0 && x <= *at && nx > *at {
                nx = *at;
            } else if dir < 0.0 && x > *at && nx <= *at {
                nx = *at + EPS;
            }
        }
        if let Some([lo, hi]) = b.bounce {
            if nx >= hi {
                nx = hi;
                dir = -1.0;
            } else if nx <= lo {
                nx = lo;
                dir = 1.0;
            }
        }
        (nx, dir)
    }

    // ponytail: toward states/bodies are frozen at instant t inside RK stages; fine for dt <= 0.1 s models.
    fn rk4(&mut self, h: f64) {
        if self.odes.is_empty() {
            return;
        }
        let y0: Vec<f64> = self.odes.iter().map(|o| self.v[o.slot]).collect();
        let k = |m: &mut Model, y: &[f64]| -> Vec<f64> {
            for (o, yi) in m.odes.iter().zip(y) {
                m.v[o.slot] = *yi;
            }
            m.eval();
            m.odes.iter().map(|o| o.f.eval(&m.v)).collect()
        };
        let at = |y: &[f64], d: &[f64], s: f64| -> Vec<f64> {
            y.iter().zip(d).map(|(a, b)| a + s * b).collect()
        };
        let k1 = k(self, &y0);
        let k2 = k(self, &at(&y0, &k1, h / 2.0));
        let k3 = k(self, &at(&y0, &k2, h / 2.0));
        let k4 = k(self, &at(&y0, &k3, h));
        for (i, o) in self.odes.iter().enumerate() {
            self.v[o.slot] = y0[i] + h / 6.0 * (k1[i] + 2.0 * k2[i] + 2.0 * k3[i] + k4[i]);
        }
    }
}

/// Topological order of expression outputs; a cycle is a load error.
fn order_outputs(exprs: Vec<(usize, Node)>, syms: &Syms) -> Result<Vec<(usize, Node)>, String> {
    let idx: HashMap<usize, usize> = exprs
        .iter()
        .enumerate()
        .map(|(i, (s, _))| (*s, i))
        .collect();
    let name = |slot: usize| {
        syms.map
            .iter()
            .find(|(_, s)| **s == slot)
            .map(|(n, _)| n.clone())
            .unwrap_or_default()
    };
    let (mut state, mut order) = (vec![0u8; exprs.len()], vec![]); // 0 new, 1 visiting, 2 done
    fn visit(
        i: usize,
        exprs: &[(usize, Node)],
        idx: &HashMap<usize, usize>,
        state: &mut [u8],
        order: &mut Vec<usize>,
        name: &dyn Fn(usize) -> String,
    ) -> Result<(), String> {
        match state[i] {
            2 => return Ok(()),
            1 => return Err(format!("output cycle through `{}`", name(exprs[i].0))),
            _ => {}
        }
        state[i] = 1;
        let mut deps = vec![];
        exprs[i].1.vars(&mut deps);
        for d in deps {
            if let Some(&j) = idx.get(&d) {
                visit(j, exprs, idx, state, order, name)?;
            }
        }
        state[i] = 2;
        order.push(i);
        Ok(())
    }
    for i in 0..exprs.len() {
        visit(i, &exprs, &idx, &mut state, &mut order, &name)?;
    }
    let mut slots: Vec<Option<(usize, Node)>> = exprs.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .map(|i| slots[i].take().unwrap())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_models() {
        let e = |y: &str| Model::load(y).err().expect("should fail");
        assert!(
            e("version: 1\ndt: 0.1\noutputs:\n  a: {type: analog, expr: nope}\n")
                .contains("`nope`")
        );
        assert!(e("version: 1\ndt: 0.1\noutputs:\n  a: {type: analog, expr: b}\n  b: {type: analog, expr: a}\n").contains("cycle"));
        assert!(e("version: 1\ndt: 0.1\nstates:\n  s: {init: 0}\n").contains("exactly one"));
        assert!(e("version: 2\ndt: 0.1\n").contains("version"));
        assert!(e("version: 1\ndt: 0.1\noutputs:\n  a: {type: analog}\n").contains("no expr"));
        assert!(e("version: 1\ndt: 0.1\nbogus: 1\n").contains("model:"));
    }

    #[test]
    fn writes_and_dependency_order() {
        // `b` is declared before `a` but depends on it; boolean input latches 0/1; binary output is typed
        let mut m = Model::load("version: 1\ndt: 0.1\ninputs:\n  i: {type: analog, bool: true}\n  r: {type: analog}\noutputs:\n  b: {type: analog, expr: 'a + 1'}\n  a: {type: analog, expr: 'i * 10 + r'}\n  f: {type: binary, expr: 'i'}\n").unwrap();
        let rej = m.apply_writes(&[
            ("i".into(), Value::Analog(7.0)),
            ("r".into(), Value::Analog(0.5)),
            ("zz".into(), Value::Analog(1.0)),
        ]);
        assert_eq!(rej, vec!["zz"]);
        assert_eq!(m.get("a"), Some(Value::Analog(10.5)));
        assert_eq!(m.get("b"), Some(Value::Analog(11.5)));
        assert_eq!(m.get("f"), Some(Value::Binary(true)));
    }
}
