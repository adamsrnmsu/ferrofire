//! pf::lu_solve (ww-1df.49): pins the behaviour of the dense elimination so the sparse one matches it.
use ferrofire::power::pf::lu_solve;

/// Deterministic xorshift in [-1, 1).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

fn matvec(a: &[f64], n: usize, x: &[f64]) -> Vec<f64> {
    (0..n)
        .map(|i| (0..n).map(|j| a[i * n + j] * x[j]).sum())
        .collect()
}

fn assert_solves(a: &[f64], n: usize, tol: f64) {
    let mut r = Rng(0x9e3779b97f4a7c15 ^ n as u64);
    let want: Vec<f64> = (0..n).map(|_| r.next() * 10.0).collect();
    let got = lu_solve(a.to_vec(), n, matvec(a, n, &want)).unwrap();
    for (w, g) in want.iter().zip(&got) {
        assert!((w - g).abs() < tol, "n={n}: want {w} got {g}");
    }
}

/// Random sparse matrix, `fill` off-diagonals per row, diagonal dominant unless `dominant` is false.
fn sparse(n: usize, fill: usize, seed: u64, dominant: bool) -> Vec<f64> {
    let mut r = Rng(seed);
    let mut a = vec![0.0; n * n];
    for i in 0..n {
        for _ in 0..fill {
            let j = ((r.next() + 1.0) / 2.0 * n as f64) as usize % n;
            a[i * n + j] = r.next();
        }
        a[i * n + i] = if dominant {
            4.0 * fill as f64 + 1.0
        } else {
            0.0
        };
    }
    a
}

#[test]
fn solves_random_sparse_systems() {
    for n in 1..40 {
        assert_solves(&sparse(n, 3, 7 + n as u64, true), n, 1e-9);
    }
    assert_solves(&sparse(300, 4, 99, true), 300, 1e-9);
}

#[test]
fn pivots_when_the_diagonal_is_zero() {
    // a permuted identity plus off-diagonal noise: every diagonal entry is zero
    let n = 12;
    let mut a = sparse(n, 2, 5, false);
    for i in 0..n {
        a[i * n + (i + 5) % n] = 3.0 + i as f64;
    }
    assert_solves(&a, n, 1e-9);
    // a small diagonal under a large off-diagonal in the same column
    assert_solves(&[1e-9, 1.0, 1.0, 1.0], 2, 1e-6);
}

#[test]
fn solves_a_network_like_matrix() {
    // laplacian of a 20 x 20 grid plus a shift: the shape of the reduced B matrix
    let w = 20;
    let n = w * w;
    let mut a = vec![0.0; n * n];
    for i in 0..n {
        for j in [i + 1, i + w]
            .into_iter()
            .filter(|&j| j < n && (j != i + 1 || j % w != 0))
        {
            a[i * n + j] = -1.0;
            a[j * n + i] = -1.0;
            a[i * n + i] += 1.0;
            a[j * n + j] += 1.0;
        }
        a[i * n + i] += 0.1;
    }
    assert_solves(&a, n, 1e-7);
}

#[test]
fn nothing_to_solve() {
    assert_eq!(lu_solve(vec![], 0, vec![]).unwrap(), Vec::<f64>::new());
    assert_eq!(lu_solve(vec![4.0], 1, vec![2.0]).unwrap(), vec![0.5]);
}

#[test]
fn singular_matrices_fail() {
    let sing = |a: Vec<f64>, n: usize| {
        let e = lu_solve(a, n, vec![1.0; n]).unwrap_err();
        assert_eq!(e, "singular matrix");
    };
    sing(vec![0.0; 9], 3); // all zero
    sing(vec![1.0, 2.0, 2.0, 4.0], 2); // dependent rows
    sing(vec![1.0, 0.0, 0.0, 0.0], 2); // zero column
    sing(vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0], 3); // zero row
    sing(vec![1.0, 1.0, 1.0, 1.0 + 1e-15], 2); // below 1e-13 of the largest entry
    sing(vec![f64::NAN, 0.0, 0.0, f64::NAN], 2);
    let mut a = sparse(30, 3, 3, true);
    for j in 0..30 {
        a[29 * 30 + j] = a[28 * 30 + j]; // duplicate row inside a bigger matrix
    }
    sing(a, 30);
}

#[test]
fn a_pivot_just_above_the_threshold_solves() {
    // the second pivot is 1e-12 of the scale: above 1e-13, so no error
    assert!(lu_solve(vec![1.0, 1.0, 1.0, 1.0 + 1e-12], 2, vec![1.0, 1.0]).is_ok());
}
