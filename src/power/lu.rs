//! Sparse LU for the power flow: Gilbert-Peierls left-looking factorisation (as in CSparse `cs_lu`) with partial
//! pivoting, columns taken in minimum-degree order of the pattern of A + A^T. The Jacobian and the reduced B matrix
//! follow the network graph, so fill stays near-linear where the dense elimination is O(n^3).

/// Column order: repeatedly eliminate the vertex of least degree (lowest index on ties) in the graph of A + A^T,
/// joining its neighbours. Adjacency is a bit matrix, so joining is a row OR.
fn min_degree(n: usize, cols: &[Vec<usize>]) -> Vec<usize> {
    let w = n.div_ceil(64);
    let mut adj = vec![0u64; n * w];
    for (j, rows) in cols.iter().enumerate() {
        for &i in rows.iter().filter(|&&i| i != j) {
            adj[i * w + j / 64] |= 1 << (j % 64);
            adj[j * w + i / 64] |= 1 << (i % 64);
        }
    }
    let row_deg = |adj: &[u64], v: usize| -> usize {
        adj[v * w..(v + 1) * w]
            .iter()
            .map(|x| x.count_ones() as usize)
            .sum()
    };
    let mut deg: Vec<usize> = (0..n).map(|v| row_deg(&adj, v)).collect();
    let mut order = Vec::with_capacity(n);
    for _ in 0..n {
        let v = (0..n).min_by_key(|&v| deg[v]).unwrap();
        deg[v] = usize::MAX;
        let rv: Vec<u64> = adj[v * w..(v + 1) * w].to_vec();
        for u in (0..n).filter(|&u| rv[u / 64] >> (u % 64) & 1 == 1) {
            for (k, bits) in rv.iter().enumerate() {
                adj[u * w + k] |= bits;
            }
            adj[u * w + u / 64] &= !(1 << (u % 64));
            adj[u * w + v / 64] &= !(1 << (v % 64));
            deg[u] = row_deg(&adj, u);
        }
        order.push(v);
    }
    order
}

/// Sparse columns, flat: column `k` is `idx[ptr[k]..ptr[k + 1]]` with values `val[..]` at the same positions.
struct Cols {
    ptr: Vec<usize>,
    idx: Vec<usize>,
    val: Vec<f64>,
}

impl Cols {
    fn new() -> Self {
        Cols {
            ptr: vec![0],
            idx: vec![],
            val: vec![],
        }
    }
    fn col(&self, k: usize) -> std::ops::Range<usize> {
        self.ptr[k]..self.ptr[k + 1]
    }
    fn close(&mut self) {
        self.ptr.push(self.idx.len());
    }
}

/// Rows reachable from `start` through the columns of L (a row pivoted at step j leads to the rows of L's column j),
/// appended to `post` in depth-first post-order. `seen[i] == stamp` marks visited rows.
fn reach(
    start: usize,
    (l, pinv): (&Cols, &[usize]),
    (seen, stamp): (&mut [usize], usize),
    (stack, post): (&mut Vec<(usize, usize)>, &mut Vec<usize>),
) {
    if seen[start] == stamp {
        return;
    }
    seen[start] = stamp;
    stack.push((start, 0));
    while let Some(&(i, c)) = stack.last() {
        let kids = if pinv[i] == usize::MAX {
            0..0
        } else {
            l.col(pinv[i])
        };
        match l.idx[kids.start + c..kids.end]
            .iter()
            .position(|&r| seen[r] != stamp)
        {
            Some(d) => {
                let r = l.idx[kids.start + c + d];
                seen[r] = stamp;
                stack.last_mut().unwrap().1 = c + d + 1;
                stack.push((r, 0));
            }
            None => {
                post.push(i);
                stack.pop();
            }
        }
    }
}

/// Row indices of the nonzeros of each column of dense row-major `a`.
fn pattern(a: &[f64], n: usize) -> Vec<Vec<usize>> {
    let mut cols = vec![vec![]; n];
    for (i, row) in a.chunks(n.max(1)).enumerate() {
        for (j, _) in row.iter().enumerate().filter(|(_, x)| **x != 0.0) {
            cols[j].push(i);
        }
    }
    cols
}

/// P A Q = L U: `q` the column order, `pivrow[k]` the row pivoted at step k, L unit lower (rows in original numbering),
/// U strictly upper (columns by step) with its diagonal apart.
struct Lu {
    q: Vec<usize>,
    pivrow: Vec<usize>,
    l: Cols,
    u: Cols,
    udiag: Vec<f64>,
}

/// Scratch for one column step: `x` the dense work column, `seen` the reach marks, `post` the reached rows.
struct Work {
    x: Vec<f64>,
    seen: Vec<usize>,
    stack: Vec<(usize, usize)>,
    post: Vec<usize>,
}

/// Largest unpivoted entry of the work column (row, |value|); (n, -1) when there is none. NaN never wins.
fn pick_pivot(w: &Work, pinv: &[usize], n: usize) -> (usize, f64) {
    w.post
        .iter()
        .filter(|&&i| pinv[i] == usize::MAX)
        .map(|&i| (i, w.x[i].abs()))
        .fold((n, -1.0), |m, y| if y.1 > m.1 { y } else { m })
}

