//! AC (Newton-Raphson) and DC power flow. Ported function by function from PYPOWER 5.1.19
//! (https://github.com/rwl/PYPOWER): runpf, ext2int/int2ext, bustypes, makeYbus, makeSbus, dSbus_dV, newtonpf,
//! pfsoln, makeBdc, dcpf. Dense complex Ybus and dense LU (spec decision 5, ceiling about 2000 buses).
//!
//! Copyright (c) 1996-2015, Power System Engineering Research Center (PSERC). All rights reserved.
//! PYPOWER is BSD 3-clause licensed; the full notice is in the NOTICE file at the repository root.
//! Port copyright (c) 2026 Ryan Adams, MIT.

use super::case::*;
use num_complex::Complex64 as C;
use std::collections::HashMap;
use std::f64::consts::PI;

pub struct Opts {
    pub tol: f64,
    pub max_it: usize,
    pub dc: bool,
    /// Drop buses the reference buses cannot reach (and type-4 buses) and publish VM = VA = 0 for them.
    pub deenergize: bool,
}
impl Default for Opts {
    fn default() -> Self {
        Opts {
            tol: 1e-8,
            max_it: 10,
            dc: false,
            deenergize: false,
        }
    } // ppoption PF_TOL, PF_MAX_IT
}

pub struct Solved {
    pub case: Case,
    pub success: bool,
}

/// ext2int result: only in-service parts, consecutive internal bus numbers, gens sorted by bus.
struct Net {
    base: f64,
    bus_ext: Vec<usize>,
    bus: Vec<BusRow>,
    gen_ext: Vec<usize>,
    gbus: Vec<usize>,
    gen: Vec<GenRow>,
    br_ext: Vec<usize>,
    fb: Vec<usize>,
    tb: Vec<usize>,
    br: Vec<BranchRow>,
}

type Index = HashMap<i64, usize>;

/// In-service buses with consecutive internal numbers: (external number -> internal, external rows, rows).
fn index_buses(case: &Case, off: &[bool]) -> Result<(Index, Vec<usize>, Vec<BusRow>), String> {
    let mut e2i = Index::new();
    let (mut bus_ext, mut bus) = (vec![], vec![]);
    for (r, b) in case.bus.iter().enumerate() {
        if b[BUS_TYPE] == NONE || off[r] {
            continue;
        }
        if e2i.insert(b[BUS_I] as i64, bus.len()).is_some() {
            return Err(format!("duplicate bus number {}", b[BUS_I]));
        }
        bus_ext.push(r);
        bus.push(*b);
    }
    Ok((e2i, bus_ext, bus))
}

/// In-service gens on in-service buses as (internal bus, external row), sorted by bus.
fn index_gens(case: &Case, e2i: &Index) -> Result<Vec<(usize, usize)>, String> {
    let known: std::collections::HashSet<i64> = case.bus.iter().map(|b| b[BUS_I] as i64).collect();
    let mut gens: Vec<(usize, usize)> = vec![];
    for (r, g) in case.gen.iter().enumerate() {
        if !known.contains(&(g[GEN_BUS] as i64)) {
            return Err(format!(
                "gen row {} sits on unknown bus {}",
                r + 1,
                g[GEN_BUS]
            ));
        }
        let internal = e2i.get(&(g[GEN_BUS] as i64));
        if let (true, Some(&i)) = (g[GEN_STATUS] > 0.0, internal) {
            gens.push((i, r));
        }
    }
    gens.sort_by_key(|g| g.0); // stable, like argsort on integer bus numbers
    Ok(gens)
}

