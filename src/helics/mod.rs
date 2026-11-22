//! `ferrofire run` as a HELICS federate (docs/helics.md). This module is safe code: the `helics:` section of a
//! model, its validation against the model, the `Federate` trait and the grant loop. The libhelics calls live in
//! `ffi` (feature `helics`), behind the trait, so every branch here is tested with a scripted fake federate.

#[cfg(feature = "helics")]
pub mod ffi;

use crate::hint::{did_you_mean, show};
use crate::model::{Kind, Model, Value, WriteSpec};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// A grant at or past this is HELICS saying the federation is over (`HELICS_TIME_MAXTIME` is 9223372036.85).
const END_OF_TIME: f64 = 9.0e9;

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pace {
    /// Step as fast as grants arrive.
    #[default]
    Lockstep,
    /// Also hold each grant to the wall clock.
    Wall,
}

/// One `subscribe:` entry: a peer's publication key feeds a model input.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubDef {
    pub key: String,
    pub tag: String,
}

/// One `publish:` entry: a model tag is published under a global key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PubDef {
    pub tag: String,
    pub key: String,
}

/// The `helics:` section of a `model.yaml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelicsDef {
    pub name: Option<String>,
    #[serde(default = "zmq")]
    pub core: String,
    pub broker: Option<String>,
    pub period: Option<f64>,
    #[serde(default)]
    pub pace: Pace,
    pub library: Option<String>,
    #[serde(default)]
    pub subscribe: Vec<SubDef>,
    #[serde(default)]
    pub publish: Vec<PubDef>,
}

fn zmq() -> String {
    "zmq".into()
}

/// Writes to model inputs: tag and value.
pub type Writes = Vec<(String, Value)>;

/// A subscription resolved against the model: the write it makes is judged by `spec` like a REP write.
#[derive(Debug, Clone, PartialEq)]
pub struct Sub {
    pub key: String,
    pub tag: String,
    pub spec: WriteSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pub {
    pub key: String,
    pub tag: String,
    pub kind: Kind,
}

/// A `HelicsDef` checked against its model.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub name: String,
    pub core: String,
    pub broker: Option<String>,
    pub library: Option<String>,
    /// Seconds of federate time per grant, and the model steps that make up one.
    pub period: f64,
    pub steps: usize,
    pub pace: Pace,
    pub subs: Vec<Sub>,
    pub pubs: Vec<Pub>,
}

/// `period` as a whole number of model steps of `dt` (default one), or why it is not.
fn steps_of(period: Option<f64>, dt: f64) -> Result<(f64, usize), String> {
    let period = period.unwrap_or(dt);
    let steps = (period / dt).round();
    if !(period > 0.0 && steps >= 1.0 && (steps * dt - period).abs() <= 1e-9 * period) {
        return Err(format!(
            "helics: period {period} must be a positive multiple of dt {dt}"
        ));
    }
    Ok((period, steps as usize))
}

fn unknown_tag<'a>(what: &str, tag: &str, known: impl Iterator<Item = &'a str>) -> String {
    let hint = did_you_mean(tag, known);
    format!("helics: {what} tag '{}' is unknown{hint}", show(tag))
}

fn resolve_sub(model: &Model, d: &SubDef) -> Result<Sub, String> {
    let Some(spec) = model.write_spec(&d.tag) else {
        return Err(unknown_tag("subscribe", &d.tag, model.input_tags()));
    };
    if !model.writable(&d.tag) {
        return Err(format!(
            "helics: subscribe tag '{}' is not writable (only inputs and writable power tags take a subscription)",
            show(&d.tag)
        ));
    }
    Ok(Sub {
        key: d.key.clone(),
        tag: d.tag.clone(),
        spec,
    })
}

fn resolve_pub(model: &Model, d: &PubDef) -> Result<Pub, String> {
    let (_, kind) = model
        .tags()
        .find(|(t, _)| *t == d.tag)
        .ok_or_else(|| unknown_tag("publish", &d.tag, model.tags().map(|(t, _)| t)))?;
    Ok(Pub {
        key: d.key.clone(),
        tag: d.tag.clone(),
        kind,
    })
}

