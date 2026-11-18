//! What the polling clients (`<modbus-client>`, `<dnp3-client>`) share: the `<mode>` switch between a stock bennu fd-client and a
//! front-end processor, the ferrofire-only keys of the latter, device-local point tags, and the echo rule that keeps a FEP from
//! writing a polled value straight back (docs/field-device-clients.md).

use super::super::cmdif::Points;
use super::super::config::{number, text, ConfigError, Opts, Tags};
use super::super::store::{same, Store};
use crate::hint::show;
use crate::model::{Kind, Value};
use roxmltree::Node;
use std::collections::BTreeMap;
use std::time::Duration;

/// `<mode>`: a stock bennu fd-client (the default), or a front-end processor that polls into tags and writes changes down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Bennu,
    Fep,
}

/// The `<mode>` of a client element.
pub fn mode(n: Node) -> Result<Mode, ConfigError> {
    let said = text(n, "mode").unwrap_or_else(|| "bennu".into());
    let known = [("bennu", Mode::Bennu), ("fep", Mode::Fep)];
    let found = known.into_iter().find(|(name, _)| *name == said);
    found
        .map(|(_, mode)| mode)
        .ok_or_else(|| ConfigError(format!("<mode> `{}` is not bennu or fep", show(&said))))
}

/// The ferrofire-only keys of one connection in `fep` mode.
#[derive(Clone, Debug, PartialEq)]
pub struct Fep {
    /// `<scan-rate>`: the time between polls (Modbus; a DNP3 master scans as stock does).
    pub scan: Duration,
    /// `<status-tag>`: a binary device-local tag, true while the session is up and the last poll succeeded.
    pub status: Option<String>,
}

/// `<scan-rate>` of a connection: seconds, decimals allowed, 1 when absent.
fn scan_rate(conn: Node) -> Result<Duration, ConfigError> {
    let secs = number::<f64>(conn, "scan-rate")?.unwrap_or(1.0);
    let scan = Duration::try_from_secs_f64(secs).ok();
    scan.filter(|d| !d.is_zero()).ok_or_else(|| {
        let said = text(conn, "scan-rate").unwrap_or_default();
        ConfigError(format!(
            "<scan-rate> `{}` is not a time above 0 s",
            show(&said)
        ))
    })
}

/// `<scan-rate>` (seconds, default 1) and `<status-tag>` of a connection. Stock has neither, so in `bennu` mode each is a warning
/// (an error under `--strict`) and nothing is returned.
pub fn fep_keys(
    conn: Node,
    mode: Mode,
    tags: &mut Tags,
    opts: &Opts,
) -> Result<Option<Fep>, ConfigError> {
    if mode == Mode::Bennu {
        for key in ["scan-rate", "status-tag"] {
            if text(conn, key).is_some() {
                opts.warn(format!("<{key}> is fep-only, ignored"))?;
            }
        }
        return Ok(None);
    }
    let scan = scan_rate(conn)?;
    let status = match text(conn, "status-tag") {
        Some(t) if point_tag(tags, &t, Kind::Binary, opts)? => Some(t),
        _ => None,
    };
    Ok(Some(Fep { scan, status }))
}

/// The `<cycle-time>` of the field device `n` sits in: how often a FEP looks for local changes to write down.
pub fn cycle(n: Node) -> Duration {
    let ms = n
        .ancestors()
        .find(|a| a.has_tag_name("field-device"))
        .and_then(|fd| number::<u64>(fd, "cycle-time").ok().flatten())
        .filter(|ms| *ms > 0)
        .unwrap_or(1000);
    Duration::from_millis(ms)
}

/// Declare the device-local tag of a point. A tag that exists already (another connection's, a `<tags>` entry) is a warning and
/// `false`: the first declaration keeps it and this mapping is left out.
pub fn point_tag(tags: &mut Tags, tag: &str, kind: Kind, opts: &Opts) -> Result<bool, ConfigError> {
    if tags.kind(tag).is_some() {
        opts.warn(format!(
            "tag `{}` is already declared; this mapping is ignored (the first declaration wins)",
            show(tag)
        ))?;
        return Ok(false);
    }
    tags.add_internal(tag, kind).map(|()| true)
}

/// The echo rule for one output point: a value goes down only when it differs both from the last value sent and from the last
/// value polled in, so what a poll brought is never written back. One write is in flight at a time, and while it is, poll results
/// for the point are ignored: the local write wins.
#[derive(Debug)]
pub struct Echo {
    last_sent: Option<Value>,
    last_polled: Option<Value>,
    pending: bool,
}

