//! `ferrofire top`: the data side of the live tag view (the screen is `tui`). A running `ferrofire run` keeps the
//! newest value of every tag it publishes (`publish`); a `top` command on its local control socket (the channel `log
//! tail` uses) streams the tags that changed since the last frame, at most `EVERY` apart, so 800 tags at 100 Hz cost
//! 4 frames a second. The client can instead listen to datagram v1 (`--stream`, from any VM) or replay a `probe record`
//! CSV (`--replay`). Every source ends up as `Msg`s for the `Table` below.
//! Wire (one line per tag, TAB separated, a frame ends with `F`): `T name value age_ms`.

use crate::bennu::{exit_on_err, group_for};
use crate::datagram::Receiver;
use crate::hint::lock;
use crate::log::{pick, run_dirs};
use crate::model::Value;
use crate::probe::{follow, json_str, watched};
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{channel, Receiver as Inbox, Sender};
use std::time::{Duration, Instant};

/// Frame period: how often a source hands the screen what changed.
pub const EVERY: Duration = Duration::from_millis(250);
/// Samples kept per tag for the trend column.
const TREND: usize = 16;
/// A changed value stays highlighted this many seconds.
const HOT: f64 = 1.0;
/// `--once` waits this long for the first frame.
const ONCE_WAIT: Duration = Duration::from_secs(5);

pub enum Msg {
    /// `name` had `value` from time `at` (seconds on the viewer's clock).
    Tag { name: String, value: f64, at: f64 },
    /// End of one batch of tags.
    Frame,
    /// A log line of the attached process, `Level: text`.
    Log(String),
    /// The source ended (process exited, replay finished, listen failed).
    Closed(String),
}

// ---- the table: what the screen and --once show -----------------------------------------------------------

pub struct Tag {
    value: f64,
    updated: f64,
    changed: f64,
    hist: VecDeque<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub value: f64,
    pub age: f64,
    pub stale: bool,
    /// Changed within the last second.
    pub hot: bool,
    pub hist: Vec<f64>,
}

pub struct Table {
    tags: BTreeMap<String, Tag>,
    stale_after: f64,
}

impl Table {
    pub fn new(stale_after: f64) -> Table {
        Table {
            tags: BTreeMap::new(),
            stale_after,
        }
    }

    pub fn apply(&mut self, name: &str, value: f64, at: f64) {
        let t = self.tags.entry(name.to_string()).or_insert(Tag {
            value,
            updated: at,
            changed: at,
            hist: VecDeque::new(),
        });
        if t.value != value {
            (t.value, t.changed) = (value, at);
        }
        t.updated = t.updated.max(at);
        if t.hist.len() == TREND {
            t.hist.pop_front();
        }
        t.hist.push_back(value);
    }

    /// One row per tag, by name; a tag is stale once `stale_after` seconds have passed since its last update.
    pub fn rows(&self, now: f64) -> Vec<Row> {
        self.tags
            .iter()
            .map(|(name, t)| {
                let age = (now - t.updated).max(0.0);
                Row {
                    name: name.clone(),
                    value: t.value,
                    age,
                    stale: age >= self.stale_after,
                    hot: now - t.changed < HOT,
                    hist: t.hist.iter().copied().collect(),
                }
            })
            .collect()
    }
}

/// `--once` output: text `tag=value age=1.2s ok|STALE`, or one JSON object per line.
pub fn once_text(rows: &[Row], json: bool) -> String {
    let line = |r: &Row| match json {
        true => format!(
            "{{\"tag\":{},\"value\":{},\"age\":{:.3},\"stale\":{}}}",
            json_str(&r.name),
            r.value,
            r.age,
            r.stale
        ),
        false => format!(
            "{}={} age={:.1}s {}",
            r.name,
            r.value,
            r.age,
            if r.stale { "STALE" } else { "ok" }
        ),
    };
    rows.iter().map(|r| line(r) + "\n").collect()
}

