//! The scan cycle (SPEC 3.2/3.4): logic, then the queued external writes to the provider, then the device-local ones.

use super::logic::Logic;
use super::store::Store;
use crate::model::Value;
use crate::transport::Requester;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// `(tag, point, value)`: a queued write as `Store::pending` returns it.
type Pending = (String, String, Value);

/// The `WRITE=` request for some pending points (SPEC 3.3): `point:value,...`, analog with 6 decimals, NUL-terminated.
fn request(points: &[Pending]) -> Vec<u8> {
    let items: Vec<String> = points
        .iter()
        .map(|(_, p, v)| match v {
            Value::Binary(b) => format!("{p}:{b}"),
            Value::Analog(x) => format!("{p}:{x:.6}"),
        })
        .collect();
    let mut r = format!("WRITE={}", items.join(",")).into_bytes();
    r.push(0);
    r
}

/// One `<output>`: a REQ client to the provider's REP.
pub struct Output {
    req: Box<dyn Requester>,
    down: bool,
}

impl Output {
    pub fn new(req: Box<dyn Requester>) -> Output {
        Output { req, down: false }
    }

    /// Send what the output owns. All points go in one request; if the provider answers `ERR=` they are retried one by one and the
    /// rejected ones dropped. No answer leaves everything queued for the next cycle, with one log line per outage.
    pub fn process(&mut self, store: &Store, owner: usize) -> Vec<String> {
        let pending = store.pending(owner);
        if pending.is_empty() {
            return vec![];
        }
        let mut notes = vec![];
        match self.req.call(&request(&pending)) {
            Ok(r) if r.starts_with(b"ACK=") => self.settle(store, &pending, &[]),
            Ok(_) => self.retry(store, &pending, &mut notes),
            Err(_) => self.outage(&mut notes),
        }
        notes
    }

    /// Ack every point of `pending` except those in `rejected`.
    fn settle(&mut self, store: &Store, pending: &[Pending], rejected: &[&str]) {
        self.down = false;
        for (tag, _, v) in pending {
            match rejected.contains(&tag.as_str()) {
                true => store.reject(tag, *v),
                false => store.ack(tag, *v),
            }
        }
    }

    fn retry(&mut self, store: &Store, pending: &[Pending], notes: &mut Vec<String>) {
        for p in pending {
            match self.req.call(&request(std::slice::from_ref(p))) {
                Ok(r) if r.starts_with(b"ACK=") => self.settle(store, std::slice::from_ref(p), &[]),
                Ok(_) => {
                    notes.push(format!(
                        "OutputModule: provider rejected {}",
                        crate::hint::show(&p.1)
                    ));
                    self.settle(store, std::slice::from_ref(p), &[&p.0]);
                }
                Err(_) => return self.outage(notes),
            }
        }
    }

    fn outage(&mut self, notes: &mut Vec<String>) {
        if !self.down {
            notes.push("E: OutputModule: provider not responding".into());
        }
        self.down = true;
    }
}

/// One cycle; returns the log lines to print.
pub fn cycle(
    logic: &mut Logic,
    outputs: &mut [Output],
    store: &Store,
    cycle_ms: u64,
) -> Vec<String> {
    let mut notes = logic.scan(store, cycle_ms);
    for (i, out) in outputs.iter_mut().enumerate() {
        notes.extend(out.process(store, i));
    }
    store.apply_internal();
    notes
}

/// Sleep `d`, waking early when `stop` is cancelled.
pub fn pause(d: Duration, stop: &CancellationToken) {
    let end = Instant::now() + d;
    while !stop.is_cancelled() && Instant::now() < end {
        std::thread::sleep(
            Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())),
        );
    }
}

