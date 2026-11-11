//! Per-protocol event log (SPEC 3.5): CSV lines `seq,YYYY-Mon-DD-HH:MM:SS,level,module,event,message`, written to a file
//! relative to the cwd and truncated at start. Messages come from the network, so control characters never reach the file.

use crate::hint::lock;
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct EventLog {
    module: String,
    out: Mutex<(u64, Box<dyn Write + Send>)>,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `YYYY-Mon-DD-HH:MM:SS` for unix seconds already shifted to local time (days-from-civil, inverted).
pub fn stamp(local_secs: i64) -> String {
    let (days, rem) = (local_secs.div_euclid(86400), local_secs.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year}-{}-{day:02}-{:02}:{:02}:{:02}",
        MONTHS[(month - 1) as usize],
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

impl EventLog {
    /// Open (truncate) `path`, or `<module>.log` when the config gave none. A file that cannot be opened logs nowhere.
    pub fn open(path: Option<&str>, module: &str) -> EventLog {
        let default = format!("{module}.log");
        let path = path.filter(|p| !p.is_empty()).unwrap_or(&default);
        let sink: Box<dyn Write + Send> = match std::fs::File::create(path) {
            Ok(f) => Box::new(f),
            Err(e) => {
                tracing::warn!("{module}: cannot open event log {path}: {e}");
                Box::new(std::io::sink())
            }
        };
        EventLog::to(module, sink)
    }

    pub fn to(module: &str, sink: Box<dyn Write + Send>) -> EventLog {
        EventLog {
            module: module.into(),
            out: Mutex::new((0, sink)),
        }
    }

    pub fn log(&self, level: &str, event: &str, message: &str) {
        let now = now_secs();
        let at = stamp(now + crate::sys::utc_offset(now));
        let mut g = lock(&self.out);
        let line = format!(
            "{},{at},{level},{},{},{}\n",
            g.0,
            self.module,
            clean(event),
            clean(message)
        );
        let _ = g.1.write_all(line.as_bytes()).and_then(|_| g.1.flush());
        g.0 += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            lock(&self.0).extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stamp_formats_boltzmann_dates() {
        assert_eq!(stamp(0), "1970-Jan-01-00:00:00");
        assert_eq!(stamp(1_762_611_725), "2025-Nov-08-14:22:05");
        assert_eq!(stamp(951_782_400 + 86_399), "2000-Feb-29-23:59:59"); // leap day
        assert_eq!(stamp(1_709_251_200), "2024-Mar-01-00:00:00");
        assert_eq!(stamp(-1), "1969-Dec-31-23:59:59");
    }

    #[test]
    fn lines_are_numbered_csv_with_clean_fields() {
        let buf = Buf::default();
        let log = EventLog::to("modbus-server", Box::new(buf.clone()));
        log.log("info", "read coils", "addr 0, qty 2");
        log.log("error", "bad\nevent", "a,b\0c");
        let text = String::from_utf8(lock(&buf.0).clone()).unwrap();
        let lines: Vec<Vec<&str>> = text.lines().map(|l| l.split(',').collect()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0], "0");
        assert_eq!(lines[0][1].len(), 20);
        assert_eq!(
            &lines[0][2..],
            ["info", "modbus-server", "read coils", "addr 0", " qty 2"]
        );
        assert_eq!(lines[1][0], "1");
        assert_eq!(
            &lines[1][2..],
            ["error", "modbus-server", "bad?event", "a", "b?c"]
        );
    }

    #[test]
    fn open_truncates_and_defaults_the_name() {
        let dir = std::env::temp_dir().join(format!("fd-evlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.log");
        std::fs::write(&p, "old\n").unwrap();
        let log = EventLog::open(Some(p.to_str().unwrap()), "m");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
        log.log("info", "e", "m");
        assert!(std::fs::read_to_string(&p).unwrap().starts_with("0,"));
        EventLog::open(Some("/nonexistent-dir/x.log"), "m").log("info", "e", "dropped");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