/// In-service branches between in-service buses as (external row, from, to).
fn index_branches(case: &Case, e2i: &Index) -> Result<Vec<(usize, usize, usize)>, String> {
    let known: std::collections::HashSet<i64> = case.bus.iter().map(|b| b[BUS_I] as i64).collect();
    let mut out = vec![];
    for (r, b) in case.branch.iter().enumerate() {
        if let Some(e) = [b[F_BUS], b[T_BUS]]
            .into_iter()
            .find(|e| !known.contains(&(*e as i64)))
        {
            return Err(format!("branch row {} touches unknown bus {e}", r + 1));
        }
        let ends = (e2i.get(&(b[F_BUS] as i64)), e2i.get(&(b[T_BUS] as i64)));
        if let (true, (Some(&f), Some(&t))) = (b[BR_STATUS] > 0.0, ends) {
            out.push((r, f, t));
        }
    }
    Ok(out)
}

fn ext2int(case: &Case, off: &[bool]) -> Result<Net, String> {
    let (e2i, bus_ext, bus) = index_buses(case, off)?;
    let gens = index_gens(case, &e2i)?;
    let brs = index_branches(case, &e2i)?;
    Ok(Net {
        base: case.base_mva,
        bus_ext,
        bus,
        gen_ext: gens.iter().map(|g| g.1).collect(),
        gbus: gens.iter().map(|g| g.0).collect(),
        gen: gens.iter().map(|g| case.gen[g.1]).collect(),
        br_ext: brs.iter().map(|b| b.0).collect(),
        fb: brs.iter().map(|b| b.1).collect(),
        tb: brs.iter().map(|b| b.2).collect(),
        br: brs.iter().map(|b| case.branch[b.0]).collect(),
    })
}

/// (ref, pv, pq). Gens are all in service here, so "has a gen" is "has any gen".
type BusSets = (Vec<usize>, Vec<usize>, Vec<usize>);

fn bustypes(n: &Net) -> Result<BusSets, String> {
    let mut has = vec![false; n.bus.len()];
    for &g in &n.gbus {
        has[g] = true;
    }
    let pick = |f: &dyn Fn(usize) -> bool| (0..n.bus.len()).filter(|&i| f(i)).collect::<Vec<_>>();
    let mut r = pick(&|i| n.bus[i][BUS_TYPE] == REF && has[i]);
    let mut pv = pick(&|i| n.bus[i][BUS_TYPE] == PV && has[i]);
    let pq = pick(&|i| n.bus[i][BUS_TYPE] == PQ || !has[i]);
    if r.is_empty() {
        if pv.is_empty() {
            return Err("no reference or PV bus with an in-service generator".into());
        }
        r = vec![pv.remove(0)]; // promote the first PV bus
    }
    Ok((r, pv, pq))
}

fn reachable(n: &Net, ref_: &[usize]) -> Vec<bool> {
    let mut adj = vec![vec![]; n.bus.len()];
    for (&f, &t) in n.fb.iter().zip(&n.tb) {
        adj[f].push(t);
        adj[t].push(f);
    }
    let mut seen = vec![false; n.bus.len()];
    let mut q: Vec<usize> = ref_.to_vec();
    for &r in ref_ {
        seen[r] = true;
    }
    while let Some(x) = q.pop() {
        for &y in &adj[x] {
            if !seen[y] {
                seen[y] = true;
                q.push(y);
            }
        }
    }
    seen
}

/// Per-branch admittances (makeYbus): (yff, yft, ytf, ytt).
fn branch_y(n: &Net) -> Vec<[C; 4]> {
    n.br.iter()
        .map(|b| {
            let ys = C::new(1.0, 0.0) / C::new(b[BR_R], b[BR_X]);
            let bc = b[BR_B];
            let ratio = if b[TAP] != 0.0 { b[TAP] } else { 1.0 };
            let tap = C::from_polar(ratio, PI / 180.0 * b[SHIFT]);
            let ytt = ys + C::new(0.0, bc / 2.0);
            [ytt / (tap * tap.conj()), -ys / tap.conj(), -ys / tap, ytt]
        })
        .collect()
}

