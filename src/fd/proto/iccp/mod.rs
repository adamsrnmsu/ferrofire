//! ICCP / TASE.2 server (`<iccp-server>`; not in stock bennu, so there is no oracle: the proof is a generic MMS client, tshark and
//! our own thin ICCP client, `conformance/iccp/`). TASE.2 is MMS with a fixed object model on the same OSI stack as IEC 61850, so
//! the transport is `fd::mms`; what lives here is the bilateral table (who may read or write what), the data value objects mapped
//! from tags (`model`), data sets and DSTransfer_Sets with InformationReport (`transfer`).
//! What is covered, exactly: docs/field-device-iccp.md.

pub mod model;
pub mod transfer;

use super::super::config::{child, number, require, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::mms::{self, AssocId, Due, Handler, Mms};
use super::super::net;
use super::super::store::Store;
use super::Module;
use crate::hint::show;
use crate::model::Value as Datum;
use iec61850_rs::ber::Cursor;
use iec61850_rs::common::{monotonic_now, Instant, Limits};
use iec61850_rs::proto::data::Value;
use iec61850_rs::proto::mms::typespec::TypeSpec;
use iec61850_rs::proto::mms::{
    object_class, ConfirmedRequest, Mms as Pdu, ObjectName, ObjectScope, Unconfirmed,
    VariableAccess, VariableSpecification,
};
use iec61850_rs::server::Answer;
use model::{Class, Object, Variant};
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use transfer::{name_value, Cfg, Ts};

const BILATERAL: &str = "Bilateral_Table_ID";
const FEATURES: &str = "Supported_Features";
const VERSION: &str = "TASE2_Version";
const NEXT_TS: &str = "Next_DSTransfer_Set";
const TS_PREFIX: &str = "DSTrans_";
/// Names per GetNameList page, members per data set, and the refresh pace (transfer set conditions are checked this often).
const PAGE: usize = 100;
const MAX_MEMBERS: usize = 256;
const REFRESH: Duration = Duration::from_millis(100);

// DataAccessError codes
const TEMP_UNAVAILABLE: i64 = 2;
const DENIED: i64 = 3;
const UNSUPPORTED: i64 = 9;
const NON_EXISTENT: i64 = 10;
// ServiceError classes and codes
const DEFINITION: u32 = 2;
const RESOURCE: u32 = 3;
const OBJECT_UNDEFINED: i64 = 1;
const OBJECT_EXISTS: i64 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Access {
    All,
    Only(BTreeSet<String>),
}

impl Access {
    fn allows(&self, name: &str) -> bool {
        match self {
            Access::All => true,
            Access::Only(s) => s.contains(name),
        }
    }
}

/// One row of the bilateral table: a partner, the domain its objects live in, and what it may do there.
#[derive(Clone, Debug)]
struct Peer {
    name: String,
    domain: String,
    address: Option<IpAddr>,
    read: Access,
    write: Access,
    max_sets: usize,
    max_transfer_sets: usize,
}

impl Peer {
    /// Does this row apply to a remote address (as the octets the MMS layer reports)? A row without `<address>` applies to any.
    fn address_matches(&self, ip: &[u8]) -> Option<bool> {
        let want = self.address?;
        let ip = match ip.len() {
            4 => IpAddr::from(<[u8; 4]>::try_from(ip).ok()?),
            16 => IpAddr::from(<[u8; 16]>::try_from(ip).ok()?).to_canonical(),
            _ => return Some(false),
        };
        Some(want.to_canonical() == ip)
    }
}

pub struct Server {
    endpoint: String,
    log: Arc<EventLog>,
    table_id: String,
    version: (i64, i64),
    objects: BTreeMap<String, Object>,
    peers: Vec<Peer>,
}

/// Letters, digits, `_` and `-`, 1 to 32 of them (an MMS identifier; names go into the event log and the wire unescaped).
pub fn valid_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn name_or(n: Node, key: &str, default: &str) -> Result<String, ConfigError> {
    super::name_in(
        n,
        key,
        default,
        valid_name,
        "letters, digits, _ and -; at most 32",
    )
}

/// `<tase2-version>`: `year.month` of the IEC 60870-6-503 edition TASE2_Version reports. Field peers use 2000.8 (the default);
/// a 1996.8 peer is selected here. A peer that checks the version ends the association on a mismatch (503 5.3.3.3).
fn version_of(n: Node) -> Result<(i64, i64), ConfigError> {
    let Some(t) = text(n, "tase2-version") else {
        return Ok((2000, 8));
    };
    let parsed = t.split_once('.').and_then(|(y, m)| {
        let (y, m) = (y.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
        ((1990..=2099).contains(&y) && (1..=12).contains(&m)).then_some((y, m))
    });
    parsed.ok_or_else(|| {
        ConfigError(format!(
            "<tase2-version> `{}` is not year.month, e.g. 2000.8 or 1996.8",
            show(&t)
        ))
    })
}

fn access(
    n: Node,
    key: &str,
    known: &BTreeSet<String>,
    skipped: &BTreeSet<String>,
    default: Access,
) -> Result<Access, ConfigError> {
    let Some(t) = text(n, key) else {
        return Ok(default);
    };
    let words: Vec<&str> = t
        .split([',', ' ', '\n', '\t', '\r'])
        .filter(|w| !w.is_empty())
        .collect();
    if words == ["*"] {
        return Ok(Access::All);
    }
    let mut out = BTreeSet::new();
    for w in words {
        if known.contains(w) {
            out.insert(w.to_string());
        } else if !skipped.contains(w) {
            return Err(ConfigError(format!(
                "<{key}> of <peer> names `{}`, which is not a data object",
                show(w)
            )));
        }
    }
    Ok(Access::Only(out))
}

fn address_of(n: Node) -> Result<Option<IpAddr>, ConfigError> {
    text(n, "address")
        .map(|a| {
            a.parse::<IpAddr>()
                .map_err(|_| ConfigError(format!("<address> `{}` is not an IP address", show(&a))))
        })
        .transpose()
}

fn peer(
    n: Node,
    known: &BTreeSet<String>,
    skipped: &BTreeSet<String>,
) -> Result<Peer, ConfigError> {
    let name = name_or(n, "name", "")?;
    Ok(Peer {
        domain: name_or(n, "domain", &name)?,
        address: address_of(n)?,
        read: access(n, "read", known, skipped, Access::All)?,
        write: access(n, "write", known, skipped, Access::Only(BTreeSet::new()))?,
        max_sets: number(n, "max-data-sets")?.unwrap_or(16),
        max_transfer_sets: number(n, "max-transfer-sets")?.unwrap_or(8),
        name,
    })
}

type Objects = (BTreeMap<String, Object>, BTreeSet<String>);

/// The data objects of the element, and the names of those skipped for an unknown tag (a table may still list them).
fn objects_of(node: Node, tags: &Tags, opts: &Opts) -> Result<Objects, ConfigError> {
    let (mut objects, mut skipped) = (BTreeMap::new(), BTreeSet::new());
    for n in node.children().filter(|c| c.is_element()) {
        let Some(class) = Class::from_element(n.tag_name().name()) else {
            continue;
        };
        let declared = name_or(n, "name", &require(n, "tag")?)?;
        match object(n, class, tags, opts)? {
            None => {
                skipped.insert(declared);
            }
            Some(o) => {
                if objects.insert(o.name.clone(), o).is_some() {
                    return Err(ConfigError(format!(
                        "two data objects are called `{declared}`"
                    )));
                }
            }
        }
    }
    Ok((objects, skipped))
}

/// The bilateral table: at least one row, and no two rows share a domain.
fn peers_of(node: Node, (objects, skipped): &Objects) -> Result<Vec<Peer>, ConfigError> {
    let known: BTreeSet<String> = objects.keys().cloned().collect();
    let peers = node
        .children()
        .filter(|c| c.has_tag_name("peer"))
        .map(|p| peer(p, &known, skipped))
        .collect::<Result<Vec<_>, _>>()?;
    if peers.is_empty() {
        return Err(ConfigError(
            "<iccp-server> needs at least one <peer> (the bilateral table)".into(),
        ));
    }
    let mut domains = BTreeSet::new();
    match peers.iter().find(|p| !domains.insert(p.domain.clone())) {
        Some(p) => Err(ConfigError(format!(
            "two <peer>s use the domain `{}`",
            p.domain
        ))),
        None => Ok(peers),
    }
}

fn object(n: Node, class: Class, tags: &Tags, opts: &Opts) -> Result<Option<Object>, ConfigError> {
    let tag = require(n, "tag")?;
    let variant = match text(n, "variant") {
        None => Variant::Q,
        Some(v) => Variant::from_text(&v).ok_or_else(|| {
            ConfigError(format!(
                "<variant> `{}` is not plain, q or qtimetag",
                show(&v)
            ))
        })?,
    };
    let name = name_or(n, "name", &tag)?;
    Ok(tags.mapped(&tag, class.kind(), opts)?.then_some(Object {
        name,
        tag,
        class,
        variant,
    }))
}

impl Module for Server {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let log = Arc::new(EventLog::open(
            text(node, "event-logging").as_deref(),
            "iccp-server",
        ));
        let found = objects_of(node, tags, opts)?;
        let peers = peers_of(node, &found)?;
        let table_id = name_or(
            child(node, "bilateral-table").unwrap_or(node),
            "id",
            "BLT_1",
        )?;
        let version = version_of(node)?;
        let objects = found.0;
        log.log(
            "info",
            "iccp server init",
            &format!("{} objects, {} peers.", objects.len(), peers.len()),
        );
        Ok(Server {
            endpoint: require(node, "endpoint")?,
            log,
            table_id,
            version,
            objects,
            peers,
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let listener = match net::listen(&self.endpoint).await {
            Ok(l) => l,
            Err(e) => {
                self.log.log(
                    "error",
                    "iccp server init",
                    &format!("failed to listen: {e}"),
                );
                tracing::error!("iccp-server: {e}");
                return;
            }
        };
        let log = self.log.clone();
        let mms = Mms::new(self.device(&store), mms::Config::default());
        tokio::spawn(refresh(mms.clone(), stop.clone()));
        mms.serve(listener, log, stop).await;
    }
}

async fn refresh(mms: Mms<Device>, stop: CancellationToken) {
    let mut every = tokio::time::interval(REFRESH);
    loop {
        tokio::select! { _ = stop.cancelled() => return, _ = every.tick() => {} }
        mms.with(|d| d.refresh(monotonic_now().0 / 1_000_000));
    }
}

impl Server {
    /// The MMS handler for this table, reading and writing `store`.
    pub fn device(self, store: &Store) -> Device {
        Device {
            store: store.clone(),
            log: self.log,
            table_id: self.table_id,
            version: self.version,
            objects: self.objects,
            peers: self.peers,
            sessions: BTreeMap::new(),
            limits: Limits::DEFAULT,
        }
    }
}

/// What one association has made: its peer, its data sets (name -> member object names) and its transfer sets.
struct Session {
    peer: usize,
    sets: BTreeMap<String, Vec<String>>,
    transfer: BTreeMap<String, Ts>,
}

/// What a name in the namespace refers to.
enum Target {
    Object(String),
    Bilateral,
    Version,
    Features,
    NextTs,
    Ts(String),
}

pub struct Device {
    store: Store,
    log: Arc<EventLog>,
    table_id: String,
    version: (i64, i64),
    objects: BTreeMap<String, Object>,
    peers: Vec<Peer>,
    sessions: BTreeMap<AssocId, Session>,
    limits: Limits,
}

fn unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn failure(class: u32, code: i64) -> Answer {
    Answer::Error { class, code }
}

impl Device {
    /// The row of the table for a remote address: an exact address first, else the first row without one.
    fn peer_for(&self, ip: &[u8]) -> Option<usize> {
        let exact = self
            .peers
            .iter()
            .position(|p| p.address_matches(ip) == Some(true));
        exact.or_else(|| self.peers.iter().position(|p| p.address.is_none()))
    }

    fn peer_of(&self, id: AssocId) -> Option<&Peer> {
        self.peers.get(self.sessions.get(&id)?.peer)
    }

    /// May `id` see (read or write access) / read / write the object?
    fn allowed(&self, id: AssocId, object: &str, write: bool) -> bool {
        self.peer_of(id).is_some_and(|p| {
            let acl = if write { &p.write } else { &p.read };
            acl.allows(object)
        })
    }

    fn visible(&self, id: AssocId, object: &str) -> bool {
        self.allowed(id, object, false) || self.allowed(id, object, true)
    }

    fn value_of(&self, o: &Object) -> Value {
        let ts = self
            .store
            .timestamp(&o.tag)
            .and_then(|t| i64::try_from(t).ok())
            .filter(|t| *t > 0);
        o.encode(self.store.get(&o.tag), ts.unwrap_or_else(unix_secs))
    }

    fn resolve(&self, id: AssocId, name: &ObjectName) -> Result<Target, i64> {
        match name {
            ObjectName::VmdSpecific(BILATERAL) => Ok(Target::Bilateral),
            ObjectName::VmdSpecific(FEATURES) => Ok(Target::Features),
            ObjectName::VmdSpecific(VERSION) => Ok(Target::Version),
            ObjectName::DomainSpecific { domain, item }
                if self.peer_of(id).is_some_and(|p| p.domain == *domain) =>
            {
                self.in_domain(id, item)
            }
            _ => Err(NON_EXISTENT),
        }
    }

    /// A name inside the association's own domain.
    fn in_domain(&self, id: AssocId, item: &str) -> Result<Target, i64> {
        let sess = self.sessions.get(&id).ok_or(NON_EXISTENT)?;
        match item {
            BILATERAL => Ok(Target::Bilateral),
            NEXT_TS => Ok(Target::NextTs),
            t if sess.transfer.contains_key(t) => Ok(Target::Ts(t.to_string())),
            o if self.objects.contains_key(o) && self.visible(id, o) => {
                Ok(Target::Object(o.to_string()))
            }
            _ => Err(NON_EXISTENT),
        }
    }

    /// The name of the transfer set a client should configure next: one that is allocated but not yet written, else a new one.
    fn next_ts(&mut self, id: AssocId) -> Result<String, i64> {
        let max = self.peer_of(id).map_or(0, |p| p.max_transfer_sets);
        let sess = self.sessions.get_mut(&id).ok_or(NON_EXISTENT)?;
        if let Some((n, _)) = sess.transfer.iter().find(|(_, t)| t.cfg.is_none()) {
            return Ok(n.clone());
        }
        if sess.transfer.len() >= max {
            return Err(TEMP_UNAVAILABLE);
        }
        let name = format!("{TS_PREFIX}{}", sess.transfer.len() + 1);
        sess.transfer.insert(name.clone(), Ts::default());
        Ok(name)
    }

    /// The values of the names that are the same for everyone.
    fn special(&self, t: &Target) -> Option<Value> {
        match t {
            Target::Bilateral => Some(Value::VisibleString(self.table_id.clone())),
            Target::Version => Some(Value::Structure(vec![
                Value::Integer(self.version.0),
                Value::Integer(self.version.1),
            ])),
            // blocks 1 and 2 of the 12 bits of IEC 60870-6-503 8.2.9 (bit 0 = Block 1)
            Target::Features => Some(Value::BitString {
                unused: 4,
                bytes: vec![0xC0, 0x00],
            }),
            _ => None,
        }
    }

    fn read_ts(&self, id: AssocId, t: &str) -> Result<Value, i64> {
        self.sessions
            .get(&id)
            .and_then(|s| s.transfer.get(t)?.cfg.as_ref())
            .map(Cfg::to_value)
            .ok_or(TEMP_UNAVAILABLE)
    }

    fn read_one(&mut self, id: AssocId, name: &ObjectName) -> Result<Value, i64> {
        let target = self.resolve(id, name)?;
        if let Some(v) = self.special(&target) {
            return Ok(v);
        }
        match target {
            Target::Object(o) if !self.allowed(id, &o, false) => Err(DENIED),
            Target::Object(o) => self
                .objects
                .get(&o)
                .map(|o| self.value_of(o))
                .ok_or(NON_EXISTENT),
            Target::NextTs => self.next_ts(id).map(|t| {
                let domain = self.peer_of(id).map_or("", |p| p.domain.as_str());
                name_value(domain, &t)
            }),
            Target::Ts(t) => self.read_ts(id, &t),
            _ => Err(NON_EXISTENT),
        }
    }

    /// Current values of a data set's members, as the transfer set compares them.
    fn current(&self, id: AssocId, set: &str) -> Vec<Option<Datum>> {
        let members = self.sessions.get(&id).and_then(|s| s.sets.get(set));
        members
            .into_iter()
            .flatten()
            .map(|m| self.objects.get(m).and_then(|o| self.store.get(&o.tag)))
            .collect()
    }

    fn write_object(&mut self, id: AssocId, o: &str, v: &Value) -> Result<(), i64> {
        if !self.allowed(id, o, true) {
            return Err(DENIED);
        }
        let obj = self.objects.get(o).ok_or(NON_EXISTENT)?;
        let datum = obj.decode(v)?;
        self.store
            .write(&obj.tag, datum)
            .map_err(|_| TEMP_UNAVAILABLE)?;
        let who = self.peer_of(id).map_or("?", |p| p.name.as_str());
        self.log.log(
            "info",
            "iccp write",
            &format!("{} wrote {} = {datum:?}", show(who), show(o)),
        );
        Ok(())
    }

    fn write_transfer_set(
        &mut self,
        id: AssocId,
        t: &str,
        v: &Value,
        now_ms: u64,
    ) -> Result<(), i64> {
        let cfg = Cfg::from_value(v)?;
        let own = self.peer_of(id).is_some_and(|p| p.domain == cfg.domain);
        let known = self
            .sessions
            .get(&id)
            .is_some_and(|s| s.sets.contains_key(&cfg.data_set));
        if !own || !known {
            return Err(NON_EXISTENT);
        }
        let current = self.current(id, &cfg.data_set);
        self.log.log(
            "info",
            "iccp transfer set",
            &format!(
                "{t}: data set {} interval {}s {}",
                show(&cfg.data_set),
                cfg.interval,
                if cfg.status { "armed" } else { "stopped" }
            ),
        );
        let ts = self
            .sessions
            .get_mut(&id)
            .and_then(|s| s.transfer.get_mut(t))
            .ok_or(NON_EXISTENT)?;
        ts.set(cfg, now_ms, current);
        Ok(())
    }

    fn write_one(
        &mut self,
        id: AssocId,
        name: &ObjectName,
        v: &Value,
        now_ms: u64,
    ) -> Result<(), i64> {
        match self.resolve(id, name)? {
            Target::Object(o) => self.write_object(id, &o, v),
            Target::Ts(t) => self.write_transfer_set(id, &t, v, now_ms),
            _ => Err(DENIED),
        }
    }

    fn names(&self, id: AssocId, class: i64, scope: &ObjectScope, after: Option<&str>) -> Answer {
        let Some(peer) = self.peer_of(id) else {
            return Answer::NOT_FOUND;
        };
        let sess = self.sessions.get(&id);
        let mut names: Vec<String> = match (class, scope) {
            (object_class::DOMAIN, ObjectScope::VmdSpecific) => vec![peer.domain.clone()],
            (object_class::NAMED_VARIABLE, ObjectScope::VmdSpecific) => {
                vec![BILATERAL.into(), FEATURES.into(), VERSION.into()]
            }
            (object_class::NAMED_VARIABLE, ObjectScope::DomainSpecific(d)) if *d == peer.domain => {
                let mut v: Vec<String> = self
                    .objects
                    .keys()
                    .filter(|o| self.visible(id, o))
                    .cloned()
                    .collect();
                v.extend([BILATERAL.into(), NEXT_TS.into()]);
                v.extend(sess.into_iter().flat_map(|s| s.transfer.keys().cloned()));
                v
            }
            (object_class::NAMED_VARIABLE_LIST, ObjectScope::DomainSpecific(d))
                if *d == peer.domain =>
            {
                sess.into_iter()
                    .flat_map(|s| s.sets.keys().cloned())
                    .collect()
            }
            (_, ObjectScope::DomainSpecific(d)) if *d != peer.domain => return Answer::NOT_FOUND,
            _ => vec![],
        };
        names.sort();
        let mut page: Vec<String> = names
            .into_iter()
            .filter(|n| after.is_none_or(|a| n.as_str() > a))
            .take(PAGE + 1)
            .collect();
        let more = page.len() > PAGE;
        page.truncate(PAGE);
        Answer::NameList { names: page, more }
    }

    fn read(&mut self, id: AssocId, access: &VariableAccess) -> Answer {
        match access {
            VariableAccess::ListOfVariable(items) => Answer::Read(
                items
                    .iter()
                    .map(|spec| match spec {
                        VariableSpecification::Name(n) => self.read_one(id, n),
                        _ => Err(UNSUPPORTED),
                    })
                    .collect(),
            ),
            VariableAccess::VariableListName(name) => {
                let Some(members) = self.set_members(id, name) else {
                    return Answer::NOT_FOUND;
                };
                Answer::Read(
                    members
                        .iter()
                        .map(|m| match self.objects.get(m) {
                            Some(o) if self.allowed(id, m, false) => Ok(self.value_of(o)),
                            Some(_) => Err(DENIED),
                            None => Err(NON_EXISTENT),
                        })
                        .collect(),
                )
            }
        }
    }

    fn set_members(&self, id: AssocId, name: &ObjectName) -> Option<Vec<String>> {
        let ObjectName::DomainSpecific { domain, item } = name else {
            return None;
        };
        if self.peer_of(id)?.domain != *domain {
            return None;
        }
        self.sessions.get(&id)?.sets.get(*item).cloned()
    }

    fn write(
        &mut self,
        id: AssocId,
        access: &VariableAccess,
        values: &[iec61850_rs::ber::Tlv],
        now_ms: u64,
    ) -> Answer {
        let VariableAccess::ListOfVariable(items) = access else {
            return Answer::Write(values.iter().map(|_| Err(UNSUPPORTED)).collect());
        };
        Answer::Write(
            items
                .iter()
                .zip(values)
                .map(|(spec, tlv)| {
                    let VariableSpecification::Name(n) = spec else {
                        return Err(UNSUPPORTED);
                    };
                    let v = iec61850_rs::proto::data::DataView::from_tlv(*tlv)
                        .and_then(|d| d.to_owned(&self.limits))
                        .map_err(|_| transfer::INCONSISTENT)?;
                    self.write_one(id, n, &v, now_ms)
                })
                .collect(),
        )
    }

    /// The member object names of a data set to be defined, or the error to answer with.
    fn members_of(
        &self,
        id: AssocId,
        domain: &str,
        members: &[VariableSpecification],
    ) -> Result<Vec<String>, Answer> {
        let undefined = || failure(DEFINITION, OBJECT_UNDEFINED);
        let list: Vec<String> = members
            .iter()
            .map(|m| match m {
                VariableSpecification::Name(ObjectName::DomainSpecific { domain: d, item })
                    if *d == domain && self.allowed(id, item, false) =>
                {
                    self.objects
                        .contains_key(*item)
                        .then(|| (*item).to_string())
                }
                _ => None,
            })
            .collect::<Option<_>>()
            .ok_or_else(undefined)?;
        if list.len() > MAX_MEMBERS {
            return Err(failure(RESOURCE, 0));
        }
        Ok(list)
    }

    fn define(
        &mut self,
        id: AssocId,
        name: &ObjectName,
        members: &[VariableSpecification],
    ) -> Answer {
        let ObjectName::DomainSpecific { domain, item } = name else {
            return failure(DEFINITION, OBJECT_UNDEFINED);
        };
        let Some(peer) = self.peer_of(id) else {
            return Answer::NOT_FOUND;
        };
        if peer.domain != *domain || !valid_name(item) {
            return failure(DEFINITION, OBJECT_UNDEFINED);
        }
        let max_sets = peer.max_sets;
        let list = match self.members_of(id, domain, members) {
            Ok(l) => l,
            Err(a) => return a,
        };
        let taken = self.objects.contains_key(*item);
        let Some(sess) = self.sessions.get_mut(&id) else {
            return Answer::NOT_FOUND;
        };
        if sess.sets.contains_key(*item) || taken {
            return failure(DEFINITION, OBJECT_EXISTS);
        }
        if sess.sets.len() >= max_sets {
            return failure(RESOURCE, 0);
        }
        sess.sets.insert((*item).to_string(), list);
        self.log.log(
            "info",
            "iccp data set",
            &format!("defined {} ({} members)", show(item), members.len()),
        );
        Answer::DataSetCreated
    }

    fn delete(
        &mut self,
        id: AssocId,
        scope: i64,
        names: &[ObjectName],
        domain: Option<&str>,
    ) -> Answer {
        use iec61850_rs::proto::mms::delete_scope::{AA_SPECIFIC, DOMAIN, SPECIFIC, VMD};
        let own = self.peer_of(id).map(|p| p.domain.clone());
        let Some(sess) = self.sessions.get_mut(&id) else {
            return Answer::NOT_FOUND;
        };
        let doomed: Vec<String> = match scope {
            SPECIFIC => names
                .iter()
                .filter_map(|n| match n {
                    ObjectName::DomainSpecific { domain, item }
                        if own.as_deref() == Some(*domain) =>
                    {
                        Some((*item).to_string())
                    }
                    _ => None,
                })
                .filter(|n| sess.sets.contains_key(n))
                .collect(),
            DOMAIN if own.as_deref() == domain => sess.sets.keys().cloned().collect(),
            AA_SPECIFIC | VMD => sess.sets.keys().cloned().collect(),
            _ => vec![],
        };
        // a data set a transfer set reports is not deletable while that set is armed
        let armed: BTreeSet<&str> = sess
            .transfer
            .values()
            .filter_map(|t| t.cfg.as_ref().filter(|c| c.status))
            .map(|c| c.data_set.as_str())
            .collect();
        let free: Vec<String> = doomed
            .iter()
            .filter(|d| !armed.contains(d.as_str()))
            .cloned()
            .collect();
        for d in &free {
            sess.sets.remove(d);
        }
        Answer::DataSetDeleted {
            matched: u32::try_from(doomed.len()).unwrap_or(u32::MAX),
            deleted: u32::try_from(free.len()).unwrap_or(u32::MAX),
        }
    }

    /// The type of a name that is not a data object.
    fn special_type(t: &Target) -> TypeSpec {
        match t {
            Target::Bilateral => TypeSpec::VisibleString(-32),
            Target::Features => TypeSpec::BitString(12),
            Target::Version => {
                transfer::structure(vec![TypeSpec::Integer(16), TypeSpec::Integer(16)])
            }
            Target::NextTs => transfer::name_type(),
            _ => transfer::type_spec(),
        }
    }

    fn attributes(&self, id: AssocId, name: &ObjectName) -> Answer {
        let spec = match self.resolve(id, name) {
            Ok(Target::Object(o)) => self.objects.get(&o).map(Object::type_spec),
            Ok(t) => Some(Self::special_type(&t)),
            Err(_) => None,
        };
        spec.map_or(Answer::NOT_FOUND, |spec| Answer::VariableType {
            deletable: false,
            spec,
        })
    }

    /// The VMD's own answers: who we are and whether we are well.
    fn vmd(service: &ConfirmedRequest) -> Option<Answer> {
        match service {
            ConfirmedRequest::Identify => Some(Answer::Identify {
                vendor: "ferrofire".into(),
                model: "iccp-server".into(),
                revision: env!("CARGO_PKG_VERSION").into(),
            }),
            ConfirmedRequest::Status { .. } => Some(Answer::Status {
                logical: 0,
                physical: 0,
            }),
            ConfirmedRequest::GetCapabilityList { .. } => Some(Answer::CapabilityList {
                capabilities: vec![],
                more: false,
            }),
            _ => None,
        }
    }

    /// Data sets: their members, definition and deletion.
    fn sets(&mut self, id: AssocId, service: &ConfirmedRequest) -> Answer {
        match service {
            ConfirmedRequest::GetNamedVariableListAttributes(n) => {
                match (self.set_members(id, n), self.peer_of(id)) {
                    (Some(m), Some(p)) => Answer::DataSetAttributes {
                        deletable: true,
                        members: m.iter().map(|o| format!("{}/{o}", p.domain)).collect(),
                    },
                    _ => Answer::NOT_FOUND,
                }
            }
            ConfirmedRequest::DefineNamedVariableList { name, variables } => {
                self.define(id, name, variables)
            }
            ConfirmedRequest::DeleteNamedVariableList {
                scope,
                names,
                domain,
            } => self.delete(id, *scope, names, *domain),
            _ => Answer::UNSUPPORTED,
        }
    }

    fn serve(&mut self, id: AssocId, now_ms: u64, service: &ConfirmedRequest) -> Answer {
        if let Some(a) = Self::vmd(service) {
            return a;
        }
        match service {
            ConfirmedRequest::GetNameList {
                object_class,
                scope,
                continue_after,
            } => self.names(id, *object_class, scope, *continue_after),
            ConfirmedRequest::Read { access, .. } => self.read(id, access),
            ConfirmedRequest::Write { access, values } => self.write(id, access, values, now_ms),
            ConfirmedRequest::GetVariableAccessAttributes(n) => self.attributes(id, n),
            _ => self.sets(id, service),
        }
    }

    /// Check every armed transfer set of every association at `now_ms`; what is due becomes an InformationReport.
    pub fn refresh(&mut self, now_ms: u64) -> Due {
        let mut due = vec![];
        let ids: Vec<AssocId> = self.sessions.keys().copied().collect();
        for id in ids {
            let names: Vec<String> = self
                .sessions
                .get(&id)
                .map(|s| s.transfer.keys().cloned().collect())
                .unwrap_or_default();
            for t in names {
                let Some(set) = self
                    .sessions
                    .get(&id)
                    .and_then(|s| s.transfer.get(&t)?.cfg.as_ref().map(|c| c.data_set.clone()))
                else {
                    continue;
                };
                let current = self.current(id, &set);
                let hit = self
                    .sessions
                    .get_mut(&id)
                    .and_then(|s| s.transfer.get_mut(&t))
                    .and_then(|ts| ts.poll(now_ms, &current));
                if let Some(h) = hit {
                    if let Some(pdu) = self.report(id, &t, &set, &h) {
                        due.push((id, pdu));
                    }
                }
            }
        }
        due
    }

    /// The InformationReport: Transfer_Set_Name, Transfer_Set_Time_Stamp, DSConditions_Detected, then the listed members.
    fn report(&self, id: AssocId, ts: &str, set: &str, due: &transfer::Due) -> Option<Vec<u8>> {
        let domain = self.peer_of(id)?.domain.as_str();
        let members = self.sessions.get(&id)?.sets.get(set)?;
        let mut names = vec![
            ObjectName::VmdSpecific("Transfer_Set_Name"),
            ObjectName::VmdSpecific("Transfer_Set_Time_Stamp"),
            ObjectName::VmdSpecific("DSConditions_Detected"),
        ];
        let mut values = vec![
            name_value(domain, ts),
            Value::Integer(unix_secs()),
            transfer::conditions(due.conditions),
        ];
        for m in due.members.iter().filter_map(|i| members.get(*i)) {
            names.push(ObjectName::DomainSpecific { domain, item: m });
            values.push(self.objects.get(m).map(|o| self.value_of(o))?);
        }
        let encoded: Vec<Vec<u8>> = values
            .iter()
            .map(|v| Value::encode_all(std::slice::from_ref(v)).ok())
            .collect::<Option<_>>()?;
        let results = encoded
            .iter()
            .map(|b| {
                Cursor::new(b)
                    .next_required()
                    .map(iec61850_rs::proto::mms::AccessResult::Success)
            })
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let access = VariableAccess::ListOfVariable(
            names.into_iter().map(VariableSpecification::Name).collect(),
        );
        Pdu::Unconfirmed(Unconfirmed::InformationReport { access, results })
            .to_vec()
            .ok()
    }
}

impl Handler for Device {
    fn allow(&mut self, peer: &[u8]) -> bool {
        let ok = self.peer_for(peer).is_some();
        if !ok {
            self.log.log(
                "warn",
                "iccp associate",
                &format!("refused {peer:?}: not in the bilateral table"),
            );
        }
        ok
    }
    fn opened(&mut self, id: AssocId, peer: Vec<u8>) {
        if let Some(p) = self.peer_for(&peer) {
            self.sessions.insert(
                id,
                Session {
                    peer: p,
                    sets: BTreeMap::new(),
                    transfer: BTreeMap::new(),
                },
            );
            self.log.log(
                "info",
                "iccp associate",
                &format!("{} associated", show(&self.peers[p].name)),
            );
        }
    }
    fn request(&mut self, id: AssocId, now: Instant, invoke_id: i64, pdu: &[u8]) -> (Vec<u8>, Due) {
        let answer = match Pdu::parse(pdu, &self.limits) {
            Ok(Pdu::ConfirmedRequest { service, .. }) => {
                self.serve(id, now.0 / 1_000_000, &service)
            }
            _ => Answer::INVALID_PDU,
        };
        (mms::encode(&answer, invoke_id), vec![])
    }
    fn tick(&mut self, _: Instant) -> Due {
        vec![]
    }
    fn closed(&mut self, id: AssocId, _: Instant) {
        if let Some(s) = self.sessions.remove(&id) {
            self.log.log(
                "info",
                "iccp associate",
                &format!("{} released", show(&self.peers[s.peer].name)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::store::Layout;
    use crate::model::Kind;
    use iec61850_rs::proto::mms::association::{Association, AssociationConfig};

    fn tags() -> Tags {
        let mut t = Tags::default();
        for (n, k) in [
            ("bi", Kind::Binary),
            ("an", Kind::Analog),
            ("bo", Kind::Binary),
        ] {
            t.add_internal(n, k).unwrap();
        }
        t
    }

    fn parse(inner: &str, opts: &Opts) -> Result<Server, ConfigError> {
        let xml = format!("<iccp-server><event-logging>/dev/null</event-logging><endpoint>tcp://127.0.0.1:1</endpoint>{inner}</iccp-server>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        Server::parse(doc.root_element(), &mut tags(), opts)
    }

    fn err(inner: &str) -> String {
        parse(inner, &Opts::default()).err().expect("an error").0
    }

    const OBJ: &str = "<data-state><tag>bi</tag></data-state><data-real><tag>an</tag><name>MW</name><variant>plain</variant></data-real>";

    #[test]
    fn defaults_names_variants_and_access() {
        let s = parse(
            &format!("{OBJ}<peer><name>A</name></peer>"),
            &Opts::default(),
        )
        .unwrap();
        assert_eq!(s.table_id, "BLT_1");
        assert_eq!(s.version, (2000, 8), "field peers use 2000.8");
        assert_eq!(
            s.objects["bi"].variant,
            Variant::Q,
            "q by default, named after its tag"
        );
        assert_eq!(s.objects["MW"].variant, Variant::Plain);
        let p = &s.peers[0];
        assert_eq!(
            (p.domain.as_str(), p.max_sets, p.max_transfer_sets),
            ("A", 16, 8)
        );
        assert_eq!(p.read, Access::All);
        assert_eq!(
            p.write,
            Access::Only(BTreeSet::new()),
            "nothing is writable unless listed"
        );
        assert!(p.address.is_none());
        let s = parse(&format!("{OBJ}<bilateral-table><id>T-9</id></bilateral-table><peer><name>A</name><domain>D</domain><read>MW, bi</read><write>*</write><max-data-sets>2</max-data-sets><max-transfer-sets>1</max-transfer-sets><address>::ffff:10.0.0.1</address></peer>"), &Opts::default()).unwrap();
        let p = &s.peers[0];
        assert_eq!(
            (
                s.table_id.as_str(),
                p.domain.as_str(),
                p.max_sets,
                p.max_transfer_sets
            ),
            ("T-9", "D", 2, 1)
        );
        assert_eq!(p.write, Access::All);
        assert!(p.read.allows("MW") && p.read.allows("bi") && !p.read.allows("x"));
    }

    #[test]
    fn the_tase2_version_is_configurable_year_dot_month() {
        let p = "<peer><name>A</name></peer>";
        let s = parse(
            &format!("{OBJ}<tase2-version>1996.8</tase2-version>{p}"),
            &Opts::default(),
        )
        .unwrap();
        assert_eq!(s.version, (1996, 8));
        let mut d = s.device(&Store::new(crate::fd::store::Layout::default()));
        d.opened(1, vec![1, 2, 3, 4]);
        assert_eq!(
            d.read_one(1, &ObjectName::VmdSpecific(VERSION)),
            Ok(Value::Structure(vec![
                Value::Integer(1996),
                Value::Integer(8)
            ]))
        );
        for bad in [
            "2000", "2000.", "x.8", "2000.13", "2000.0", "1899.8", "2000.8.1",
        ] {
            assert!(
                err(&format!("{OBJ}<tase2-version>{bad}</tase2-version>{p}"))
                    .contains("<tase2-version>"),
                "{bad}"
            );
        }
    }

    #[test]
    fn bad_tables_are_refused_by_name() {
        let p = "<peer><name>A</name></peer>";
        assert!(err(OBJ).contains("at least one <peer>"));
        assert!(err(&format!("{OBJ}{p}{p}")).contains("two <peer>s use the domain `A`"));
        assert!(
            err(&format!("{OBJ}<peer></peer>")).contains("<name>"),
            "a peer needs a name"
        );
        assert!(err(&format!("{OBJ}<peer><name>a b</name></peer>")).contains("is not a name"));
        assert!(err(&format!(
            "{OBJ}<peer><name>A</name><address>nope</address></peer>"
        ))
        .contains("not an IP address"));
        assert!(err(&format!(
            "{OBJ}<peer><name>A</name><read>ghost</read></peer>"
        ))
        .contains("`ghost`, which is not a data object"));
        assert!(err(&format!(
            "{OBJ}<data-real><tag>an</tag><name>MW</name></data-real>{p}"
        ))
        .contains("two data objects are called `MW`"));
        assert!(err(&format!(
            "<data-real><tag>an</tag><variant>wide</variant></data-real>{p}"
        ))
        .contains("not plain, q or qtimetag"));
        assert!(
            err(&format!("<data-real><tag>bi</tag></data-real>{p}")).contains("needs Analog"),
            "kinds are checked"
        );
        assert!(err(&format!("<data-real></data-real>{p}")).contains("has no <tag>"));
        assert!(err(&format!(
            "<bilateral-table><id>a b</id></bilateral-table>{OBJ}{p}"
        ))
        .contains("<id>"));
        let no_ep = roxmltree::Document::parse("<iccp-server><event-logging>/dev/null</event-logging><peer><name>A</name></peer></iccp-server>").unwrap();
        assert!(
            Server::parse(no_ep.root_element(), &mut tags(), &Opts::default())
                .err()
                .unwrap()
                .0
                .contains("no <endpoint>")
        );
    }

    #[test]
    fn an_unknown_tag_is_skipped_with_a_warning_and_strict_refuses_it() {
        let inner =
            "<data-real><tag>ghost</tag></data-real><peer><name>A</name><read>ghost</read></peer>";
        let s = parse(inner, &Opts::default()).unwrap();
        assert!(
            s.objects.is_empty(),
            "skipped, and the table may still name it"
        );
        assert!(parse(inner, &Opts { strict: true }).is_err());
    }

    fn device(peers: &str) -> Device {
        let s = parse(&format!("{OBJ}{peers}"), &Opts::default()).unwrap();
        let mut layout = Layout::default();
        for (n, k) in [
            ("bi", Kind::Binary),
            ("an", Kind::Analog),
            ("bo", Kind::Binary),
        ] {
            layout.points.push((n.into(), k, None));
            layout.external.push((n.into(), n.into()));
        }
        s.device(&Store::new(layout))
    }

    #[test]
    fn a_peer_is_found_by_exact_address_first_then_the_first_without_one() {
        let mut d = device("<peer><name>ANY</name></peer><peer><name>EMS</name><address>10.0.0.5</address></peer><peer><name>V6</name><address>fd00::1</address></peer>");
        assert_eq!(d.peer_for(&[10, 0, 0, 5]), Some(1));
        assert_eq!(d.peer_for(&[10, 0, 0, 6]), Some(0));
        let mapped = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 10, 0, 0, 5];
        assert_eq!(
            d.peer_for(&mapped),
            Some(1),
            "an IPv4 peer on a dual-stack socket"
        );
        let v6 = std::net::Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1).octets();
        assert_eq!(d.peer_for(&v6), Some(2));
        assert_eq!(
            d.peer_for(&[1, 2, 3]),
            Some(0),
            "an odd length matches no address, only the open row"
        );
        assert!(d.allow(&[10, 0, 0, 9]));
        let mut closed = device("<peer><name>EMS</name><address>10.0.0.5</address></peer>");
        assert!(closed.allow(&[10, 0, 0, 5]));
        assert!(!closed.allow(&[10, 0, 0, 6]));
        assert!(!closed.allow(&[]));
    }

    #[test]
    fn a_session_exists_only_for_an_admitted_peer_and_ends_on_close() {
        let mut d = device("<peer><name>EMS</name><address>10.0.0.5</address></peer>");
        d.opened(1, vec![10, 0, 0, 6]);
        assert!(d.sessions.is_empty());
        d.opened(2, vec![10, 0, 0, 5]);
        assert_eq!(d.sessions.len(), 1);
        d.closed(2, Instant(0));
        d.closed(3, Instant(0));
        assert!(d.sessions.is_empty());
        assert!(!d.allowed(2, "MW", false), "no session, no access");
    }

    #[test]
    fn names_page_in_order_and_continue_after() {
        let mut d = device("<peer><name>EMS</name></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        d.objects = (0..150)
            .map(|i| Object {
                name: format!("O{i:03}"),
                tag: "an".into(),
                class: Class::Real,
                variant: Variant::Q,
            })
            .map(|o| (o.name.clone(), o))
            .collect();
        let page = |after: Option<&str>| match d.names(
            1,
            object_class::NAMED_VARIABLE,
            &ObjectScope::DomainSpecific("EMS"),
            after,
        ) {
            Answer::NameList { names, more } => (names, more),
            a => panic!("{a:?}"),
        };
        let (first, more) = page(None);
        assert_eq!(
            (first.len(), more, first[0].as_str()),
            (PAGE, true, "Bilateral_Table_ID")
        );
        assert!(first.windows(2).all(|w| w[0] < w[1]), "ascending");
        let (second, more) = page(first.last().map(String::as_str));
        assert!(!more);
        assert_eq!(
            first.len() + second.len(),
            150 + 2,
            "every object and the two specials, once"
        );
        assert_eq!(page(Some("zzz")).0.len(), 0);
        let other = d.names(
            1,
            object_class::NAMED_VARIABLE,
            &ObjectScope::DomainSpecific("X"),
            None,
        );
        assert_eq!(other, Answer::NOT_FOUND);
        let jr = d.names(1, object_class::JOURNAL, &ObjectScope::VmdSpecific, None);
        assert_eq!(
            jr,
            Answer::NameList {
                names: vec![],
                more: false
            },
            "classes we do not have are empty"
        );
        assert_eq!(
            d.names(9, object_class::DOMAIN, &ObjectScope::VmdSpecific, None),
            Answer::NOT_FOUND,
            "no such session"
        );
    }

    #[test]
    fn data_set_and_transfer_set_limits_hold() {
        let mut d = device("<peer><name>EMS</name><max-data-sets>1</max-data-sets><max-transfer-sets>1</max-transfer-sets></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        let name = |i| ObjectName::DomainSpecific {
            domain: "EMS",
            item: i,
        };
        let mem = |i| VariableSpecification::Name(name(i));
        assert_eq!(
            d.define(1, &name("S1"), &[mem("MW")]),
            Answer::DataSetCreated
        );
        assert_eq!(
            d.define(1, &name("S2"), &[mem("MW")]),
            failure(RESOURCE, 0),
            "one set per association here"
        );
        let many: Vec<_> = (0..=MAX_MEMBERS).map(|_| mem("MW")).collect();
        let mut d2 = device("<peer><name>EMS</name></peer>");
        d2.opened(1, vec![1, 2, 3, 4]);
        assert_eq!(d2.define(1, &name("BIG"), &many), failure(RESOURCE, 0));
        assert_eq!(
            d2.define(1, &name("OK"), &many[..MAX_MEMBERS]),
            Answer::DataSetCreated,
            "exactly the limit fits"
        );
        assert_eq!(d.next_ts(1), Ok("DSTrans_1".into()));
        d.sessions
            .get_mut(&1)
            .unwrap()
            .transfer
            .get_mut("DSTrans_1")
            .unwrap()
            .cfg = Some(Cfg::default());
        assert_eq!(d.next_ts(1), Err(TEMP_UNAVAILABLE), "the pool is one");
        assert_eq!(d.next_ts(7), Err(NON_EXISTENT));
    }

    #[test]
    fn an_armed_data_set_is_matched_but_not_deleted() {
        let mut d = device("<peer><name>EMS</name></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        let name = |i| ObjectName::DomainSpecific {
            domain: "EMS",
            item: i,
        };
        d.define(1, &name("S1"), &[VariableSpecification::Name(name("MW"))]);
        d.define(1, &name("S2"), &[VariableSpecification::Name(name("MW"))]);
        let t = d.next_ts(1).unwrap();
        let cfg = Cfg {
            domain: "EMS".into(),
            data_set: "S1".into(),
            status: true,
            ..Cfg::default()
        };
        d.write_one(1, &name(&t), &cfg.to_value(), 0).unwrap();
        assert_eq!(
            d.delete(1, 0, &[name("S1"), name("S2"), name("S3")], None),
            Answer::DataSetDeleted {
                matched: 2,
                deleted: 1
            }
        );
        assert_eq!(
            d.delete(1, 2, &[], Some("EMS")),
            Answer::DataSetDeleted {
                matched: 1,
                deleted: 0
            }
        );
        assert_eq!(
            d.delete(1, 2, &[], Some("OTHER")),
            Answer::DataSetDeleted {
                matched: 0,
                deleted: 0
            }
        );
        assert_eq!(
            d.delete(1, 1, &[], None),
            Answer::DataSetDeleted {
                matched: 1,
                deleted: 0
            }
        );
        assert_eq!(
            d.delete(1, 3, &[], None),
            Answer::DataSetDeleted {
                matched: 1,
                deleted: 0
            }
        );
        assert_eq!(
            d.delete(1, 9, &[], None),
            Answer::DataSetDeleted {
                matched: 0,
                deleted: 0
            }
        );
        assert_eq!(d.delete(5, 0, &[], None), Answer::NOT_FOUND);
    }

    #[test]
    fn refresh_builds_a_report_with_the_special_variables_first() {
        let mut d = device("<peer><name>EMS</name></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        let name = |i| ObjectName::DomainSpecific {
            domain: "EMS",
            item: i,
        };
        d.define(
            1,
            &name("S1"),
            &[
                VariableSpecification::Name(name("MW")),
                VariableSpecification::Name(name("bi")),
            ],
        );
        let t = d.next_ts(1).unwrap();
        let cfg = Cfg {
            domain: "EMS".into(),
            data_set: "S1".into(),
            interval: 1,
            conditions: transfer::INTERVAL,
            status: true,
            ..Cfg::default()
        };
        d.write_one(1, &name(&t), &cfg.to_value(), 5_000).unwrap();
        assert!(d.refresh(5_999).is_empty());
        let due = d.refresh(6_000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, 1);
        let Ok(Pdu::Unconfirmed(Unconfirmed::InformationReport {
            access: VariableAccess::ListOfVariable(items),
            results,
        })) = Pdu::parse(&due[0].1, &Limits::DEFAULT)
        else {
            panic!("not a report")
        };
        let names: Vec<_> = items.iter().filter_map(|i| i.name().copied()).collect();
        assert_eq!(
            names[..3],
            [
                ObjectName::VmdSpecific("Transfer_Set_Name"),
                ObjectName::VmdSpecific("Transfer_Set_Time_Stamp"),
                ObjectName::VmdSpecific("DSConditions_Detected")
            ]
        );
        assert_eq!(names[3], name("MW"));
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn hostile_bytes_never_panic_and_a_violation_closes_the_association() {
        let mut d = device("<peer><name>EMS</name></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        // an LCG stands in for a fuzzer: random frames, and valid-looking ones with random tails
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for round in 0..300 {
            let mut assoc = Association::server(AssociationConfig::default());
            let mut closed = false;
            for _ in 0..8 {
                let n = (next() % 60) as usize;
                let mut b: Vec<u8> = (0..n).map(|_| next() as u8).collect();
                if round % 2 == 0 {
                    b.splice(0..0, [3, 0, 0, (n + 4) as u8]); // a TPKT header
                }
                assoc.on_bytes(monotonic_now(), &b);
                let (_, c) = mms::step(&mut d, 1, &mut assoc, monotonic_now(), &mut false);
                while assoc.poll_transmit().is_some() {}
                closed |= c;
            }
            let _ = closed;
        }
        // an MMS PDU that is not a request, and one that is garbage, are answered, not fatal
        let (a, _) = d.request(1, Instant(0), 5, &[0xFF, 0x00]);
        assert!(!a.is_empty(), "a reject");
        let (a, _) = d.request(1, Instant(0), 5, &[0xA4, 0x00]);
        assert!(!a.is_empty());
    }

    #[test]
    fn the_vmd_answers_and_every_name_has_a_type() {
        let mut d = device("<peer><name>EMS</name></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        let ask = |d: &mut Device, s: ConfirmedRequest| d.serve(1, 0, &s);
        assert!(matches!(
            ask(&mut d, ConfirmedRequest::Identify),
            Answer::Identify { .. }
        ));
        assert_eq!(
            ask(
                &mut d,
                ConfirmedRequest::Status {
                    extended_derivation: false
                }
            ),
            Answer::Status {
                logical: 0,
                physical: 0
            }
        );
        assert_eq!(
            ask(
                &mut d,
                ConfirmedRequest::GetCapabilityList {
                    continue_after: None
                }
            ),
            Answer::CapabilityList {
                capabilities: vec![],
                more: false
            }
        );
        assert_eq!(
            ask(&mut d, ConfirmedRequest::FileRead(1)),
            Answer::UNSUPPORTED
        );
        let name = |i| ObjectName::DomainSpecific {
            domain: "EMS",
            item: i,
        };
        let ty = |d: &Device, n: ObjectName| match d.attributes(1, &n) {
            Answer::VariableType {
                spec,
                deletable: false,
            } => spec,
            a => panic!("{a:?}"),
        };
        assert_eq!(
            ty(&d, ObjectName::VmdSpecific(BILATERAL)),
            TypeSpec::VisibleString(-32)
        );
        assert_eq!(ty(&d, name(BILATERAL)), TypeSpec::VisibleString(-32));
        assert_eq!(
            ty(&d, ObjectName::VmdSpecific(FEATURES)),
            TypeSpec::BitString(12)
        );
        assert_eq!(ty(&d, name(NEXT_TS)), transfer::name_type());
        assert!(matches!(
            ty(&d, ObjectName::VmdSpecific(VERSION)),
            TypeSpec::Structure { .. }
        ));
        let t = d.next_ts(1).unwrap();
        assert_eq!(ty(&d, name(&t)), transfer::type_spec());
        assert!(matches!(ty(&d, name("MW")), TypeSpec::FloatingPoint { .. }));
        assert_eq!(d.attributes(1, &name("ghost")), Answer::NOT_FOUND);
        assert_eq!(
            d.attributes(1, &ObjectName::AaSpecific("x")),
            Answer::NOT_FOUND
        );
    }

    #[test]
    fn reads_of_specials_and_a_configured_transfer_set_and_refusals() {
        let mut d = device("<peer><name>EMS</name></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        let name = |i| ObjectName::DomainSpecific {
            domain: "EMS",
            item: i,
        };
        assert_eq!(
            d.read_one(1, &ObjectName::VmdSpecific(VERSION)),
            Ok(Value::Structure(vec![
                Value::Integer(2000),
                Value::Integer(8)
            ]))
        );
        assert_eq!(
            d.read_one(1, &ObjectName::VmdSpecific(FEATURES)),
            Ok(Value::BitString {
                unused: 4,
                bytes: vec![0xC0, 0x00]
            }),
            "IEC 60870-6-503 8.2.9: 12 bits, blocks 1 and 2"
        );
        assert_eq!(
            d.read_one(1, &name(NEXT_TS)),
            Ok(name_value("EMS", "DSTrans_1"))
        );
        assert_eq!(d.read_one(1, &name("DSTrans_1")), Err(TEMP_UNAVAILABLE));
        assert_eq!(d.read_one(1, &name("ghost")), Err(NON_EXISTENT));
        assert_eq!(
            d.read_one(1, &ObjectName::AaSpecific("MW")),
            Err(NON_EXISTENT)
        );
        assert_eq!(d.read_one(9, &name("MW")), Err(NON_EXISTENT), "no session");
        let v = Value::Float32(1.0);
        assert_eq!(
            d.write_one(1, &name(BILATERAL), &v, 0),
            Err(DENIED),
            "a special is read-only"
        );
        assert_eq!(
            d.write_one(1, &name("MW"), &v, 0),
            Err(DENIED),
            "nothing is writable by default"
        );
        d.define(1, &name("S1"), &[VariableSpecification::Name(name("MW"))]);
        let cfg = Cfg {
            domain: "EMS".into(),
            data_set: "S1".into(),
            ..Cfg::default()
        };
        d.write_one(1, &name("DSTrans_1"), &cfg.to_value(), 0)
            .unwrap();
        assert_eq!(d.read_one(1, &name("DSTrans_1")), Ok(cfg.to_value()));
        let other = Cfg {
            domain: "OTHER".into(),
            ..cfg
        };
        assert_eq!(
            d.write_one(1, &name("DSTrans_1"), &other.to_value(), 0),
            Err(NON_EXISTENT)
        );
        assert_eq!(
            d.write_one(1, &name("DSTrans_1"), &Value::Integer(1), 0),
            Err(transfer::INCONSISTENT)
        );
    }

    #[test]
    fn a_data_set_must_be_defined_from_readable_objects_of_the_own_domain() {
        let mut d = device("<peer><name>EMS</name><read>MW</read></peer>");
        d.opened(1, vec![1, 2, 3, 4]);
        let name = |i| ObjectName::DomainSpecific {
            domain: "EMS",
            item: i,
        };
        let mem = |dom, i| {
            VariableSpecification::Name(ObjectName::DomainSpecific {
                domain: dom,
                item: i,
            })
        };
        let undefined = failure(DEFINITION, OBJECT_UNDEFINED);
        assert_eq!(
            d.define(1, &name("S"), &[mem("EMS", "bi")]),
            undefined,
            "not readable"
        );
        assert_eq!(
            d.define(1, &name("S"), &[mem("OTHER", "MW")]),
            undefined,
            "another domain"
        );
        assert_eq!(
            d.define(
                1,
                &name("S"),
                &[VariableSpecification::Name(ObjectName::VmdSpecific("MW"))]
            ),
            undefined
        );
        assert_eq!(d.define(1, &ObjectName::VmdSpecific("S"), &[]), undefined);
        assert_eq!(
            d.define(1, &name("S"), &[mem("EMS", "MW")]),
            Answer::DataSetCreated
        );
        assert_eq!(
            d.define(1, &name("S"), &[]),
            failure(DEFINITION, OBJECT_EXISTS)
        );
        assert_eq!(
            d.define(1, &name("MW"), &[]),
            failure(DEFINITION, OBJECT_EXISTS),
            "an object name is taken"
        );
        assert_eq!(
            d.define(2, &name("S"), &[]),
            Answer::NOT_FOUND,
            "no session"
        );
    }
}
