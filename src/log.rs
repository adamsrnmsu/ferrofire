//! Runtime log control. Every long-running subcommand (`run`, `pull-bridge`, `pypower`, `simulink`) opens a local
//! Unix control socket (0600, never on the network) named `<name>-<pid>.sock`; `ferrofire log status|on|off|level|tail`
//! talks to it, so logging can be switched on a live process without a restart or an environment variable.
//!
//! The logger itself is `tracing` (`crate::trace`): the level is its reload filter, and its writer hands every
//! formatted line to `route` here. Default: stderr only. `on` sends the lines to a size-capped rotating file instead;
//! `tail` streams them live over the socket with no file (it can only narrow the process level, never widen it). The commands are idempotent (`on` twice is one file and one sink) and every reply says where
//! the process logs to, at what level, and the pid and name. Wire format: one request line (words joined by TAB) and
//! the reply lines, `error: ...` for a refusal.

use crate::hint::{did_you_mean, lock, show};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

const LEVELS: [(&str, Level); 4] = [
    ("error", Level::Error),
    ("warn", Level::Warn),
    ("info", Level::Info),
    ("debug", Level::Debug),
];

impl Level {
    fn parse(s: &str) -> Result<Level, String> {
        let l = s.to_ascii_lowercase();
        LEVELS
            .iter()
            .find(|(n, _)| *n == l)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                format!(
                    "unknown level `{}`{}",
                    show(s),
                    did_you_mean(&l, LEVELS.map(|l| l.0))
                )
            })
    }
    fn filter(self) -> tracing::level_filters::LevelFilter {
        use tracing::level_filters::LevelFilter as F;
        match self {
            Level::Error => F::ERROR,
            Level::Warn => F::WARN,
            Level::Info => F::INFO,
            Level::Debug => F::DEBUG,
        }
    }
    fn name(self) -> &'static str {
        LEVELS
            .iter()
            .find(|(_, v)| *v == self)
            .map_or("", |(n, _)| n)
    }
    /// The level a line announces with its `Warn:` / `Info:` prefix; no prefix is info.
    fn of_line(text: &str) -> Level {
        let prefix = text.split_once(':').map_or("", |(p, _)| p);
        Level::parse(prefix).unwrap_or(Level::Info)
    }
}

const SIZE: [(char, u64); 3] = [('k', 1 << 10), ('m', 1 << 20), ('g', 1 << 30)];
const TIME: [(char, u64); 3] = [('s', 1), ('m', 60), ('h', 3600)];
const ON_FLAGS: [&str; 4] = ["--file", "--level", "--max-size", "--keep"];
const COMMANDS: [&str; 5] = ["status", "on", "off", "level", "tail"];
const TAIL_WRITE: Duration = Duration::from_millis(100);

/// `50M`, `10m`, `90` (no unit: the base unit): a positive whole number with an optional unit suffix.
fn scaled(s: &str, units: &[(char, u64)]) -> Result<u64, String> {
    let unit = s
        .chars()
        .last()
        .and_then(|c| units.iter().find(|u| u.0 == c.to_ascii_lowercase()));
    let (digits, mult) = unit.map_or((s, 1), |u| (&s[..s.len() - 1], u.1));
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("bad value `{}`", show(s)))
}

type Opts<'a> = Vec<(&'a str, &'a str)>;

/// `--flag value` pairs; unknown flags get a suggestion.
fn opts<'a>(words: &[&'a str], known: &[&str]) -> Result<Opts<'a>, String> {
    let (mut out, mut it) = (vec![], words.iter());
    while let Some(&k) = it.next() {
        if !known.contains(&k) {
            let hint = did_you_mean(k, known.iter().copied());
            return Err(format!("unknown option `{}`{hint}", show(k)));
        }
        out.push((k, *it.next().ok_or_else(|| format!("{k} needs a value"))?));
    }
    Ok(out)
}

fn opt<'a>(o: &Opts<'a>, key: &str) -> Option<&'a str> {
    o.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The log file: opened for append (never truncated), rotated `path` -> `path.1` -> ... -> `path.<keep>` at `max` bytes.