fn make_ybus(n: &Net, y: &[[C; 4]]) -> Vec<C> {
    let nb = n.bus.len();
    let mut m = vec![C::new(0.0, 0.0); nb * nb];
    for (k, a) in y.iter().enumerate() {
        let (f, t) = (n.fb[k], n.tb[k]);
        m[f * nb + f] += a[0];
        m[f * nb + t] += a[1];
        m[t * nb + f] += a[2];
        m[t * nb + t] += a[3];
    }
    for (i, b) in n.bus.iter().enumerate() {
        m[i * nb + i] += C::new(b[GS], b[BS]) / n.base;
    }
    m
}

fn make_sbus(n: &Net) -> Vec<C> {
    let mut s: Vec<C> = n
        .bus
        .iter()
        .map(|b| -C::new(b[PD], b[QD]) / n.base)
        .collect();
    for (g, &b) in n.gen.iter().zip(&n.gbus) {
        s[b] += C::new(g[PG], g[QG]) / n.base;
    }
    s
}

fn matvec(y: &[C], v: &[C]) -> Vec<C> {
    let nb = v.len();
    (0..nb)
        .map(|i| {
            y[i * nb..(i + 1) * nb]
                .iter()
                .zip(v)
                .map(|(a, b)| a * b)
                .sum()
        })
        .collect()
}

/// Row `k..n` of column `k` with the largest magnitude: (row, |value|).
fn lu_pivot(a: &[f64], n: usize, k: usize) -> (usize, f64) {
    (k..n)
        .map(|i| (i, a[i * n + k].abs()))
        .fold((k, -1.0), |m, x| if x.1 > m.1 { x } else { m })
}

/// Eliminate column `k` below the pivot row.
fn lu_eliminate(a: &mut [f64], b: &mut [f64], n: usize, k: usize) {
    for i in k + 1..n {
        let f = a[i * n + k] / a[k * n + k];
        if f == 0.0 {
            continue;
        }
        for j in k..n {
            a[i * n + j] -= f * a[k * n + j];
        }
        b[i] -= f * b[k];
    }
}

/// Dense LU with partial pivoting; solves A x = b. Err when singular.
pub fn lu_solve(mut a: Vec<f64>, n: usize, mut b: Vec<f64>) -> Result<Vec<f64>, String> {
    let scale = a.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    for k in 0..n {
        let (p, pv) = lu_pivot(&a, n, k);
        if pv.is_nan() || pv <= 1e-13 * scale {
            return Err("singular matrix".into());
        }
        if p != k {
            for j in 0..n {
                a.swap(k * n + j, p * n + j);
            }
            b.swap(k, p);
        }
        lu_eliminate(&mut a, &mut b, n, k);
    }
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|j| a[i * n + j] * b[j]).sum();
        b[i] = (b[i] - s) / a[i * n + i];
    }
    Ok(b)
}

/// Power mismatch at the unknowns: P of pv+pq, then Q of pq.
fn mismatch(ybus: &[C], sbus: &[C], v: &[C], pv: &[usize], pq: &[usize]) -> Vec<f64> {
    let ib = matvec(ybus, v);
    let m: Vec<C> = (0..v.len())
        .map(|i| v[i] * ib[i].conj() - sbus[i])
        .collect();
    pv.iter()
        .chain(pq)
        .map(|&i| m[i].re)
        .chain(pq.iter().map(|&i| m[i].im))
        .collect()
}

/// Infinity norm; any non-finite entry makes it infinite.
fn norm_inf(f: &[f64]) -> f64 {
    f.iter().fold(0.0f64, |m, x| {
        if x.is_finite() {
            m.max(x.abs())
        } else {
            f64::INFINITY
        }
    })
}

/// Write `f(row, col)` into the block of `j` (row length `n`) starting at (`r0`, `c0`).
fn fill_block(
    j: &mut [f64],
    n: usize,
    at: (usize, usize),
    (rows, cols): (&[usize], &[usize]),
    f: impl Fn(usize, usize) -> f64,
) {
    for (a, &r) in rows.iter().enumerate() {
        for (b, &c) in cols.iter().enumerate() {
            j[(at.0 + a) * n + at.1 + b] = f(r, c);
        }
    }
}