// ---- the serving side: newest value per tag, streamed as frames -------------------------------------------

struct Store {
    seq: u64,
    map: BTreeMap<String, (f64, u64, Instant)>,
}

static TAGS: std::sync::Mutex<Store> = std::sync::Mutex::new(Store {
    seq: 0,
    map: BTreeMap::new(),
});

fn number(v: Value) -> f64 {
    match v {
        Value::Analog(x) => x,
        Value::Binary(b) => f64::from(u8::from(b)),
    }
}

/// Remember what was just published (one lock per publish cycle).
pub fn publish(snap: &[(String, Value)], now: Instant) {
    let mut st = lock(&TAGS);
    st.seq += 1;
    let seq = st.seq;
    for (name, v) in snap {
        let entry = (number(*v), seq, now);
        match st.map.get_mut(name) {
            Some(e) => *e = entry,
            None => {
                st.map.insert(name.clone(), entry);
            }
        }
    }
}

/// Every tag with its value and the age of that value at `now`, by name. The lock is held for the copy only.
pub fn tags(now: Instant) -> Vec<(String, f64, f64)> {
    let st = lock(&TAGS);
    let age = |at: Instant| now.saturating_duration_since(at).as_secs_f64();
    st.map
        .iter()
        .map(|(n, (v, _, at))| (n.clone(), *v, age(*at)))
        .collect()
}

/// The tags updated after `since`, as wire lines ending in `F` (nothing when none), and the newest seq.
fn frame(st: &Store, since: u64, now: Instant) -> (String, u64) {
    let mut out = String::new();
    for (name, (v, _, at)) in st.map.iter().filter(|(_, e)| e.1 > since) {
        let age = now.saturating_duration_since(*at).as_millis();
        out += &format!("T\t{name}\t{v}\t{age}\n");
    }
    if !out.is_empty() {
        out += "F\n";
    }
    (out, st.seq)
}

/// The `top` control command: stream frames to this peer until it goes away.
pub fn attach(peer: &UnixStream) -> Result<String, String> {
    let mut s = peer.try_clone().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let mut since = 0;
        loop {
            std::thread::sleep(EVERY);
            let (text, seq) = frame(&lock(&TAGS), since, Instant::now());
            since = seq;
            if !text.is_empty() && s.write_all(text.as_bytes()).is_err() {
                break;
            }
        }
    });
    Ok(String::new())
}

// ---- the client sources -----------------------------------------------------------------------------------

pub fn parse_line(l: &str, now: f64) -> Option<Msg> {
    match l.split_once('\t') {
        None if l == "F" => Some(Msg::Frame),
        Some(("T", rest)) => parse_tag(rest, now),
        _ => None,
    }
}

/// `name TAB value TAB age_ms`
fn parse_tag(rest: &str, now: f64) -> Option<Msg> {
    let mut f = rest.split('\t');
    let name = f.next()?.to_string();
    let value = f.next()?.parse().ok()?;
    let age_ms: f64 = f.next()?.parse().ok()?;
    Some(Msg::Tag {
        name,
        value,
        at: now - age_ms / 1000.0,
    })
}

/// Connect to the process's control socket and send one command; the reply and then the stream come back.
fn ask(sock: &std::path::Path, req: &str) -> Result<UnixStream, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| format!("{}: {e}", sock.display()))?;
    writeln!(s, "{req}").map_err(|e| e.to_string())?;
    Ok(s)
}

