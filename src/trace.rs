//! Process logging on `tracing`. Default output is what the hand-written `eprintln!`s printed: `Warn: text` on stderr,
//! info and up. The level (a reload layer) and the destination (stderr or an open file) can be changed while the
//! process runs; `ferrofire log` (ww-otj.30) drives `set_level` and `set_output`.

use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{filter::LevelFilter, reload, util::SubscriberInitExt, Registry};

type Handle = reload::Handle<LevelFilter, Registry>;
static LEVEL: OnceLock<Handle> = OnceLock::new();
static FILE: Mutex<Option<File>> = Mutex::new(None);

/// `Level: message k=v ...` (no time, target or span: the lines stay the shape they always were).
struct Plain;

impl<S, N> FormatEvent<S, N> for Plain
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut w: Writer<'_>,
        ev: &Event<'_>,
    ) -> fmt::Result {
        let name = match *ev.metadata().level() {
            Level::ERROR => "Error",
            Level::WARN => "Warn",
            Level::INFO => "Info",
            Level::DEBUG => "Debug",
            Level::TRACE => "Trace",
        };
        write!(w, "{name}: ")?;
        ctx.field_format().format_fields(w.by_ref(), ev)?;
        writeln!(w)
    }
}

/// Each event goes to the open file if one is set, else stderr. One `write` per line, so lines never interleave.
struct Out;

impl Write for Out {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if crate::log::route(b) {
            return Ok(b.len()); // the control socket's file sink or tails took it (ferrofire log)
        }
        match crate::hint::lock(&FILE).as_mut() {
            Some(f) => f.write(b),
            None => io::stderr().write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Out {
    type Writer = Out;
    fn make_writer(&'a self) -> Out {
        Out
    }
}

/// Install the global subscriber once (a second call is a no-op), at info.
pub fn init() {
    LEVEL.get_or_init(|| {
        let (filter, handle) = reload::Layer::new(LevelFilter::INFO);
        let layer = tracing_subscriber::fmt::layer()
            .event_format(Plain)
            .with_ansi(false)
            .with_writer(Out);
        let _ = Registry::default().with(filter).with(layer).try_init();
        handle
    });
}

/// Change the level at run time; false before `init`.
pub fn set_level(l: LevelFilter) -> bool {
    LEVEL.get().is_some_and(|h| h.reload(l).is_ok())
}

/// Send the log to `file` (already open, append) or, with `None`, back to stderr. Dropping the old file closes it.
pub fn set_output(file: Option<File>) {
    *crate::hint::lock(&FILE) = file;
}
