//! IEC 61850 server (`<iec61850-server>`, not part of stock bennu, so there is no oracle: the proof is libiec61850 and
//! tshark, `conformance/iec61850/`). An MMS server over the OSI stack (`fd::mms`) whose namespace is generated from the
//! point map (`scl`), plus a GOOSE publisher on a raw Ethernet interface.
//!
//! Reads come from the datastore (a 4 Hz refresh pushes changes into the model, which reports them to subscribed clients
//! and GOOSE). A client's `Oper` on a controllable point becomes a write on the datastore's queue and takes effect when
//! the provider ACKs it, like every other protocol. GOOSE needs `CAP_NET_RAW` and an L2 segment; without them the
//! publisher logs why and the MMS server runs on.
//! The default GOOSE group address is 01-0C-CD-01-00-01 like libiec61850's examples: its subscriber (v1.6, observed) never
//! delivers frames sent to 01-0C-CD-01-00-00 although both are in the GOOSE range.
//! GOOSE subscriber: not built (no consumer for the values in a field device).

pub mod scl;

use super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::mms::{self, AssocId, Due, Handler, Mms};
use super::super::net;
use super::super::store::Store;
use super::Module;
use crate::hint::show;
use crate::model::{Kind, Value as Datum};
use iec61850_rs::common::{Clock, Instant, Limits, MacAddr, SystemClock};
use iec61850_rs::net::{Link, LinkOptions};
use iec61850_rs::proto::data::Value;
use iec61850_rs::proto::mms::association::Negotiated;
use iec61850_rs::proto::mms::control::AddCause;
use iec61850_rs::proto::mms::Mms as Pdu;
use iec61850_rs::server::{Acsi, AcsiConfig, Answer, ControlEvent, Ied, Stage};
use roxmltree::Node;
use scl::{Goose, Model, Point, Role};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Defaults are the names libiec61850's example server uses, so its stock clients and subscribers work unmodified.
const IED: &str = "simpleIO";
const LD: &str = "GenericIO";
const GOOSE_CB: &str = "gcbAnalogValues";
const REFRESH: Duration = Duration::from_millis(250);

pub struct Server {
    endpoint: String,
    log: Arc<EventLog>,
    model: Model,
    ied: Ied,
}

fn name_or(n: Node, key: &str, default: &str) -> Result<String, ConfigError> {
    let v = text(n, key).unwrap_or_else(|| default.to_string());
    if scl::valid_name(&v) {
        Ok(v)
    } else {
        Err(ConfigError(format!(
            "<{key}> `{}` is not a name (letters, digits, _; at most 32)",
            show(&v)
        )))
    }
}

fn mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn goose_mac(g: Node) -> Result<String, ConfigError> {
    let m = text(g, "mac").unwrap_or_else(|| "01-0C-CD-01-00-01".into());
    if mac(&m) {
        Ok(m)
    } else {
        Err(ConfigError(format!(
            "<mac> `{}` is not XX-XX-XX-XX-XX-XX",
            show(&m)
        )))
    }
}

/// What the interface name and the retransmission times must satisfy.
fn goose_check(g: &Goose) -> Result<(), ConfigError> {
    if g.interface.is_empty() || g.interface.len() > 15 || g.interface.contains('/') {
        return Err(ConfigError(format!(
            "<interface> `{}` is not an interface name",
            show(&g.interface)
        )));
    }
    if g.min_ms == 0 || g.min_ms > g.max_ms {
        return Err(ConfigError(
            "<goose> needs 0 < min-time-ms <= max-time-ms".into(),
        ));
    }
    Ok(())
}

/// `<vlan-id>` (0-4095, none by default) and `<vlan-priority>` (0-7, 4 by default); out of range falls back.
fn goose_vlan(g: Node) -> Result<(Option<u16>, u8), ConfigError> {
    let id = number::<u16>(g, "vlan-id")?.filter(|v| *v < 4096);
    let prio = number::<u8>(g, "vlan-priority")?.filter(|p| *p < 8);
    Ok((id, prio.unwrap_or(4)))
}

