//! GOOSE subscriber (`<iec61850-rtu>`, ww-n29): what bennu's iec61850 module would do if it were built (the stock image never
//! builds it, ww-1w6, so there is no oracle; the source is `sceptre-bennu/src/bennu/devices/modules/comms/iec61850`).
//!
//! Two ways to say what to subscribe to, chosen by `<ied-name>`:
//!
//! - **bennu** (no `<ied-name>`): the `<configuration-file>` is a CID with SEL's vendor block
//!   `<Private type="SEL_GooseSubscription"><esel:GooseSubscription datSetRef=...>`, and a frame is taken when its `datSet` is
//!   one of those references, whatever its MAC or APPID (`subscriber.cpp:243-297`). The values are read as bennu reads them
//!   (`IEC61850RTU.cpp`, `process`): a true at position 0 is a TRIP, and while no TRIP is being handled it writes `false` to the
//!   tag of `<entry>` register 0, once; any false value in the data set (at any position) ends the TRIP. So a data set whose
//!   later members are false trips again on every frame, retransmissions included, as bennu does.
//! - **standard** (`<ied-name>`): the file is an SCD/CID and the subscriptions are that IED's `Inputs/ExtRef`s, resolved by
//!   `iec61850_rs::scl` to the publisher's address, APPID, `gocbRef` and `confRev`. Each `ExtRef` with an `intAddr` feeds the
//!   tag it names; on a new state every mapped value is written when it changed. A lost stream (time allowed to live
//!   exceeded) is logged and the tags keep their last values.
//!
//! The decisions are sans-IO (`Rx::frame`, `Rx::timeout` return `Act`s); `run` is a blocking receive loop on the raw link,
//! which needs `CAP_NET_RAW` like the publisher.

use super::super::super::config::{require, text, ConfigError, Opts, Tags};
use super::super::super::eventlog::EventLog;
use super::super::super::store::Store;
use super::super::Module;
use crate::hint::show;
use crate::model::{Kind, Value as Datum};
use iec61850_rs::common::{monotonic_now, Instant, Limits, MacAddr};
use iec61850_rs::model::{ExtRef, Fcda, IedModel};
use iec61850_rs::net::{Link, LinkOptions};
use iec61850_rs::proto::data::Value;
use iec61850_rs::proto::ethernet::{Frame, ETHERTYPE_GOOSE};
use iec61850_rs::proto::goose::{GoosePduView, Subscriber, SubscriberEvent};
use iec61850_rs::scl::{Scl, Subscription};
use roxmltree::Node;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// What a frame or a timer asks of the device.
#[derive(Debug, PartialEq)]
pub enum Act {
    Write(String, Datum),
    Log(&'static str, &'static str, String),
}

/// One standard subscription: the crate's subscriber, and which data-set position feeds which tag.
pub struct Stream {
    sub: Subscriber,
    name: String,
    dst: MacAddr,
    map: Vec<(usize, String, Kind)>,
}

pub enum Plan {
    /// bennu: the data-set references of the SEL blocks, and the tag of `<entry>` register 0.
    Sel {
        refs: Vec<String>,
        trip: Option<String>,
    },
    Std(Vec<Stream>),
}

/// The subscriber's state: the plan, bennu's TRIP latch, and the last value written per tag (standard mode writes changes only).
pub struct Rx {
    plan: Plan,
    tripping: bool,
    last: HashMap<String, Datum>,
}

impl Rx {
    pub fn new(plan: Plan) -> Rx {
        Rx {
            plan,
            tripping: false,
            last: HashMap::new(),
        }
    }

    /// The groups to join: standard subscriptions name their addresses, bennu takes the whole GOOSE range.
    pub fn groups(&self) -> Vec<MacAddr> {
        match &self.plan {
            Plan::Sel { .. } => vec![],
            Plan::Std(s) => s.iter().map(|s| s.dst).collect(),
        }
    }

