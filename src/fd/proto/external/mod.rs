//! External protocol modules: a program that speaks a protocol for the device over ffmod/1 (JSON lines).
//!
//! `<external-module>` with transport `stdio`: the device starts `<command>` (split on ASCII whitespace: no shell, no quoting),
//! speaks ffmod/1 over its stdin and stdout, logs its stderr lines, restarts it on the backoff when it exits or breaks the
//! session, and stops it (`stop`, stdin closed, SIGTERM at 2 s, SIGKILL at 5 s) with the device.

pub mod msg;
pub mod session;

use super::{backoff, device_name, name_in, Module, BACKOFF_FIRST};
use crate::fd::config::{child, number, require, text, ConfigError, Opts, Tags};
use crate::fd::store::Store;
use crate::hint::show;
use msg::MAX_LINE;
use roxmltree::Node;
use session::{session, End, Spec, READY_TIMEOUT};
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout_at, Instant};
use tokio_util::sync::CancellationToken;

/// Counted from the device's stop: until when a module may exit by itself, then SIGTERM; until when after that, then SIGKILL.
const TERM_AT: Duration = Duration::from_secs(2);
const KILL_AT: Duration = Duration::from_secs(5);

/// A module's `error` under `--strict`, for `fd::run` to return once the device has stopped.
static FATAL: Mutex<Option<String>> = Mutex::new(None);

/// Stop the device because of `msg`; `fd::run` returns it as its error.
pub fn fatal(msg: String, stop: &CancellationToken) {
    *FATAL.lock().unwrap_or_else(PoisonError::into_inner) = Some(msg);
    stop.cancel();
}

pub fn take_fatal() -> Option<String> {
    FATAL.lock().unwrap_or_else(PoisonError::into_inner).take()
}

pub struct External {
    spec: Spec,
    /// `<command>` split on ASCII whitespace; never empty.
    argv: Vec<String>,
}

impl Module for External {
    fn parse(node: Node, _: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let name = name_in(node, "name", "", |s| !s.is_empty(), "not empty")?;
        let transport = text(node, "transport").unwrap_or_else(|| "stdio".into());
        if transport != "stdio" {
            let t = show(&transport);
            return Err(ConfigError(format!(
                "<transport> `{t}` is not supported (stdio)"
            )));
        }
        // the inner XML of `<config>` as written: its first element
        let inner = child(node, "config").and_then(|c| c.first_element_child());
        let config = inner.map_or("", |e| &node.document().input_text()[e.range()]);
        External::new(node, name, &require(node, "command")?, config, opts)
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let mut pause = BACKOFF_FIRST;
        loop {
            let began = Instant::now();
            match self.once(&store, &stop).await {
                End::Stopped => return,
                End::Refused(msg) => return self.refused(msg, &stop),
                End::Broken => {}
            }
            if was_up(began.elapsed()) {
                pause = BACKOFF_FIRST;
            }
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = sleep(pause) => {}
            }
            pause = backoff(pause, false);
        }
    }
}

impl External {
    /// An `External` for `node`, a stock element a module's manifest serves: `node` itself is the module's config.
    pub fn from_element(node: Node, cmd: &str, opts: &Opts) -> Result<External, ConfigError> {
        let config = &node.document().input_text()[node.range()];
        External::new(node, node.tag_name().name().into(), cmd, config, opts)
    }

    fn new(
        node: Node,
        name: String,
        cmd: &str,
        config: &str,
        opts: &Opts,
    ) -> Result<External, ConfigError> {
        let argv: Vec<String> = cmd.split_ascii_whitespace().map(String::from).collect();
        if argv.is_empty() {
            return Err(ConfigError(format!("module {name}: <command> is empty")));
        }
        let fd = node.ancestors().find(|a| a.has_tag_name("field-device"));
        let cycle = fd.map(|fd| number::<u64>(fd, "cycle-time")).transpose()?;
        let cycle = cycle.flatten().filter(|c| *c > 0).unwrap_or(1000);
        let spec = Spec {
            name,
            device: device_name(node),
            strict: opts.strict,
            config: config.into(),
            cycle: Duration::from_millis(cycle),
            cmdif: text(node, "command-interface").filter(|s| !s.is_empty()),
        };
        Ok(External { spec, argv })
    }