/// Jacobian [dP/dVa dP/dVm; dQ/dVa dQ/dVm] from dSbus_dV, evaluated only for the entries it uses.
fn jacobian(ybus: &[C], v: &[C], pv: &[usize], pq: &[usize]) -> Vec<f64> {
    let nb = v.len();
    let ib = matvec(ybus, v);
    let vn: Vec<C> = v.iter().map(|x| x / x.norm()).collect();
    let dvm = |r: usize, c: usize| -> C {
        v[r] * (ybus[r * nb + c] * vn[c]).conj()
            + if r == c {
                ib[r].conj() * vn[r]
            } else {
                C::new(0.0, 0.0)
            }
    };
    let dva = |r: usize, c: usize| -> C {
        C::new(0.0, 1.0)
            * v[r]
            * ((if r == c { ib[r] } else { C::new(0.0, 0.0) }) - ybus[r * nb + c] * v[c]).conj()
    };
    let pvpq: Vec<usize> = pv.iter().chain(pq).copied().collect();
    let (np, nq) = (pvpq.len(), pq.len());
    let n = np + nq;
    let mut j = vec![0.0; n * n];
    fill_block(&mut j, n, (0, 0), (&pvpq, &pvpq), |r, c| dva(r, c).re);
    fill_block(&mut j, n, (0, np), (&pvpq, pq), |r, c| dvm(r, c).re);
    fill_block(&mut j, n, (np, 0), (pq, &pvpq), |r, c| dva(r, c).im);
    fill_block(&mut j, n, (np, np), (pq, pq), |r, c| dvm(r, c).im);
    j
}

/// Apply the Newton step `dx` (Va of pv+pq, then Vm of pq) to the voltage vector.
fn step_voltages(v: &[C], dx: &[f64], pv: &[usize], pq: &[usize]) -> Vec<C> {
    let (npv, npq) = (pv.len(), pq.len());
    let mut va: Vec<f64> = v.iter().map(|x| x.arg()).collect();
    let mut vm: Vec<f64> = v.iter().map(|x| x.norm()).collect();
    for (k, &b) in pv.iter().enumerate() {
        va[b] += dx[k];
    }
    for (k, &b) in pq.iter().enumerate() {
        va[b] += dx[npv + k];
        vm[b] += dx[npv + npq + k];
    }
    (0..v.len()).map(|k| C::from_polar(vm[k], va[k])).collect()
}

/// Newton's method (newtonpf). Returns (V, converged).
fn newtonpf(
    ybus: &[C],
    sbus: &[C],
    v0: &[C],
    (pv, pq): (&[usize], &[usize]),
    o: &Opts,
) -> Result<(Vec<C>, bool), String> {
    let mut v = v0.to_vec();
    let mut f = mismatch(ybus, sbus, &v, pv, pq);
    let mut converged = norm_inf(&f) < o.tol;
    let mut i = 0;
    while !converged && i < o.max_it && norm_inf(&f).is_finite() {
        i += 1;
        let j = jacobian(ybus, &v, pv, pq);
        let dx = lu_solve(j, f.len(), f.iter().map(|x| -x).collect())?;
        v = step_voltages(&v, &dx, pv, pq);
        f = mismatch(ybus, sbus, &v, pv, pq);
        converged = norm_inf(&f) < o.tol;
    }
    Ok((v, converged))
}

/// pfsoln, bus part: VM/VA of every bus.
fn bus_results(n: &mut Net, v: &[C]) {
    for (i, b) in n.bus.iter_mut().enumerate() {
        b[VM] = v[i].norm();
        b[VA] = v[i].arg() * 180.0 / PI;
    }
}