    pub fn frame(&mut self, now: Instant, frame: &[u8]) -> Vec<Act> {
        match &mut self.plan {
            Plan::Sel { refs, trip } => sel(refs, trip.as_deref(), &mut self.tripping, frame),
            Plan::Std(streams) => {
                let mut acts = vec![];
                for s in streams.iter_mut() {
                    s.sub.on_frame(now, frame);
                    drain(s, &mut self.last, &mut acts);
                }
                acts
            }
        }
    }

    pub fn timeout(&mut self, now: Instant) -> Vec<Act> {
        let mut acts = vec![];
        if let Plan::Std(streams) = &mut self.plan {
            for s in streams.iter_mut() {
                s.sub.on_timeout(now);
                drain(s, &mut self.last, &mut acts);
            }
        }
        acts
    }
}

/// bennu's `process`: a frame whose `datSet` is subscribed, read position by position.
fn sel(refs: &[String], trip: Option<&str>, tripping: &mut bool, frame: &[u8]) -> Vec<Act> {
    let Some(values) = Frame::parse(frame)
        .ok()
        .filter(|f| f.ethertype == ETHERTYPE_GOOSE)
        .and_then(|f| GoosePduView::parse(f.apdu).ok())
        .filter(|p| refs.iter().any(|r| r == p.dat_set))
        .and_then(|p| p.all_data_owned(&Limits::DEFAULT).ok())
    else {
        return vec![];
    };
    if values.is_empty() {
        let m = "There is no data in the iec61850 data set!".to_string();
        return vec![Act::Log("error", "read data store", m)];
    }
    let mut acts = vec![];
    for (i, v) in values.iter().enumerate() {
        match (i, v) {
            (0, Value::Boolean(true)) if !*tripping => {
                *tripping = true;
                acts.extend(match trip {
                    Some(tag) => vec![
                        Act::Write(tag.to_string(), Datum::Binary(false)),
                        Act::Log(
                            "info",
                            "read data store",
                            "Value at register address 0 was received".into(),
                        ),
                    ],
                    None => vec![Act::Log(
                        "error",
                        "read data store",
                        "no <entry> for register address 0".into(),
                    )],
                });
            }
            (_, Value::Boolean(false)) => *tripping = false,
            _ => {}
        }
    }
    acts
}

/// The subscriber's events for one stream as writes (changed values) and log lines.
fn drain(s: &mut Stream, last: &mut HashMap<String, Datum>, acts: &mut Vec<Act>) {
    while let Some(e) = s.sub.poll_event() {
        match e {
            SubscriberEvent::NewState { values, .. } => new_state(s, &values, last, acts),
            other => acts.extend(lost(&s.name, &other)),
        }
    }
}

/// A new state: each mapped member whose value changed is written to its tag.
fn new_state(s: &Stream, values: &[Value], last: &mut HashMap<String, Datum>, acts: &mut Vec<Act>) {
    for (pos, tag, kind) in &s.map {
        let Some(d) = values.get(*pos).and_then(|v| datum(*kind, v)) else {
            let m = format!("{}: member {pos} is no {kind:?} value for `{tag}`", s.name);
            acts.push(Act::Log("error", "goose subscription", m));
            continue;
        };
        if last.insert(tag.clone(), d) != Some(d) {
            acts.push(Act::Write(tag.clone(), d));
        }
    }
}

/// The log line for an event that makes a stream's values unusable; `None` for the rest (retransmissions, invalid frames).
fn lost(name: &str, e: &SubscriberEvent) -> Option<Act> {
    let what = match e {
        SubscriberEvent::Expired => {
            "no frame within its time allowed to live; values kept".to_string()
        }
        SubscriberEvent::ConfRevMismatch { received, expected } => {
            format!("confRev {received}, the file says {expected}; frames dropped")
        }
        SubscriberEvent::NeedsCommissioning => {
            "the publisher sets ndsCom; values not used".to_string()
        }
        _ => return None,
    };
    Some(Act::Log(
        "error",
        "goose subscription",
        format!("{name}: {what}"),
    ))
}