struct Sink {
    path: PathBuf,
    file: File,
    size: u64,
}

fn open_append(path: &Path) -> Result<File, String> {
    let open = OpenOptions::new().append(true).create(true).open(path);
    open.map_err(|e| format!("{}: {e}", path.display()))
}

fn numbered(path: &Path, i: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{i}"));
    s.into()
}

impl Sink {
    fn open(path: PathBuf) -> Result<Sink, String> {
        let file = open_append(&path)?;
        let size = file.metadata().map_or(0, |m| m.len());
        Ok(Sink { path, file, size })
    }

    fn rotate(&mut self, keep: usize) -> Result<(), String> {
        let _ = std::fs::remove_file(numbered(&self.path, keep));
        for i in (1..keep).rev() {
            let _ = std::fs::rename(numbered(&self.path, i), numbered(&self.path, i + 1));
        }
        let _ = std::fs::rename(&self.path, numbered(&self.path, 1));
        self.file = open_append(&self.path)?;
        self.size = 0;
        Ok(())
    }

    fn write(&mut self, line: &str, max: u64, keep: usize) -> Result<(), String> {
        let n = line.len() as u64;
        if self.size > 0 && self.size + n > max {
            self.rotate(keep)?;
        }
        self.file
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        self.size += n;
        Ok(())
    }
}

pub struct State {
    name: String,
    sock: PathBuf,
    level: Level,
    sink: Option<Sink>,
    max: u64,
    keep: usize,
    /// `--for`: when to fall back, and to what.
    revert: Option<(Instant, Level)>,
    tails: Vec<(UnixStream, Level)>,
}

static STATE: Mutex<State> = Mutex::new(State::new());

impl State {
    const fn new() -> State {
        State {
            name: String::new(),
            sock: PathBuf::new(),
            level: Level::Info,
            sink: None,
            max: 50 << 20,
            keep: 3,
            revert: None,
            tails: vec![],
        }
    }

    /// The one place the level changes: our copy (for `status`) and the live tracing filter.
    fn set(&mut self, l: Level) {
        self.level = l;
        crate::trace::set_level(l.filter());
    }

    fn expire(&mut self, now: Instant) {
        if let Some((_, back)) = self.revert.filter(|r| now >= r.0) {
            self.set(back);
            self.revert = None;
        }
    }

    /// Where, at what level, and who: the line every command ends with.
    fn describe(&self, now: Instant) -> String {
        let dest = self
            .sink
            .as_ref()
            .map_or("stderr".into(), |s| s.path.display().to_string());
        let back = self.revert.map_or(String::new(), |(at, l)| {
            format!(
                ", reverts to {} in {}s",
                l.name(),
                at.saturating_duration_since(now).as_secs()
            )
        });
        let (name, pid, lvl) = (&self.name, std::process::id(), self.level.name());
        format!("ferrofire {name} (pid {pid}): logging to {dest}, level {lvl}{back}")
    }