/// Gens on one bus share its reactive output in proportion to their Q ranges.
fn share_reactive(n: &mut Net) {
    let (nb, ng) = (n.bus.len(), n.gen.len());
    let mut cnt = vec![0.0; nb];
    for &b in &n.gbus {
        cnt[b] += 1.0;
    }
    for k in 0..ng {
        n.gen[k][QG] /= cnt[n.gbus[k]];
    }
    let (mut tot, mut mn, mut mx) = (vec![0.0; nb], vec![0.0; nb], vec![0.0; nb]);
    for k in 0..ng {
        let b = n.gbus[k];
        tot[b] += n.gen[k][QG];
        mn[b] += n.gen[k][QMIN];
        mx[b] += n.gen[k][QMAX];
    }
    let eps = f64::EPSILON;
    let saved: Vec<f64> = (0..ng).map(|k| n.gen[k][QG]).collect();
    for (k, &sv) in saved.iter().enumerate() {
        let b = n.gbus[k];
        n.gen[k][QG] = if mn[b] == mx[b] {
            sv
        } else {
            n.gen[k][QMIN]
                + (tot[b] - mn[b]) / (mx[b] - mn[b] + eps) * (n.gen[k][QMAX] - n.gen[k][QMIN])
        };
    }
}

/// The reference bus's first gen picks up the slack: its Pg is what the bus needs minus the other gens there.
fn slack_pg(n: &mut Net, sg: &[C], ref_: &[usize]) {
    for &r in ref_ {
        let rg: Vec<usize> = (0..n.gen.len()).filter(|&k| n.gbus[k] == r).collect();
        if let Some(&g0) = rg.first() {
            let mut pg = sg[g0].re * n.base + n.bus[r][PD];
            for &g in &rg[1..] {
                pg -= n.gen[g][PG];
            }
            n.gen[g0][PG] = pg;
        }
    }
}

fn branch_flows(n: &mut Net, y: &[[C; 4]], v: &[C]) {
    for (k, a) in y.iter().enumerate() {
        let (vf, vt) = (v[n.fb[k]], v[n.tb[k]]);
        let sf = vf * (a[0] * vf + a[1] * vt).conj() * n.base;
        let st = vt * (a[2] * vf + a[3] * vt).conj() * n.base;
        n.br[k][PF] = sf.re;
        n.br[k][QF] = sf.im;
        n.br[k][PT] = st.re;
        n.br[k][QT] = st.im;
    }
}

/// pfsoln: update VM/VA, Qg of all gens, Pg of the slack gen(s) and branch flows.
fn pfsoln(n: &mut Net, ybus: &[C], y: &[[C; 4]], v: &[C], ref_: &[usize]) {
    bus_results(n, v);
    let ib = matvec(ybus, v);
    let sg: Vec<C> = n.gbus.iter().map(|&b| v[b] * ib[b].conj()).collect();
    for (k, s) in sg.iter().enumerate() {
        n.gen[k][QG] = s.im * n.base + n.bus[n.gbus[k]][QD];
    }
    if n.gen.len() > 1 {
        share_reactive(n);
    }
    slack_pg(n, &sg, ref_);
    branch_flows(n, y, v);
}

/// makeBdc: branch susceptance matrix Bbus, per-branch b, and the phase-shifter injections.
struct Bdc {
    bm: Vec<f64>,
    b: Vec<f64>,
    pfinj: Vec<f64>,
    pbusinj: Vec<f64>,
}

fn make_bdc(n: &Net) -> Bdc {
    let nb = n.bus.len();
    let mut bm = vec![0.0; nb * nb];
    let (mut b, mut pfinj, mut pbusinj) = (vec![], vec![], vec![0.0; nb]);
    for (k, br) in n.br.iter().enumerate() {
        let tap = if br[TAP] != 0.0 { br[TAP] } else { 1.0 };
        let bk = 1.0 / br[BR_X] / tap;
        let pj = bk * (-br[SHIFT] * PI / 180.0);
        let (f, t) = (n.fb[k], n.tb[k]);
        bm[f * nb + f] += bk;
        bm[f * nb + t] -= bk;
        bm[t * nb + f] -= bk;
        bm[t * nb + t] += bk;
        pbusinj[f] += pj;
        pbusinj[t] -= pj;
        b.push(bk);
        pfinj.push(pj);
    }
    Bdc {
        bm,
        b,
        pfinj,
        pbusinj,
    }
}