/// A data-set member as a tag value: the first leaf of a structure (`stVal` of a data object), else `binary` or `analog`.
fn datum(kind: Kind, v: &Value) -> Option<Datum> {
    if let Value::Structure(m) = v {
        return m.first().and_then(|v| datum(kind, v));
    }
    match kind {
        Kind::Binary => binary(v).map(Datum::Binary),
        Kind::Analog => analog(v).map(Datum::Analog),
    }
}

/// A boolean, or a double point: `10` on, `01` off (intermediate `00` and bad `11` are no value).
fn binary(v: &Value) -> Option<bool> {
    match v {
        Value::Boolean(b) => Some(*b),
        Value::BitString { bytes, .. } => match bytes.first().map(|b| b >> 6) {
            Some(0b10) => Some(true),
            Some(0b01) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn analog(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(n) => Some(*n as f64),
        Value::Unsigned(n) => Some(*n as f64),
        Value::Float32(x) => Some(f64::from(*x)),
        Value::Float64(x) => Some(*x),
        _ => None,
    }
}

// ---- configuration ---------------------------------------------------------------------------------------------------------

/// The data-set references of a CID's SEL subscription blocks (bennu reads `datSetRef` of each `esel:GooseSubscription`).
pub fn sel_refs(cid: &str) -> Result<Vec<String>, String> {
    let doc = roxmltree::Document::parse(cid).map_err(|e| e.to_string())?;
    Ok(doc
        .descendants()
        .filter(|n| {
            n.has_tag_name("Private") && n.attribute("type") == Some("SEL_GooseSubscription")
        })
        .flat_map(|p| {
            p.children()
                .filter(|c| c.tag_name().name() == "GooseSubscription")
        })
        .filter_map(|g| g.attribute("datSetRef").map(String::from))
        .collect())
}

/// The standard subscriptions of `ied` with the tags their `ExtRef`s' `intAddr` name, each at its position in the publisher's
/// data set. An `ExtRef` without `intAddr`, or naming no tag of this device, is reported in the second list and left out.
pub fn std_streams(
    scd: &str,
    ied: &str,
    tags: &Tags,
) -> Result<(Vec<Stream>, Vec<String>), String> {
    let scl = Scl::parse(scd).map_err(|e| e.to_string())?;
    let subs = scl.subscriptions(ied, 50).map_err(|e| e.to_string())?;
    let mut skipped: Vec<String> = subs.unresolved.iter().map(|d| d.message.clone()).collect();
    let mut streams = vec![];
    for g in &subs.goose {
        let publisher = scl.model(Some(&g.publisher)).map_err(|e| e.to_string())?;
        let members =
            members(&publisher, g).ok_or_else(|| format!("{}: no data set", g.control_block))?;
        let map = wire(g, members, tags, &mut skipped);
        streams.push(Stream {
            sub: Subscriber::new(g.goose_config()),
            name: g.identifier.clone(),
            dst: g.dst,
            map,
        });
    }
    Ok((streams, skipped))
}

/// The `ExtRef`s of one subscription as (data-set position, tag, kind); the ones that feed nothing go to `skipped`.
fn wire(
    g: &Subscription,
    members: &[Fcda],
    tags: &Tags,
    skipped: &mut Vec<String>,
) -> Vec<(usize, String, Kind)> {
    let mut map = vec![];
    for x in &g.ext_refs {
        let Some(tag) = x.int_addr.as_deref() else {
            skipped.push(format!("{}: an ExtRef has no intAddr", g.control_block));
            continue;
        };
        match (tags.kind(tag), members.iter().position(|m| covers(m, x))) {
            (Some(kind), Some(pos)) => map.push((pos, tag.to_string(), kind)),
            (None, _) => skipped.push(format!("intAddr `{}` is no tag of this device", show(tag))),
            (_, None) => skipped.push(format!(
                "intAddr `{}`: not in the data set of {}",
                show(tag),
                g.control_block
            )),
        }
    }
    map
}

/// The members of the data set the control block `LD/LN.cb` publishes.
fn members<'m>(model: &'m IedModel, g: &Subscription) -> Option<&'m [Fcda]> {
    let (path, cb) = g.control_block.rsplit_once('.')?;
    let (ld, ln) = path.split_once('/')?;
    let ln = model
        .logical_devices
        .iter()
        .find(|d| d.name == ld)?
        .logical_nodes
        .iter()
        .find(|n| n.name == ln)?;
    let ds = ln
        .gse_controls
        .iter()
        .find(|c| c.name == cb)?
        .dat_set
        .as_deref()?;
    Some(&ln.data_sets.iter().find(|d| d.name == ds)?.members)
}