impl HelicsDef {
    /// Check the section against `model`. `default_name` is the federate name when the section gives none.
    pub fn plan(&self, model: &Model, default_name: &str) -> Result<Plan, String> {
        let (period, steps) = steps_of(self.period, model.dt)?;
        Ok(Plan {
            name: self.name.clone().unwrap_or_else(|| default_name.into()),
            core: self.core.clone(),
            broker: self.broker.clone(),
            library: self.library.clone(),
            period,
            steps,
            pace: self.pace,
            subs: self
                .subscribe
                .iter()
                .map(|d| resolve_sub(model, d))
                .collect::<Result<_, _>>()?,
            pubs: self
                .publish
                .iter()
                .map(|d| resolve_pub(model, d))
                .collect::<Result<_, _>>()?,
        })
    }
}

/// `ferrofire run` flags that override the section.
#[derive(Debug, Default, clap::Args)]
pub struct Cli {
    /// HELICS broker address (overrides `helics: broker:`), e.g. tcp://broker:23404
    #[arg(long = "helics-broker", value_name = "ADDR")]
    pub broker: Option<String>,
    /// Federate name (overrides `helics: name:`; default: the model file's name)
    #[arg(long = "helics-name", value_name = "NAME")]
    pub name: Option<String>,
    /// libhelics.so to load (overrides `helics: library:` and $FERROFIRE_HELICS_LIB)
    #[arg(long = "helics-lib", value_name = "PATH")]
    pub lib: Option<String>,
}