/// Run cycles every `cycle_ms` (plus the work) until `stop`.
pub fn run(
    mut logic: Logic,
    mut outputs: Vec<Output>,
    store: Store,
    cycle_ms: u64,
    stop: CancellationToken,
) {
    while !stop.is_cancelled() {
        for n in cycle(&mut logic, &mut outputs, &store, cycle_ms) {
            tracing::warn!("{n}");
        }
        pause(Duration::from_millis(cycle_ms), &stop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::config::Opts;
    use crate::fd::store::Layout;
    use crate::model::Kind;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    type Replies = VecDeque<Result<Vec<u8>, String>>;

    struct Script(Arc<Mutex<Replies>>, Arc<Mutex<Vec<String>>>);

    impl Requester for Script {
        fn call(&mut self, req: &[u8]) -> Result<Vec<u8>, String> {
            self.1
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(req).into_owned());
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err("no reply".into()))
        }
    }

    type Sent = Arc<Mutex<Vec<String>>>;

    fn output(replies: Replies) -> (Output, Arc<Mutex<Replies>>, Sent) {
        let (r, sent) = (Arc::new(Mutex::new(replies)), Sent::default());
        (
            Output::new(Box::new(Script(r.clone(), sent.clone()))),
            r,
            sent,
        )
    }

    fn ack() -> Result<Vec<u8>, String> {
        Ok(b"ACK=\0".to_vec())
    }

    fn err() -> Result<Vec<u8>, String> {
        Ok(b"ERR=no\0".to_vec())
    }

    fn timeout() -> Result<Vec<u8>, String> {
        Err("timeout".into())
    }

    /// External analog `ta` -> `p.a` and binary `tb` -> `p.b` (both owned by output 0), plus internal analog `n`.
    fn store() -> Store {
        Store::new(Layout {
            points: vec![
                ("p.a".into(), Kind::Analog, Some(0)),
                ("p.b".into(), Kind::Binary, Some(0)),
            ],
            external: vec![("ta".into(), "p.a".into()), ("tb".into(), "p.b".into())],
            internal: vec![("n".into(), Value::Analog(0.0))],
        })
    }

    fn queue_both(s: &Store) {
        s.write("ta", Value::Analog(1.5)).unwrap();
        s.write("tb", Value::Binary(true)).unwrap();
    }

    #[test]
    fn request_format() {
        let p = |t: &str, v| (t.to_string(), format!("p.{t}"), v);
        assert_eq!(
            request(&[p("a", Value::Analog(1.5)), p("b", Value::Binary(false))]),
            b"WRITE=p.a:1.500000,p.b:false\0"
        );
        assert_eq!(request(&[p("b", Value::Binary(true))]), b"WRITE=p.b:true\0");
    }

    #[test]
    fn an_ack_sets_the_points_and_clears_the_queue() {
        let (mut o, _, sent) = output([ack()].into());
        let s = store();
        queue_both(&s);
        assert!(o.process(&s, 0).is_empty());
        assert_eq!(*sent.lock().unwrap(), ["WRITE=p.a:1.500000,p.b:true\0"]);
        assert_eq!((s.get_f64("ta"), s.get_bool("tb")), (Some(1.5), Some(true)));
        assert!(!s.is_queued("ta") && !s.is_queued("tb"));
    }

    #[test]
    fn nothing_pending_or_not_mine_sends_nothing() {
        let (mut o, _, sent) = output([].into());
        let s = store();
        assert!(o.process(&s, 0).is_empty());
        queue_both(&s);
        assert!(o.process(&s, 1).is_empty()); // both points belong to output 0
        s.write("n", Value::Analog(1.0)).unwrap();
        assert!(sent.lock().unwrap().is_empty());
    }

    #[test]
    fn an_err_retries_per_point_and_drops_the_rejected() {
        let (mut o, _, sent) = output([err(), ack(), err()].into());
        let s = store();
        queue_both(&s);
        let notes = o.process(&s, 0);
        assert_eq!(notes, ["OutputModule: provider rejected p.b"]);
        assert_eq!(sent.lock().unwrap().len(), 3);
        assert_eq!(sent.lock().unwrap()[1], "WRITE=p.a:1.500000\0");
        assert_eq!(
            (s.get_f64("ta"), s.get_bool("tb")),
            (Some(1.5), Some(false))
        );
        assert!(!s.is_queued("ta") && !s.is_queued("tb"));
    }

    #[test]
    fn no_reply_keeps_the_queue_and_logs_once_per_outage() {
        let (mut o, replies, _) = output([timeout(), timeout()].into());
        let s = store();
        queue_both(&s);
        assert_eq!(
            o.process(&s, 0),
            ["E: OutputModule: provider not responding"]
        );
        assert!(o.process(&s, 0).is_empty());
        assert!(s.is_queued("ta") && s.is_queued("tb"));
        assert_eq!(s.get_f64("ta"), Some(0.0));
        replies.lock().unwrap().extend([ack(), timeout()]);
        o.process(&s, 0); // back
        s.write("ta", Value::Analog(2.0)).unwrap();
        assert_eq!(
            o.process(&s, 0),
            ["E: OutputModule: provider not responding"]
        ); // a new outage
    }

    #[test]
    fn a_timeout_in_the_middle_of_the_retries_stops_them() {
        let (mut o, _, sent) = output([err(), timeout()].into());
        let s = store();
        queue_both(&s);
        assert_eq!(
            o.process(&s, 0),
            ["E: OutputModule: provider not responding"]
        );
        assert_eq!(sent.lock().unwrap().len(), 2);
        assert!(s.is_queued("ta") && s.is_queued("tb"));
    }

    #[test]
    fn a_cycle_runs_logic_then_outputs_then_local_writes() {
        let s = store();
        let kinds: HashMap<String, Kind> = [("ta", Kind::Analog), ("n", Kind::Analog)]
            .map(|(t, k)| (t.to_string(), k))
            .into();
        let mut logic = Logic::parse("ta = 4\nn = 6", &kinds, &Opts::default()).unwrap();
        let (o, _, sent) = output([ack()].into());
        let mut outs = [o];
        assert!(cycle(&mut logic, &mut outs, &s, 100).is_empty());
        assert_eq!(*sent.lock().unwrap(), ["WRITE=p.a:4.000000\0"]);
        assert_eq!((s.get_f64("ta"), s.get_f64("n")), (Some(4.0), Some(6.0)));
        assert_eq!(cycle(&mut logic, &mut outs, &s, 100), Vec::<String>::new());
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_cycle_returns_logic_and_output_notes() {
        let s = store();
        let kinds: HashMap<String, Kind> = [("n".to_string(), Kind::Analog)].into();
        let mut logic = Logic::parse("n = 1 % 0", &kinds, &Opts::default()).unwrap();
        let (o, _, _) = output([timeout()].into());
        s.write("ta", Value::Analog(1.0)).unwrap();
        let notes = cycle(&mut logic, &mut [o], &s, 100);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].starts_with("logic `n`"));
    }

    #[test]
    fn pause_waits_but_wakes_on_stop() {
        let stop = CancellationToken::new();
        let t = Instant::now();
        pause(Duration::from_millis(120), &stop);
        assert!(t.elapsed() >= Duration::from_millis(120));
        stop.cancel();
        let t = Instant::now();
        pause(Duration::from_secs(30), &stop);
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn run_cycles_until_stopped() {
        let s = store();
        let kinds: HashMap<String, Kind> = [("n".to_string(), Kind::Analog)].into();
        let logic = Logic::parse("n = 3", &kinds, &Opts::default()).unwrap();
        let stop = CancellationToken::new();
        let (s2, stop2) = (s.clone(), stop.clone());
        let h = std::thread::spawn(move || run(logic, vec![], s2, 20, stop2));
        let t = Instant::now();
        while s.get_f64("n") != Some(3.0) && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.cancel();
        h.join().unwrap();
        assert_eq!(s.get_f64("n"), Some(3.0));
    }
}
