//! Power-flow tests (spec 6.1 case parsers, 6.2 goldens, 6.4 properties) and the pump-station coupling.
//! Goldens come from `make golden` (PYPOWER 5.1.19, generated into target/golden, never committed); a missing
//! golden is a failure, not a skip.
use ferrofire::model::{Model, Value};
use ferrofire::power::case::*;
use ferrofire::power::{pf, Islands, Power};
use std::path::PathBuf;

fn gdir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/golden")
}
fn case_of(name: &str) -> Case {
    let p = gdir().join(format!("{name}.py"));
    load_file(&p).unwrap_or_else(|e| panic!("{e} (run `make golden` first)"))
}
fn golden(name: &str) -> serde_json::Value {
    let p = gdir().join(format!("{name}.json"));
    serde_json::from_str(
        &std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("{}: {e} (run `make golden` first)", p.display())),
    )
    .unwrap()
}
fn col(g: &serde_json::Value, k: &str) -> Vec<f64> {
    g[k].as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

/// Max abs difference between a solved case and the golden columns; asserts the spec 6.2 tolerances.
fn compare(name: &str, s: &Case, g: &serde_json::Value, ac: bool) {
    let d = |k: &str, got: Vec<f64>, tol: f64| {
        let want = col(g, k);
        assert_eq!(want.len(), got.len(), "{name} {k} length");
        let worst = want
            .iter()
            .zip(&got)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        assert!(worst < tol, "{name} {k}: max diff {worst:e} >= {tol:e}");
    };
    d("VM", s.bus.iter().map(|b| b[VM]).collect(), 1e-6);
    d("VA", s.bus.iter().map(|b| b[VA]).collect(), 1e-4);
    d("PG", s.gen.iter().map(|b| b[PG]).collect(), 1e-4);
    d("PF", s.branch.iter().map(|b| b[PF]).collect(), 1e-4);
    d("PT", s.branch.iter().map(|b| b[PT]).collect(), 1e-4);
    if ac {
        d("QG", s.gen.iter().map(|b| b[QG]).collect(), 1e-4);
        d("QF", s.branch.iter().map(|b| b[QF]).collect(), 1e-4);
        d("QT", s.branch.iter().map(|b| b[QT]).collect(), 1e-4);
    }
}

#[test]
fn ac_matches_pypower_goldens() {
    for name in ["case9", "case14", "case30", "case57", "case118", "case300"] {
        let (c, g) = (case_of(name), golden(name));
        let s = pf::runpf(&c, &pf::Opts::default()).unwrap();
        assert_eq!(
            s.success,
            g["success"].as_i64() == Some(1),
            "{name} success"
        );
        compare(name, &s.case, &g, true);
    }
}

#[test]
fn dc_matches_pypower_goldens() {
    for name in ["case9", "case30"] {
        let s = pf::runpf(
            &case_of(name),
            &pf::Opts {
                dc: true,
                ..Default::default()
            },
        )
        .unwrap();
        compare(name, &s.case, &golden(&format!("{name}_dc")), false);
    }
}

#[test]
fn branch_write_matches_golden_with_that_row_out_of_service() {
    let mut p = Power::new(case_of("case9"), Islands::Fail, false, &[], &[]).unwrap();
    let t = p.tag_index("branch-1_4-5.active").unwrap();
    assert_eq!(p.get(t), 1.0);
    p.set(t, 0.0);
    p.solve();
    assert!(!p.alert);
    compare("case9_br45off", &p.case, &golden("case9_br45off"), true);
    assert_eq!(p.case.branch[1][PF], 0.0);
}

#[test]
fn case300_parses_to_300_69_411_rows() {
    let c = case_of("case300");
    assert_eq!((c.bus.len(), c.gen.len(), c.branch.len()), (300, 69, 411));
    assert_eq!(c.base_mva, 100.0);
}

#[test]
fn case9_tag_names_match_pybennu_vocabulary() {
    let p = Power::new(case_of("case9"), Islands::Fail, false, &[], &[]).unwrap();
    let got: std::collections::BTreeSet<String> =
        (0..p.n_tags()).map(|i| p.tag_name(i).to_string()).collect();
    let mut want = std::collections::BTreeSet::new();
    want.insert("bus-alert.active".to_string());
    for n in 1..=9 {
        for f in [
            "voltage",
            "angle",
            "gen_mw",
            "gen_mvar",
            "load_mw",
            "load_mvar",
            "shunt_mvar",
            "base_kv",
            "active",
            "slack",
        ] {
            want.insert(format!("bus-{n}.{f}"));
        }
    }
    for n in [5, 7, 9] {
        for f in ["bus", "mw", "mvar", "active"] {
            want.insert(format!("load-1_bus-{n}.{f}"));
        }
    }
    for n in [1, 2, 3] {
        for f in [
            "mw", "mvar", "mw_max", "mw_min", "mvar_max", "mvar_min", "voltage", "active",
        ] {
            want.insert(format!("generator-1_bus-{n}.{f}"));
        }
    }
    for (a, b) in [
        (1, 4),
        (4, 5),
        (5, 6),
        (3, 6),
        (6, 7),
        (7, 8),
        (8, 2),
        (8, 9),
        (9, 4),
    ] {
        for f in [
            "source",
            "target",
            "resistance",
            "reactance",
            "charging",
            "mva_max",
            "turns_ratio",
            "phase_angle",
            "active",
        ] {
            want.insert(format!("branch-1_{a}-{b}.{f}"));
        }
    }
    assert_eq!(got, want);
    assert_eq!(p.n_tags(), 1 + 90 + 12 + 24 + 81);
    let v = p.tag_index("bus-5.voltage").unwrap();
    assert!(
        (p.get(v) - 0.9).abs() < 0.2 && p.get(v) != 1.0,
        "bus-5 voltage is a solved value"
    );
    let k = p.tag_index("bus-1.slack").unwrap();
    assert_eq!(p.get(k), 1.0);
}

const M3: &str = "function mpc = case3\nmpc.version = '2';\nmpc.baseMVA = 100;\n% bus\nmpc.bus = [\n 1 3 0 0 0 0 1 1 0 13.8 1 1.1 0.9;\n 2 1 0 0 0 0 1 1 0 13.8 1 1.1 0.9;\n 3 1 40 20 0 0 1 1 0 13.8 1 1.1 0.9;\n];\nmpc.gen = [\n 1 0 0 50 -50 1 100 1 50 0;\n];\nmpc.gencost = [ 2 0 0 2 1 0; ];\nmpc.branch = [\n 1 2 0.01 0.05 0 10 10 10 0 0 1 -360 360;\n 2 3 0.02 0.08 0 10 10 10 0 0 1 -360 360;\n];\n";

#[test]
fn matpower_m_and_inline_yaml_agree() {
    let m = parse_m(M3).unwrap();
    assert_eq!((m.bus.len(), m.gen.len(), m.branch.len()), (3, 1, 2));
    let y = "baseMVA: 100\nbuses: [{id: 1, type: slack, base_kv: 13.8}, {id: 2, type: pq, base_kv: 13.8}, {id: 3, type: pq, pd: 40, qd: 20, base_kv: 13.8}]\nbranches: [{from: 1, to: 2, r: 0.01, x: 0.05, rate: 10}, {from: 2, to: 3, r: 0.02, x: 0.08, rate: 10}]\ngens: [{bus: 1, qmax: 50, qmin: -50, pmax: 50}]\n";
    let i: Inline = serde_saphyr::from_str(y).unwrap();
    let i = i.to_case().unwrap();
    let (a, b) = (
        pf::runpf(&m, &pf::Opts::default()).unwrap(),
        pf::runpf(&i, &pf::Opts::default()).unwrap(),
    );
    assert!(a.success && b.success);
    for k in 0..3 {
        assert!((a.case.bus[k][VM] - b.case.bus[k][VM]).abs() < 1e-12);
    }
    assert!(a.case.bus[2][VM] < 1.0);
}

#[test]
fn py_parser_rejects_non_literals_with_the_line() {
    let src = "ppc[\"baseMVA\"] = 100.0\nppc[\"bus\"] = array([[1, 3, 0, 0, 0, 0, 1, 1, 0, 345, 1, 1.1, 0.9]])\nppc[\"gen\"] = np.zeros((3, 21))\nppc[\"branch\"] = array([])\n";
    let e = parse_py(src).unwrap_err();
    assert!(e.contains("line 3") && e.contains("np.zeros"), "{e}");
    assert!(parse_py("ppc[\"baseMVA\"] = 100\n")
        .unwrap_err()
        .contains("bus"));
}

// ---- properties ----------------------------------------------------------------------------------------------

/// Independent check from the published results: per-bus P/Q balance (flows leaving + shunt = Pg - Pd) and the
/// system balance Sum Pg - Sum Pd - losses = 0.
fn balance(name: &str, c: &Case) {
    let idx = |id: f64| c.bus.iter().position(|b| b[BUS_I] == id).unwrap();
    let nb = c.bus.len();
    let (mut p, mut q) = (vec![0.0; nb], vec![0.0; nb]);
    for b in &c.branch {
        p[idx(b[F_BUS])] += b[PF];
        q[idx(b[F_BUS])] += b[QF];
        p[idx(b[T_BUS])] += b[PT];
        q[idx(b[T_BUS])] += b[QT];
    }
    let (mut pg, mut qg) = (vec![0.0; nb], vec![0.0; nb]);
    for g in &c.gen {
        if g[GEN_STATUS] > 0.0 {
            pg[idx(g[GEN_BUS])] += g[PG];
            qg[idx(g[GEN_BUS])] += g[QG];
        }
    }
    let mut worst = 0.0f64;
    for (i, b) in c.bus.iter().enumerate() {
        let v2 = b[VM] * b[VM];
        worst = worst
            .max((p[i] + b[GS] * v2 - (pg[i] - b[PD])).abs())
            .max((q[i] - b[BS] * v2 - (qg[i] - b[QD])).abs());
    }
    assert!(worst < 1e-4, "{name}: bus balance off by {worst:e} MW/Mvar"); // tol 1e-8 pu * 100 MVA, with margin
}

#[test]
fn power_balance_holds_on_goldens() {
    for name in ["case9", "case14", "case30", "case118"] {
        let s = pf::runpf(&case_of(name), &pf::Opts::default()).unwrap();
        assert!(s.success);
        balance(name, &s.case);
    }
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
fn dc_and_ac_angles_agree_on_lightly_loaded_random_5_bus_cases() {
    let mut r = Lcg(7);
    for k in 0..25 {
        let mut c = Case {
            base_mva: 100.0,
            ..Default::default()
        };
        for i in 1..=5 {
            let ty = if i == 1 {
                REF
            } else if i == 2 {
                PV
            } else {
                PQ
            };
            let load = if i >= 3 { 5.0 + 20.0 * r.next() } else { 0.0 };
            c.bus.push([
                i as f64,
                ty,
                load,
                load * 0.2,
                0.0,
                0.0,
                1.0,
                1.0,
                0.0,
                13.8,
                1.0,
                1.1,
                0.9,
            ]);
        }
        for (f, t) in [(1, 2), (2, 3), (3, 4), (4, 5), (1, 5), (2, 4)] {
            let x = 0.05 + 0.15 * r.next();
            let mut b = [0.0; 17];
            b[..13].copy_from_slice(&[
                f as f64,
                t as f64,
                x / 10.0,
                x,
                0.0,
                100.0,
                100.0,
                100.0,
                0.0,
                0.0,
                1.0,
                -360.0,
                360.0,
            ]);
            c.branch.push(b);
        }
        let mut g1 = [0.0; 21];
        g1[..10].copy_from_slice(&[1.0, 0.0, 0.0, 100.0, -100.0, 1.0, 100.0, 1.0, 200.0, 0.0]);
        let mut g2 = g1;
        g2[0] = 2.0;
        g2[PG] = 20.0;
        c.gen = vec![g1, g2];
        let ac = pf::runpf(&c, &pf::Opts::default()).unwrap();
        let dc = pf::runpf(
            &c,
            &pf::Opts {
                dc: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(ac.success && dc.success, "case {k}");
        for i in 0..5 {
            assert!(
                (ac.case.bus[i][VA] - dc.case.bus[i][VA]).abs() < 2.0,
                "case {k} bus {i}: {} vs {}",
                ac.case.bus[i][VA],
                dc.case.bus[i][VA]
            );
        }
    }
}

// ---- islands, writes, drive ----------------------------------------------------------------------------------

#[test]
fn islands_deenergize_vs_fail() {
    let c = parse_m(M3).unwrap();
    let mut fail = Power::new(c.clone(), Islands::Fail, false, &[], &[]).unwrap();
    let mut den = Power::new(c, Islands::Deenergize, false, &[], &[]).unwrap();
    for p in [&mut fail, &mut den] {
        let t = p.tag_index("branch-1_2-3.active").unwrap();
        p.set(t, 0.0);
        p.solve();
    }
    let v3 = |p: &Power| p.get(p.tag_index("bus-3.voltage").unwrap());
    assert!(!den.alert);
    assert_eq!(v3(&den), 0.0);
    assert_eq!(den.get(den.tag_index("bus-3.angle").unwrap()), 0.0);
    assert!(den.get(den.tag_index("bus-2.voltage").unwrap()) > 0.99);
    // PyPower-faithful: islanded load bus -> singular Jacobian -> bus-alert with the stale voltage kept
    assert!(fail.alert);
    assert!(v3(&fail) > 0.9);
    // reconnect: de-energised bus comes back from a flat start
    let t = den.tag_index("branch-1_2-3.active").unwrap();
    den.set(t, 1.0);
    den.solve();
    assert!(!den.alert && v3(&den) > 0.9 && v3(&den) < 1.0);
}

#[test]
fn bus_and_load_writes_change_the_solve() {
    let mut p = Power::new(case_of("case9"), Islands::Deenergize, false, &[], &[]).unwrap();
    let pg = |p: &Power| p.get(p.tag_index("generator-1_bus-1.mw").unwrap());
    let before = pg(&p);
    let t = p.tag_index("load-1_bus-5.active").unwrap();
    p.set(t, 0.0);
    p.solve();
    assert!(!p.alert);
    assert!(
        before - pg(&p) > 80.0,
        "slack output drops by about the 90 MW load: {before} -> {}",
        pg(&p)
    ); // Decision 8
    let t = p.tag_index("bus-5.active").unwrap();
    p.set(t, 0.0);
    p.solve();
    assert!(!p.alert);
    assert_eq!(p.get(p.tag_index("bus-5.voltage").unwrap()), 0.0);
    assert!(p.get(p.tag_index("bus-4.voltage").unwrap()) > 0.9);
}

#[test]
fn dc_fallback_publishes_dc_and_raises_alert() {
    // a PQ-only island with no reference gen cannot solve in `fail` mode; with dc_fallback the DC angles are published
    let mut p = Power::new(parse_m(M3).unwrap(), Islands::Fail, true, &[], &[]).unwrap();
    assert!(!p.alert);
    let t = p.tag_index("bus-3.load_mw").unwrap();
    p.set(t, 1e6); // absurd load: Newton diverges
    p.solve();
    assert!(p.alert);
    assert_eq!(p.get(p.tag_index("bus-2.voltage").unwrap()), 1.0);
}

fn yaml_power(drive: &str) -> String {
    format!("version: 1\ndt: 0.1\nstates:\n  s: {{toward: {{target: 1, rate: 1}}, init: 0}}\noutputs:\n  s: {{type: analog}}\npower:\n  drive:\n{drive}  inline:\n    buses: [{{id: 1, type: slack}}, {{id: 2, type: pq, pd: 10}}]\n    branches: [{{from: 1, to: 2, r: 0.01, x: 0.05}}]\n    gens: [{{bus: 1, qmax: 50, qmin: -50, pmax: 50}}]\n")
}

#[test]
fn drive_targets_are_validated_and_placeholders_created() {
    assert!(Model::load(&yaml_power("    \"load-1_bus-9.mw\": \"1\"\n"))
        .err()
        .unwrap()
        .contains("no bus 9"));
    assert!(Model::load(&yaml_power("    \"nope.mw\": \"1\"\n"))
        .err()
        .unwrap()
        .contains("not a power tag"));
    assert!(Model::load(&yaml_power("    \"bus-2.gen_mw\": \"1\"\n"))
        .err()
        .unwrap()
        .contains("read-only"));
    // bus 1 has no load: the drive creates load-1_bus-1 and the solve sees it
    let mut m = Model::load(&yaml_power("    \"load-1_bus-1.mw\": \"s * 20\"\n")).unwrap();
    assert!(m.get("load-1_bus-1.mw").is_some());
    let g0 = num(m.get("generator-1_bus-1.mw"));
    for _ in 0..10 {
        m.step(0.1);
    }
    assert!(num(m.get("load-1_bus-1.mw")) > 5.0);
    assert!(num(m.get("generator-1_bus-1.mw")) > g0 + 5.0);
}

fn num(v: Option<Value>) -> f64 {
    match v.unwrap() {
        Value::Analog(x) => x,
        Value::Binary(b) => b as u8 as f64,
    }
}

#[test]
fn pump_station_trips_when_the_feeder_opens() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
    let yaml = std::fs::read_to_string(dir.join("pump-station.yaml")).unwrap();
    let mut m = Model::load_in(&yaml, Some(&dir)).unwrap();
    assert!(num(m.get("bus-3.voltage")) > 0.95);
    assert_eq!(num(m.get("bus-alert.active")), 0.0);
    let bad = m.apply_writes(&[
        ("pump.run".into(), Value::Binary(true)),
        ("valve.open".into(), Value::Analog(0.1)),
        ("bus-3.gen_mw".into(), Value::Analog(1.0)),
    ]);
    assert_eq!(bad, vec!["bus-3.gen_mw"]); // read-only power tag is refused
                                           // a writable power tag is judged by its wire type: active is binary, so 5 is refused and 0 or true pass
    let spec = m.write_spec("branch-1_2-3.active").unwrap();
    assert_eq!(spec.kind, ferrofire::model::Kind::Binary);
    assert!(spec
        .check("branch-1_2-3.active", Value::Analog(5.0))
        .is_err());
    assert!(spec
        .check("branch-1_2-3.active", Value::Analog(0.0))
        .is_ok());
    assert!(m.write_spec("nope").is_none());
    let mut level_before_open = 0.0;
    for k in 0..600 {
        if k == 300 {
            level_before_open = num(m.get("tank.level"));
            assert!(m
                .apply_writes(&[("branch-1_2-3.active".into(), Value::Analog(0.0))])
                .is_empty());
        }
        m.step(0.1);
        if k == 299 {
            assert_eq!(num(m.get("pump.flow")), 0.5);
        }
    }
    assert!(
        level_before_open > 2.2,
        "tank filled while the pump ran: {level_before_open}"
    );
    assert_eq!(num(m.get("bus-3.voltage")), 0.0);
    assert_eq!(num(m.get("pump.flow")), 0.0);
    assert_eq!(num(m.get("load-1_bus-3.mw")), 0.0);
    assert!(
        num(m.get("tank.level")) < level_before_open,
        "tank drains through the valve after the trip"
    );
    assert_eq!(num(m.get("bus-alert.active")), 0.0);
}

#[test]
fn inline_bus_types_by_name_and_number() {
    let inline = |ty: &str| {
        let y = format!(
            "version: 1\ndt: 0.1\npower:\n  inline:\n    buses:\n      - {{id: 1, type: {ty}}}\n"
        );
        Model::load(&y).err().map(|e| e.to_string())
    };
    assert!(inline("bogus")
        .unwrap()
        .contains("inline bus 1: unknown type `bogus`"));
    for ok in ["slack", "ref", "pv", "pq", "isolated", "none", "3"] {
        // parsing succeeds; the solver then refuses a case with no generator, which is not a type error
        let e = inline(ok).unwrap_or_default();
        assert!(!e.contains("unknown type"), "{ok}: {e}");
    }
}

// ---- malformed cases -----------------------------------------------------------------------------------------

#[test]
fn short_rows_name_the_array_and_the_row() {
    let e = parse_py("ppc[\"baseMVA\"] = 100\nppc[\"bus\"] = [[1, 3, 0]]\nppc[\"gen\"] = []\nppc[\"branch\"] = []\n").unwrap_err();
    assert!(e.contains("bus row 1: 3 columns, need at least 10"), "{e}");
}

#[test]
fn solver_refuses_gens_and_branches_on_unknown_buses() {
    let ok = parse_m(M3).unwrap();
    let mut gen = ok.clone();
    gen.gen[0][GEN_BUS] = 77.0;
    let e = pf::runpf(&gen, &pf::Opts::default()).err().unwrap();
    assert_eq!(e, "gen row 1 sits on unknown bus 77");
    let mut br = ok;
    br.branch[1][T_BUS] = 88.0;
    let e = pf::runpf(&br, &pf::Opts::default()).err().unwrap();
    assert_eq!(e, "branch row 2 touches unknown bus 88");
}

#[test]
fn a_case_without_a_reference_bus_promotes_its_first_pv_bus() {
    let mut c = parse_m(M3).unwrap();
    c.bus[0][BUS_TYPE] = PV; // the only generator sits on bus 1, which was the reference
    let s = pf::runpf(&c, &pf::Opts::default()).unwrap();
    assert!(s.success);
    assert!((s.case.bus[0][VM] - c.gen[0][VG]).abs() < 1e-9); // promoted bus holds the gen set point
    c.bus[0][BUS_TYPE] = PQ; // and with no PV bus either there is nothing to promote
    let e = pf::runpf(&c, &pf::Opts::default()).err().unwrap();
    assert!(e.contains("no reference or PV bus"));
}