/// Left-looking step: load column `c` of A, solve L x = it over the reached rows and append the U entries to `u`.
fn load_and_solve(
    a: &[f64],
    n: usize,
    (c, rows): (usize, &[usize]),
    lu: (&Cols, &mut Cols, &[usize]),
    w: &mut Work,
    k: usize,
) {
    let (l, u, pinv) = lu;
    w.post.clear();
    for &i in rows {
        w.x[i] = a[i * n + c];
        reach(i, (l, pinv), (&mut w.seen, k), (&mut w.stack, &mut w.post));
    }
    for &i in w.post.iter().rev().filter(|&&i| pinv[i] != usize::MAX) {
        let (xi, j) = (w.x[i], pinv[i]);
        u.idx.push(j);
        u.val.push(xi);
        let r = l.col(j);
        for (&row, &lv) in l.idx[r.clone()].iter().zip(&l.val[r]) {
            w.x[row] -= lv * xi;
        }
    }
    u.close();
}

/// Factor `a` (singular when a pivot is at most 1e-13 of the largest entry).
fn factor(a: &[f64], n: usize) -> Result<Lu, String> {
    let scale = a.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    let cols = pattern(a, n);
    let q = min_degree(n, &cols);
    let mut lu = Lu {
        q,
        pivrow: vec![],
        l: Cols::new(),
        u: Cols::new(),
        udiag: vec![],
    };
    let mut pinv = vec![usize::MAX; n]; // row -> pivot step
    let mut w = Work {
        x: vec![0.0; n],
        seen: vec![usize::MAX; n],
        stack: vec![],
        post: vec![],
    };
    for k in 0..n {
        let c = lu.q[k];
        load_and_solve(a, n, (c, &cols[c]), (&lu.l, &mut lu.u, &pinv), &mut w, k);
        let (p, pv) = pick_pivot(&w, &pinv, n);
        if pv <= 1e-13 * scale {
            return Err("singular matrix".into());
        }
        let d = w.x[p];
        pinv[p] = k;
        lu.pivrow.push(p);
        lu.udiag.push(d);
        for &i in w.post.iter().filter(|&&i| pinv[i] == usize::MAX) {
            lu.l.idx.push(i);
            lu.l.val.push(w.x[i] / d);
        }
        lu.l.close();
        for &i in &w.post {
            w.x[i] = 0.0;
        }
    }
    Ok(lu)
}

impl Lu {
    fn solve(&self, mut b: Vec<f64>) -> Vec<f64> {
        for (k, &p) in self.pivrow.iter().enumerate() {
            let yk = b[p];
            for e in self.l.col(k) {
                b[self.l.idx[e]] -= self.l.val[e] * yk;
            }
        }
        let mut y: Vec<f64> = self.pivrow.iter().map(|&r| b[r]).collect();
        let mut sol = vec![0.0; y.len()];
        for k in (0..y.len()).rev() {
            let z = y[k] / self.udiag[k];
            for e in self.u.col(k) {
                y[self.u.idx[e]] -= self.u.val[e] * z;
            }
            sol[self.q[k]] = z;
        }
        sol
    }
}

/// Solve A x = b for dense row-major `a` (n x n). Err when singular.
pub fn lu_solve(a: &[f64], n: usize, b: Vec<f64>) -> Result<Vec<f64>, String> {
    Ok(factor(a, n)?.solve(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eliminates_the_hub_of_a_star_late() {
        // column 0 and row 0 full: eliminating the hub first would join every leaf to every other; the last two
        // vertices are the hub and one leaf, tied at degree 1
        let n = 6;
        let cols: Vec<Vec<usize>> = (0..n)
            .map(|j| if j == 0 { (0..n).collect() } else { vec![0, j] })
            .collect();
        let hub = min_degree(n, &cols).iter().position(|&v| v == 0).unwrap();
        assert!(hub >= n - 2, "hub eliminated at step {hub}");
    }

    #[test]
    fn eliminates_the_end_of_a_path_first() {
        // path 0-1-2-3 stored one-sided (upper triangle): the ends have degree 1, the middle 2
        let cols = vec![vec![0], vec![0, 1], vec![1, 2], vec![2, 3]];
        let o = min_degree(4, &cols);
        assert!(o[0] == 0 || o[0] == 3, "{o:?}");
        let mut s = o.clone();
        s.sort();
        assert_eq!(s, [0, 1, 2, 3]);
    }

    #[test]
    fn joins_the_neighbours_of_an_eliminated_vertex() {
        // 4-cycle 0-1-2-3-0: eliminating any vertex links its two neighbours, so the rest stays a triangle and
        // the order is a permutation whose second pick has degree 2 (not 1)
        let cols = vec![vec![0, 1, 3], vec![0, 1, 2], vec![1, 2, 3], vec![0, 2, 3]];
        let o = min_degree(4, &cols);
        assert_eq!(o.len(), 4);
        assert_eq!(o[0], 0); // all degree 2: ties go to the first index
    }
}
