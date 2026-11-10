//! Small helpers for failing softly: "did you mean" for misspelt names, a bounded echo of untrusted text,
//! a rate limiter for repeated warnings and a mutex lock that survives a panicked holder.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Edit distance counting a swap of two neighbouring chars as one edit (optimal string alignment): `hieght` -> `height` is 1.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut rows = vec![(0..=b.len()).collect::<Vec<usize>>(); 3]; // [two back, previous, current]
    for i in 1..=a.len() {
        rows[2][0] = i;
        for j in 1..=b.len() {
            let sub = rows[1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut d = sub.min(rows[1][j] + 1).min(rows[2][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(rows[0][j - 2] + 1);
            }
            rows[2][j] = d;
        }
        rows.rotate_left(1); // previous becomes two back, current becomes previous
        rows[2] = rows[1].clone();
    }
    rows[1][b.len()]
}

/// ` (did you mean `x`?)` for the closest candidate within a third of the name's length (at least 1), else "".
pub fn did_you_mean<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> String {
    let max = (name.chars().count() / 3).max(1);
    candidates
        .into_iter()
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, c)| *d <= max && *c != name)
        .min_by_key(|(d, _)| *d)
        .map_or(String::new(), |(_, c)| format!(" (did you mean `{c}`?)"))
}

/// Untrusted text for an error message: at most 64 chars, control characters shown as `?`.
pub fn show(s: &str) -> String {
    let mut out: String = s
        .chars()
        .take(64)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if s.chars().nth(64).is_some() {
        out.push_str("...");
    }
    out
}

/// Lets one event in per `every`; the rest are counted by the caller.
pub struct RateLimit {
    every: Duration,
    last: Mutex<Option<Instant>>,
}

impl RateLimit {
    pub fn new(every: Duration) -> RateLimit {
        RateLimit {
            every,
            last: Mutex::new(None),
        }
    }

    /// True when `now` is at least `every` after the last time this returned true (or never was).
    pub fn allow(&self, now: Instant) -> bool {
        let mut last = lock(&self.last);
        let ok = last.is_none_or(|t| now.saturating_duration_since(t) >= self.every);
        if ok {
            *last = Some(now);
        }
        ok
    }
}

/// Lock, ignoring poison: a handler that panicked must not take the engine and every later request with it.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_and_suggestions() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "ab"), 2);
        assert_eq!(edit_distance("ab", ""), 2);
        assert_eq!(edit_distance("lock.hieght", "lock.height"), 1);
        let tags = ["lock.height", "lowervalve.open", "uppergate.open"];
        let d = |n| did_you_mean(n, tags);
        assert_eq!(d("lock.hieght"), " (did you mean `lock.height`?)");
        assert_eq!(d("lowervalve.opn"), " (did you mean `lowervalve.open`?)");
        assert_eq!(d("nothing.like.it"), "");
        assert_eq!(d("lock.height"), ""); // exact: nothing to suggest
        assert_eq!(d("x"), ""); // one char off from nothing here
    }

    #[test]
    fn show_bounds_and_cleans() {
        assert_eq!(show("a\0b\n"), "a?b?");
        assert_eq!(show(&"x".repeat(100)), format!("{}...", "x".repeat(64)));
        assert_eq!(show(&"x".repeat(64)), "x".repeat(64));
    }

    #[test]
    fn rate_limit_lets_one_per_window() {
        let r = RateLimit::new(Duration::from_secs(10));
        let t = Instant::now();
        assert!(r.allow(t));
        assert!(!r.allow(t + Duration::from_secs(9)));
        assert!(r.allow(t + Duration::from_secs(10)));
    }

    #[test]
    fn lock_survives_a_poisoned_mutex() {
        let m = std::sync::Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("holder died");
        })
        .join();
        assert!(m.is_poisoned());
        assert_eq!(*lock(&m), 1);
    }
}