    /// One log line. Returns the text that belongs on stderr (no file sink, or the file failed).
    fn emit(&mut self, text: &str, now: Instant) -> Option<String> {
        self.expire(now);
        let lvl = Level::of_line(text);
        let msg = format!("{text}\n");
        self.tails
            .retain_mut(|(s, l)| lvl > *l || s.write_all(msg.as_bytes()).is_ok());
        if lvl > self.level {
            return None;
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let stamped = format!("{secs:.3} {msg}");
        let wrote = self
            .sink
            .as_mut()
            .map(|s| s.write(&stamped, self.max, self.keep));
        (wrote != Some(Ok(()))).then(|| text.to_string())
    }

    fn command(&mut self, line: &str, peer: &UnixStream, now: Instant) -> String {
        self.expire(now);
        let words: Vec<&str> = line.trim_end_matches(['\n', '\r']).split('\t').collect();
        let rest = &words[1..];
        let reply = match words[0] {
            "status" => Ok(String::new()),
            "on" => opts(rest, &ON_FLAGS).and_then(|o| self.on(&o)),
            "off" => Ok(self.off()),
            "level" => self.set_level(rest, now),
            "tail" => opts(rest, &["--level"]).and_then(|o| self.tail(&o, peer)),
            c => Err(format!(
                "unknown command `{}`{}",
                show(c),
                did_you_mean(c, COMMANDS)
            )),
        };
        match reply {
            Ok(note) => format!("{note}{}", self.describe(now)),
            Err(e) => format!("error: {e}"),
        }
    }

    /// Log to a file. Same file: nothing reopens, truncates or rotates, only the settings change.
    fn on(&mut self, o: &Opts) -> Result<String, String> {
        let path = opt(o, "--file").map_or_else(|| self.sock.with_extension("log"), PathBuf::from);
        if !path.is_absolute() {
            return Err(format!(
                "log file `{}` must be an absolute path",
                path.display()
            ));
        }
        let max = opt(o, "--max-size").map(|v| scaled(v, &SIZE)).transpose()?;
        let keep = opt(o, "--keep").map(|v| scaled(v, &[])).transpose()?;
        let level = opt(o, "--level").map(Level::parse).transpose()?;
        let same = self.sink.as_ref().is_some_and(|s| s.path == path);
        let note = match same {
            true => format!("already logging to {}\n", path.display()),
            false => {
                self.sink = Some(Sink::open(path.clone())?); // the old file closes here
                format!("logging to {}\n", path.display())
            }
        };
        self.max = max.unwrap_or(self.max);
        self.keep = keep.map_or(self.keep, |k| k as usize);
        if let Some(l) = level {
            self.set(l);
            self.revert = None;
        }
        Ok(note)
    }

    fn off(&mut self) -> String {
        match self.sink.take() {
            Some(s) => format!("stopped logging to {}\n", s.path.display()),
            None => "already off\n".into(),
        }
    }

    /// `level L [--for 10m]`: with `--for` the level falls back by itself.
    fn set_level(&mut self, words: &[&str], now: Instant) -> Result<String, String> {
        let (l, rest) = words
            .split_first()
            .ok_or("level needs a value: error|warn|info|debug")?;
        let level = Level::parse(l)?;
        let secs = opt(&opts(rest, &["--for"])?, "--for")
            .map(|v| scaled(v, &TIME))
            .transpose()?;
        let base = self.revert.map_or(self.level, |r| r.1); // a second --for keeps the original base
        self.revert = secs.map(|s| (now + Duration::from_secs(s), base));
        self.set(level);
        Ok(String::new())
    }

    fn tail(&mut self, o: &Opts, peer: &UnixStream) -> Result<String, String> {
        let level = opt(o, "--level").map_or(Ok(self.level), Level::parse)?;
        let s = peer.try_clone().map_err(|e| e.to_string())?;
        self.tails.push((s, level));
        let more = (level > self.level).then(|| {
            format!(
                " (the process logs at {}: `ferrofire log level {} --for 10m` to see more)",
                self.level.name(),
                level.name()
            )
        });
        Ok(format!(
            "tailing at level {}{}\n",
            level.name(),
            more.unwrap_or_default()
        ))
    }
}

/// Called by the tracing writer with one formatted line: true when the file sink or the level gate took it, false when
/// it belongs on stderr.
pub fn route(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut st = lock(&STATE);
    // no control socket (not started, or tracing used directly): nothing to route
    !st.sock.as_os_str().is_empty()
        && st
            .emit(text.trim_end_matches('\n'), Instant::now())
            .is_none()
}

fn serve_conn(mut s: UnixStream) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(TAIL_WRITE));
    let mut req = String::new();
    if BufReader::new(&s).read_line(&mut req).is_ok_and(|n| n > 0) {
        let reply = lock(&STATE).command(&req, &s, Instant::now());
        let _ = writeln!(s, "{reply}");
    }
}

