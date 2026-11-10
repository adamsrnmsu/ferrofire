//! `ffbench pull`: N simulated RTUs against one provider, for the push-vs-pull comparison (ww-otj.11).
//! `--mode pull`: each RTU is a real `ferrofire pull-bridge` process (K tags: sim.t plus K-1 others) republishing on loopback,
//! with a local subscriber in front of it that plays the stock field device. `--mode push` (nng): each RTU is a plain SUB0
//! subscriber of the provider's PUB0, so it receives all tags. JSON per RTU (`pub_stats`) plus a merged-age block on stdout.
use crate::{arg, now_ns, stats, sub_nng, sub_zmq, Cycle, Req, SubLog};
use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const BASE_PORT: u16 = 41000;

/// RTU `i`'s tag list: sim.t (the staleness probe) then K-1 tags taken round-robin from the rest, disjoint between RTUs while they fit.
pub fn rtu_tags(all: &[String], i: usize, k: usize) -> Vec<String> {
    let rest: Vec<&String> = all.iter().filter(|t| *t != "sim.t").collect();
    let mut tags = vec!["sim.t".to_string()];
    tags.extend((0..k - 1).map(|j| rest[(i * (k - 1) + j) % rest.len()].clone()));
    tags
}

fn spawn_bridge(a: &Args, i: usize, tags: &[String]) -> Child {
    let port = BASE_PORT + i as u16;
    let publish = if a.transport == "zmq" { format!("udp://127.0.0.1:{port}") } else { format!("tcp://127.0.0.1:{port}") };
    Command::new(&a.ff)
        .args(["pull-bridge", "--transport", &a.transport, "--server", &format!("tcp://{}:5555", a.host)])
        .args(["--publish", &publish, "--tags", &tags.join(","), "--rate", &format!("{}", (a.period * 1000.0).round() as u64)])
        .stdout(Stdio::null())
        .spawn()
        .unwrap()
}

struct Args {
    mode: String,
    transport: String,
    host: String,
    rtus: usize,
    k: usize,
    period: f64,
    ff: String,
}

fn subscriber(a: &Args, i: usize, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<SubLog> {
    let (port, tr, host, push) = (BASE_PORT + i as u16, a.transport.clone(), a.host.clone(), a.mode == "push");
    std::thread::spawn(move || match (push, tr.as_str()) {
        (true, _) => sub_nng(&format!("tcp://{host}:5556"), stop),
        (_, "zmq") => sub_zmq(port, false, stop),
        _ => sub_nng(&format!("tcp://127.0.0.1:{port}"), stop),
    })
}

pub fn run(args: &[String]) {
    let a = Args {
        mode: arg(args, "--mode", "pull"),
        transport: arg(args, "--transport", "zmq"),
        host: arg(args, "--host", "ffb-prov"),
        rtus: arg(args, "--rtus", "8").parse().unwrap(),
        k: arg(args, "--k", "50").parse().unwrap(),
        period: arg(args, "--period", "0.1").parse().unwrap(),
        ff: arg(args, "--ff", "/src/target-linux/release/ferrofire"),
    };
    let pub_s: f64 = arg(args, "--pub-secs", "30").parse().unwrap();
    assert!(a.mode == "pull" || a.transport == "nng", "push mode is nng only: zmq multicast is one copy however many subscribers");
    let req = Req::connect(&a.transport, &a.host);
    let all: Vec<String> = req.call("QUERY=").trim_start_matches("ACK=").split(',').filter(|t| !t.is_empty()).map(String::from).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let subs: Vec<_> = (0..a.rtus).map(|i| subscriber(&a, i, stop.clone())).collect();
    let bridges: Vec<Child> = match a.mode.as_str() {
        "pull" => (0..a.rtus).map(|i| spawn_bridge(&a, i, &rtu_tags(&all, i, a.k))).collect(),
        _ => vec![],
    };
    std::thread::sleep(Duration::from_secs(3));
    let start = now_ns();
    std::thread::sleep(Duration::from_secs_f64(pub_s));
    let win = (start, now_ns());
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    for mut b in bridges {
        unsafe { libc::kill(b.id() as i32, libc::SIGTERM) };
        let _ = b.wait();
    }
    let logs: Vec<SubLog> = subs.into_iter().map(|h| h.join().unwrap()).collect();
    let n = if a.mode == "pull" { a.k } else { all.len() };
    let per_rtu: Vec<_> = logs.iter().map(|l| stats::pub_stats(&l.cycles, win, n, a.period)).collect();
    let merged: Vec<Cycle> = logs.iter().flat_map(|l| l.cycles.clone()).collect();
    let out = json!({
        "mode": a.mode, "transport": a.transport, "rtus": a.rtus, "k": a.k, "tags_per_message": n, "period_s": a.period,
        "phases": {"publish": {"start_ns": win.0, "end_ns": win.1}},
        "rtu": per_rtu, "merged_age": stats::pub_stats(&merged, win, n, a.period)["age_above_floor"].clone(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[cfg(test)]
mod tests {
    use super::rtu_tags;
    #[test]
    fn rtus_get_sim_t_plus_disjoint_slices() {
        let all: Vec<String> = ["sim.t", "a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
        assert_eq!(rtu_tags(&all, 0, 3), ["sim.t", "a", "b"]);
        assert_eq!(rtu_tags(&all, 1, 3), ["sim.t", "c", "d"]);
        assert_eq!(rtu_tags(&all, 2, 3), ["sim.t", "e", "a"]); // wraps
    }
}
