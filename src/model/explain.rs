//! `ferrofire explain <tag>`: what defines a tag, read offline from the model YAML (no stepping, no sockets).
//! One fact per declaration; a name can have two (a state and the output that republishes it).

use super::*;

type Rows = Vec<(&'static str, String)>;

struct Fact {
    name: String,
    kind: String,
    rows: Rows,
    deps: Vec<String>,
}

struct Ctx<'a> {
    syms: &'a Syms,
    names: Vec<&'a str>, // slot -> name
}

/// Labelled rows, skipping the ones with nothing to say.
fn rows<const N: usize>(r: [(&'static str, Option<String>); N]) -> Rows {
    r.into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect()
}

fn num(x: Option<f64>) -> Option<String> {
    x.map(|x| x.to_string())
}

fn kind_name(k: Kind) -> String {
    format!("{k:?}").to_lowercase()
}

impl Ctx<'_> {
    /// A fact whose dependencies are the names its expressions `exprs` read.
    fn fact(&self, name: &str, kind: &str, rows: Rows, exprs: &[String]) -> Result<Fact, String> {
        let mut deps = vec![];
        for e in exprs {
            let mut slots = vec![];
            comp(self.syms, e, name)?.vars(&mut slots);
            deps.extend(slots.into_iter().map(|s| self.names[s].to_string()));
        }
        deps.sort();
        deps.dedup();
        Ok(Fact {
            name: name.into(),
            kind: kind.into(),
            rows,
            deps,
        })
    }

    fn params(&self, doc: &Doc) -> Result<Vec<Fact>, String> {
        (doc.params.0.iter())
            .map(|(n, x)| self.fact(n, "param", rows([("default", num(Some(*x)))]), &[]))
            .collect()
    }

    fn inputs(&self, doc: &Doc) -> Result<Vec<Fact>, String> {
        (doc.inputs.0.iter())
            .map(|(n, d)| {
                let ty = kind_name(d.ty)
                    + if d.is_bool {
                        ", boolean in meaning"
                    } else {
                        ""
                    };
                let default = d.default.as_ref().map(|v| match v {
                    Default_::B(b) => b.to_string(),
                    Default_::N(x) => format!("{x:?}"),
                });
                let r = rows([
                    ("type", Some(ty)),
                    ("default", default),
                    ("min", num(d.min)),
                    ("max", num(d.max)),
                    ("units", d.units.clone()),
                ]);
                self.fact(n, "input", r, &[])
            })
            .collect()
    }

    fn states(&self, doc: &Doc) -> Result<Vec<Fact>, String> {
        let mut out = vec![];
        for (n, d) in &doc.states.0 {
            let (label, text, mut exprs) = match (&d.ode, &d.toward) {
                (Some(f), _) => ("ode", format!("d/dt = {}", f.text()), vec![f.text()]),
                (_, Some(t)) => (
                    "toward",
                    format!("target {}, rate {}", t.target.text(), t.rate.text()),
                    vec![t.target.text(), t.rate.text()],
                ),
                _ => {
                    return Err(format!(
                        "state {n}: exactly one of `ode` or `toward` is required"
                    ))
                }
            };
            exprs.push(d.init.text());
            let r = rows([
                (label, Some(text)),
                ("init", Some(d.init.text())),
                ("min", num(d.min)),
                ("max", num(d.max)),
                ("units", d.units.clone()),
            ]);
            out.push(self.fact(n, "state", r, &exprs)?);
        }
        Ok(out)
    }

    fn bodies(&self, doc: &Doc) -> Result<Vec<Fact>, String> {
        let mut out = vec![];
        for (n, d) in &doc.bodies.0 {
            let mut exprs = vec![d.speed.text()];
            let mut r = rows([("speed", Some(d.speed.text()))]);
            for b in &d.barriers {
                r.push(("barrier", format!("at {}: {}", b.at, b.open)));
                exprs.push(b.open.clone());
            }
            r.extend(rows([(
                "bounce",
                d.bounce.map(|[a, b]| format!("{a}..{b}")),
            )]));
            for (field, init) in [("x", d.init.x), ("dir", d.init.dir)] {
                let mut rr = r.clone();
                rr.push(("init", init.to_string()));
                out.push(self.fact(&format!("{n}.{field}"), "body", rr, &exprs)?);
            }
        }
        Ok(out)
    }

    fn outputs(&self, doc: &Doc) -> Result<Vec<Fact>, String> {
        (doc.outputs.0.iter())
            .map(|(n, o)| {
                let (label, e) = match &o.expr {
                    Some(e) => ("expr", e.text()),
                    None => ("republishes", format!("`{n}`")),
                };
                let r = rows([
                    ("type", Some(kind_name(o.ty))),
                    (label, Some(e.clone())),
                    ("units", o.units.clone()),
                ]);
                self.fact(n, "output", r, &[e])
            })
            .collect()
    }

    fn power(&self, doc: &Doc, p: &Power) -> Result<Vec<Fact>, String> {
        let drive = |n: &str| {
            let hit = doc.power.as_ref()?.drive.0.iter().find(|(k, _)| k == n);
            hit.map(|(_, e)| e.text())
        };
        (0..p.n_tags())
            .map(|i| {
                let n = p.tag_name(i);
                let d = drive(n);
                let kind = format!(
                    "power tag ({})",
                    if p.writable(i) {
                        "writable"
                    } else {
                        "read-only"
                    }
                );
                let r = rows([
                    ("type", Some(kind_name(p.tag_kind(i)))),
                    ("drive", d.clone()),
                    (
                        "solved by",
                        d.is_none()
                            .then(|| "AC power flow of the power case".into()),
                    ),
                    ("units", power_unit(n).map(String::from)),
                ]);
                let exprs: Vec<String> = d.into_iter().collect();
                self.fact(n, &kind, r, &exprs)
            })
            .collect()
    }
}

const POWER_UNITS: [(&str, &str); 14] = [
    ("voltage", "pu"),
    ("resistance", "pu"),
    ("reactance", "pu"),
    ("charging", "pu"),
    ("turns_ratio", "pu"),
    ("angle", "deg"),
    ("base_kv", "kV"),
    ("mva_max", "MVA"),
    ("mvar", "MVAr"),
    ("mvar_max", "MVAr"),
    ("mvar_min", "MVAr"),
    ("mw", "MW"),
    ("mw_max", "MW"),
    ("mw_min", "MW"),
];

/// Unit of a power tag from its field name (`bus-3.voltage`, `gen-1_bus-1.mw`).
fn power_unit(tag: &str) -> Option<&'static str> {
    let f = tag.rsplit_once('.')?.1;
    (POWER_UNITS.iter().find(|(s, _)| f.ends_with(s))).map(|u| u.1)
}