/// dcpf: bus angles (radians) from the reduced B matrix; reference angles stay as given.
fn dc_angles(
    n: &Net,
    d: &Bdc,
    pbus: &[f64],
    ref_: &[usize],
    pvpq: &[usize],
) -> Result<Vec<f64>, String> {
    let nb = n.bus.len();
    let va0: Vec<f64> = n.bus.iter().map(|x| x[VA] * PI / 180.0).collect();
    let m = pvpq.len();
    let mut a = vec![0.0; m * m];
    for (i, &r) in pvpq.iter().enumerate() {
        for (j, &c) in pvpq.iter().enumerate() {
            a[i * m + j] = d.bm[r * nb + c];
        }
    }
    let rhs: Vec<f64> = pvpq
        .iter()
        .map(|&r| pbus[r] - ref_.iter().map(|&c| d.bm[r * nb + c] * va0[c]).sum::<f64>())
        .collect();
    let x = if m > 0 { lu_solve(a, m, rhs)? } else { vec![] };
    let mut va = va0.clone();
    for (i, &r) in pvpq.iter().enumerate() {
        va[r] = x[i];
    }
    Ok(va)
}

/// DC results: branch flows, VM = 1, VA in degrees.
fn dc_flows(n: &mut Net, d: &Bdc, va: &[f64]) {
    for k in 0..n.br.len() {
        let pf = (d.b[k] * (va[n.fb[k]] - va[n.tb[k]]) + d.pfinj[k]) * n.base;
        n.br[k][PF] = pf;
        n.br[k][PT] = -pf;
        n.br[k][QF] = 0.0;
        n.br[k][QT] = 0.0;
    }
    for (i, bus) in n.bus.iter_mut().enumerate() {
        bus[VM] = 1.0;
        bus[VA] = va[i] * 180.0 / PI;
    }
}

/// The first gen on each reference bus makes up the DC injection mismatch.
fn dc_slack(n: &mut Net, d: &Bdc, va: &[f64], pbus: &[f64], ref_: &[usize]) {
    let nb = n.bus.len();
    for &r in ref_ {
        if let Some(g) = (0..n.gen.len()).find(|&g| n.gbus[g] == r) {
            let inj: f64 = (0..nb).map(|c| d.bm[r * nb + c] * va[c]).sum();
            n.gen[g][PG] += (inj - pbus[r]) * n.base;
        }
    }
}

/// DC power flow (makeBdc + dcpf + the runpf DC branch).
fn dc(n: &mut Net, (ref_, pv, pq): (&[usize], &[usize], &[usize])) -> Result<(), String> {
    let d = make_bdc(n);
    let sbus = make_sbus(n);
    let nb = n.bus.len();
    let pbus: Vec<f64> = (0..nb)
        .map(|i| sbus[i].re - d.pbusinj[i] - n.bus[i][GS] / n.base)
        .collect();
    let pvpq: Vec<usize> = pv.iter().chain(pq).copied().collect();
    let va = dc_angles(n, &d, &pbus, ref_, &pvpq)?;
    dc_flows(n, &d, &va);
    dc_slack(n, &d, &va, &pbus, ref_);
    Ok(())
}

/// Starting voltages from the stored VM/VA, with generator set points on non-PQ buses.
fn initial_voltages(n: &Net, pq: &[usize]) -> Vec<C> {
    let mut v0: Vec<C> = n
        .bus
        .iter()
        .map(|b| {
            // a bus left at VM = 0 (de-energised earlier) restarts flat; PYPOWER would hit a singular Jacobian
            let (m, a) = if b[VM] > 0.0 {
                (b[VM], b[VA])
            } else {
                (1.0, 0.0)
            };
            C::from_polar(m, PI / 180.0 * a)
        })
        .collect();
    let orig = v0.clone();
    let mut is_pq = vec![false; v0.len()];
    for &i in pq {
        is_pq[i] = true;
    }
    for (g, &b) in n.gen.iter().zip(&n.gbus) {
        if !is_pq[b] {
            v0[b] = g[VG] / orig[b].norm() * orig[b];
        }
    }
    v0
}