/// The federate this run joins: the model's `helics:` section with the flags applied, or None for a plain run.
pub fn plan_for(model: &Model, cli: &Cli, path: Option<&str>) -> Result<Option<Plan>, String> {
    let Some(def) = &model.helics else {
        let given = cli.broker.is_some() || cli.name.is_some() || cli.lib.is_some();
        return if given {
            Err("--helics-* flags need a helics: section in the model".into())
        } else {
            Ok(None)
        };
    };
    let mut def = def.clone();
    def.broker = cli.broker.clone().or(def.broker);
    def.name = cli.name.clone().or(def.name);
    def.library = cli.lib.clone().or(def.library);
    let stem = path
        .and_then(|p| std::path::Path::new(p).file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("ferrofire");
    def.plan(model, stem).map(Some)
}

/// The engine of `ferrofire run`: the plain real-time one, or, for a model with a `helics:` section, the grant loop.
pub fn run_engine(
    cli: &Cli,
    path: Option<&str>,
    model: &mut Model,
    sh: &std::sync::Mutex<crate::bennu::Shared>,
    targets: &[crate::bennu::Target],
    every: u64,
) -> Result<(), String> {
    let ran = match plan_for(model, cli, path) {
        Ok(None) => {
            crate::bennu::engine(model, sh, targets, every);
            Ok(())
        }
        Ok(Some(plan)) => engine(model, sh, targets, every, &plan),
        Err(e) => Err(e),
    };
    if ran.is_err() {
        // the REP thread `run` joins afterwards only ends on a stop
        crate::bennu::STOP.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    ran
}

/// The grant loop over libhelics, ticking and publishing like the plain engine.
#[cfg(feature = "helics")]
fn engine(
    model: &mut Model,
    sh: &std::sync::Mutex<crate::bennu::Shared>,
    targets: &[crate::bennu::Target],
    every: u64,
    plan: &Plan,
) -> Result<(), String> {
    use std::sync::atomic::Ordering::SeqCst;
    fn stopped() -> bool {
        crate::bennu::STOP.load(SeqCst)
    }
    let lib = ffi::Lib::open(plan.library.as_deref())?;
    let mut fed = ffi::Fed::from_plan(&lib, plan, stopped)?;
    println!(
        "Info: helics {}: federate {} ({} core), period {}s, {} subscriptions, {} publications",
        lib.version(),
        plan.name,
        plan.core,
        plan.period,
        plan.subs.len(),
        plan.pubs.len()
    );
    let (mut snap, mut k) = (vec![], 0u64);
    let mut tick = |m: &mut Model, writes: Vec<(String, Value)>| {
        crate::hint::lock(sh).queue.extend(writes);
        crate::bennu::tick_and_publish(m, sh, targets, &mut snap, k, every);
        k += 1;
    };
    run_federate(model, plan, &mut fed, &stopped, &mut tick)
}

/// Unreachable in practice (a model with a `helics:` section does not load without the feature).
#[cfg(not(feature = "helics"))]
fn engine(
    _: &mut Model,
    _: &std::sync::Mutex<crate::bennu::Shared>,
    _: &[crate::bennu::Target],
    _: u64,
    _: &Plan,
) -> Result<(), String> {
    Err("this build has no `helics` feature".into())
}

/// Set when a stop request cut a blocked HELICS call short: libhelics then cannot be shut down cleanly.
static ABANDONED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Last thing `ferrofire run` does: when a federate was abandoned mid-call, libhelics' exit handlers wait for it forever,
/// so leave without them (everything ferrofire owns is already stopped and flushed).
pub fn finish() {
    if ABANDONED.load(std::sync::atomic::Ordering::SeqCst) {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        crate::sys::exit_now(0);
    }
}

/// The calls the grant loop makes on a federate; `ffi::Fed` is the libhelics one. `i` indexes `Plan::subs` / `Plan::pubs`.
pub trait Federate {
    fn enter_exec(&mut self) -> Result<(), String>;
    /// The subscription's value when it changed since the last read.
    fn read(&mut self, i: usize) -> Result<Option<Value>, String>;
    fn publish(&mut self, i: usize, v: Value) -> Result<(), String>;
    /// Block until `t` is granted; `None` when the wait was cut short by a stop request.
    fn request_time(&mut self, t: f64) -> Result<Option<f64>, String>;
    fn finalize(&mut self);
}

/// Subscription values that changed, as writes the model would accept; a refused one is warned about once per subscription.
fn collect_writes(
    plan: &Plan,
    fed: &mut dyn Federate,
    warned: &mut HashSet<usize>,
) -> Result<Writes, String> {
    let mut writes = vec![];
    for (i, s) in plan.subs.iter().enumerate() {
        let Some(v) = fed.read(i)? else { continue };
        match s.spec.check(&s.tag, v) {
            Ok(()) => writes.push((s.tag.clone(), v)),
            Err(e) if warned.insert(i) => println!("Warn: helics {}: {e}", s.key),
            Err(_) => {}
        }
    }
    Ok(writes)
}

fn publish_state(plan: &Plan, model: &Model, fed: &mut dyn Federate) -> Result<(), String> {
    for (i, p) in plan.pubs.iter().enumerate() {
        if let Some(v) = model.get(&p.tag) {
            fed.publish(i, v)?;
        }
    }
    Ok(())
}

/// One grant per iteration: read what the peers published, publish the state at this instant, step the model
/// `plan.steps` times (`tick` also applies the writes, REP ones first so these win), then block until the next grant.
/// Ends on `stop`, on the federation finishing, or on a HELICS error; the federate is finalized either way.
pub fn run_federate(
    model: &mut Model,
    plan: &Plan,
    fed: &mut dyn Federate,
    stop: &dyn Fn() -> bool,
    tick: &mut dyn FnMut(&mut Model, Writes),
) -> Result<(), String> {
    let res = grants(model, plan, fed, stop, tick);
    fed.finalize();
    res
}

fn grants(
    model: &mut Model,
    plan: &Plan,
    fed: &mut dyn Federate,
    stop: &dyn Fn() -> bool,
    tick: &mut dyn FnMut(&mut Model, Writes),
) -> Result<(), String> {
    fed.enter_exec()?;
    let (t0, mut warned, mut grant) = (Instant::now(), HashSet::new(), 0u64);
    while !stop() {
        let writes = collect_writes(plan, fed, &mut warned)?;
        publish_state(plan, model, fed)?;
        tick(model, writes);
        for _ in 1..plan.steps {
            tick(model, vec![]);
        }
        grant += 1;
        if !next_grant(plan, fed, grant, t0)? {
            break;
        }
    }
    Ok(())
}

/// Block until grant number `grant` (and, with `pace: wall`, its wall-clock time since `t0`). False when the run is over:
/// a stop request cut the wait short, or the federation finished.
fn next_grant(
    plan: &Plan,
    fed: &mut dyn Federate,
    grant: u64,
    t0: Instant,
) -> Result<bool, String> {
    let target = grant as f64 * plan.period;
    match fed.request_time(target)? {
        Some(g) if g < END_OF_TIME => {}
        _ => return Ok(false),
    }
    if plan.pace == Pace::Wall {
        let due = t0 + Duration::from_secs_f64(target);
        crate::bennu::sleep_until(due, Duration::from_millis(5));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const MODEL: &str = "version: 1\ndt: 0.5\ninputs:\n  pump.run: {type: binary, default: false}\n  valve.open: {type: analog, default: 0.5, min: 0, max: 1}\nstates:\n  tank.level: {ode: 'valve.open - 0.25', init: 1.0, min: 0, max: 10}\noutputs:\n  tank.level: {type: analog}\n";

    fn model_with(helics: &str) -> Result<Model, String> {
        Model::load(&format!("{MODEL}helics:\n{helics}"))
    }

    const FULL: &str = "  name: tank\n  period: 1.0\n  subscribe:\n    - {key: ctl/valve, tag: valve.open}\n    - {key: ctl/run, tag: pump.run}\n  publish:\n    - {tag: tank.level, key: tank/level}\n";

    #[test]
    fn config_resolves_against_the_model() {
        let m = model_with(FULL).unwrap();
        let p = m.helics.as_ref().unwrap().plan(&m, "x").unwrap();
        assert_eq!(
            (p.name.as_str(), p.core.as_str(), p.steps, p.period),
            ("tank", "zmq", 2, 1.0)
        );
        assert_eq!(p.subs[0].spec.kind, Kind::Analog);
        assert_eq!(p.subs[1].spec.kind, Kind::Binary);
        assert_eq!(p.pubs[0].kind, Kind::Analog);
        assert_eq!(p.pace, Pace::Lockstep);
        // no name: the caller's default; no period: one step
        let m = model_with("  publish: [{tag: tank.level, key: k}]\n").unwrap();
        let p = m.helics.as_ref().unwrap().plan(&m, "stem").unwrap();
        assert_eq!((p.name.as_str(), p.steps, p.period), ("stem", 1, 0.5));
    }

    #[test]
    fn config_errors_name_the_problem() {
        let err = |h: &str| model_with(h).err().unwrap();
        assert!(
            err("  subscribe: [{key: k, tag: valve.opn}]\n").contains("did you mean `valve.open`")
        );
        assert!(err("  subscribe: [{key: k, tag: tank.level}]\n").contains("not writable"));
        assert!(err("  publish: [{tag: tank.lvl, key: k}]\n")
            .contains("publish tag 'tank.lvl' is unknown"));
        assert!(err("  period: 0.7\n").contains("multiple of dt"));
        assert!(err("  period: 0.25\n").contains("multiple of dt"));
        assert!(err("  bogus: 1\n").contains("unknown field"));
        assert!(err("  core: [1]\n").contains("model:"));
    }

    #[test]
    fn cli_flags_override_the_section() {
        let m = model_with(FULL).unwrap();
        let cli = Cli {
            broker: Some("tcp://b:1".into()),
            name: Some("n".into()),
            lib: Some("/x.so".into()),
        };
        let p = plan_for(&m, &cli, Some("d/tank.yaml")).unwrap().unwrap();
        assert_eq!(
            (p.name.as_str(), p.broker.as_deref(), p.library.as_deref()),
            ("n", Some("tcp://b:1"), Some("/x.so"))
        );
        let m = model_with("  publish: [{tag: tank.level, key: k}]\n").unwrap();
        let p = plan_for(&m, &Cli::default(), Some("d/tank.yaml"))
            .unwrap()
            .unwrap();
        assert_eq!((p.name.as_str(), p.broker), ("tank", None));
        // a plain model: no plan, and flags without a section are an error
        let plain = Model::load(MODEL).unwrap();
        assert_eq!(plan_for(&plain, &Cli::default(), None), Ok(None));
        assert!(plan_for(&plain, &cli, None)
            .unwrap_err()
            .contains("need a helics: section"));
    }

    #[test]
    fn steps_of_accepts_exact_multiples_only() {
        assert_eq!(steps_of(None, 0.1), Ok((0.1, 1)));
        assert_eq!(steps_of(Some(0.3), 0.1), Ok((0.3, 3))); // 0.3 / 0.1 is not exactly 3
        assert!(steps_of(Some(-1.0), 0.1).is_err());
        assert!(steps_of(Some(f64::NAN), 0.1).is_err());
    }

    /// Scripted peer: `incoming[g][i]` is what subscription `i` shows after grant `g`; records publishes and requests.
    #[derive(Default)]
    struct Fake {
        incoming: Vec<Vec<Option<Value>>>,
        granted: usize,
        published: Vec<(usize, Value)>,
        requests: Vec<f64>,
        finalized: bool,
        end_after: Option<usize>,
        fail_publish: bool,
    }

    impl Federate for Fake {
        fn enter_exec(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn read(&mut self, i: usize) -> Result<Option<Value>, String> {
            Ok(self.incoming.get(self.granted).and_then(|r| r[i]))
        }
        fn publish(&mut self, i: usize, v: Value) -> Result<(), String> {
            if self.fail_publish {
                return Err("boom".into());
            }
            self.published.push((i, v));
            Ok(())
        }
        fn request_time(&mut self, t: f64) -> Result<Option<f64>, String> {
            self.requests.push(t);
            self.granted += 1;
            Ok(Some(if self.end_after == Some(self.granted) {
                9.2e9
            } else {
                t
            }))
        }
        fn finalize(&mut self) {
            self.finalized = true;
        }
    }

    fn drive(
        m: &mut Model,
        fed: &mut Fake,
        stop_after: usize,
    ) -> (Result<(), String>, Vec<Writes>) {
        let plan = m.helics.clone().unwrap().plan(m, "t").unwrap();
        let log = RefCell::new(vec![]);
        let n = RefCell::new(0usize);
        let res = run_federate(
            m,
            &plan,
            fed,
            &|| *n.borrow() >= stop_after,
            &mut |m: &mut Model, w: Vec<(String, Value)>| {
                *n.borrow_mut() += 1;
                m.apply_writes(&w);
                m.step(m.dt);
                log.borrow_mut().push(w);
            },
        );
        (res, log.into_inner())
    }

    #[test]
    fn loop_applies_updates_publishes_and_steps_per_grant() {
        let mut m = model_with(FULL).unwrap();
        let mut f = Fake {
            incoming: vec![
                vec![None, None],
                vec![Some(Value::Analog(1.0)), Some(Value::Binary(true))],
                vec![None, None],
            ],
            ..Fake::default()
        };
        let (res, log) = drive(&mut m, &mut f, 6); // 2 ticks per grant: 3 grants
        assert_eq!(res, Ok(()));
        assert!(f.finalized);
        assert_eq!(f.requests, vec![1.0, 2.0, 3.0]);
        assert_eq!(log.len(), 6);
        assert!(log[0].is_empty() && log[1].is_empty()); // grant 0: nothing yet; a second tick carries no writes
        assert_eq!(
            log[2],
            vec![
                ("valve.open".into(), Value::Analog(1.0)),
                ("pump.run".into(), Value::Binary(true))
            ]
        );
        assert!(log[3].is_empty());
        // published at t = 0 (level 1.0) and t = 1 (two steps of 0.5 s at 0.25 /s)
        let levels: Vec<_> = f.published.iter().map(|(_, v)| *v).collect();
        assert_eq!(levels[0], Value::Analog(1.0));
        assert_eq!(levels[1], Value::Analog(1.25));
        assert_eq!(m.time, 3.0);
    }

    #[test]
    fn out_of_limit_value_is_dropped_and_warned_once() {
        let mut m = model_with(FULL).unwrap();
        let bad = vec![Some(Value::Analog(7.0)), None];
        let mut f = Fake {
            incoming: vec![bad.clone(), bad],
            ..Fake::default()
        };
        let (_, log) = drive(&mut m, &mut f, 4);
        assert!(log.iter().all(|w| w.is_empty()));
        let mut warned = HashSet::new();
        let plan = m.helics.clone().unwrap().plan(&m, "t").unwrap();
        f.granted = 0;
        assert!(collect_writes(&plan, &mut f, &mut warned)
            .unwrap()
            .is_empty());
        assert_eq!(warned.len(), 1);
    }

    #[test]
    fn federation_end_and_errors_finalize() {
        let mut m = model_with(FULL).unwrap();
        let mut f = Fake {
            end_after: Some(2),
            ..Fake::default()
        };
        let (res, log) = drive(&mut m, &mut f, 100);
        assert_eq!((res, log.len(), f.finalized), (Ok(()), 4, true));
        let mut f = Fake {
            fail_publish: true,
            ..Fake::default()
        };
        let (res, _) = drive(&mut m, &mut f, 100);
        assert_eq!(res, Err("boom".into()));
        assert!(f.finalized);
    }

    #[test]
    fn wall_pace_holds_each_grant_to_the_clock() {
        let mut m = Model::load(&format!("{MODEL}helics:\n  pace: wall\n  period: 0.5\n  publish: [{{tag: tank.level, key: k}}]\n")).unwrap();
        let t = Instant::now();
        let _ = drive(&mut m, &mut Fake::default(), 2);
        assert!(t.elapsed() >= Duration::from_millis(990));
    }
}
