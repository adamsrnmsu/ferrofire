//! Percentiles and publish-cycle statistics.
use crate::Cycle;
use serde_json::{json, Value};

/// RADIO datagram `[u8 group_len][group][body]` -> body.
pub fn unframe(d: &[u8]) -> Option<&[u8]> {
    let n = *d.first()? as usize;
    d.get(1 + n..)
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    sorted[(((sorted.len() - 1) as f64) * p).round() as usize]
}

/// n, mean, stdev, p50/p95/p99/max of nanosecond samples, reported in microseconds.
pub fn summary_us(ns: &mut [u64]) -> Value {
    if ns.is_empty() {
        return json!({"n": 0});
    }
    ns.sort_unstable();
    let n = ns.len() as f64;
    let mean = ns.iter().map(|&x| x as f64).sum::<f64>() / n;
    let var = ns.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n;
    let us = |x: u64| x as f64 / 1e3;
    json!({"n": ns.len(), "mean_us": mean / 1e3, "stdev_us": var.sqrt() / 1e3, "p50_us": us(pct(ns, 0.5)),
           "p95_us": us(pct(ns, 0.95)), "p99_us": us(pct(ns, 0.99)), "max_us": us(ns[ns.len() - 1]), "min_us": us(ns[0])})
}

/// Publish statistics for the cycles whose first chunk arrived inside `w` (epoch ns).
/// `age` = last-chunk receipt time minus (sim.t + E0), E0 chosen so the smallest age in the run is 0: it measures
/// freshness variation (engine tick + publish scheduling + transport), not an absolute one-way latency.
pub fn pub_stats(all: &[Cycle], w: (u64, u64), n: usize, period: f64) -> Value {
    let cs: Vec<&Cycle> = all.iter().filter(|c| c.first_ns >= w.0 && c.first_ns < w.1).collect();
    let secs = (w.1 - w.0) as f64 / 1e9;
    let complete: Vec<&&Cycle> = cs.iter().filter(|c| c.tags as usize == n).collect();
    let mut gaps: Vec<u64> = cs.windows(2).map(|p| p[1].first_ns - p[0].first_ns).collect();
    let mut spread: Vec<u64> = complete.iter().map(|c| c.last_ns - c.first_ns).collect();
    let off = |c: &Cycle| c.last_ns as f64 - c.simt * 1e9;
    let floor = all.iter().filter(|c| c.tags as usize == n).map(|c| off(c)).fold(f64::INFINITY, f64::min);
    let mut age: Vec<u64> = complete.iter().map(|c| (off(c) - floor).max(0.0) as u64).collect();
    let chunks: u32 = cs.iter().map(|c| c.chunks).sum();
    let bytes: u64 = cs.iter().map(|c| c.bytes as u64).sum();
    let expected = (secs / period).round();
    json!({
        "seconds": secs, "cycles": cs.len(), "complete_cycles": complete.len(), "expected_cycles": expected,
        "cycles_per_s": cs.len() as f64 / secs, "chunks_per_cycle": chunks as f64 / cs.len().max(1) as f64,
        "loss_pct": 100.0 * (1.0 - complete.len() as f64 / expected).max(0.0),
        "payload_bytes_per_s": bytes as f64 / secs, "tags_per_s": cs.iter().map(|c| c.tags as f64).sum::<f64>() / secs,
        "interarrival": summary_us(&mut gaps), "cycle_spread": summary_us(&mut spread), "age_above_floor": summary_us(&mut age),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unframe_strips_group() {
        assert_eq!(unframe(&[2, b'a', b'b', b'x', 0]), Some(&[b'x', 0][..]));
        assert_eq!(unframe(&[9, 1]), None);
    }
    #[test]
    fn percentiles() {
        let mut v: Vec<u64> = (1..=100).map(|x| x * 1000).collect();
        let s = summary_us(&mut v);
        assert_eq!(s["p50_us"], 51.0);
        assert_eq!(s["max_us"], 100.0);
    }
    #[test]
    fn loss_counts_incomplete_cycles() {
        let c = |t, tags| Cycle { first_ns: t, last_ns: t + 10, chunks: 1, tags, bytes: 1, simt: 0.0 };
        let cs = [c(0, 5), c(100_000_000, 3), c(200_000_000, 5)];
        let s = pub_stats(&cs, (0, 300_000_000), 5, 0.1);
        assert_eq!(s["complete_cycles"], 2);
        assert!((s["loss_pct"].as_f64().unwrap() - 100.0 / 3.0).abs() < 1e-6);
    }
}