    /// One run of the module: spawn it, hold its session, and see it gone.
    async fn once(&self, store: &Store, stop: &CancellationToken) -> End {
        let name = &self.spec.name;
        tracing::info!("module {name}: starting {:?}", self.argv);
        let mut cmd = Command::new(&self.argv[0]);
        cmd.args(&self.argv[1..]).env("FERROFIRE_MODULE", name);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // its own process group, so that a signal reaches what a wrapper script started too
        let child = cmd.process_group(0).kill_on_drop(true).spawn();
        match child {
            Ok(child) => self.session(child, store, stop).await,
            Err(e) => {
                tracing::error!("module {name}: cannot start: {e}");
                End::Broken
            }
        }
    }

    /// Run the session to its end (never dropped half way: its writer would be left running), then reap the module.
    async fn session(&self, mut child: Child, store: &Store, stop: &CancellationToken) -> End {
        let (w, r) = (child.stdin.take(), child.stdout.take());
        let (w, r) = (w.expect("piped"), BufReader::new(r.expect("piped")));
        stderr(&self.spec.name, child.stderr.take().expect("piped"));
        let s = session(r, w, &self.spec, store.clone(), stop.clone());
        tokio::pin!(s);
        // when the device's stop came: the 2 s and 5 s count from there, whatever the session spends saying `stop`
        let mut stopped = None;
        let end = loop {
            tokio::select! {
                end = &mut s => break end,
                _ = stop.cancelled(), if stopped.is_none() => stopped = Some(Instant::now()),
            }
        };
        reap(&self.spec.name, child, stopped.unwrap_or_else(Instant::now)).await;
        end
    }

    /// The module said `error`: not restarted; under `--strict` the device stops with it.
    fn refused(&self, msg: String, stop: &CancellationToken) {
        tracing::error!("module {}: not restarted", self.spec.name);
        if self.spec.strict {
            fatal(msg, stop);
        }
    }
}

/// Whether a run that ended after `ran` (spawn, session and reap) had come up.
// ponytail: `End` does not say whether `ready` came. A run without it lasts READY_TIMEOUT plus the session's stop grace (1 s) plus
// the reap (up to 5 s), so only a run longer than twice READY_TIMEOUT surely had it; a module that crashes sooner after `ready`
// backs off as if it never came up. Exact: `End::Broken { was_ready }` in session.rs.
fn was_up(ran: Duration) -> bool {
    ran > 2 * READY_TIMEOUT
}

/// Log the module's stderr, a line (at most `MAX_LINE` bytes: a longer one is logged in pieces) at a time at `info`, until it closes.
fn stderr(name: &str, err: impl AsyncRead + Unpin + Send + 'static) {
    let name = name.to_string();
    let mut err = BufReader::new(err);
    tokio::spawn(async move {
        let mut line = vec![];
        let cap = MAX_LINE as u64;
        while (&mut err)
            .take(cap)
            .read_until(b'\n', &mut line)
            .await
            .is_ok_and(|n| n > 0)
        {
            let text = String::from_utf8_lossy(&line);
            tracing::info!("module {name}: {}", text.trim_end().escape_debug());
            line.clear();
        }
    });
}