/// Listen on `<dir>/<name>-<pid>.sock` (dir 0700, socket 0600); returns the socket path.
pub fn serve(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let sock = dir.join(format!("{name}-{}.sock", std::process::id()));
    let err = |e: std::io::Error| format!("{}: {e}", sock.display());
    std::fs::create_dir_all(dir).map_err(err)?;
    std::fs::set_permissions(dir, PermissionsExt::from_mode(0o700)).map_err(err)?;
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).map_err(err)?;
    std::fs::set_permissions(&sock, PermissionsExt::from_mode(0o600)).map_err(err)?;
    {
        let mut st = lock(&STATE);
        (st.name, st.sock) = (name.to_string(), sock.clone());
    }
    std::thread::spawn(move || listener.incoming().flatten().for_each(serve_conn));
    Ok(sock)
}

/// Where control sockets live: `$FERROFIRE_RUN_DIR` alone if set, else `/run/ferrofire`, `$XDG_RUNTIME_DIR/ferrofire`,
/// `/tmp/ferrofire-<uid>` in that order of preference.
pub fn run_dirs() -> Vec<PathBuf> {
    if let Ok(d) = std::env::var("FERROFIRE_RUN_DIR") {
        return vec![d.into()];
    }
    let xdg = std::env::var("XDG_RUNTIME_DIR").map(|x| Path::new(&x).join("ferrofire"));
    let mut dirs = vec![PathBuf::from("/run/ferrofire")];
    dirs.extend(xdg);
    dirs.push(format!("/tmp/ferrofire-{}", crate::sys::getuid()).into());
    dirs
}

/// Open this process's control socket in the first directory that works. Never fatal: logging control is optional.
pub fn start(name: &str) {
    let mut errs = vec![];
    for dir in run_dirs() {
        match serve(&dir, name) {
            Ok(p) => return tracing::info!("log control socket {}", p.display()),
            Err(e) => errs.push(e),
        }
    }
    tracing::warn!("no log control socket ({})", errs.join("; "));
}

/// Remove the control socket on a normal exit (a crash leaves it; discovery sweeps stale ones).
pub fn stop() {
    let sock = lock(&STATE).sock.clone();
    let _ = std::fs::remove_file(sock);
}

// ---- the `ferrofire log` client ------------------------------------------------------------------------------

const USAGE: &str = "ferrofire log [--pid N | --name NAME] status | on [--file PATH] [--level L] [--max-size 50M] [--keep 3] | off | level L [--for 10m] | tail [--level L]";

/// `name-pid.sock` -> (name, pid).
fn parse_sock(file: &str) -> Option<(String, u32)> {
    let (name, pid) = file.strip_suffix(".sock")?.rsplit_once('-')?;
    Some((name.into(), pid.parse().ok()?))
}

/// Live control sockets matching `--pid` / `--name`; dead ones (crashed processes) are removed on the way.
fn discover(dirs: &[PathBuf], pid: Option<u32>, name: Option<&str>) -> Vec<(String, u32, PathBuf)> {
    let entries = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten();
    entries
        .filter_map(|e| {
            let (n, p) = parse_sock(&e.file_name().to_string_lossy())?;
            Some((n, p, e.path()))
        })
        .filter(|(n, p, _)| pid.is_none_or(|w| w == *p) && name.is_none_or(|w| w == n))
        .filter(|(_, _, path)| {
            UnixStream::connect(path).is_ok() || std::fs::remove_file(path).is_err()
        })
        .collect()
}

fn pick(dirs: &[PathBuf], pid: Option<u32>, name: Option<&str>) -> Result<PathBuf, String> {
    let mut found = discover(dirs, pid, name);
    match found.len() {
        0 => Err(format!(
            "no running ferrofire process with a control socket in {dirs:?}"
        )),
        1 => Ok(found.remove(0).2),
        _ => {
            let all: Vec<String> = found
                .iter()
                .map(|(n, p, _)| format!("{n} (pid {p})"))
                .collect();
            Err(format!(
                "several ferrofire processes: {}; choose with --pid N or --name NAME",
                all.join(", ")
            ))
        }
    }
}