/// Whether data-set member `m` carries what `x` asks for (as `iec61850_rs::scl` resolves it): the same logical node and data
/// object, and the same attribute when both name one.
fn covers(m: &Fcda, x: &ExtRef) -> bool {
    x.ld_inst.as_deref().is_none_or(|i| i == m.ld_inst)
        && x.prefix == m.prefix
        && x.ln_class.as_deref() == Some(m.ln_class.as_str())
        && x.ln_inst == m.ln_inst
        && x.do_name.as_deref() == Some(m.do_name.as_str())
        && (m.da_name.is_none() || x.da_name.is_none() || m.da_name == x.da_name)
}

pub struct Rtu {
    interface: String,
    log: Arc<EventLog>,
    plan: Plan,
}

/// bennu's `<entry>`s: register address to tag, binary (the TRIP writes a boolean). Unknown tags are skipped, as everywhere.
fn entries(node: Node, tags: &Tags, opts: &Opts) -> Result<Option<String>, ConfigError> {
    let mut trip = None;
    for e in node.children().filter(|c| c.has_tag_name("entry")) {
        let addr = require(e, "register-address")?;
        let tag = require(e, "tag")?;
        if tags.mapped(&tag, Kind::Binary, opts)? && addr.trim() == "0" {
            trip = Some(tag);
        }
    }
    Ok(trip)
}

/// What to subscribe to: the SEL blocks of the file (bennu), or with `<ied-name>` that IED's `ExtRef`s, whose leftovers are logged.
fn plan(node: Node, tags: &Tags, opts: &Opts, log: &EventLog) -> Result<Plan, ConfigError> {
    let file = require(node, "configuration-file")?;
    let bad = |e: String| {
        ConfigError(format!(
            "<iec61850-rtu> configuration-file {}: {e}",
            show(&file)
        ))
    };
    let xml = std::fs::read_to_string(&file).map_err(|e| bad(e.to_string()))?;
    let Some(ied) = text(node, "ied-name") else {
        return Ok(Plan::Sel {
            refs: sel_refs(&xml).map_err(bad)?,
            trip: entries(node, tags, opts)?,
        });
    };
    let (streams, skipped) = std_streams(&xml, &ied, tags).map_err(bad)?;
    for s in skipped {
        log.log("error", "goose subscription", &s);
        tracing::warn!("iec61850-rtu: {s}");
    }
    Ok(Plan::Std(streams))
}