impl Echo {
    /// `initial` is the value the tag starts with: it counts as sent, so a FEP does not open by zeroing every output.
    pub fn new(initial: Value) -> Echo {
        Echo {
            last_sent: Some(initial),
            last_polled: None,
            pending: false,
        }
    }

    fn is(slot: &Option<Value>, v: &Value) -> bool {
        slot.as_ref().is_some_and(|s| same(s, v))
    }

    /// A poll read `v`. `false`: a local write is pending and the poll is ignored. A value other than the one last sent means
    /// the device moved on, so that value may be sent again.
    pub fn polled(&mut self, v: Value) -> bool {
        if self.pending {
            return false;
        }
        if !Echo::is(&self.last_sent, &v) {
            self.last_sent = None;
        }
        self.last_polled = Some(v);
        true
    }

    /// The tag holds `now`: `Some` when that must be sent, which starts a write the caller ends with `sent_ok` or `sent_failed`.
    pub fn due(&mut self, now: Value) -> Option<Value> {
        if self.pending || Echo::is(&self.last_sent, &now) || Echo::is(&self.last_polled, &now) {
            return None;
        }
        self.pending = true;
        Some(now)
    }

    /// The write under way was answered: `v` is what the device now has from us.
    pub fn sent_ok(&mut self, v: Value) {
        self.last_sent = Some(v);
        self.sent_failed();
    }

    /// The write under way got no answer: nothing is under way any more, and the value is due again.
    pub fn sent_failed(&mut self) {
        self.pending = false;
    }
}

/// One output point of a FEP session: its echo state and the tag traffic around it.
pub struct Out {
    pub echo: Echo,
    /// The polled value this session last queued for the tag: a queued value that is not this one is somebody's local write.
    wrote: Option<Value>,
}

impl Out {
    pub fn new(initial: Value) -> Out {
        Out {
            echo: Echo::new(initial),
            wrote: None,
        }
    }

    /// The local change to send down now, if the tag holds one. A write still queued for the tag has not landed, so the tag
    /// is judged once it has. The caller ends the write with `echo.sent_ok` or `echo.sent_failed`.
    pub fn due(&mut self, store: &Store, tag: &str) -> Option<Value> {
        match (store.get(tag), store.queued(tag)) {
            (Some(now), None) => self.echo.due(now),
            _ => None,
        }
    }

    /// A poll read `v` for this output. A local change wins: one that has landed is returned to be sent down (as `due` would),
    /// one still queued is left to land, and while one is on its way the poll is ignored. Otherwise `v` goes into the tag.
    pub fn polled(&mut self, store: &Store, tag: &str, v: Value) -> Option<Value> {
        if let Some(local) = self.due(store, tag) {
            return Some(local);
        }
        let queued = store.queued(tag);
        let ours = |q: &Value| self.wrote.as_ref().is_some_and(|w| same(w, q));
        let theirs = queued.as_ref().is_some_and(|q| !ours(q));
        if theirs || !self.echo.polled(v) {
            return None;
        }
        let settled = queued.is_none() && store.get(tag).is_some_and(|now| same(&now, &v));
        if !settled {
            let _ = store.write(tag, v);
            self.wrote = Some(v);
        }
        None
    }
}

/// A poll read `v` for an input point: the device owns the value.
pub fn input_polled(store: &Store, tag: &str, v: Value) {
    let settled = !store.is_queued(tag) && store.get(tag).is_some_and(|now| same(&now, &v));
    if !settled {
        let _ = store.write(tag, v);
    }
}

/// The command interface of a `fep` mode client: a READ answers from the tag; a WRITE queues the tag, and the session sends it
/// down through the echo rule like any other local change.
pub struct TagPoints {
    store: Store,
    /// Tag -> whether the point is an output.
    tags: BTreeMap<String, bool>,
}

impl TagPoints {
    pub fn new(store: Store, tags: impl IntoIterator<Item = (String, bool)>) -> TagPoints {
        TagPoints {
            store,
            tags: tags.into_iter().collect(),
        }
    }
}

impl Points for TagPoints {
    fn names(&self) -> Vec<String> {
        self.tags.keys().cloned().collect()
    }