/// `[--pid N] [--name NAME] command args...` -> the request words.
type Request = (Option<u32>, Option<String>, Vec<String>);

/// `--pid N` / `--name NAME` in front of the command.
fn selector(args: &[String]) -> Result<(Option<u32>, Option<String>, usize), String> {
    let (mut pid, mut name, mut i) = (None, None, 0);
    while let Some(flag) = args.get(i).filter(|a| *a == "--pid" || *a == "--name") {
        let v = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} needs a value"))?;
        if flag == "--pid" {
            let n = v.parse();
            pid = Some(n.map_err(|_| format!("--pid: `{}` is not a number", show(v)))?);
        } else {
            name = Some(v.clone());
        }
        i += 2;
    }
    Ok((pid, name, i))
}

/// The process's working directory is not ours: send an absolute log path.
fn absolutize_file(words: &mut [String]) -> Result<(), String> {
    let at = words.iter().position(|w| w == "--file");
    if let Some(p) = at.and_then(|k| words.get_mut(k + 1)) {
        let abs = std::path::absolute(&*p).map_err(|e| format!("{p}: {e}"))?;
        *p = abs.display().to_string();
    }
    Ok(())
}

fn parse_client(args: &[String]) -> Result<Request, String> {
    let (pid, name, i) = selector(args)?;
    let mut words = args[i..].to_vec();
    let cmd = words.first().ok_or("no command given")?;
    if !COMMANDS.contains(&cmd.as_str()) {
        let hint = did_you_mean(cmd, COMMANDS);
        return Err(format!("unknown command `{}`{hint}", show(cmd)));
    }
    absolutize_file(&mut words)?;
    Ok((pid, name, words))
}

pub fn main(args: &[String]) {
    crate::bennu::exit_on_err("log", USAGE, run(args));
}