fn render(facts: &[Fact], tag: &str) -> Result<String, String> {
    let mine: Vec<&Fact> = facts.iter().filter(|f| f.name == tag).collect();
    if mine.is_empty() {
        let hint = did_you_mean(tag, facts.iter().map(|f| f.name.as_str()));
        return Err(format!("unknown tag `{tag}`{hint}"));
    }
    let mut used: Vec<&str> = (facts.iter())
        .filter(|f| f.name != tag && f.deps.iter().any(|d| d == tag))
        .map(|f| f.name.as_str())
        .collect();
    used.dedup();
    let mut out = String::new();
    for f in mine {
        out += &format!("{}: {}\n", f.name, f.kind);
        for (k, v) in &f.rows {
            out += &format!("  {k}: {v}\n");
        }
        let deps: Vec<&String> = f.deps.iter().filter(|d| **d != f.name).collect();
        if !deps.is_empty() {
            out += &format!("  depends on: {}\n", join(&deps));
        }
    }
    if !used.is_empty() {
        out += &format!("used by: {}\n", join(&used));
    }
    Ok(out)
}

fn join<S: AsRef<str>>(v: &[S]) -> String {
    (v.iter())
        .map(|s| s.as_ref())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Describe `tag` of the model in `yaml` (`dir`: where a relative `power.case` path is resolved).
pub fn explain(yaml: &str, dir: Option<&Path>, tag: &str) -> Result<String, String> {
    let doc = parse_doc(yaml)?;
    let mut syms = Syms::default();
    declare(&doc, &mut syms)?;
    let power = build_power(&doc, dir, &mut syms)?;
    let mut names = vec![""; syms.n];
    syms.map.iter().for_each(|(n, &s)| names[s] = n);
    let cx = Ctx { syms: &syms, names };
    render(&all_facts(&cx, &doc, power.as_ref().map(|p| &p.0))?, tag)
}

fn all_facts(cx: &Ctx, doc: &Doc, power: Option<&Power>) -> Result<Vec<Fact>, String> {
    let groups = [
        cx.params(doc),
        cx.inputs(doc),
        cx.states(doc),
        cx.bodies(doc),
        cx.outputs(doc),
        power.map_or(Ok(vec![]), |p| cx.power(doc, p)),
    ];
    let groups: Vec<Vec<Fact>> = groups.into_iter().collect::<Result<_, _>>()?;
    Ok(groups.into_iter().flatten().collect())
}