    async fn read(&self, tag: &str) -> Result<Value, String> {
        self.store.get(tag).ok_or_else(|| "no such tag".into())
    }

    async fn write(&self, tag: &str, v: Value) -> Result<(), String> {
        match (self.tags.get(tag), self.store.kind(tag)) {
            (Some(true), Some(kind)) => {
                let kind = format!("{kind:?}").to_lowercase();
                self.store
                    .write(tag, v)
                    .map_err(|_| format!("the tag is {kind}"))
            }
            (Some(false), _) => Err("the point is an input".into()),
            _ => Err("no such tag".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Value::{Analog, Binary};

    fn doc(xml: &str) -> roxmltree::Document<'_> {
        roxmltree::Document::parse(xml).unwrap()
    }

    #[test]
    fn the_mode_is_bennu_unless_it_says_fep_and_anything_else_is_named() {
        let of = |xml: &str| mode(doc(xml).root_element());
        assert_eq!(of("<c/>"), Ok(Mode::Bennu));
        assert_eq!(of("<c><mode>bennu</mode></c>"), Ok(Mode::Bennu));
        assert_eq!(of("<c><mode> fep </mode></c>"), Ok(Mode::Fep));
        let e = of("<c><mode>FEP2</mode></c>").unwrap_err();
        assert_eq!(e.0, "<mode> `FEP2` is not bennu or fep");
        assert!(of("<c><mode/></c>").is_err());
    }

    #[test]
    fn the_stock_samples_are_bennu_mode_clients_with_no_fep_keys() {
        for (xml, client, conn) in [
            (
                include_str!("../../../tests/conformance/modbus-client.xml"),
                "modbus-client",
                "modbus-connection",
            ),
            (
                include_str!("../../../tests/conformance/dnp3-client.xml"),
                "dnp3-client",
                "dnp3-connection",
            ),
        ] {
            let d = doc(xml);
            let c = d.descendants().find(|n| n.has_tag_name(client)).unwrap();
            let conn = c.children().find(|n| n.has_tag_name(conn)).unwrap();
            let mut tags = Tags::default();
            assert_eq!(mode(c), Ok(Mode::Bennu));
            assert_eq!(
                fep_keys(conn, Mode::Bennu, &mut tags, &Opts { strict: true }),
                Ok(None)
            );
        }
        // the generator closes <modbus-client> with </modbus-tcp-client>
        let fixed = crate::fd::config::normalise(include_str!(
            "../../../tests/conformance/fd-client-generated.xml"
        ));
        assert!(doc(&fixed)
            .descendants()
            .any(|n| n.has_tag_name("modbus-client")));
    }

    fn keys(inner: &str, mode: Mode, strict: bool) -> (Result<Option<Fep>, ConfigError>, Tags) {
        let xml = format!("<conn>{inner}</conn>");
        let mut tags = Tags::default();
        tags.add_internal("taken", Kind::Analog).unwrap();
        let r = fep_keys(doc(&xml).root_element(), mode, &mut tags, &Opts { strict });
        (r, tags)
    }

    #[test]
    fn fep_keys_count_in_fep_mode_and_are_warned_about_in_bennu_mode() {
        let s = Duration::from_secs_f64;
        let (r, _) = keys("", Mode::Fep, true);
        assert_eq!(
            r,
            Ok(Some(Fep {
                scan: s(1.0),
                status: None
            }))
        );
        let (r, tags) = keys(
            "<scan-rate>2</scan-rate><status-tag>up</status-tag>",
            Mode::Fep,
            true,
        );
        assert_eq!(
            r,
            Ok(Some(Fep {
                scan: s(2.0),
                status: Some("up".into())
            }))
        );
        assert_eq!(tags.kind("up"), Some(Kind::Binary));
        assert_eq!(
            keys("<scan-rate>0.25</scan-rate>", Mode::Fep, true)
                .0
                .unwrap()
                .unwrap()
                .scan,
            s(0.25)
        );
        for bad in ["0", "-1", "x", "nan", "inf", "1e400"] {
            let e = keys(&format!("<scan-rate>{bad}</scan-rate>"), Mode::Fep, false)
                .0
                .unwrap_err();
            assert!(e.0.contains("<scan-rate>") && e.0.contains(bad), "{e}");
        }
        // bennu mode: stock has neither key
        let (r, tags) = keys(
            "<scan-rate>2</scan-rate><status-tag>up</status-tag>",
            Mode::Bennu,
            false,
        );
        assert_eq!(r, Ok(None));
        assert_eq!(tags.kind("up"), None);
        let e = keys("<scan-rate>2</scan-rate>", Mode::Bennu, true)
            .0
            .unwrap_err();
        assert_eq!(e.0, "<scan-rate> is fep-only, ignored");
        let e = keys("<status-tag>up</status-tag>", Mode::Bennu, true)
            .0
            .unwrap_err();
        assert_eq!(e.0, "<status-tag> is fep-only, ignored");
        // a status tag that is already a tag: the first declaration wins
        let (r, _) = keys("<status-tag>taken</status-tag>", Mode::Fep, false);
        assert_eq!(r.unwrap().unwrap().status, None);
        assert!(keys("<status-tag>taken</status-tag>", Mode::Fep, true)
            .0
            .is_err());
    }

    #[test]
    fn the_cycle_is_the_field_devices_cycle_time() {
        let of = |xml: &str| {
            let d = doc(xml);
            cycle(d.descendants().find(|n| n.has_tag_name("c")).unwrap())
        };
        let ms = Duration::from_millis;
        assert_eq!(
            of("<field-device><cycle-time>250</cycle-time><comms><c/></comms></field-device>"),
            ms(250)
        );
        assert_eq!(
            of("<field-device><comms><c/></comms></field-device>"),
            ms(1000)
        );
        assert_eq!(
            of("<field-device><cycle-time>0</cycle-time><comms><c/></comms></field-device>"),
            ms(1000)
        );
        assert_eq!(
            of("<field-device><cycle-time>x</cycle-time><comms><c/></comms></field-device>"),
            ms(1000)
        );
        assert_eq!(of("<c/>"), ms(1000));
    }

    #[test]
    fn a_point_tag_is_declared_once_and_the_first_declaration_wins() {
        let mut tags = Tags::default();
        let lax = Opts::default();
        assert_eq!(point_tag(&mut tags, "p", Kind::Analog, &lax), Ok(true));
        assert_eq!(tags.kind("p"), Some(Kind::Analog));
        assert_eq!(point_tag(&mut tags, "p", Kind::Binary, &lax), Ok(false));
        assert_eq!(tags.kind("p"), Some(Kind::Analog));
        let e = point_tag(&mut tags, "p", Kind::Analog, &Opts { strict: true }).unwrap_err();
        assert!(e.0.contains("`p`") && e.0.contains("already"), "{e}");
    }

    #[test]
    fn the_echo_rule() {
        let mut e = Echo::new(Analog(0.0));
        assert_eq!(e.due(Analog(0.0)), None, "the initial value is not a write");
        assert!(e.polled(Analog(5.0)));
        assert_eq!(e.due(Analog(5.0)), None, "a polled value is not sent back");
        assert_eq!(e.due(Analog(6.0)), Some(Analog(6.0)));
        assert_eq!(e.due(Analog(6.0)), None, "one write at a time");
        assert!(
            !e.polled(Analog(7.0)),
            "a poll result is ignored while a local write is pending"
        );
        e.sent_failed();
        assert_eq!(
            e.due(Analog(6.0)),
            Some(Analog(6.0)),
            "a failed write is due again"
        );
        e.sent_ok(Analog(6.0));
        assert_eq!(e.due(Analog(6.0)), None);
        assert_eq!(e.due(Analog(5.0)), None, "still the last value polled");
        assert!(e.polled(Analog(6.0)), "the device shows what was sent");
        assert_eq!(e.due(Analog(6.0)), None);
    }

    #[test]
    fn a_value_sent_before_is_due_again_once_the_device_has_shown_another() {
        let mut e = Echo::new(Binary(false));
        assert_eq!(e.due(Binary(true)), Some(Binary(true)));
        e.sent_ok(Binary(true));
        assert!(e.polled(Binary(false)), "someone else switched it off");
        assert_eq!(e.due(Binary(false)), None);
        assert_eq!(e.due(Binary(true)), Some(Binary(true)));
    }

    #[test]
    fn a_nan_is_the_same_value_as_itself() {
        let mut e = Echo::new(Analog(0.0));
        assert!(e.due(Analog(f64::NAN)).is_some());
        e.sent_ok(Analog(f64::NAN));
        assert_eq!(e.due(Analog(f64::NAN)), None);
    }

    fn store(tags: &[(&str, Value)]) -> Store {
        Store::new(crate::fd::store::Layout {
            internal: tags.iter().map(|(t, v)| (t.to_string(), *v)).collect(),
            ..Default::default()
        })
    }

    #[test]
    fn a_polled_output_value_lands_in_the_tag_and_is_never_due() {
        let s = store(&[("t", Analog(0.0))]);
        let mut o = Out::new(Analog(0.0));
        assert_eq!(o.polled(&s, "t", Analog(5.0)), None);
        assert_eq!(s.queued("t"), Some(Analog(5.0)));
        assert_eq!(o.due(&s, "t"), None, "not judged while the write is queued");
        s.apply_internal();
        assert_eq!(s.get("t"), Some(Analog(5.0)));
        assert_eq!(o.due(&s, "t"), None, "what a poll brought is not sent back");
        // a newer poll before the cycle ends replaces ours; the same value again queues nothing
        assert_eq!(o.polled(&s, "t", Analog(6.0)), None);
        assert_eq!(o.polled(&s, "t", Analog(7.0)), None);
        s.apply_internal();
        assert_eq!(s.get("t"), Some(Analog(7.0)));
        assert_eq!(o.polled(&s, "t", Analog(7.0)), None);
        assert_eq!(s.queued("t"), None);
        assert_eq!(o.due(&s, "t"), None);
    }

    #[test]
    fn a_local_write_wins_over_a_poll_whether_it_has_landed_or_is_still_queued() {
        let s = store(&[("t", Analog(0.0))]);
        let mut o = Out::new(Analog(0.0));
        assert_eq!(o.polled(&s, "t", Analog(5.0)), None);
        s.apply_internal();
        // landed: the poll hands the local value back to be sent, and leaves the tag
        s.write("t", Analog(9.0)).unwrap();
        s.apply_internal();
        assert_eq!(o.polled(&s, "t", Analog(5.0)), Some(Analog(9.0)));
        assert_eq!(s.queued("t"), None);
        assert_eq!(
            o.polled(&s, "t", Analog(5.0)),
            None,
            "ignored while the write is on its way"
        );
        assert_eq!(s.queued("t"), None);
        o.echo.sent_ok(Analog(9.0));
        assert_eq!(o.due(&s, "t"), None);
        // still queued: the poll does not replace it, and the next cycle finds it due
        s.write("t", Analog(11.0)).unwrap();
        assert_eq!(o.polled(&s, "t", Analog(9.0)), None);
        assert_eq!(s.queued("t"), Some(Analog(11.0)));
        s.apply_internal();
        assert_eq!(o.due(&s, "t"), Some(Analog(11.0)));
    }

    #[test]
    fn an_input_takes_whatever_the_device_says() {
        let s = store(&[("i", Binary(false))]);
        input_polled(&s, "i", Binary(false));
        assert_eq!(s.queued("i"), None, "nothing to change");
        input_polled(&s, "i", Binary(true));
        assert_eq!(s.queued("i"), Some(Binary(true)));
        // a local write to an input does not survive the next poll
        s.apply_internal();
        s.write("i", Binary(false)).unwrap();
        input_polled(&s, "i", Binary(true));
        s.apply_internal();
        assert_eq!(s.get("i"), Some(Binary(true)));
    }

    #[tokio::test]
    async fn the_fep_command_interface_reads_tags_and_queues_writes_to_outputs_of_the_right_kind() {
        use crate::fd::cmdif::Points;
        let s = store(&[("out", Analog(1.5)), ("in", Binary(true))]);
        let p = TagPoints::new(
            s.clone(),
            [("out".to_string(), true), ("in".to_string(), false)],
        );
        assert_eq!(p.names(), ["in", "out"]);
        assert_eq!(p.read("out").await, Ok(Analog(1.5)));
        assert_eq!(p.read("in").await, Ok(Binary(true)));
        assert!(p.read("nope").await.is_err());
        assert_eq!(p.write("out", Analog(2.0)).await, Ok(()));
        assert_eq!(s.queued("out"), Some(Analog(2.0)));
        assert_eq!(
            p.write("out", Binary(true)).await,
            Err("the tag is analog".into())
        );
        assert_eq!(
            p.write("in", Binary(false)).await,
            Err("the point is an input".into())
        );
        assert_eq!(s.queued("in"), None);
        assert!(p.write("nope", Analog(1.0)).await.is_err());
    }
}
