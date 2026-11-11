//! The bounded queue between a producer that must never wait (the engine tick, the datagram receive loop) and the worker threads
//! that talk to the server. Full means the oldest item is dropped and counted; the workers retry a failed batch with backoff
//! while the queue keeps absorbing (and, past its bound, dropping) new items.

use super::http::Http;
use super::{backoff, bulk_body, judge, Config, Doc};
use crate::hint::lock;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Most documents and most bytes of source in one `_bulk` request (Elasticsearch refuses requests over 100 MB by default, proxies often much less).
const MAX_DOCS: usize = 1000;
const MAX_BYTES: usize = 8 << 20;
/// How long a worker waits for items before looking at the stop flag again.
const POLL: Duration = Duration::from_millis(500);
/// After `finish`, how long the workers keep trying before they give up on what is left.
const GRACE: Duration = Duration::from_secs(10);

struct Inner<T> {
    items: VecDeque<T>,
    closed: bool,
}

pub struct Queue<T> {
    inner: Mutex<Inner<T>>,
    ready: Condvar,
    cap: usize,
    dropped: AtomicU64,
}

impl<T> Queue<T> {
    pub fn new(cap: usize) -> Queue<T> {
        Queue {
            inner: Mutex::new(Inner {
                items: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
            cap,
            dropped: AtomicU64::new(0),
        }
    }

    /// Add an item; never waits for the consumers. When full the oldest item is dropped (and counted).
    pub fn push(&self, t: T) {
        let mut q = lock(&self.inner);
        if q.items.len() >= self.cap {
            q.items.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.items.push_back(t);
        drop(q);
        self.ready.notify_one();
    }

    /// Up to `max` items, waiting up to `wait` for the first. Empty on timeout, and once closed and drained.
    pub fn pop_batch(&self, max: usize, wait: Duration) -> Vec<T> {
        let mut q = lock(&self.inner);
        if q.items.is_empty() && !q.closed {
            q = self
                .ready
                .wait_timeout(q, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let n = q.items.len().min(max);
        q.items.drain(..n).collect()
    }

    /// No more items will be pushed: consumers finish what is queued, then `drained` is true.
    pub fn close(&self) {
        lock(&self.inner).closed = true;
        self.ready.notify_all();
    }

    pub fn drained(&self) -> bool {
        let q = lock(&self.inner);
        q.closed && q.items.is_empty()
    }

    pub fn len(&self) -> usize {
        lock(&self.inner).items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Items dropped so far because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Counters the workers keep; read them after `finish`.
#[derive(Default)]
pub struct Stats {
    /// Documents the server accepted.
    pub sent: AtomicU64,
    /// Documents the server refused for good (a mapping conflict), or abandoned at shutdown (queued items not yet turned into documents
    /// count one each).
    pub lost: AtomicU64,
    /// Requests that failed and were tried again.
    pub retries: AtomicU64,
}

/// Turns a queued item into documents; the `Duration` is the time the batch is sent (`event.ingested`).
pub type Build<T> = Arc<dyn Fn(T, Duration) -> Vec<Doc> + Send + Sync>;

pub struct Sink<T> {
    pub queue: Arc<Queue<T>>,
    pub stats: Arc<Stats>,
    end: Arc<End>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

/// When the workers must stop trying: unset while running, a moment after `finish`.
#[derive(Default)]
struct End(Mutex<Option<Instant>>);

impl End {
    fn set(&self, at: Instant) {
        *lock(&self.0) = Some(at);
    }

    fn passed(&self) -> bool {
        lock(&self.0).is_some_and(|t| Instant::now() >= t)
    }
}

struct Worker<T> {
    http: Http,
    queue: Arc<Queue<T>>,
    stats: Arc<Stats>,
    end: Arc<End>,
    build: Build<T>,
    /// Items taken from the queue at a time.
    pop: usize,
    /// The index template, installed once before the first document by whichever worker gets here first.
    template: Arc<Mutex<Option<(String, serde_json::Value)>>>,
    seen_drops: u64,
}

/// `docs` cut into runs of at most `max_docs` documents and `max_bytes` of source each (one bigger document is a run of its own).
pub fn runs(docs: &[Doc], max_docs: usize, max_bytes: usize) -> Vec<&[Doc]> {
    let (mut out, mut start, mut bytes) = (vec![], 0, 0);
    for (i, d) in docs.iter().enumerate() {
        let n = d.source.len() + d.index.len();
        if i > start && (i - start >= max_docs || bytes + n > max_bytes) {
            out.push(&docs[start..i]);
            (start, bytes) = (i, 0);
        }
        bytes += n;
    }
    if start < docs.len() {
        out.push(&docs[start..]);
    }
    out
}

/// Sleep `d`, ending early once the grace period after `finish` is over.
fn nap(d: Duration, end: &End) {
    let until = Instant::now() + d;
    while !end.passed() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50).min(d));
    }
}

impl<T: Send + 'static> Sink<T> {
    /// Start the workers. `tpl` is `(index template name, body)`: installed before any document is sent. `pop` is how many queued items a
    /// worker takes at a time: many for items that are one document, few for items that are many (a provider snapshot).
    pub fn start(
        c: &Config,
        tpl: Option<(String, serde_json::Value)>,
        pop: usize,
        build: Build<T>,
    ) -> Result<Sink<T>, String> {
        let queue = Arc::new(Queue::new(c.queue));
        let (stats, end) = (Arc::new(Stats::default()), Arc::new(End::default()));
        let tpl = Arc::new(Mutex::new(tpl));
        let mut workers = vec![];
        for _ in 0..c.threads {
            let w = Worker {
                http: Http::new(c)?,
                queue: queue.clone(),
                stats: stats.clone(),
                end: end.clone(),
                build: build.clone(),
                pop,
                template: tpl.clone(),
                seen_drops: 0,
            };
            workers.push(std::thread::spawn(move || w.run()));
        }
        Ok(Sink {
            queue,
            stats,
            end,
            workers: Mutex::new(workers),
        })
    }

    /// Queue an item; never waits.
    pub fn push(&self, t: T) {
        self.queue.push(t);
    }

    /// Send what is queued, for at most a grace period, then stop the workers. Calling it again does nothing.
    pub fn finish(&self) {
        self.finish_within(GRACE);
    }

    /// `finish` with a grace period of `grace`.
    pub fn finish_within(&self, grace: Duration) {
        self.end.set(Instant::now() + grace);
        self.queue.close();
        for w in lock(&self.workers).drain(..) {
            let _ = w.join();
        }
    }
}

impl<T> Worker<T> {
    fn run(mut self) {
        while !self.queue.drained() {
            if self.end.passed() {
                self.stats.lost.fetch_add(
                    self.queue.pop_batch(usize::MAX, Duration::ZERO).len() as u64,
                    Ordering::Relaxed,
                );
                break;
            }
            self.note_drops();
            let items = self.queue.pop_batch(self.pop, POLL);
            if items.is_empty() {
                continue;
            }
            self.install_template();
            let ingested = super::now();
            let docs: Vec<Doc> = items
                .into_iter()
                .flat_map(|i| (self.build)(i, ingested))
                .collect();
            for run in runs(&docs, MAX_DOCS, MAX_BYTES) {
                self.deliver(run.to_vec());
            }
        }
    }

    /// Say so (once per change) when the queue has dropped items since the last look.
    fn note_drops(&mut self) {
        let d = self.queue.dropped();
        if d > self.seen_drops {
            tracing::warn!(
                "elastic: queue full, dropped the oldest {} items ({d} in all); the server is slower than the producer or unreachable",
                d - self.seen_drops
            );
            self.seen_drops = d;
        }
    }

    /// PUT the template; retried with backoff until it is accepted (or the sink is finishing). Other workers wait here.
    fn install_template(&self) {
        let mut slot = lock(&self.template);
        let mut attempt = 0;
        while let Some((name, body)) = slot.as_ref() {
            match self.http.request(
                "PUT",
                &format!("/_index_template/{name}"),
                "application/json",
                body.to_string().as_bytes(),
            ) {
                Ok((200..=299, _)) => *slot = None,
                other => {
                    tracing::warn!(
                        "elastic: installing index template `{name}` failed ({}), retrying",
                        describe(&other)
                    );
                    if self.end.passed() {
                        *slot = None; // finishing: send the documents anyway, dynamic mapping takes over
                    } else {
                        nap(backoff(attempt), &self.end);
                        attempt += 1;
                    }
                }
            }
        }
    }

    /// Send `docs` until the server has taken or refused every one (or the grace period after `finish` is over).
    fn deliver(&mut self, mut docs: Vec<Doc>) {
        let mut attempt = 0;
        while !docs.is_empty() {
            let result = self.http.request(
                "POST",
                "/_bulk",
                "application/x-ndjson",
                bulk_body(&docs).as_bytes(),
            );
            if let Ok((400 | 413, _)) = result {
                return self.split(docs, &result);
            }
            let verdict = match &result {
                Ok((200..=299, body)) => judge(body),
                _ => None,
            };
            match self.account(&docs, verdict) {
                Some(again) => docs = again.into_iter().map(|i| docs[i].clone()).collect(),
                None => tracing::warn!("elastic: bulk request failed ({})", describe(&result)),
            }
            if docs.is_empty() {
                return;
            }
            self.note_drops();
            if self.end.passed() {
                self.stats
                    .lost
                    .fetch_add(docs.len() as u64, Ordering::Relaxed);
                tracing::warn!("elastic: gave up on {} documents at shutdown", docs.len());
                return;
            }
            self.stats.retries.fetch_add(1, Ordering::Relaxed);
            nap(backoff(attempt), &self.end);
            attempt += 1;
        }
    }

    /// The server refused the whole request as malformed (400) or too large (413): sending it again would be refused again. Send the
    /// halves instead; a single document that is still refused is lost, and counted.
    fn split(&mut self, mut docs: Vec<Doc>, why: &Result<(u16, Vec<u8>), String>) {
        if docs.len() == 1 {
            self.stats.lost.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("elastic: a document was refused ({})", describe(why));
            return;
        }
        let tail = docs.split_off(docs.len() / 2);
        self.deliver(docs);
        self.deliver(tail);
    }

    /// Count what the server took and refused; the positions to send again, or `None` when the response was not a bulk response.
    fn account(
        &self,
        docs: &[Doc],
        verdict: Option<(Vec<usize>, Vec<String>)>,
    ) -> Option<Vec<usize>> {
        let (again, refused) = verdict?;
        let taken = docs.len() - again.len() - refused.len();
        self.stats.sent.fetch_add(taken as u64, Ordering::Relaxed);
        self.stats
            .lost
            .fetch_add(refused.len() as u64, Ordering::Relaxed);
        if let Some(first) = refused.first() {
            tracing::warn!(
                "elastic: {} documents refused, first: {first}",
                refused.len()
            );
        }
        Some(again)
    }
}

fn describe(r: &Result<(u16, Vec<u8>), String>) -> String {
    match r {
        Ok((code, body)) => format!(
            "HTTP {code}: {}",
            String::from_utf8_lossy(&body[..body.len().min(200)])
        ),
        Err(e) => e.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_queue_drops_the_oldest_and_counts() {
        let q = Queue::new(3);
        for i in 0..5 {
            q.push(i);
        }
        assert_eq!((q.len(), q.dropped()), (3, 2));
        assert_eq!(q.pop_batch(10, Duration::ZERO), vec![2, 3, 4]);
        assert!(q.is_empty());
    }

    #[test]
    fn pop_batch_takes_at_most_max_and_times_out_empty() {
        let q = Queue::new(10);
        (0..5).for_each(|i| q.push(i));
        assert_eq!(q.pop_batch(2, Duration::ZERO), vec![0, 1]);
        assert_eq!(q.pop_batch(9, Duration::ZERO), vec![2, 3, 4]);
        let t = Instant::now();
        assert!(q.pop_batch(1, Duration::from_millis(30)).is_empty());
        assert!(t.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn push_wakes_a_waiting_consumer() {
        let q = Arc::new(Queue::new(10));
        let q2 = q.clone();
        let h = std::thread::spawn(move || q2.pop_batch(10, Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(50));
        q.push(7);
        assert_eq!(h.join().unwrap(), vec![7]);
    }

    #[test]
    fn close_lets_consumers_drain_then_stop() {
        let q = Queue::new(10);
        q.push(1);
        assert!(!q.drained());
        q.close();
        assert!(!q.drained()); // one item left
        assert_eq!(q.pop_batch(10, Duration::from_secs(10)), vec![1]); // closed: no waiting
        assert!(q.drained());
        let t = Instant::now();
        assert!(q.pop_batch(10, Duration::from_secs(10)).is_empty());
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_stuck_consumer_never_blocks_the_producer() {
        let q = Queue::new(100);
        let t = Instant::now();
        for i in 0..100_000 {
            q.push(i);
        }
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!((q.len(), q.dropped()), (100, 99_900));
    }

    #[test]
    fn nap_ends_early_after_the_grace_period_and_sleeps_otherwise() {
        let over = End::default();
        over.set(Instant::now());
        let t = Instant::now();
        nap(Duration::from_secs(10), &over);
        assert!(t.elapsed() < Duration::from_secs(1));
        let go = End::default();
        assert!(!go.passed());
        let t = Instant::now();
        nap(Duration::from_millis(120), &go);
        assert!(t.elapsed() >= Duration::from_millis(100));
    }

    fn d(n: usize) -> Doc {
        Doc {
            index: "i".into(),
            source: "x".repeat(n - 1), // n bytes with the index name
        }
    }

    #[test]
    fn runs_are_cut_by_count_and_by_bytes() {
        let docs: Vec<Doc> = (0..7).map(|_| d(10)).collect();
        let sizes = |m: usize, b: usize| {
            runs(&docs, m, b)
                .iter()
                .map(|r| r.len())
                .collect::<Vec<_>>()
        };
        assert_eq!(sizes(3, 1000), vec![3, 3, 1]);
        assert_eq!(sizes(7, 1000), vec![7]);
        assert_eq!(sizes(1000, 25), vec![2, 2, 2, 1]); // 20 bytes fit, 30 do not
        assert_eq!(sizes(1000, 30), vec![3, 3, 1]); // exactly the limit fits
        assert_eq!(sizes(1000, 5), vec![1; 7]); // a document over the limit is a run of its own
        assert!(runs(&[], 3, 30).is_empty());
        let one = [d(10)];
        assert_eq!(runs(&one, 1, 10).len(), 1);
    }

    #[test]
    fn describing_failures() {
        assert_eq!(describe(&Err("refused".into())), "refused");
        assert_eq!(describe(&Ok((503, b"busy".to_vec()))), "HTTP 503: busy");
        let long = describe(&Ok((500, vec![b'x'; 1000])));
        assert_eq!(long.len(), "HTTP 500: ".len() + 200);
    }
}
