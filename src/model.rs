//! Engine core: YAML model -> stepper. No clock (spec section 2); the only IO is reading a `power.case` file at load.
//! Inputs, params, states (`ode` via RK4, `toward`), 1-D bodies, outputs, and an optional `power:` block (AC power
//! flow re-solved when an input or a `drive` coupling changes).
//!
//! Every named quantity lives in one `Vec<f64>` of slots; expressions are compiled against it.
//! Outputs with an `expr` own a slot; an output without one republishes the input/state/body field of that name.

use crate::expr::{compile, Node};
use crate::hint::did_you_mean;
use crate::power::{case, Islands, Power};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

mod explain;
pub use explain::explain;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Analog(f64),
    Binary(bool),
}

/// What a writable tag accepts: its wire type and, for analog inputs, the `min`/`max` the model declares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WriteSpec {
    pub kind: Kind,
    pub min: f64,
    pub max: f64,
}

impl WriteSpec {
    /// Binary tags take true/false/0/1, analog tags a number within the limits; the error names the limit.
    pub fn check(&self, tag: &str, val: Value) -> Result<(), String> {
        match (self.kind, val) {
            (Kind::Binary, Value::Binary(_)) => Ok(()),
            (Kind::Binary, Value::Analog(x)) if x == 0.0 || x == 1.0 => Ok(()),
            (Kind::Binary, Value::Analog(x)) => Err(format!(
                "Tag '{tag}' is binary: value {x} is not true/false/0/1"
            )),
            (Kind::Analog, Value::Binary(_)) => Err(format!(
                "Tag '{tag}' is analog: expected a number, not true/false"
            )),
            (Kind::Analog, Value::Analog(x)) if x < self.min => Err(format!(
                "Tag '{tag}' value {x} is below its min {}",
                self.min
            )),
            (Kind::Analog, Value::Analog(x)) if x > self.max => Err(format!(
                "Tag '{tag}' value {x} is above its max {}",
                self.max
            )),
            _ => Ok(()),
        }
    }
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
    min: Option<f64>,
    max: Option<f64>,
    units: Option<String>,
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
    units: Option<String>,
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
    units: Option<String>,
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
    /// Hold generators to their Qmin/Qmax (PYPOWER ENFORCE_Q_LIMS); off by default, as sceptre runs PyPower.
    #[serde(default)]
    enforce_q_lims: bool,
    islands: Option<String>,
    /// Generator ramp: `rate` MW/s for every generator, `gens` (device name -> MW/s) overriding it.
    ramp: Option<RampDef>,
    #[allow(dead_code)]
    noise: Option<f64>, // pypower-compat mode only (phase 3)
    #[serde(default)]
    drive: Ordered<Src>,
    inline: Option<case::Inline>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RampDef {
    rate: Option<f64>,
    #[serde(default)]
    gens: Ordered<f64>,
}

/// One publish endpoint and the tags it carries: listed inline, read from a bennu `config.xml`, or both.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceSpec {
    pub endpoint: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub tags_from: Option<String>,
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
    /// Per-RTU publish endpoints, each carrying a slice of the tags.
    #[serde(default)]
    publish_slices: Vec<SliceSpec>,
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
    min: f64,
    max: f64,
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
    pub publish_slices: Vec<SliceSpec>,
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

/// Where a body going from `x` to `nx` ends up against a closed barrier at `at`.
fn barrier_limit(at: f64, (x, nx): (f64, f64), dir: f64) -> f64 {
    if dir > 0.0 && x <= at && nx > at {
        at
    } else if dir < 0.0 && x > at && nx <= at {
        at + EPS
    } else {
        nx
    }
}

/// Reflect at the ends of `[lo, hi]`: (position, direction).
fn bounce_at(lo: f64, hi: f64, nx: f64, dir: f64) -> (f64, f64) {
    if nx >= hi {
        (hi, -1.0)
    } else if nx <= lo {
        (lo, 1.0)
    } else {
        (nx, dir)
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

fn parse_doc(yaml: &str) -> Result<Doc, String> {
    let doc: Doc = serde_saphyr::from_str(yaml).map_err(|e| {
        let e = e.to_string();
        format!("model: {e}{}", unknown_field_hint(&e))
    })?;
    if doc.version != 1 {
        return Err(format!("model: unsupported version {}", doc.version));
    }
    if (doc.dt.is_nan() || doc.dt <= 0.0) || doc.substeps == 0 {
        return Err("model: dt must be > 0 and substeps >= 1".into());
    }
    Ok(doc)
}

/// serde's `unknown field `x`, expected one of a, b, c` -> the "did you mean" for `x`.
fn unknown_field_hint(e: &str) -> String {
    let Some((name, rest)) = e
        .split_once("unknown field `")
        .and_then(|(_, r)| r.split_once('`'))
    else {
        return String::new();
    };
    let list = rest
        .split_once("expected one of ")
        .map_or("", |(_, l)| l.lines().next().unwrap_or(""));
    did_you_mean(name, list.split(", "))
}

/// Compile `src` against the symbol table; `ctx` says where the expression came from.
fn comp(syms: &Syms, src: &str, ctx: &str) -> Result<Node, String> {
    compile(src, &syms.map).map_err(|e| format!("{ctx}: `{src}` {e}"))
}

fn initial_values(n: usize, init: &[(usize, f64)]) -> Vec<f64> {
    let mut v = vec![0.0; n];
    for &(s, x) in init {
        v[s] = x;
    }
    v
}

fn bit(b: bool) -> f64 {
    b as u8 as f64
}

/// Slots handed out for the declarations, in the order params, inputs, states, body fields, expression outputs.
struct Decl {
    init: Vec<(usize, f64)>,
    inputs: HashMap<String, Input>,
    states: Vec<usize>,
    bodies: Vec<(usize, usize)>,
    outs: Vec<Option<usize>>,
}

fn declare(doc: &Doc, syms: &mut Syms) -> Result<Decl, String> {
    let mut init = vec![];
    for (n, val) in &doc.params.0 {
        init.push((syms.add(n, "param")?, *val));
    }
    let inputs = declare_inputs(doc, syms, &mut init)?;
    let states = (doc.states.0.iter())
        .map(|(n, _)| syms.add(n, "state"))
        .collect::<Result<_, _>>()?;
    let bodies = declare_bodies(doc, syms)?;
    let outs = declare_outputs(doc, syms)?;
    Ok(Decl {
        init,
        inputs,
        states,
        bodies,
        outs,
    })
}

fn declare_inputs(
    doc: &Doc,
    syms: &mut Syms,
    init: &mut Vec<(usize, f64)>,
) -> Result<HashMap<String, Input>, String> {
    let mut inputs = HashMap::new();
    for (n, d) in &doc.inputs.0 {
        let slot = syms.add(n, "input")?;
        let is_bool = d.is_bool || d.ty == Kind::Binary;
        let dv = match d.default {
            Some(Default_::B(b)) => bit(b),
            Some(Default_::N(x)) => x,
            None => 0.0,
        };
        init.push((slot, if is_bool { bit(dv != 0.0) } else { dv }));
        inputs.insert(
            n.clone(),
            Input {
                slot,
                is_bool,
                min: d.min.unwrap_or(f64::NEG_INFINITY),
                max: d.max.unwrap_or(f64::INFINITY),
            },
        );
    }
    Ok(inputs)
}

fn declare_bodies(doc: &Doc, syms: &mut Syms) -> Result<Vec<(usize, usize)>, String> {
    let mut slots = vec![];
    for (n, _) in &doc.bodies.0 {
        slots.push((
            syms.add(&format!("{n}.x"), "body field")?,
            syms.add(&format!("{n}.dir"), "body field")?,
        ));
    }
    Ok(slots)
}

/// Only outputs with an `expr` own a slot; the rest republish the input/state/param of that name.
fn declare_outputs(doc: &Doc, syms: &mut Syms) -> Result<Vec<Option<usize>>, String> {
    doc.outputs
        .0
        .iter()
        .map(|(n, o)| o.expr.as_ref().map(|_| syms.add(n, "output")).transpose())
        .collect()
}

fn check_power_options(pd: &PowerDef) -> Result<(), String> {
    if pd.tags.as_deref().is_some_and(|t| t != "pypower") {
        return Err("power.tags: only `pypower` is supported".into());
    }
    if pd.resolve.as_deref().is_some_and(|t| t != "on_change") {
        return Err("power.resolve: only `on_change` is supported".into());
    }
    Ok(())
}

fn power_islands(pd: &PowerDef) -> Result<Islands, String> {
    match pd.islands.as_deref() {
        None | Some("deenergize") => Ok(Islands::Deenergize),
        Some("fail") => Ok(Islands::Fail),
        Some(o) => Err(format!("power.islands: `{o}` (expected fail | deenergize)")),
    }
}

fn power_case(pd: &PowerDef, dir: Option<&Path>) -> Result<case::Case, String> {
    match (&pd.case, &pd.inline) {
        (Some(f), None) => case::load_file(&dir.map(|d| d.join(f)).unwrap_or_else(|| f.into())),
        (None, Some(i)) => i.to_case().map_err(|e| format!("power.inline: {e}")),
        _ => Err("power: exactly one of `case` or `inline` is required".into()),
    }
}

/// Bus numbers of the `load-1_bus-N` / `shunt-1_bus-N` devices named by `power.drive` targets.
fn drive_devices(pd: &PowerDef) -> (Vec<i64>, Vec<i64>) {
    let (mut loads, mut shunts) = (vec![], vec![]);
    for (k, _) in &pd.drive.0 {
        let dev = k.rsplit_once('.').map_or(k.as_str(), |x| x.0);
        let num = |p: &str| dev.strip_prefix(p).and_then(|n| n.parse::<i64>().ok());
        if let Some(n) = num("load-1_bus-") {
            loads.push(n);
        } else if let Some(n) = num("shunt-1_bus-") {
            shunts.push(n);
        }
    }
    (loads, shunts)
}

/// The case and tag vocabulary; every power tag gets a slot so expressions can read it.
fn build_power(
    doc: &Doc,
    dir: Option<&Path>,
    syms: &mut Syms,
) -> Result<Option<(Power, Vec<usize>)>, String> {
    let Some(pd) = &doc.power else {
        return Ok(None);
    };
    check_power_options(pd)?;
    let islands = power_islands(pd)?;
    let cs = power_case(pd, dir)?;
    let (loads, shunts) = drive_devices(pd);
    let mut p = Power::new_with(
        cs,
        islands,
        (pd.dc_fallback, pd.enforce_q_lims),
        &loads,
        &shunts,
    )
    .map_err(|e| format!("power: {e}"))?;
    if let Some(r) = &pd.ramp {
        p.enable_ramp(r.rate, &r.gens.0)
            .map_err(|e| format!("power: {e}"))?;
    }
    let slots = (0..p.n_tags())
        .map(|i| syms.add(p.tag_name(i), "power tag"))
        .collect::<Result<_, _>>()?;
    Ok(Some((p, slots)))
}

/// A state advances either by an ODE or by moving toward a target.
enum Dynamics {
    Ode(Ode),
    Toward(TowardState),
}

fn compile_state(
    (n, d): &(String, StateDef),
    slot: usize,
    syms: &Syms,
    v: &mut [f64],
) -> Result<Dynamics, String> {
    v[slot] = comp(syms, &d.init.text(), &format!("state {n} init"))?.eval(v); // may use params
    match (&d.ode, &d.toward) {
        (Some(f), None) => Ok(Dynamics::Ode(Ode {
            slot,
            f: comp(syms, &f.text(), &format!("state {n} ode"))?,
        })),
        (None, Some(t)) => Ok(Dynamics::Toward(TowardState {
            slot,
            target: comp(syms, &t.target.text(), &format!("state {n} toward.target"))?,
            rate: comp(syms, &t.rate.text(), &format!("state {n} toward.rate"))?,
        })),
        _ => Err(format!(
            "state {n}: exactly one of `ode` or `toward` is required"
        )),
    }
}

fn state_bound(d: &StateDef, slot: usize) -> Option<Bound> {
    (d.min.is_some() || d.max.is_some()).then(|| Bound {
        slot,
        min: d.min.unwrap_or(f64::NEG_INFINITY),
        max: d.max.unwrap_or(f64::INFINITY),
    })
}

type StateParts = (Vec<Ode>, Vec<TowardState>, Vec<Bound>);

fn compile_states(
    doc: &Doc,
    slots: &[usize],
    syms: &Syms,
    v: &mut [f64],
) -> Result<StateParts, String> {
    let (mut odes, mut towards, mut bounds) = (vec![], vec![], vec![]);
    for (st, &slot) in doc.states.0.iter().zip(slots) {
        match compile_state(st, slot, syms, v)? {
            Dynamics::Ode(o) => odes.push(o),
            Dynamics::Toward(t) => towards.push(t),
        }
        bounds.extend(state_bound(&st.1, slot));
    }
    Ok((odes, towards, bounds))
}

fn compile_bodies(
    doc: &Doc,
    slots: &[(usize, usize)],
    syms: &Syms,
    v: &mut [f64],
) -> Result<Vec<Body>, String> {
    let mut bodies = vec![];
    for ((n, d), &(x, dir)) in doc.bodies.0.iter().zip(slots) {
        v[x] = d.init.x;
        v[dir] = d.init.dir;
        let barriers = (d.barriers.iter())
            .map(|b| Ok((b.at, comp(syms, &b.open, &format!("body {n} barrier"))?)))
            .collect::<Result<_, String>>()?;
        bodies.push(Body {
            x,
            dir,
            speed: comp(syms, &d.speed.text(), &format!("body {n} speed"))?,
            barriers,
            bounce: d.bounce,
        });
    }
    Ok(bodies)
}

/// Expression outputs, compiled and put in dependency order.
fn compile_outputs(
    doc: &Doc,
    slots: &[Option<usize>],
    syms: &Syms,
) -> Result<Vec<(usize, Node)>, String> {
    let mut exprs: Vec<(usize, Node)> = vec![];
    for ((n, o), slot) in doc.outputs.0.iter().zip(slots) {
        if let (Some(e), Some(s)) = (&o.expr, slot) {
            exprs.push((*s, comp(syms, &e.text(), &format!("output {n}"))?));
        }
    }
    order_outputs(exprs, syms)
}

/// Power at run time: `target = expr` drive entries, evaluated in declaration order.
fn build_runtime(
    power: Option<(Power, Vec<usize>)>,
    doc: &Doc,
    syms: &Syms,
    v: &mut [f64],
) -> Result<Option<PowerRt>, String> {
    let (Some((pw, slots)), Some(pd)) = (power, &doc.power) else {
        return Ok(None);
    };
    let mut drive = vec![];
    for (k, e) in &pd.drive.0 {
        let ti = (pw.tag_index(k)).ok_or(format!("power.drive: `{k}` is not a power tag"))?;
        if !pw.writable(ti) {
            return Err(format!("power.drive: `{k}` is read-only"));
        }
        drive.push((ti, comp(syms, &e.text(), &format!("power.drive {k}"))?));
    }
    let rt = PowerRt {
        pw,
        slots,
        drive,
        max_interval: pd.max_interval.unwrap_or(1.0),
        last_solve: 0.0,
        dirty: true,
    };
    rt.sync(v);
    Ok(Some(rt))
}

/// Everything a client can read: outputs (declaration order), then power tags, optionally reordered.
fn build_published(
    doc: &Doc,
    out_slots: &[Option<usize>],
    syms: &Syms,
    power: Option<&PowerRt>,
) -> Result<Vec<(String, Kind, usize)>, String> {
    let mut published = vec![];
    for ((n, o), slot) in doc.outputs.0.iter().zip(out_slots) {
        let s = match slot {
            Some(s) => *s,
            None => *syms.map.get(n).ok_or(format!(
                "output {n}: no expr and no input/state/param of that name"
            ))?,
        };
        published.push((n.clone(), o.ty, s));
    }
    if let Some(p) = power {
        for (i, &s) in p.slots.iter().enumerate() {
            published.push((p.pw.tag_name(i).to_string(), p.pw.tag_kind(i), s));
        }
    }
    match &doc.publish_order {
        Some(order) => reorder(published, order),
        None => Ok(published),
    }
}

fn reorder(
    published: Vec<(String, Kind, usize)>,
    order: &[String],
) -> Result<Vec<(String, Kind, usize)>, String> {
    order
        .iter()
        .map(|t| {
            (published.iter().find(|p| &p.0 == t).cloned()).ok_or_else(|| {
                let hint = did_you_mean(t, published.iter().map(|p| p.0.as_str()));
                format!("publish_order: unknown output `{t}`{hint}")
            })
        })
        .collect()
}

impl Model {
    pub fn load(yaml: &str) -> Result<Model, String> {
        Model::load_in(yaml, None)
    }

    /// `dir`: where a relative `power.case` path is resolved (the model file's directory).
    pub fn load_in(yaml: &str, dir: Option<&Path>) -> Result<Model, String> {
        let doc = parse_doc(yaml)?;
        let mut syms = Syms::default();
        let decl = declare(&doc, &mut syms)?;
        let power = build_power(&doc, dir, &mut syms)?;
        let mut m = Model::compile(&doc, decl, power, &syms)?;
        m.eval();
        m.power_update();
        m.eval();
        Ok(m)
    }

    /// Compile every expression against the slot table; the model starts at t = 0 with initial values in place.
    fn compile(
        doc: &Doc,
        decl: Decl,
        power: Option<(Power, Vec<usize>)>,
        syms: &Syms,
    ) -> Result<Model, String> {
        let mut v = initial_values(syms.n, &decl.init);
        let (odes, towards, bounds) = compile_states(doc, &decl.states, syms, &mut v)?;
        let bodies = compile_bodies(doc, &decl.bodies, syms, &mut v)?;
        let eval_order = compile_outputs(doc, &decl.outs, syms)?;
        let power = build_runtime(power, doc, syms, &mut v)?;
        let published = build_published(doc, &decl.outs, syms, power.as_ref())?;
        Ok(Model {
            dt: doc.dt,
            publish_rate: doc.publish_rate,
            publish_slices: doc.publish_slices.clone(),
            time: 0.0,
            substeps: doc.substeps,
            v,
            inputs: decl.inputs,
            odes,
            towards,
            bounds,
            bodies,
            eval_order,
            published,
            power,
        })
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

    /// Latch one write: a model input, else a writable power tag. False when the tag is neither.
    fn apply_write(&mut self, tag: &str, val: Value) -> bool {
        let x = match val {
            Value::Analog(x) => x,
            Value::Binary(b) => bit(b),
        };
        if let Some(i) = self.inputs.get(tag) {
            self.v[i.slot] = if i.is_bool { bit(x != 0.0) } else { x };
            return true;
        }
        let Some(p) = self.power.as_mut() else {
            return false;
        };
        let Some(i) = p.pw.tag_index(tag).filter(|&i| p.pw.writable(i)) else {
            return false;
        };
        p.pw.set(i, x);
        p.dirty = true;
        p.sync(&mut self.v);
        true
    }

    /// Latch writes into inputs. Returns the tags that are not inputs (they are ignored). Boolean inputs latch 0/1.
    pub fn apply_writes(&mut self, writes: &[(String, Value)]) -> Vec<String> {
        let mut rejected = vec![];
        for (tag, val) in writes {
            if !self.apply_write(tag, *val) {
                rejected.push(tag.clone());
            }
        }
        self.eval();
        rejected
    }

    /// The wire type and limits a write to `tag` is judged by (None when the tag is not published).
    pub fn write_spec(&self, tag: &str) -> Option<WriteSpec> {
        if let Some(i) = self.inputs.get(tag) {
            let kind = if i.is_bool {
                Kind::Binary
            } else {
                Kind::Analog
            };
            return Some(WriteSpec {
                kind,
                min: i.min,
                max: i.max,
            });
        }
        let (_, kind) = self.tags().find(|(t, _)| *t == tag)?;
        Some(WriteSpec {
            kind,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
        })
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
        if let Some(p) = self.power.as_mut() {
            p.dirty |= p.pw.advance(dt);
        }
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
        let (x, dir) = (self.v[b.x], self.v[b.dir]);
        let mut nx = x + dir * b.speed.eval(&self.v) * dt;
        for (at, open) in &b.barriers {
            if open.eval(&self.v) == 0.0 {
                nx = barrier_limit(*at, (x, nx), dir);
            }
        }
        match b.bounce {
            Some([lo, hi]) => bounce_at(lo, hi, nx, dir),
            None => (nx, dir),
        }
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
        assert!(e(
            "version: 1\ndt: 0.1\noutputs:\n  a: {type: analog, expr: 1}\npublish_order: [a, zz]\n"
        )
        .contains("publish_order: unknown output `zz`"));
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