fn goose(node: Node) -> Result<Option<Goose>, ConfigError> {
    let Some(g) = super::super::config::child(node, "goose") else {
        return Ok(None);
    };
    let (vlan_id, vlan_priority) = goose_vlan(g)?;
    let out = Goose {
        interface: require(g, "interface")?,
        mac: goose_mac(g)?,
        appid: number(g, "appid")?.unwrap_or(1000),
        vlan_id,
        vlan_priority,
        min_ms: number(g, "min-time-ms")?.unwrap_or(4),
        max_ms: number(g, "max-time-ms")?.unwrap_or(1000),
        cb_name: name_or(g, "control-block", GOOSE_CB)?,
    };
    goose_check(&out)?;
    Ok(Some(out))
}

/// One point element: the data object is `Ind1`, `AnIn2`, ... in logical node `GGIO1` unless `<ln>` / `<name>` say otherwise.
fn point(
    n: Node,
    role: Role,
    seen: &mut BTreeMap<(String, u16, Role), usize>,
) -> Result<Point, ConfigError> {
    let (class, inst) = match text(n, "ln") {
        None => ("GGIO".to_string(), 1),
        Some(s) => scl::parse_ln(&s).ok_or_else(|| {
            ConfigError(format!(
                "<ln> `{}` is not a logical node like GGIO1",
                show(&s)
            ))
        })?,
    };
    let count = seen.entry((class.clone(), inst, role)).or_insert(0);
    *count += 1;
    let name = match text(n, "name") {
        None => scl::default_name(role, *count),
        Some(_) => name_or(n, "name", "")?,
    };
    Ok(Point {
        tag: require(n, "tag")?,
        role,
        class,
        inst,
        name,
        sbo: role.controllable() && matches!(text(n, "sbo").as_deref(), Some("1" | "true")),
    })
}

fn points(node: Node, tags: &Tags, opts: &Opts) -> Result<Vec<Point>, ConfigError> {
    let mut seen = BTreeMap::new();
    let mut out = vec![];
    for n in node.children().filter(|c| c.is_element()) {
        let Some(role) = Role::from_element(n.tag_name().name()) else {
            continue;
        };
        let p = point(n, role, &mut seen)?;
        if tags.mapped(&p.tag, role.kind(), opts)? {
            out.push(p);
        }
    }
    Ok(out)
}