/// Tags and log lines of a local process: two connections to its control socket (`top` and `tail`).
fn local(
    pid: Option<u32>,
    name: Option<&str>,
    t0: Instant,
    tx: Sender<Msg>,
) -> Result<String, String> {
    let sock = pick(&run_dirs(), pid, name)?;
    let (top, tail) = (ask(&sock, "top")?, ask(&sock, "tail\t--level\tdebug")?);
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        for l in BufReader::new(top).lines().map_while(Result::ok) {
            let now = t0.elapsed().as_secs_f64();
            parse_line(&l, now).into_iter().for_each(|m| {
                let _ = tx.send(m);
            });
        }
        let _ = tx.send(Msg::Closed("process exited".into()));
    });
    std::thread::spawn(move || {
        for l in BufReader::new(tail).lines().map_while(Result::ok) {
            let _ = tx2.send(Msg::Log(l));
        }
    });
    Ok(sock.display().to_string())
}

/// Datagram v1 on `endpoint`: values batched into one frame per `EVERY`.
/// ponytail: a batch is flushed when the next datagram arrives after `EVERY`, so the last partial batch of a feed that stops is not shown.
fn stream(endpoint: String, tags: Vec<String>, t0: Instant, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let mut rx = Receiver::new(&group_for(&endpoint), tags.iter().map(String::as_str));
        let (mut pending, mut flushed) = (BTreeMap::new(), Instant::now());
        let end = follow(&endpoint, None, |d| {
            pending.extend(rx.accept(d).unwrap_or_default());
            if flushed.elapsed() >= EVERY && !pending.is_empty() {
                let at = t0.elapsed().as_secs_f64();
                for (name, value) in std::mem::take(&mut pending) {
                    let _ = tx.send(Msg::Tag { name, value, at });
                }
                let _ = tx.send(Msg::Frame);
                flushed = Instant::now();
            }
        });
        let _ = tx.send(Msg::Closed(end.err().unwrap_or("listener stopped".into())));
    });
}

/// One recorded cycle: its time and the tags with a fresh value.
type ReplayRow = (f64, Vec<(String, f64)>);

/// `cycle,t,<tag>,<tag>_stale,...` rows of `probe record` -> (t, the tags with a fresh value in that row). A stale
/// or empty cell is left out, so the viewer's own clock decides when the tag turns stale.
pub fn parse_replay(csv: &str) -> Result<Vec<ReplayRow>, String> {
    let mut lines = csv.lines();
    let head: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    if head.get(..2) != Some(&["cycle", "t"]) {
        return Err("not a probe record CSV (header must start cycle,t)".into());
    }
    let mut rows = vec![];
    for (n, line) in lines.enumerate() {
        let cells: Vec<&str> = line.split(',').collect();
        let bad = || format!("line {}: expected {} cells", n + 2, head.len());
        if cells.len() != head.len() {
            return Err(bad());
        }
        let t = cells[1].parse().map_err(|_| bad())?;
        let fresh = (2..head.len()).step_by(2).filter_map(|i| {
            let v = cells[i].parse().ok().filter(|_| cells[i + 1] != "1");
            v.map(|v| (head[i].to_string(), v))
        });
        rows.push((t, fresh.collect()));
    }
    Ok(rows)
}

fn replay(rows: Vec<ReplayRow>, speed: f64, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let t0 = Instant::now();
        for (at, tags) in rows {
            let due = Duration::from_secs_f64(at / speed);
            std::thread::sleep(due.saturating_sub(t0.elapsed()));
            for (name, value) in tags {
                let _ = tx.send(Msg::Tag { name, value, at });
            }
            let _ = tx.send(Msg::Frame);
        }
        let _ = tx.send(Msg::Closed("replay finished".into()));
    });
}

// ---- the command ------------------------------------------------------------------------------------------

const USAGE: &str = "ferrofire top [--pid N | --name NAME | --stream udp://GROUP:PORT [--server tcp://H:P] [--tags a,b] | --replay run.csv [--speed 1]] [--stale-after 2] [--filter GLOB] [--once [--json]]";