/// AC solve of `net` in place; false when Newton did not converge.
fn solve_ac(
    net: &mut Net,
    (ref_, pv, pq): (&[usize], &[usize], &[usize]),
    o: &Opts,
) -> Result<bool, String> {
    let v0 = initial_voltages(net, pq);
    let y = branch_y(net);
    let ybus = make_ybus(net, &y);
    let sbus = make_sbus(net);
    let (v, ok) = newtonpf(&ybus, &sbus, &v0, (pv, pq), o)?;
    pfsoln(net, &ybus, &y, &v, ref_);
    Ok(ok)
}

/// Switch off the buses the reference buses cannot reach and rebuild the network without them.
fn drop_unreachable(
    case: &Case,
    off: &mut [bool],
    net: Net,
    sets: BusSets,
) -> Result<(Net, BusSets), String> {
    let seen = reachable(&net, &sets.0);
    if seen.iter().all(|s| *s) {
        return Ok((net, sets));
    }
    for (i, s) in seen.iter().enumerate() {
        if !s {
            off[net.bus_ext[i]] = true;
        }
    }
    let net = ext2int(case, off)?;
    let sets = bustypes(&net)?;
    Ok((net, sets))
}

/// int2ext: copy the solved internal rows back onto a copy of `case`; everything not solved reads zero.
fn int2ext(case: &Case, net: &Net, off: &[bool], deenergize: bool) -> Case {
    let mut out = case.clone();
    for (r, b) in out.bus.iter_mut().enumerate() {
        if off[r] || (deenergize && b[BUS_TYPE] == NONE) {
            b[VM] = 0.0;
            b[VA] = 0.0;
        }
    }
    for (i, b) in net.bus.iter().enumerate() {
        out.bus[net.bus_ext[i]][VM] = b[VM];
        out.bus[net.bus_ext[i]][VA] = b[VA];
    }
    for g in out.gen.iter_mut() {
        g[PG] = 0.0;
        g[QG] = 0.0;
    }
    for (k, g) in net.gen.iter().enumerate() {
        out.gen[net.gen_ext[k]][PG] = g[PG];
        out.gen[net.gen_ext[k]][QG] = g[QG];
    }
    for b in out.branch.iter_mut() {
        b[PF] = 0.0;
        b[QF] = 0.0;
        b[PT] = 0.0;
        b[QT] = 0.0;
    }
    for (k, b) in net.br.iter().enumerate() {
        let r = &mut out.branch[net.br_ext[k]];
        r[PF] = b[PF];
        r[QF] = b[QF];
        r[PT] = b[PT];
        r[QT] = b[QT];
    }
    out
}

/// runpf: solve `case` (not modified) and return the case with VM/VA, Pg/Qg and branch flows filled in.
/// `Err` means the problem could not be set up or the Jacobian was singular; `success == false` means no convergence.
pub fn runpf(case: &Case, o: &Opts) -> Result<Solved, String> {
    let mut off = vec![false; case.bus.len()];
    let mut net = ext2int(case, &off)?;
    let mut sets = bustypes(&net)?;
    if o.deenergize {
        (net, sets) = drop_unreachable(case, &mut off, net, sets)?;
    }
    let (ref_, pv, pq) = (&sets.0[..], &sets.1[..], &sets.2[..]);
    let success = if o.dc {
        dc(&mut net, (ref_, pv, pq))?;
        true
    } else {
        solve_ac(&mut net, (ref_, pv, pq), o)?
    };
    Ok(Solved {
        case: int2ext(case, &net, &off, o.deenergize),
        success,
    })
}
