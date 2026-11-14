//! `ferrofire watch`: run whatever executable sits at `--firmware` as `field-device --file <config>`, and restart it
//! when the firmware or the config changes (same hash on two consecutive polls). A crashed child restarts with backoff.
//! No allow-list: any executable is run. ponytail: plain polling, no inotify; add it if sub-poll latency matters.
use crate::sys;
use clap::Args;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct Cli {
    /// Firmware executable to supervise (copied to <run-dir>/active before each start)
    #[arg(long, value_name = "PATH")]
    firmware: PathBuf,
    /// Field-device config passed to the firmware as `field-device --file <PATH>`
    #[arg(long, value_name = "PATH")]
    file: PathBuf,
    /// Poll interval for change detection, milliseconds
    #[arg(long, default_value_t = 500)]
    poll_ms: u64,
    /// Directory for the private copy of the firmware
    #[arg(long, default_value = "/run/ferrofire")]
    run_dir: PathBuf,
}

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn on_stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

type Hashes = (Option<String>, Option<String>);

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn sha256(p: &Path) -> Option<String> {
    fs::read(p).ok().map(|b| hex(&b))
}

fn status_text(s: ExitStatus) -> String {
    s.code().map_or_else(
        || format!("signal {}", s.signal().unwrap_or(0)),
        |c| c.to_string(),
    )
}

/// SIGTERM, up to 5 s grace, then SIGKILL.
fn stop_child(c: &mut Child) {
    sys::kill(c.id() as i32, libc::SIGTERM);
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if matches!(c.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = c.kill();
    let _ = c.wait();
}

/// Copy the firmware to `active` and run it. The bytes are read once and must hash to `want` (what passed the
/// stability check); otherwise the file is mid-push, nothing runs and `Ok(None)` is returned.
fn start(cli: &Cli, want: &str) -> Result<Option<Child>, String> {
    let bytes =
        fs::read(&cli.firmware).map_err(|e| format!("read {}: {e}", cli.firmware.display()))?;
    let got = hex(&bytes);
    if got != want {
        return Ok(None);
    }
    let active = cli.run_dir.join("active");
    let tmp = cli.run_dir.join("active.tmp");
    fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &active).map_err(|e| e.to_string())?;
    eprintln!("watch: firmware sha256={got}");
    Command::new(&active)
        .args(["field-device", "--file"])
        .arg(&cli.file)
        .current_dir(&cli.run_dir) // the device writes event logs to relative paths; the device root is read-only
        .spawn()
        .map(Some)
        .map_err(|e| e.to_string())
}

struct Watch {
    cli: Cli,
    child: Option<Child>,
    applied: Option<Hashes>, // what the current/last start used
    pending: Option<Hashes>, // seen once, not yet stable
    backoff: u64,
    retry_at: Instant,
}

impl Watch {
    fn fail(&mut self) {
        self.retry_at = Instant::now() + Duration::from_secs(self.backoff);
        self.backoff = (self.backoff * 2).min(30);
    }

    /// Hash both files; true when a new pair has been stable for two polls (the old child is stopped).
    fn changed(&mut self) -> bool {
        let now: Hashes = (sha256(&self.cli.firmware), sha256(&self.cli.file));
        if self.applied.as_ref() == Some(&now) {
            self.pending = None;
            return false;
        }
        if self.pending.as_ref() != Some(&now) {
            self.pending = Some(now);
            return false;
        }
        if let Some(a) = &self.applied {
            let what = if a.0 != now.0 { "firmware" } else { "config" };
            eprintln!("watch: restart: {what} changed");
        }
        self.applied = self.pending.take();
        self.backoff = 1;
        self.stop();
        true
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            stop_child(&mut c);
        }
    }

    fn reap(&mut self) {
        let Some(Ok(Some(s))) = self.child.as_mut().map(Child::try_wait) else {
            return;
        };
        eprintln!("watch: exited status={}", status_text(s));
        self.child = None;
        self.fail();
    }

    fn launch(&mut self) {
        let hash = self.applied.as_ref().and_then(|a| a.0.clone());
        let r = hash
            .ok_or_else(|| "firmware unreadable".to_string())
            .and_then(|h| start(&self.cli, &h));
        match r {
            Ok(Some(c)) => {
                eprintln!("watch: started pid={}", c.id());
                self.child = Some(c);
            }
            // firmware changed under us: the next tick sees the new hash, so retry at once
            Ok(None) => self.retry_at = Instant::now(),
            Err(e) => {
                eprintln!("watch: start failed: {e}");
                self.fail();
            }
        }
    }

    fn tick(&mut self) {
        let go = self.changed();
        self.reap();
        if self.child.is_none()
            && (go || (self.applied.is_some() && Instant::now() >= self.retry_at))
        {
            self.launch();
        }
    }
}

pub fn main(cli: Cli) -> i32 {
    if let Err(e) = fs::create_dir_all(&cli.run_dir) {
        eprintln!("watch: cannot create {}: {e}", cli.run_dir.display());
        return 1;
    }
    sys::on_stop_signals(on_stop);
    let poll = Duration::from_millis(cli.poll_ms);
    let mut w = Watch {
        cli,
        child: None,
        applied: None,
        pending: None,
        backoff: 1,
        retry_at: Instant::now(),
    };
    while !STOP.load(Ordering::SeqCst) {
        w.tick();
        std::thread::sleep(poll);
    }
    w.stop();
    0
}