#[derive(clap::Args)]
pub struct TopArgs {
    /// Attach to the local process with this pid (default: the only running one)
    #[arg(long)]
    pid: Option<u32>,
    /// Attach to the local process with this name (run, pull-bridge, ...)
    #[arg(long)]
    name: Option<String>,
    /// Listen to a datagram v1 publish endpoint instead (any VM); tags come from --tags or the provider's QUERY
    #[arg(long, value_name = "ENDPOINT", conflicts_with_all = ["replay", "pid", "name"])]
    stream: Option<String>,
    /// Provider REP endpoint, to list the tags of a --stream
    #[arg(long, default_value = "tcp://127.0.0.1:5555")]
    server: String,
    /// Tags of a --stream, globs allowed (default: all the provider has)
    #[arg(long, value_delimiter = ',', value_name = "A,B,C")]
    tags: Vec<String>,
    /// Play back a `probe record` CSV
    #[arg(long, value_name = "FILE", conflicts_with_all = ["pid", "name"])]
    replay: Option<String>,
    /// Replay speed factor
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
    /// Seconds without an update after which a tag is stale
    #[arg(long, default_value_t = 2.0)]
    stale_after: f64,
    /// Show only tags matching this (substring, or a glob with * and ?)
    #[arg(long, default_value = "")]
    filter: String,
    /// Print the table once and exit instead of opening the screen
    #[arg(long)]
    once: bool,
    /// With --once: one JSON object per line
    #[arg(long, requires = "once")]
    json: bool,
}

pub fn main(a: TopArgs) {
    exit_on_err("top", USAGE, run(a));
}

fn run(a: TopArgs) -> Result<(), String> {
    if !(a.speed > 0.0 && a.stale_after > 0.0) {
        return Err("--speed and --stale-after must be > 0".into());
    }
    let mut table = Table::new(a.stale_after);
    if let (Some(f), true) = (&a.replay, a.once) {
        let rows = parse_replay(&std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?)?;
        let end = rows.last().map_or(0.0, |r| r.0);
        rows.iter()
            .flat_map(|(at, tags)| tags.iter().map(move |(n, v)| (n, *v, *at)))
            .for_each(|(n, v, at)| table.apply(n, v, at));
        return print_once(&table, end, &a);
    }
    let (t0, (tx, rx)) = (Instant::now(), channel());
    let (label, speed) = start(&a, t0, tx)?;
    match a.once {
        true => once(&mut table, &rx, t0, &a),
        false => {
            crate::tui::tui(table, label, a.filter, rx, (t0, speed)).map_err(|e| e.to_string())
        }
    }
}

/// Start the chosen source; its label for the screen header and the clock speed (replay may run faster).
fn start(a: &TopArgs, t0: Instant, tx: Sender<Msg>) -> Result<(String, f64), String> {
    if let Some(f) = &a.replay {
        let rows = parse_replay(&std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?)?;
        replay(rows, a.speed, tx);
        return Ok((format!("replay {f}"), a.speed));
    }
    if let Some(ep) = &a.stream {
        let mut req = crate::transport::requester(&a.server, 2000)?;
        stream(ep.clone(), watched(&mut *req, a.tags.clone())?, t0, tx);
        return Ok((format!("stream {ep}"), 1.0));
    }
    let sock = local(a.pid, a.name.as_deref(), t0, tx)?;
    Ok((sock, 1.0))
}

/// Wait for the first frame, then print the table.
fn once(table: &mut Table, rx: &Inbox<Msg>, t0: Instant, a: &TopArgs) -> Result<(), String> {
    loop {
        match rx.recv_timeout(ONCE_WAIT) {
            Ok(Msg::Tag { name, value, at }) => table.apply(&name, value, at),
            Ok(Msg::Frame) => break,
            Ok(Msg::Closed(why)) => return Err(why),
            Ok(Msg::Log(_)) => {}
            Err(_) => return Err(format!("no tags within {}s", ONCE_WAIT.as_secs())),
        }
    }
    print_once(table, t0.elapsed().as_secs_f64(), a)
}