impl Module for Rtu {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        require(node, "name")?;
        let interface = require(node, "interface")?;
        let log = Arc::new(EventLog::open(
            text(node, "event-logging").as_deref(),
            "iec61850-rtu",
        ));
        let plan = plan(node, tags, opts, &log)?;
        Ok(Rtu {
            interface,
            log,
            plan,
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let _ = tokio::task::spawn_blocking(move || self.listen(&store, &stop)).await;
    }
}

impl Rtu {
    /// Receive until `stop`, on the configured interface. Without `CAP_NET_RAW` the subscriber logs why and the device runs on.
    fn listen(self, store: &Store, stop: &CancellationToken) {
        let mut rx = Rx::new(self.plan);
        let Some(mut link) = open(&self.interface, &rx.groups(), &self.log) else {
            return;
        };
        while !stop.is_cancelled() {
            let acts = receive(&mut link, &mut rx, &self.interface);
            apply(
                acts.into_iter().chain(rx.timeout(monotonic_now())),
                store,
                &self.log,
            );
        }
    }
}

/// The interface in promiscuous mode, joined to `groups`; `None` (logged) when it cannot be opened.
fn open(interface: &str, groups: &[MacAddr], log: &EventLog) -> Option<Link> {
    let opts = groups
        .iter()
        .fold(LinkOptions::goose().promiscuous(true), |o, g| o.join(*g));
    match Link::open(interface, &opts) {
        Ok(l) => {
            log.log(
                "info",
                "iec61850 goose",
                format!("subscribing on {interface}"),
            );
            Some(l)
        }
        Err(e) => {
            let m = format!(
                "cannot open {interface} (needs CAP_NET_RAW): {e}; the GOOSE subscriber is off"
            );
            log.log("error", "iec61850 goose", &m);
            tracing::error!("iec61850-rtu: {m}");
            None
        }
    }
}

/// The next frame within 50 ms, as acts.
fn receive(link: &mut Link, rx: &mut Rx, interface: &str) -> Vec<Act> {
    match link.recv(Some(Duration::from_millis(50))) {
        Ok(Some(r)) => rx.frame(r.at, r.frame),
        Ok(None) => vec![],
        Err(e) => {
            tracing::warn!("iec61850-rtu: receive on {interface}: {e}");
            std::thread::sleep(Duration::from_millis(100));
            vec![]
        }
    }
}

/// Writes go to the datastore's queue (a refused one is logged), log lines to the event log.
fn apply(acts: impl IntoIterator<Item = Act>, store: &Store, log: &EventLog) {
    for a in acts {
        match a {
            Act::Write(tag, v) => {
                if let Err(e) = store.write(&tag, v) {
                    log.log("error", "write data store", format!("`{tag}`: {e:?}"));
                }
            }
            Act::Log(level, event, m) => log.log(level, event, m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::super::mms::Handler;
    use super::super::{Frames, Server};
    use super::*;
    use crate::fd::store::Layout;
    use std::sync::mpsc;

    const KINDS: [(&str, Kind); 6] = [
        ("bi", Kind::Binary),
        ("bi2", Kind::Binary),
        ("an", Kind::Analog),
        ("trip", Kind::Binary),
        ("mirror", Kind::Binary),
        ("level", Kind::Analog),
    ];

    fn tags() -> Tags {
        let mut t = Tags::default();
        for (n, k) in KINDS {
            t.add_internal(n, k).unwrap();
        }
        t
    }

    fn store() -> Store {
        let mut l = Layout::default();
        for (n, k) in KINDS {
            l.points.push((n.to_string(), k, None));
            l.external.push((n.to_string(), n.to_string()));
        }
        Store::new(l)
    }

    /// Our own `<iec61850-server>` publishing GOOSE into a channel: its frames are what the subscriber is fed.
    struct Publisher {
        st: Store,
        dev: super::super::Device,
        rx: mpsc::Receiver<Vec<u8>>,
    }

    fn server(points: &str) -> Server {
        let xml = format!("<iec61850-server><event-logging>/dev/null</event-logging><endpoint>tcp://127.0.0.1:1</endpoint>{points}<goose><interface>lo</interface></goose></iec61850-server>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        Server::parse(doc.root_element(), &mut tags(), &Opts::default()).unwrap()
    }

    impl Publisher {
        fn new(points: &str) -> Publisher {
            let (tx, rx) = mpsc::channel();
            let frames: Frames = (
                MacAddr([2, 0, 0, 0, 0, 1]),
                Box::new(move |f| tx.send(f.to_vec()).unwrap()),
            );
            let st = store();
            let dev = server(points).device(&st, Some(frames));
            Publisher { st, dev, rx }
        }
        /// Publish `point = x` and return the frames that left (the new state first).
        fn set(&mut self, point: &str, x: f64) -> Vec<Vec<u8>> {
            self.st.publish(point, x, 1);
            self.dev.refresh(&self.st);
            self.dev.tick(monotonic_now());
            self.rx.try_iter().collect()
        }
    }

    const BI: &str = "<binary-input><tag>bi</tag></binary-input>";
    const BI2: &str = "<binary-input><tag>bi2</tag></binary-input>";
    const AN: &str = "<analog-input><tag>an</tag></analog-input>";
    const EVENTS: &str = "simpleIOGenericIO/LLN0$Events";

    fn sel_rx(trip: Option<&str>) -> Rx {
        Rx::new(Plan::Sel {
            refs: vec![EVENTS.into()],
            trip: trip.map(String::from),
        })
    }

    fn writes(acts: &[Act]) -> Vec<(String, Datum)> {
        acts.iter()
            .filter_map(|a| match a {
                Act::Write(t, v) => Some((t.clone(), *v)),
                Act::Log(..) => None,
            })
            .collect()
    }

    fn feed(rx: &mut Rx, frames: &[Vec<u8>]) -> Vec<Act> {
        frames
            .iter()
            .flat_map(|f| rx.frame(monotonic_now(), f))
            .collect()
    }

    #[test]
    fn the_sel_blocks_of_a_cid_give_the_subscribed_data_sets() {
        let cid = r#"<SCL xmlns="http://www.iec.ch/61850/2003/SCL" xmlns:esel="http://www.selinc.com/2006/61850"><IED name="SUB">
          <Private type="SEL_GooseSubscription"><esel:GooseSubscription iedName="PUB" ldInst="CFG" cbName="gcb1" datSet="ds1"
              datSetRef="PUBCFG/LLN0$ds1" confRev="1" mAddr="01-0C-CD-01-00-02" APPID="2"><esel:GooseRxEntry tdlString="VB001"/>
              <esel:GooseRxEntry tdlString="RA001"/></esel:GooseSubscription></Private>
          <Private type="SEL_Other"><esel:GooseSubscription datSetRef="ignored"/></Private></IED></SCL>"#;
        assert_eq!(sel_refs(cid).unwrap(), ["PUBCFG/LLN0$ds1"]);
        assert!(sel_refs("<SCL").is_err());
    }

    #[test]
    fn a_trip_at_position_0_writes_false_once_per_rising_edge() {
        let mut p = Publisher::new(BI);
        let mut rx = sel_rx(Some("trip"));
        let first = p.set("bi", 0.0);
        assert!(!first.is_empty());
        assert!(
            writes(&feed(&mut rx, &first)).is_empty(),
            "false is no trip"
        );
        let edge = p.set("bi", 1.0);
        let acts = feed(&mut rx, &edge);
        assert_eq!(
            writes(&acts),
            [("trip".to_string(), Datum::Binary(false))],
            "{acts:?}"
        );
        assert!(acts.contains(&Act::Log(
            "info",
            "read data store",
            "Value at register address 0 was received".into()
        )));
        // the retransmissions of the same state, and the same frame again, do nothing while the trip is handled
        assert!(writes(&feed(&mut rx, &edge)).is_empty());
        assert!(writes(&feed(&mut rx, &p.set("bi", 1.0))).is_empty());
        // it falls, and the next rise trips again
        assert!(writes(&feed(&mut rx, &p.set("bi", 0.0))).is_empty());
        assert_eq!(writes(&feed(&mut rx, &p.set("bi", 1.0))).len(), 1);
    }

    #[test]
    fn a_false_anywhere_in_the_data_set_ends_the_trip_so_it_trips_on_every_frame() {
        // bennu's loop: `if (value && i == 0) ... else if (value == false) mHandlingRisingEdge = false`, over every position
        let mut p = Publisher::new(&format!("{BI}{BI2}"));
        let mut rx = sel_rx(Some("trip"));
        let frames = p.set("bi", 1.0);
        assert_eq!(writes(&feed(&mut rx, &frames)).len(), 1);
        // a retransmission carries the same values: bi2 is false, so it is a new trip
        assert_eq!(
            writes(&feed(&mut rx, &frames)).len(),
            1,
            "bi2 is false, so every frame is a new trip"
        );
    }

    #[test]
    fn other_data_sets_and_a_missing_entry_are_handled_as_bennu_does() {
        let mut p = Publisher::new(BI);
        let edge = p.set("bi", 1.0);
        let mut other = Rx::new(Plan::Sel {
            refs: vec!["PUB/LLN0$other".into()],
            trip: Some("trip".into()),
        });
        assert!(
            feed(&mut other, &edge).is_empty(),
            "a data set nobody subscribed to"
        );
        let acts = sel_rx(None).frame(monotonic_now(), &edge[0]);
        assert_eq!(
            acts,
            [Act::Log(
                "error",
                "read data store",
                "no <entry> for register address 0".into()
            )]
        );
        assert!(
            sel_rx(Some("trip"))
                .frame(monotonic_now(), &[0u8; 60])
                .is_empty(),
            "not a GOOSE frame"
        );
    }

    /// Our publisher's SCL plus a subscribing IED `RTU1` wired to it by `ExtRef`s.
    fn scd(inputs: &str) -> String {
        let publisher = server(&format!("{BI}{AN}")).model.scl();
        let rtu = format!(
            "<IED name=\"RTU1\"><AccessPoint name=\"P1\"><Server><LDevice inst=\"LD0\"><LN0 lnClass=\"LLN0\" inst=\"\" lnType=\"LLN0_T\"><Inputs>{inputs}</Inputs></LN0></LDevice></Server></AccessPoint></IED><DataTypeTemplates>"
        );
        publisher.replacen("<DataTypeTemplates>", &rtu, 1)
    }

    fn ext_ref(do_name: &str, da: &str, int_addr: &str) -> String {
        format!("<ExtRef iedName=\"simpleIO\" ldInst=\"GenericIO\" lnClass=\"GGIO\" lnInst=\"1\" doName=\"{do_name}\"{da} serviceType=\"GOOSE\" srcLDInst=\"GenericIO\" srcCBName=\"gcbAnalogValues\"{int_addr}/>")
    }

    #[test]
    fn standard_ext_refs_feed_the_tags_their_int_addr_names_on_change() {
        let file = scd(&[
            ext_ref("Ind1", " daName=\"stVal\"", " intAddr=\"mirror\""),
            ext_ref("AnIn1", "", " intAddr=\"level\""),
            ext_ref("Ind1", " daName=\"q\"", ""),
            ext_ref("Ind1", " daName=\"stVal\"", " intAddr=\"nosuch\""),
        ]
        .concat());
        let (streams, skipped) = std_streams(&file, "RTU1", &tags()).unwrap();
        assert_eq!(streams.len(), 1, "one control block");
        assert_eq!(
            streams[0].map,
            [
                (0, "mirror".to_string(), Kind::Binary),
                (2, "level".to_string(), Kind::Analog)
            ]
        );
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        let mut rx = Rx::new(Plan::Std(streams));
        assert_eq!(rx.groups(), [MacAddr([0x01, 0x0C, 0xCD, 0x01, 0x00, 0x01])]);

        let mut p = Publisher::new(&format!("{BI}{AN}"));
        let w = writes(&feed(&mut rx, &p.set("an", 7.5)));
        assert_eq!(
            w,
            [
                ("mirror".to_string(), Datum::Binary(false)),
                ("level".to_string(), Datum::Analog(7.5))
            ]
        );
        // retransmissions are not new states; a change writes only what changed
        assert!(writes(&feed(&mut rx, &p.set("an", 7.5))).is_empty());
        assert_eq!(
            writes(&feed(&mut rx, &p.set("bi", 1.0))),
            [("mirror".to_string(), Datum::Binary(true))]
        );
    }

    #[test]
    fn a_lost_stream_is_logged_and_the_values_are_kept() {
        let file = scd(&ext_ref("Ind1", " daName=\"stVal\"", " intAddr=\"mirror\""));
        let (streams, _) = std_streams(&file, "RTU1", &tags()).unwrap();
        let mut rx = Rx::new(Plan::Std(streams));
        let mut p = Publisher::new(&format!("{BI}{AN}"));
        feed(&mut rx, &p.set("bi", 1.0));
        let later = monotonic_now().plus_nanos(60_000_000_000);
        let acts = rx.timeout(later);
        assert!(
            matches!(&acts[..], [Act::Log("error", "goose subscription", m)] if m.contains("time allowed to live")),
            "{acts:?}"
        );
        assert!(writes(&acts).is_empty());
    }

    #[test]
    fn a_double_point_and_a_data_object_read_as_their_first_value() {
        let dbpos = |b: u8| Value::BitString {
            unused: 6,
            bytes: vec![b],
        };
        assert_eq!(
            datum(Kind::Binary, &dbpos(0b1000_0000)),
            Some(Datum::Binary(true))
        );
        assert_eq!(
            datum(Kind::Binary, &dbpos(0b0100_0000)),
            Some(Datum::Binary(false))
        );
        assert_eq!(datum(Kind::Binary, &dbpos(0b1100_0000)), None, "bad state");
        assert_eq!(datum(Kind::Binary, &dbpos(0)), None, "intermediate");
        let st = Value::Structure(vec![Value::Boolean(true), dbpos(0)]);
        assert_eq!(datum(Kind::Binary, &st), Some(Datum::Binary(true)));
        assert_eq!(
            datum(Kind::Analog, &Value::Integer(-3)),
            Some(Datum::Analog(-3.0))
        );
        assert_eq!(datum(Kind::Analog, &Value::Boolean(true)), None);
    }

    #[test]
    fn events_that_spoil_a_stream_are_logged_and_the_rest_are_not() {
        let line = |e: SubscriberEvent| match lost("gcb", &e) {
            Some(Act::Log("error", "goose subscription", m)) => m,
            other => panic!("{other:?}"),
        };
        assert!(line(SubscriberEvent::ConfRevMismatch {
            received: 2,
            expected: 1
        })
        .contains("confRev 2, the file says 1"));
        assert!(line(SubscriberEvent::NeedsCommissioning).contains("ndsCom"));
        assert!(line(SubscriberEvent::Expired).starts_with("gcb: no frame"));
        assert_eq!(lost("gcb", &SubscriberEvent::IgnoredSimulation), None);
    }

    #[test]
    fn a_member_of_the_wrong_type_is_logged_and_not_written() {
        let file = scd(&ext_ref("Ind1", " daName=\"stVal\"", " intAddr=\"level\""));
        let (streams, _) = std_streams(&file, "RTU1", &tags()).unwrap();
        let (mut last, mut acts) = (HashMap::new(), vec![]);
        new_state(&streams[0], &[Value::Boolean(true)], &mut last, &mut acts);
        assert!(
            matches!(&acts[..], [Act::Log("error", _, m)] if m.contains("is no Analog value for `level`")),
            "{acts:?}"
        );
        assert_eq!(analog(&Value::Unsigned(4)), Some(4.0));
        assert_eq!(analog(&Value::Float64(0.5)), Some(0.5));
        assert_eq!(analog(&Value::Float32(1.5)), Some(1.5));
    }

    #[derive(Clone, Default)]
    struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writes_reach_the_queue_a_refused_one_and_a_closed_interface_are_logged() {
        let (buf, st) = (Buf::default(), store());
        let log = EventLog::to("iec61850-rtu", Box::new(buf.clone()));
        apply(
            [
                Act::Write("trip".into(), Datum::Binary(false)),
                Act::Write("nosuch".into(), Datum::Binary(true)),
                Act::Log("info", "read data store", "seen".into()),
            ],
            &st,
            &log,
        );
        assert_eq!(st.queued("trip"), Some(Datum::Binary(false)));
        assert!(open("nosuch0", &[], &log).is_none());
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(text.contains("`nosuch`: UnknownTag"), "{text}");
        assert!(text.contains("seen"), "{text}");
        assert!(text.contains("cannot open nosuch0"), "{text}");
    }
}