impl Module for Server {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let log = Arc::new(EventLog::open(
            text(node, "event-logging").as_deref(),
            "iec61850-server",
        ));
        let model = Model {
            ied: name_or(node, "ied-name", IED)?,
            ld: name_or(node, "ld-inst", LD)?,
            points: points(node, tags, opts)?,
            goose: goose(node)?,
        };
        model.check().map_err(ConfigError)?;
        let ied = Ied::from_scl(&model.scl(), Some(&model.ied))
            .map_err(|e| ConfigError(format!("<iec61850-server> model: {e}")))?;
        log.log(
            "info",
            "iec61850 server init",
            &format!("{} points in {}.", model.points.len(), model.domain()),
        );
        Ok(Server {
            endpoint: require(node, "endpoint")?,
            log,
            model,
            ied,
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let listener = match net::listen(&self.endpoint).await {
            Ok(l) => l,
            Err(e) => {
                self.log.log(
                    "error",
                    "iec61850 server init",
                    &format!("failed to listen: {e}"),
                );
                eprintln!("ERROR: iec61850-server: {e}");
                return;
            }
        };
        let log = self.log.clone();
        let link = self
            .model
            .goose
            .as_ref()
            .and_then(|g| goose_link(g, &self.log));
        let mms = Mms::new(self.device(&store, link), mms::Config::default());
        mms.with(|d| d.refresh(&store));
        tokio::spawn(refresh(mms.clone(), store, stop.clone()));
        mms.serve(listener, log, stop).await;
    }
}

/// A GOOSE frame sink and the source address the frames carry.
pub type Sink = Box<dyn FnMut(&[u8]) + Send>;
pub type Frames = (MacAddr, Sink);

impl Server {
    /// The MMS handler: the model, its control hook onto `store`, and where GOOSE frames go (`None`: nowhere).
    pub fn device(self, store: &Store, frames: Option<Frames>) -> Device {
        let domain = self.model.domain();
        let controls: BTreeMap<String, (String, Kind)> = self
            .model
            .points
            .iter()
            .filter(|p| p.role.controllable())
            .map(|p| (p.control_ref(&domain), (p.tag.clone(), p.role.kind())))
            .collect();
        let cfg = AcsiConfig {
            max_obtains: 0,
            ..AcsiConfig::default()
        };
        let mut acsi = Acsi::with_config(self.ied, cfg);
        acsi.on_control(control_hook(store.clone(), controls, self.log.clone()));
        let sink = frames.map(|(mac, sink)| {
            acsi.set_source_mac(mac);
            sink
        });
        let go_ena = self
            .model
            .goose
            .as_ref()
            .map(|g| format!("{domain}/LLN0$GO${}$GoEna", g.cb_name));
        if let Some(r) = go_ena {
            let _ = acsi.ied.write_leaf(&r, Value::Boolean(true));
        }
        Device {
            acsi,
            sink,
            points: self.model.points,
            domain,
            last: BTreeMap::new(),
            limits: Limits::DEFAULT,
        }
    }
}

/// The configured interface as a frame sink; `None` (logged) when it cannot be opened.
fn goose_link(g: &Goose, log: &EventLog) -> Option<Frames> {
    match Link::open(&g.interface, &LinkOptions::goose()) {
        Ok(link) => {
            log.log(
                "info",
                "iec61850 goose",
                &format!("publishing {} on {}", g.cb_name, g.interface),
            );
            let mac = MacAddr(link.mac().0);
            Some((
                mac,
                Box::new(move |f| {
                    let _ = link.send(f);
                }),
            ))
        }
        Err(e) => {
            log.log(
                "error",
                "iec61850 goose",
                &format!(
                    "cannot open {} (needs CAP_NET_RAW): {e}; GOOSE is off",
                    g.interface
                ),
            );
            eprintln!("ERROR: iec61850-server: GOOSE on {}: {e}", g.interface);
            None
        }
    }
}

/// The number a client's `ctlVal` carries for a point of `kind`: a boolean, or a float (bare or in an `AnalogueValue`).
pub fn ctl_datum(kind: Kind, v: &Value) -> Option<Datum> {
    match (kind, v) {
        (Kind::Binary, Value::Boolean(b)) => Some(Datum::Binary(*b)),
        (Kind::Analog, Value::Float32(f)) => Some(Datum::Analog(f64::from(*f))),
        (Kind::Analog, Value::Float64(f)) => Some(Datum::Analog(*f)),
        (Kind::Analog, Value::Integer(i)) => Some(Datum::Analog(*i as f64)),
        (Kind::Analog, Value::Structure(m)) => m.first().and_then(|v| ctl_datum(kind, v)),
        _ => None,
    }
}

/// `Oper` becomes a write on the datastore queue (it takes effect when the provider ACKs); select and cancel are allowed.
fn control_hook(
    store: Store,
    controls: BTreeMap<String, (String, Kind)>,
    log: Arc<EventLog>,
) -> iec61850_rs::server::ControlHook {
    Box::new(move |ev: &ControlEvent| {
        if ev.stage != Stage::Operate {
            return Ok(());
        }
        let (tag, kind) = controls.get(&ev.object).ok_or(AddCause::NotSupported)?;
        let datum = ctl_datum(*kind, &ev.request.ctl_val).ok_or(AddCause::Unknown)?;
        store.write(tag, datum).map_err(|_| AddCause::Unknown)?;
        log.log(
            "info",
            "iec61850 operate",
            &format!("{} = {datum:?}", show(&ev.object)),
        );
        Ok(())
    })
}

pub struct Device {
    acsi: Acsi,
    sink: Option<Sink>,
    points: Vec<Point>,
    domain: String,
    /// What was last pushed into the model per tag, so only changes are written.
    last: BTreeMap<String, Datum>,
    limits: Limits,
}

fn model_value(d: Datum) -> Value {
    match d {
        Datum::Binary(b) => Value::Boolean(b),
        Datum::Analog(x) => Value::Float32(x as f32),
    }
}

impl Device {
    /// Push every changed datastore value (and its timestamp) into the model; the commit makes reports and GOOSE.
    /// ponytail: an `Oper` already moved a controllable point's status in the model; it is corrected only when the
    /// datastore value next changes, so a rejected write leaves the status wrong until then.
    fn refresh(&mut self, store: &Store) -> Due {
        for p in &self.points {
            let Some(v) = store.get(&p.tag) else { continue };
            if self.last.get(&p.tag) == Some(&v) {
                continue;
            }
            let _ = self
                .acsi
                .ied
                .write_leaf(&p.value_ref(&self.domain), model_value(v));
            let _ = self
                .acsi
                .ied
                .write_leaf(&p.time_ref(&self.domain), Value::UtcTime(SystemClock.now()));
            self.last.insert(p.tag.clone(), v);
        }
        let due = self.acsi.commit(iec61850_rs::common::monotonic_now());
        self.drain();
        due
    }