fn print_once(table: &Table, now: f64, a: &TopArgs) -> Result<(), String> {
    let rows = crate::tui::visible(table.rows(now), &a.filter, crate::tui::Sort::Name);
    print!("{}", once_text(&rows, a.json));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_tracks_age_staleness_change_and_a_short_trend() {
        let mut t = Table::new(2.0);
        t.apply("a", 1.0, 0.0);
        t.apply("b", 5.0, 0.0);
        t.apply("a", 1.0, 1.5); // refreshed, same value
        t.apply("b", 6.0, 1.5); // changed
        let rows = t.rows(2.0);
        let (a, b) = (&rows[0], &rows[1]);
        assert_eq!(
            (a.name.as_str(), a.value, a.age, a.stale),
            ("a", 1.0, 0.5, false)
        );
        assert!(
            !a.hot && b.hot,
            "only b changed in the last second: {a:?} {b:?}"
        );
        assert_eq!(b.hist, [5.0, 6.0]);
        let later = t.rows(3.5);
        assert!(later[0].stale && !later[0].hot && later[1].age == 2.0 && later[1].stale);
        assert!(t.rows(0.0)[0].age == 0.0, "never negative");
        t.apply("a", 9.0, 1.0); // an older sample cannot make the tag younger
        assert_eq!(t.rows(3.5)[0].age, 2.0);
        for i in 0..40 {
            t.apply("a", f64::from(i), 4.0);
        }
        assert_eq!(t.rows(4.0)[0].hist.len(), TREND);
        assert_eq!(*t.rows(4.0)[0].hist.last().unwrap(), 39.0);
    }

    #[test]
    fn frames_carry_only_what_changed_since_the_last_one() {
        let now = Instant::now();
        let mut st = Store {
            seq: 0,
            map: BTreeMap::new(),
        };
        assert_eq!(frame(&st, 0, now), (String::new(), 0)); // nothing published: no frame
        st.map.insert("a".into(), (1.5, 1, now));
        st.map
            .insert("b".into(), (0.0, 2, now - Duration::from_millis(40)));
        st.seq = 2;
        assert_eq!(
            frame(&st, 0, now),
            ("T\ta\t1.5\t0\nT\tb\t0\t40\nF\n".into(), 2)
        );
        assert_eq!(frame(&st, 1, now).0, "T\tb\t0\t40\nF\n");
        assert_eq!(frame(&st, 2, now).0, ""); // a quiet period sends nothing; ages grow on the viewer
    }

    #[test]
    fn publish_keeps_the_newest_value_and_attach_streams_it() {
        let now = Instant::now();
        publish(
            &[
                ("top.unit.a".into(), Value::Analog(2.5)),
                ("top.unit.b".into(), Value::Binary(true)),
            ],
            now,
        );
        publish(&[("top.unit.a".into(), Value::Analog(3.5))], now);
        let (server, client) = UnixStream::pair().unwrap();
        assert_eq!(attach(&server), Ok(String::new()));
        let mut seen = vec![];
        for l in BufReader::new(client).lines().map(Result::unwrap) {
            seen.push(l.clone());
            if l == "F" {
                break;
            }
        }
        let want = |t: &str| seen.iter().any(|l| l.starts_with(t));
        assert!(
            want("T\ttop.unit.a\t3.5\t") && want("T\ttop.unit.b\t1\t"),
            "{seen:?}"
        );
        assert!(!want("T\ttop.unit.a\t2.5"));
    }

    #[test]
    fn wire_lines_become_messages() {
        let tag = |l| match parse_line(l, 10.0) {
            Some(Msg::Tag { name, value, at }) => Some((name, value, at)),
            _ => None,
        };
        assert_eq!(tag("T\tx.y\t-1.5\t2500"), Some(("x.y".into(), -1.5, 7.5)));
        assert!(matches!(parse_line("F", 0.0), Some(Msg::Frame)));
        for bad in [
            "",
            "X\ta",
            "T",
            "T\ta",
            "T\ta\tx\t1",
            "T\ta\t1\tz",
            "T\ta\t1",
        ] {
            assert!(parse_line(bad, 0.0).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_probe_record_csv_replays_its_fresh_cells() {
        let csv = "cycle,t,a,a_stale,b,b_stale\n1,0.000,1,0,,\n2,0.100,2,0,7,0\n3,0.200,2,1,8,0\n";
        let rows = parse_replay(csv).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], (0.0, vec![("a".into(), 1.0)])); // b has no value yet
        assert_eq!(rows[1].1, [("a".into(), 2.0), ("b".into(), 7.0)]);
        assert_eq!(rows[2], (0.2, vec![("b".into(), 8.0)])); // a is stale in the recording: left out
        for (csv, want) in [
            ("", "not a probe record CSV"),
            ("a,b\n", "not a probe record CSV"),
            ("cycle,t,a,a_stale\n1,0\n", "line 2: expected 4 cells"),
            ("cycle,t,a,a_stale\n1,x,1,0\n", "line 2:"),
        ] {
            assert!(parse_replay(csv).unwrap_err().contains(want), "{csv:?}");
        }
        assert_eq!(parse_replay("cycle,t\n").unwrap(), vec![]);
    }

    #[test]
    fn replay_sends_each_row_then_closes() {
        let (tx, rx) = channel();
        replay(
            vec![
                (0.0, vec![("a".into(), 1.0)]),
                (0.5, vec![("a".into(), 2.0)]),
            ],
            1000.0,
            tx,
        );
        let got: Vec<String> = rx
            .iter()
            .map(|m| match m {
                Msg::Tag { name, value, at } => format!("{name}={value}@{at}"),
                Msg::Frame => "F".into(),
                Msg::Closed(w) => w,
                Msg::Log(_) => unreachable!(),
            })
            .collect();
        assert_eq!(got, ["a=1@0", "F", "a=2@0.5", "F", "replay finished"]);
    }

    #[test]
    fn once_prints_text_or_json() {
        let mut t = Table::new(1.0);
        t.apply("a\"b", 1.5, 0.0);
        t.apply("c", 0.0, 4.0);
        let rows = t.rows(4.0);
        assert_eq!(
            once_text(&rows, false),
            "a\"b=1.5 age=4.0s STALE\nc=0 age=0.0s ok\n"
        );
        assert_eq!(
            once_text(&rows, true),
            "{\"tag\":\"a\\\"b\",\"value\":1.5,\"age\":4.000,\"stale\":true}\n{\"tag\":\"c\",\"value\":0,\"age\":0.000,\"stale\":false}\n"
        );
        assert_eq!(once_text(&[], false), "");
    }

    #[test]
    fn once_waits_for_a_frame_and_reports_a_dead_source() {
        let args = |json| TopArgs {
            pid: None,
            name: None,
            stream: None,
            server: String::new(),
            tags: vec![],
            replay: None,
            speed: 1.0,
            stale_after: 2.0,
            filter: "a".into(),
            once: true,
            json,
        };
        let (tx, rx) = channel();
        tx.send(Msg::Log("Info: x".into())).unwrap();
        tx.send(Msg::Tag {
            name: "a".into(),
            value: 1.0,
            at: 0.0,
        })
        .unwrap();
        tx.send(Msg::Tag {
            name: "zzz".into(),
            value: 2.0,
            at: 0.0,
        })
        .unwrap();
        tx.send(Msg::Frame).unwrap();
        let mut t = Table::new(2.0);
        once(&mut t, &rx, Instant::now(), &args(false)).unwrap();
        assert_eq!(t.rows(0.0).len(), 2);
        tx.send(Msg::Closed("process exited".into())).unwrap();
        assert_eq!(
            once(&mut t, &rx, Instant::now(), &args(false)),
            Err("process exited".into())
        );
    }
}