fn run(args: &[String]) -> Result<(), String> {
    let (pid, name, words) = parse_client(args)?;
    let sock = pick(&run_dirs(), pid, name.as_deref())?;
    let mut s = UnixStream::connect(&sock).map_err(|e| format!("{}: {e}", sock.display()))?;
    writeln!(s, "{}", words.join("\t")).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout().lock();
    for l in BufReader::new(s).lines() {
        let l = l.map_err(|e| e.to_string())?;
        if let Some(e) = l.strip_prefix("error: ") {
            return Err(e.into());
        }
        if writeln!(out, "{l}").and_then(|_| out.flush()).is_err() {
            break; // reader went away (`| head`)
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn state(dir: &Path) -> State {
        let mut st = State::new();
        (st.name, st.sock) = ("run".into(), dir.join("run-1.sock"));
        st
    }
    fn cmd(st: &mut State, line: &str, now: Instant) -> String {
        st.command(line, &UnixStream::pair().unwrap().0, now)
    }
    fn count(path: &Path, needle: &str) -> usize {
        std::fs::read_to_string(path)
            .unwrap()
            .matches(needle)
            .count()
    }

    #[test]
    fn on_three_times_is_one_file_one_sink_no_duplicate_lines() {
        let (dir, now) = (tmp("idem"), Instant::now());
        let f = dir.join("a.log");
        let on = format!("on\t--file\t{}", f.display());
        let mut st = state(&dir);
        let first = cmd(&mut st, &on, now);
        assert!(
            first.starts_with(&format!("logging to {}\n", f.display())),
            "{first}"
        );
        assert_eq!(st.emit("Info: one", now), None);
        for _ in 0..2 {
            let again = cmd(&mut st, &on, now);
            assert!(
                again.starts_with(&format!("already logging to {}\n", f.display())),
                "{again}"
            );
        }
        assert_eq!(st.emit("Info: two", now), None);
        assert_eq!((count(&f, "one"), count(&f, "two")), (1, 1)); // appended, never truncated, never doubled
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1); // one file, no rotation
    }

    #[test]
    fn every_reply_says_where_level_pid_and_name() {
        let (dir, now) = (tmp("where"), Instant::now());
        let f = dir.join("w.log");
        let mut st = state(&dir);
        let who = format!("ferrofire run (pid {}): logging to ", std::process::id());
        assert_eq!(
            cmd(&mut st, "status", now),
            format!("{who}stderr, level info")
        );
        let on = cmd(
            &mut st,
            &format!("on\t--file\t{}\t--level\tdebug", f.display()),
            now,
        );
        assert!(
            on.ends_with(&format!("{who}{}, level debug", f.display())),
            "{on}"
        );
        for c in ["status", "level\twarn", "off"] {
            assert!(cmd(&mut st, c, now).contains(&who), "{c}");
        }
        assert_eq!(
            cmd(&mut st, "level\twarn", now),
            format!("{who}stderr, level warn")
        );
    }

    #[test]
    fn off_twice_is_a_noop_and_switching_files_closes_the_old_one() {
        let (dir, now) = (tmp("off"), Instant::now());
        let (a, b) = (dir.join("a.log"), dir.join("b.log"));
        let mut st = state(&dir);
        assert!(cmd(&mut st, "off", now).starts_with("already off\n"));
        cmd(&mut st, &format!("on\t--file\t{}", a.display()), now);
        let sw = cmd(&mut st, &format!("on\t--file\t{}", b.display()), now);
        assert!(sw.starts_with(&format!("logging to {}\n", b.display())));
        st.emit("Info: x", now);
        assert_eq!((count(&a, "x"), count(&b, "x")), (0, 1));
        assert!(
            cmd(&mut st, "off", now).starts_with(&format!("stopped logging to {}\n", b.display()))
        );
        assert!(cmd(&mut st, "off", now).starts_with("already off\n"));
        assert_eq!(st.emit("Info: y", now).as_deref(), Some("Info: y")); // back on stderr
        assert_eq!(count(&b, "y"), 0);
    }

    #[test]
    fn default_file_sits_beside_the_socket_and_must_be_absolute() {
        let (dir, now) = (tmp("def"), Instant::now());
        let mut st = state(&dir);
        assert!(cmd(&mut st, "on", now).contains(&dir.join("run-1.log").display().to_string()));
        assert!(cmd(&mut st, "on\t--file\trel.log", now)
            .starts_with("error: log file `rel.log` must be an absolute path"));
    }

    #[test]
    fn levels_filter_and_revert_by_themselves() {
        let (dir, t0) = (tmp("lvl"), Instant::now());
        let mut st = state(&dir);
        cmd(
            &mut st,
            &format!("on\t--file\t{}", dir.join("l.log").display()),
            t0,
        );
        assert_eq!(st.emit("Debug: hidden", t0), None);
        assert!(!std::fs::read_to_string(dir.join("l.log"))
            .unwrap()
            .contains("hidden"));
        let r = cmd(&mut st, "level\tdebug\t--for\t10m", t0);
        assert!(r.ends_with("level debug, reverts to info in 600s"), "{r}");
        st.emit("Debug: shown", t0 + Duration::from_secs(599));
        assert_eq!(count(&dir.join("l.log"), "shown"), 1);
        let again = cmd(
            &mut st,
            "level\tdebug\t--for\t5s",
            t0 + Duration::from_secs(1),
        );
        assert!(again.ends_with("reverts to info in 5s"), "{again}"); // the base stays info
        let after = cmd(&mut st, "status", t0 + Duration::from_secs(6));
        assert!(after.ends_with("level info"), "{after}");
        assert!(cmd(&mut st, "level\tdebug\t--for\t5s", t0).contains("reverts"));
        assert!(cmd(&mut st, "level\twarn", t0).ends_with("level warn")); // no --for cancels the timer
        assert!(cmd(&mut st, "status", t0 + Duration::from_secs(60)).ends_with("level warn"));
        assert_eq!(st.emit("Info: quiet", t0), None);
        assert_eq!(count(&dir.join("l.log"), "quiet"), 0);
        assert!(st.emit("Error: loud", t0).is_none() && count(&dir.join("l.log"), "loud") == 1);
    }

    #[test]
    fn level_prefix_decides_the_line_level() {
        assert_eq!(Level::of_line("Warn: x"), Level::Warn);
        assert_eq!(Level::of_line("DEBUG: x"), Level::Debug);
        assert_eq!(Level::of_line("no prefix"), Level::Info);
        assert_eq!(Level::of_line("ferrofire: x"), Level::Info);
    }

    #[test]
    fn files_rotate_at_the_cap_and_keep_n() {
        let (dir, now) = (tmp("rot"), Instant::now());
        let f = dir.join("r.log");
        let mut st = state(&dir);
        cmd(
            &mut st,
            &format!("on\t--file\t{}\t--max-size\t1k\t--keep\t2", f.display()),
            now,
        );
        for i in 0..200 {
            st.emit(&format!("Info: line {i:03} {}", "x".repeat(40)), now);
        }
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["r.log", "r.log.1", "r.log.2"]);
        for n in &names {
            assert!(std::fs::metadata(dir.join(n)).unwrap().len() <= 1024, "{n}");
        }
        assert_eq!(count(&f, "line 199"), 1); // the newest line is in the live file
                                              // switching the cap on the same file changes nothing else
        assert!(cmd(
            &mut st,
            &format!("on\t--file\t{}\t--max-size\t1g", f.display()),
            now
        )
        .starts_with("already"));
    }

    #[test]
    fn bad_input_is_refused_with_a_hint_and_changes_nothing() {
        let (dir, now) = (tmp("bad"), Instant::now());
        let mut st = state(&dir);
        for (c, want) in [
            ("sttus", "unknown command `sttus` (did you mean `status`?)"),
            ("level", "level needs a value"),
            (
                "level\tdbeug",
                "unknown level `dbeug` (did you mean `debug`?)",
            ),
            (
                "level\tdebug\t--four\t1m",
                "unknown option `--four` (did you mean `--for`?)",
            ),
            ("level\tdebug\t--for", "--for needs a value"),
            ("level\tdebug\t--for\t0", "bad value `0`"),
            ("level\tdebug\t--for\tx", "bad value `x`"),
            ("on\t--max-size\t0", "bad value `0`"),
            ("on\t--keep\t-1", "bad value `-1`"),
            ("on\t--level\tloud", "unknown level `loud`"),
            ("tail\t--level\tloud", "unknown level `loud`"),
            (
                "on\t--file\t/nonexistent-dir/x.log",
                "/nonexistent-dir/x.log:",
            ),
        ] {
            let r = cmd(&mut st, c, now);
            assert!(r.starts_with("error: ") && r.contains(want), "{c}: {r}");
        }
        assert_eq!(
            cmd(&mut st, "status", now),
            format!(
                "ferrofire run (pid {}): logging to stderr, level info",
                std::process::id()
            )
        );
        assert_eq!(scaled("2G", &SIZE), Ok(2 << 30));
        assert_eq!(scaled("90", &TIME), Ok(90));
        assert_eq!(scaled("2h", &TIME), Ok(7200));
        assert!(scaled("18446744073709551615g", &SIZE).is_err());
    }

    #[test]
    fn tail_streams_at_its_own_level_and_drops_dead_readers() {
        let (dir, now) = (tmp("tail"), Instant::now());
        let mut st = state(&dir);
        let (server, client) = UnixStream::pair().unwrap();
        let r = st.command("tail\t--level\tdebug", &server, now);
        assert!(
            r.contains("tailing at level debug (the process logs at info"),
            "{r}"
        );
        st.level = Level::Debug; // tracing only hands us lines the process level lets through
        assert_eq!(st.emit("Debug: d", now).as_deref(), Some("Debug: d")); // no file: stderr, and the tail
        st.emit("Warn: w", now);
        let mut lines = BufReader::new(&client).lines();
        assert_eq!(lines.next().unwrap().unwrap(), "Debug: d");
        assert_eq!(lines.next().unwrap().unwrap(), "Warn: w");
        st.level = Level::Info;
        let (s2, c2) = UnixStream::pair().unwrap();
        st.command("tail", &s2, now); // defaults to the current level
        st.emit("Debug: gone", now);
        st.emit("Info: i", now);
        assert_eq!(
            BufReader::new(&c2).lines().next().unwrap().unwrap(),
            "Info: i"
        );
        assert_eq!(st.tails.len(), 2);
        drop((client, c2));
        st.emit("Info: j", now);
        st.emit("Info: k", now);
        assert_eq!(st.tails.len(), 0);
    }

    #[test]
    fn socket_names_parse_and_discovery_finds_one_or_says_which() {
        assert_eq!(
            parse_sock("pull-bridge-42.sock"),
            Some(("pull-bridge".into(), 42))
        );
        for bad in ["run-x.sock", "run.sock", "run-1.log", "-.sock"] {
            assert_eq!(parse_sock(bad), None, "{bad}");
        }
        let dir = tmp("disc");
        let dirs = [dir.clone(), dir.join("missing")];
        assert!(pick(&dirs, None, None)
            .unwrap_err()
            .contains("no running ferrofire process"));
        let live: Vec<_> = [("run", 10), ("pypower", 11)]
            .iter()
            .map(|(n, p)| UnixListener::bind(dir.join(format!("{n}-{p}.sock"))).unwrap())
            .collect();
        std::fs::write(dir.join("run-12.sock"), "").unwrap(); // a crashed process: dead socket file
        let e = pick(&dirs, None, None).unwrap_err();
        assert!(
            e.contains("run (pid 10)") && e.contains("pypower (pid 11)") && e.contains("--pid"),
            "{e}"
        );
        assert!(!dir.join("run-12.sock").exists()); // swept
        assert_eq!(
            pick(&dirs, Some(11), None).unwrap(),
            dir.join("pypower-11.sock")
        );
        assert_eq!(
            pick(&dirs, None, Some("run")).unwrap(),
            dir.join("run-10.sock")
        );
        assert!(pick(&dirs, Some(11), Some("run")).is_err());
        drop(live);
    }

    #[test]
    fn client_args_pick_the_process_and_absolutize_the_file() {
        let a = |v: &[&str]| parse_client(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let (pid, name, words) =
            a(&["--pid", "7", "--name", "run", "on", "--file", "x.log"]).unwrap();
        assert_eq!((pid, name.as_deref()), (Some(7), Some("run")));
        assert_eq!(words[..2], ["on", "--file"]);
        assert!(
            words[2].starts_with('/') && words[2].ends_with("/x.log"),
            "{words:?}"
        );
        assert_eq!(a(&["status"]).unwrap().2, ["status"]);
        for (v, want) in [
            (vec![], "no command given"),
            (vec!["--pid"], "--pid needs a value"),
            (vec!["--pid", "x", "status"], "--pid: `x` is not a number"),
            (vec!["offf"], "(did you mean `off`?)"),
        ] {
            assert!(a(&v).unwrap_err().contains(want), "{v:?}");
        }
    }

    #[test]
    fn serve_makes_a_private_socket_that_answers_and_cleans_up() {
        let dir = tmp("serve").join("sub");
        let sock = serve(&dir, "unit").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&sock), mode(&dir)), (0o600, 0o700));
        assert_eq!(sock, dir.join(format!("unit-{}.sock", std::process::id())));
        let ask = |req: &str| {
            let mut s = UnixStream::connect(&sock).unwrap();
            writeln!(s, "{req}").unwrap();
            BufReader::new(s).lines().next().unwrap().unwrap()
        };
        let who = format!("ferrofire unit (pid {}): logging to ", std::process::id());
        assert!(ask("status").starts_with(&who));
        assert!(ask("nope").starts_with("error: unknown command"));
        assert!(serve(Path::new("/proc/nope"), "unit").is_err());
        stop();
        assert!(!sock.exists());
        tracing::debug!("nothing listens for this"); // no subscriber here: no stderr, no panic
    }
}