    fn drain(&mut self) {
        if let Some(sink) = self.sink.as_mut() {
            self.acsi.drain_frames(|f| sink(f));
        }
    }
}

impl Handler for Device {
    fn opened(&mut self, id: AssocId, peer: Vec<u8>) {
        self.acsi.on_association_opened(id, peer);
    }
    fn established(&mut self, id: AssocId, n: &Negotiated) {
        self.acsi.set_association_max_pdu(id, n.max_pdu);
        self.acsi.set_authenticated(id, n.authenticated);
    }
    fn request(&mut self, id: AssocId, now: Instant, invoke_id: i64, pdu: &[u8]) -> (Vec<u8>, Due) {
        let answer = match Pdu::parse(pdu, &self.limits) {
            Ok(Pdu::ConfirmedRequest { service, .. }) => match self.acsi.request(id, now, &service)
            {
                Answer::Deferred => Answer::UNSUPPORTED,
                a => a,
            },
            _ => Answer::INVALID_PDU,
        };
        let bytes = answer.encode(invoke_id).unwrap_or_default();
        (bytes, self.acsi.commit(now))
    }
    fn tick(&mut self, now: Instant) -> Due {
        let due = self.acsi.on_timeout(now);
        self.drain();
        due
    }
    fn closed(&mut self, id: AssocId, now: Instant) {
        self.acsi.on_association_closed(id, now);
    }
}

async fn refresh(mms: Mms<Device>, store: Store, stop: CancellationToken) {
    let mut every = tokio::time::interval(REFRESH);
    loop {
        tokio::select! { _ = stop.cancelled() => return, _ = every.tick() => {} }
        mms.with(|d| d.refresh(&store));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::store::Layout;
    use iec61850_rs::proto::ethernet::Frame;
    use iec61850_rs::proto::goose::GoosePduView;
    use std::sync::mpsc;

    fn tags() -> Tags {
        let mut t = Tags::default();
        for (n, k) in [
            ("bi", Kind::Binary),
            ("an", Kind::Analog),
            ("bo", Kind::Binary),
            ("ao", Kind::Analog),
        ] {
            t.add_internal(n, k).unwrap();
        }
        t
    }

    fn parse(inner: &str, opts: &Opts) -> Result<Server, ConfigError> {
        let xml = format!("<iec61850-server><event-logging>/dev/null</event-logging><endpoint>tcp://127.0.0.1:1</endpoint>{inner}</iec61850-server>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        Server::parse(doc.root_element(), &mut tags(), opts)
    }

    fn err(inner: &str) -> String {
        parse(inner, &Opts::default()).err().expect("an error").0
    }

    const POINTS: &str = "<binary-input><tag>bi</tag></binary-input><analog-input><tag>an</tag></analog-input><binary-output><tag>bo</tag></binary-output><analog-output><tag>ao</tag><sbo>1</sbo></analog-output>";

    #[test]
    fn points_get_default_names_in_ggio1() {
        let s = parse(POINTS, &Opts::default()).unwrap();
        let names: Vec<(String, String, bool)> = s
            .model
            .points
            .iter()
            .map(|p| (p.ln(), p.name.clone(), p.sbo))
            .collect();
        assert_eq!(
            names,
            [
                ("GGIO1", "Ind1", false),
                ("GGIO1", "AnIn1", false),
                ("GGIO1", "SPCSO1", false),
                ("GGIO1", "APCSO1", true)
            ]
            .map(|(a, b, c)| (a.to_string(), b.to_string(), c))
        );
        assert_eq!(s.model.domain(), "simpleIOGenericIO");
        assert!(s.model.goose.is_none());
    }

    #[test]
    fn a_second_point_of_a_role_counts_up_and_names_and_nodes_can_be_chosen() {
        let s = parse(
            "<ied-name>IEDX</ied-name><ld-inst>LD1</ld-inst><binary-input><tag>bi</tag></binary-input><binary-input><tag>bo</tag></binary-input><analog-input><tag>an</tag><ln>MMXU3</ln><name>TotW</name></analog-input>",
            &Opts::default(),
        )
        .unwrap();
        let got: Vec<String> = s
            .model
            .points
            .iter()
            .map(|p| format!("{}.{}", p.ln(), p.name))
            .collect();
        assert_eq!(got, ["GGIO1.Ind1", "GGIO1.Ind2", "MMXU3.TotW"]);
        assert_eq!(s.model.domain(), "IEDXLD1");
    }

    #[test]
    fn sbo_belongs_to_outputs_and_is_a_flag() {
        let s = parse("<binary-input><tag>bi</tag><sbo>1</sbo></binary-input><binary-output><tag>bo</tag><sbo>true</sbo></binary-output><analog-output><tag>ao</tag><sbo>0</sbo></analog-output>", &Opts::default()).unwrap();
        let sbo: Vec<bool> = s.model.points.iter().map(|p| p.sbo).collect();
        assert_eq!(sbo, [false, true, false]);
    }

    #[test]
    fn bad_names_and_keys_are_refused_by_name() {
        assert!(err("<ied-name>a b</ied-name>").contains("<ied-name> `a b` is not a name"));
        assert!(err("<ld-inst>\"</ld-inst>").contains("<ld-inst>"));
        let one = |inner: &str| {
            err(&format!(
                "<binary-input><tag>bi</tag>{inner}</binary-input>"
            ))
        };
        assert!(one("<ln>ggio1</ln>").contains("`ggio1` is not a logical node"));
        assert!(one("<name>a.b</name>").contains("<name>"));
        assert!(err("<binary-input></binary-input>").contains("has no <tag>"));
        let twice = err("<binary-input><tag>bi</tag><name>X</name></binary-input><binary-input><tag>bo</tag><name>X</name></binary-input>");
        assert!(twice.contains("GGIO1.X"), "{twice}");
        let no_ep = roxmltree::Document::parse(
            "<iec61850-server><event-logging>/dev/null</event-logging></iec61850-server>",
        )
        .unwrap();
        let e = Server::parse(no_ep.root_element(), &mut tags(), &Opts::default())
            .err()
            .unwrap();
        assert!(e.0.contains("has no <endpoint>"), "{e}");
    }

    #[test]
    fn unknown_tags_are_skipped_wrong_kinds_refused_and_other_elements_ignored() {
        let skip = "<binary-input><tag>nope</tag></binary-input><junk/>";
        assert_eq!(parse(skip, &Opts::default()).unwrap().model.points.len(), 0);
        assert!(parse(skip, &Opts { strict: true }).is_err());
        assert!(err("<binary-input><tag>an</tag></binary-input>")
            .contains("is Analog but its mapping needs Binary"));
    }

    const GOOSE: &str = "<goose><interface>lo</interface></goose>";

    #[test]
    fn goose_defaults_match_libiec61850s_example() {
        let g = parse(&format!("{POINTS}{GOOSE}"), &Opts::default())
            .unwrap()
            .model
            .goose
            .unwrap();
        assert_eq!(
            (
                g.interface.as_str(),
                g.mac.as_str(),
                g.appid,
                g.vlan_id,
                g.vlan_priority,
                g.min_ms,
                g.max_ms,
                g.cb_name.as_str()
            ),
            (
                "lo",
                "01-0C-CD-01-00-01",
                1000,
                None,
                4,
                4,
                1000,
                "gcbAnalogValues"
            )
        );
    }

    #[test]
    fn goose_settings_are_read_and_checked() {
        let set = |inner: &str| format!("<goose><interface>eth0</interface>{inner}</goose>");
        let g = parse(&set("<mac>01-0c-cd-01-00-07</mac><appid>7</appid><vlan-id>100</vlan-id><vlan-priority>6</vlan-priority><min-time-ms>2</min-time-ms><max-time-ms>500</max-time-ms><control-block>gcbX</control-block>"), &Opts::default()).unwrap().model.goose.unwrap();
        assert_eq!(
            (
                g.mac.as_str(),
                g.appid,
                g.vlan_id,
                g.vlan_priority,
                g.min_ms,
                g.max_ms,
                g.cb_name.as_str()
            ),
            ("01-0c-cd-01-00-07", 7, Some(100), 6, 2, 500, "gcbX")
        );
        assert!(err("<goose></goose>").contains("<goose> has no <interface>"));
        assert!(err(&set("<mac>01:0C:CD:01:00:00</mac>")).contains("is not XX-XX-XX-XX-XX-XX"));
        assert!(err(&set("<mac>01-0C-CD-01-00-0G</mac>")).contains("is not XX"));
        assert!(err(&set("<mac>1-0C-CD-01-00-00-00</mac>")).contains("is not XX"));
        assert!(
            err("<goose><interface>a/b</interface></goose>").contains("is not an interface name")
        );
        assert!(
            err("<goose><interface>0123456789abcdef</interface></goose>")
                .contains("is not an interface name")
        );
        assert!(err(&set("<min-time-ms>0</min-time-ms>")).contains("min-time-ms <= max-time-ms"));
        assert!(err(&set(
            "<min-time-ms>9</min-time-ms><max-time-ms>8</max-time-ms>"
        ))
        .contains("min-time-ms"));
        let ok = parse(
            &set("<min-time-ms>8</min-time-ms><max-time-ms>8</max-time-ms>"),
            &Opts::default(),
        );
        assert!(ok.is_ok(), "equal times are allowed");
        let vlan = parse(
            &set("<vlan-id>4096</vlan-id><vlan-priority>8</vlan-priority>"),
            &Opts::default(),
        )
        .unwrap()
        .model
        .goose
        .unwrap();
        assert_eq!(
            (vlan.vlan_id, vlan.vlan_priority),
            (None, 4),
            "out of range values fall back"
        );
    }

    #[test]
    fn ctl_values_become_data() {
        use Datum::*;
        let f = |v| ctl_datum(Kind::Analog, &v);
        assert_eq!(
            ctl_datum(Kind::Binary, &Value::Boolean(true)),
            Some(Binary(true))
        );
        assert_eq!(
            ctl_datum(Kind::Binary, &Value::Boolean(false)),
            Some(Binary(false))
        );
        assert_eq!(ctl_datum(Kind::Binary, &Value::Float32(1.0)), None);
        assert_eq!(f(Value::Float32(55.5)), Some(Analog(55.5)));
        assert_eq!(f(Value::Float64(-2.25)), Some(Analog(-2.25)));
        assert_eq!(f(Value::Integer(7)), Some(Analog(7.0)));
        assert_eq!(
            f(Value::Structure(vec![Value::Float32(1.5)])),
            Some(Analog(1.5))
        );
        assert_eq!(f(Value::Structure(vec![])), None);
        assert_eq!(f(Value::Boolean(true)), None);
    }

    fn store() -> Store {
        let mut l = Layout::default();
        for (n, k) in [
            ("bi", Kind::Binary),
            ("an", Kind::Analog),
            ("bo", Kind::Binary),
            ("ao", Kind::Analog),
        ] {
            l.points.push((n.to_string(), k, None));
            l.external.push((n.to_string(), n.to_string()));
        }
        Store::new(l)
    }

    #[test]
    fn a_refresh_pushes_changes_into_the_model_once() {
        let st = store();
        let mut d = parse(POINTS, &Opts::default()).unwrap().device(&st, None);
        let d_ref =
            |d: &Device, p: usize| d.acsi.ied.value(&d.points[p].value_ref(&d.domain)).cloned();
        d.refresh(&st);
        assert_eq!(d_ref(&d, 1), Some(Value::Float32(0.0)));
        st.publish("an", 400.5, 1);
        st.publish("bi", 1.0, 1);
        d.refresh(&st);
        assert_eq!(d_ref(&d, 1), Some(Value::Float32(400.5)));
        assert_eq!(d_ref(&d, 0), Some(Value::Boolean(true)));
        // a value already pushed is not written again: a model value set by something else stays until the store moves
        d.acsi
            .ied
            .write_leaf(&d.points[1].value_ref(&d.domain), Value::Float32(1.0))
            .unwrap();
        d.refresh(&st);
        assert_eq!(d_ref(&d, 1), Some(Value::Float32(1.0)));
        st.publish("an", 2.0, 2);
        d.refresh(&st);
        assert_eq!(d_ref(&d, 1), Some(Value::Float32(2.0)));
    }

    #[test]
    fn the_control_hook_queues_operates_and_refuses_what_it_cannot_write() {
        use iec61850_rs::common::{TimeQuality, UtcTime};
        use iec61850_rs::proto::mms::control::{ControlModel, ControlRequest};
        let st = store();
        let m: BTreeMap<String, (String, Kind)> = [
            ("D/G$CO$A".to_string(), ("bo".to_string(), Kind::Binary)),
            ("D/G$CO$B".to_string(), ("ao".to_string(), Kind::Analog)),
        ]
        .into();
        let log = Arc::new(EventLog::open(Some("/dev/null"), "t"));
        let hook = control_hook(st.clone(), m, log);
        let ev = |object: &str, stage, v| ControlEvent {
            object: object.into(),
            stage,
            request: ControlRequest::new(v, 1, UtcTime::from_unix(0, 0, TimeQuality::SYNCHRONIZED)),
            model: ControlModel::DirectNormal,
        };
        assert_eq!(
            hook(&ev("D/G$CO$A", Stage::Select, Value::Boolean(true))),
            Ok(())
        );
        assert_eq!(
            hook(&ev("D/G$CO$A", Stage::Cancel, Value::Boolean(true))),
            Ok(())
        );
        assert!(!st.is_queued("bo"), "only Operate writes");
        assert_eq!(
            hook(&ev("D/G$CO$A", Stage::Operate, Value::Boolean(true))),
            Ok(())
        );
        assert!(st.is_queued("bo"));
        assert_eq!(
            hook(&ev(
                "D/G$CO$B",
                Stage::Operate,
                Value::Structure(vec![Value::Float32(55.5)])
            )),
            Ok(())
        );
        assert!(st.is_queued("ao"));
        assert_eq!(
            hook(&ev("D/G$CO$X", Stage::Operate, Value::Boolean(true))),
            Err(AddCause::NotSupported)
        );
        assert_eq!(
            hook(&ev("D/G$CO$A", Stage::Operate, Value::Float32(1.0))),
            Err(AddCause::Unknown)
        );
    }

    #[test]
    fn enabled_goose_publishes_the_data_set_and_a_change_is_a_new_state() {
        let st = store();
        let (tx, rx) = mpsc::channel();
        let frames: Frames = (
            MacAddr([2, 0, 0, 0, 0, 0x61]),
            Box::new(move |f| tx.send(f.to_vec()).unwrap()),
        );
        let s = parse(&format!("{POINTS}{GOOSE}"), &Opts::default()).unwrap();
        let mut d = s.device(&st, Some(frames));
        d.refresh(&st);
        d.tick(iec61850_rs::common::monotonic_now());
        let first = rx.try_recv().expect("the enabled block published at once");
        let goose = |f: &[u8]| {
            let fr = Frame::parse(f).unwrap();
            let v = GoosePduView::parse(fr.apdu).unwrap();
            (
                fr.appid,
                v.gocb_ref.to_string(),
                v.dat_set.to_string(),
                v.st_num,
                f[6..12].to_vec(),
            )
        };
        assert_eq!(
            goose(&first),
            (
                1000,
                "simpleIOGenericIO/LLN0$GO$gcbAnalogValues".into(),
                "simpleIOGenericIO/LLN0$Events".into(),
                1,
                vec![2, 0, 0, 0, 0, 0x61]
            )
        );
        assert_eq!(
            &first[0..6],
            &[0x01, 0x0C, 0xCD, 0x01, 0x00, 0x01],
            "to the configured group address"
        );
        while rx.try_recv().is_ok() {}
        st.publish("an", 9.0, 1);
        d.refresh(&st);
        d.tick(iec61850_rs::common::monotonic_now());
        let changed: Vec<u32> = rx.try_iter().map(|f| goose(&f).3).collect();
        assert!(changed.contains(&2), "stNum advanced: {changed:?}");
    }

    #[test]
    fn a_state_change_leaves_at_once_not_at_the_next_tick() {
        // stNum n sqNum 0 and the first retransmission (T1) must not leave the host in one burst
        let st = store();
        let (tx, rx) = mpsc::channel();
        let frames: Frames = (
            MacAddr([2, 0, 0, 0, 0, 0x61]),
            Box::new(move |f| tx.send(f.to_vec()).unwrap()),
        );
        let s = parse(&format!("{POINTS}{GOOSE}"), &Opts::default()).unwrap();
        let mut d = s.device(&st, Some(frames));
        d.refresh(&st);
        d.tick(iec61850_rs::common::monotonic_now());
        while rx.try_recv().is_ok() {}
        st.publish("an", 9.0, 1);
        d.refresh(&st);
        assert_eq!(rx.try_iter().count(), 1, "sent by the refresh itself");
    }

    #[test]
    fn an_interface_that_cannot_be_opened_turns_goose_off_and_says_why() {
        let g = |i: &str| Goose {
            interface: i.into(),
            ..parse(&format!("{POINTS}{GOOSE}"), &Opts::default())
                .unwrap()
                .model
                .goose
                .unwrap()
        };
        let path = std::env::temp_dir().join(format!("iec61850-goose-{}.log", std::process::id()));
        let log = EventLog::open(Some(&path.display().to_string()), "t");
        assert!(goose_link(&g("nope0"), &log).is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("cannot open nope0 (needs CAP_NET_RAW)"),
            "{text}"
        );
        // with CAP_NET_RAW (root in the test container) loopback opens and the sink takes frames
        if let Some((_, mut sink)) = goose_link(&g("lo"), &log) {
            sink(&[0u8; 60]);
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn without_a_frame_sink_goose_is_off_and_the_rest_works() {
        let st = store();
        let mut d = parse(&format!("{POINTS}{GOOSE}"), &Opts::default())
            .unwrap()
            .device(&st, None);
        d.refresh(&st);
        d.tick(iec61850_rs::common::monotonic_now());
        assert!(d.sink.is_none());
    }
}