/// Wait for the module to exit after its session; its process group gets SIGTERM at `from + TERM_AT`, SIGKILL at `from + KILL_AT`.
async fn reap(name: &str, mut child: Child, from: Instant) {
    for (at, sig, what) in [
        (TERM_AT, libc::SIGTERM, "SIGTERM"),
        (KILL_AT, libc::SIGKILL, "SIGKILL"),
    ] {
        if timeout_at(from + at, child.wait()).await.is_ok() {
            return;
        }
        tracing::warn!("module {name}: still running after {at:?}, {what}");
        // the group is the child's pid, which stays ours until `wait` reaps it
        if let Some(pid) = child.id() {
            crate::sys::kill(-(pid as i32), sig);
        }
    }
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Result<External, ConfigError> {
        let doc = roxmltree::Document::parse(xml).unwrap();
        let node = doc
            .descendants()
            .find(|n| n.has_tag_name("external-module"));
        External::parse(node.unwrap(), &mut Tags::default(), &Opts::default())
    }

    #[test]
    fn parse_reads_name_command_config_and_the_device_cycle() {
        let m = parse(
            "<field-device><name>rtu</name><cycle-time>250</cycle-time><comms><external-module>\
             <name>bac</name><command> python3  m.py\t-v </command>\
             <config> <bacnet-server><x a=\"1\"/></bacnet-server> </config>\
             <command-interface>tcp://127.0.0.1:1340</command-interface></external-module></comms></field-device>",
        )
        .unwrap();
        assert_eq!(m.argv, ["python3", "m.py", "-v"]);
        assert_eq!(m.spec.name, "bac");
        assert_eq!(m.spec.device, "rtu");
        assert_eq!(m.spec.config, "<bacnet-server><x a=\"1\"/></bacnet-server>");
        assert_eq!(m.spec.cycle, Duration::from_millis(250));
        assert_eq!(m.spec.cmdif.as_deref(), Some("tcp://127.0.0.1:1340"));
        let m = parse("<external-module><name>m</name><command>x</command><transport>stdio</transport></external-module>");
        let m = m.unwrap();
        assert_eq!(
            (m.spec.config.as_str(), m.spec.cycle),
            ("", Duration::from_secs(1))
        );
        assert_eq!(m.spec.cmdif, None);
    }

    #[test]
    fn parse_errors_name_the_key() {
        let err = |xml: &str| parse(xml).err().unwrap().0;
        assert!(err("<external-module><command>x</command></external-module>").contains("<name>"));
        assert!(err("<external-module><name>m</name></external-module>").contains("<command>"));
        assert!(
            err("<external-module><name>m</name><command> </command></external-module>")
                .contains("<command> is empty")
        );
        let tcp = "<external-module><name>m</name><command>x</command><transport>tcp://h:1</transport></external-module>";
        assert!(err(tcp).contains("<transport> `tcp://h:1`"));
    }

    #[test]
    fn only_a_run_longer_than_a_ready_timeout_and_its_stop_counts_as_up() {
        // no `ready`: the timeout, the session's stop grace and a module that has to be killed
        assert!(!was_up(READY_TIMEOUT + Duration::from_secs(6)));
        assert!(was_up(2 * READY_TIMEOUT + Duration::from_millis(1)));
        assert!(!was_up(Duration::ZERO));
    }

    #[test]
    fn from_element_passes_the_stock_element_on() {
        let xml = "<field-device><name>d</name><comms><bacnet-client><command-interface>tcp://127.0.0.1:9</command-interface>\
                   </bacnet-client></comms></field-device>";
        let doc = roxmltree::Document::parse(xml).unwrap();
        let node = doc
            .descendants()
            .find(|n| n.has_tag_name("bacnet-client"))
            .unwrap();
        let m = External::from_element(
            node,
            "python3 module.py",
            &Opts {
                strict: true,
                ..Opts::default()
            },
        )
        .unwrap();
        assert_eq!(m.spec.name, "bacnet-client");
        assert!(m
            .spec
            .config
            .starts_with("<bacnet-client><command-interface>"));
        assert_eq!(m.spec.cmdif.as_deref(), Some("tcp://127.0.0.1:9"));
        assert!(m.spec.strict);
    }
}
