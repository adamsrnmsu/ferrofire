//! A thin ICCP/TASE.2 client for tests and the acceptance harness (no open ICCP peer exists). It speaks the same dialect the server
//! documents (docs/field-device-iccp.md), so it proves the server against our own reading of TASE.2, not against a third party:
//! the third-party evidence is a generic MMS client (libiec61850's `mms_utility`) and tshark.
#![allow(dead_code)]

use iec61850_rs::ber::Cursor;
use iec61850_rs::common::{Instant, Limits};
use iec61850_rs::proto::data::{DataView, Value};
use iec61850_rs::proto::mms::association::{Association, AssociationConfig, AssociationEvent};
use iec61850_rs::proto::mms::{
    object_class, AccessResult, ConfirmedRequest, ConfirmedResponse, Mms, ObjectName, ObjectScope,
    Unconfirmed, VariableAccess, VariableSpecification, WriteResult,
};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant as Clock};

/// What an InformationReport carried.
#[derive(Debug, PartialEq)]
pub struct Report {
    pub transfer_set: String,
    pub time: i64,
    /// DSConditions_Detected, first octet (bit 0 = 0x80).
    pub conditions: u8,
    /// `(item, value)` of each data set member listed.
    pub values: Vec<(String, Value)>,
}

pub struct Iccp {
    stream: TcpStream,
    assoc: Association,
    epoch: Clock,
    reports: VecDeque<Report>,
    limits: Limits,
}

fn owned(r: &AccessResult) -> Result<Value, i64> {
    match r {
        AccessResult::Failure(c) => Err(*c),
        AccessResult::Success(t) => DataView::from_tlv(*t)
            .and_then(|d| d.to_owned(&Limits::DEFAULT))
            .map_err(|_| -1),
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::VisibleString(s) => s.clone(),
        _ => String::new(),
    }
}

fn parse_report(pdu: &[u8]) -> Option<Report> {
    let Ok(Mms::Unconfirmed(Unconfirmed::InformationReport {
        access: VariableAccess::ListOfVariable(items),
        results,
    })) = Mms::parse(pdu, &Limits::DEFAULT)
    else {
        return None;
    };
    let values: Vec<Result<Value, i64>> = results.iter().map(owned).collect();
    let (name, time, cond) = (
        values.first()?.as_ref().ok()?,
        values.get(1)?.as_ref().ok()?,
        values.get(2)?.as_ref().ok()?,
    );
    let transfer_set = text(name.members()?.get(2)?);
    let time = match time {
        Value::Integer(t) => *t,
        _ => return None,
    };
    let conditions = match cond {
        Value::BitString { bytes, .. } => *bytes.first()?,
        _ => return None,
    };
    let mut out = vec![];
    for (spec, v) in items.iter().zip(&values).skip(3) {
        let Some(ObjectName::DomainSpecific { item, .. }) = spec.name() else {
            return None;
        };
        out.push(((*item).to_string(), v.clone().ok()?));
    }
    Some(Report {
        transfer_set,
        time,
        conditions,
        values: out,
    })
}

impl Iccp {
    /// Connect and associate. Fails with the reason if the server refuses (a peer not in its bilateral table).
    pub fn connect(addr: &str) -> Result<Iccp, String> {
        let stream = TcpStream::connect(addr).map_err(|e| format!("{addr}: {e}"))?;
        stream.set_nodelay(true).ok();
        let mut c = Iccp {
            stream,
            assoc: Association::client(AssociationConfig::default()),
            epoch: Clock::now(),
            reports: VecDeque::new(),
            limits: Limits::DEFAULT,
        };
        let now = c.now();
        c.assoc.start(now).map_err(|e| e.to_string())?;
        c.flush()?;
        let deadline = Clock::now() + Duration::from_secs(5);
        while !c.assoc.is_established() {
            if Clock::now() > deadline {
                return Err("association timed out".into());
            }
            c.pump(Duration::from_millis(200))?;
        }
        Ok(c)
    }

    fn now(&self) -> Instant {
        Instant(self.epoch.elapsed().as_nanos() as u64)
    }

    fn flush(&mut self) -> Result<(), String> {
        while let Some(p) = self.assoc.poll_transmit() {
            let p = p.to_vec();
            self.stream.write_all(&p).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Read what arrives for up to `wait`; returns the responses seen as (invoke id, PDU). Reports are queued.
    fn pump(&mut self, wait: Duration) -> Result<Vec<(i64, Vec<u8>)>, String> {
        self.stream
            .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
            .ok();
        let mut buf = [0u8; 8192];
        match self.stream.read(&mut buf) {
            Ok(0) => return Err("closed by the peer".into()),
            Ok(n) => {
                let now = self.now();
                self.assoc.on_bytes(now, &buf[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
        let mut answers = vec![];
        while let Some(ev) = self.assoc.poll_event() {
            match ev {
                AssociationEvent::Response { invoke_id, pdu } => answers.push((invoke_id, pdu)),
                AssociationEvent::Unconfirmed { pdu } => self.reports.extend(parse_report(&pdu)),
                AssociationEvent::Refused { layer, code } => {
                    return Err(format!("refused at {layer}: {code:?}"))
                }
                AssociationEvent::Closed(r) => return Err(format!("closed: {r:?}")),
                _ => {}
            }
        }
        self.flush()?;
        Ok(answers)
    }

    /// One confirmed request; the response PDU, or the service error as `Err("service error class code")`.
    fn call(&mut self, service: &ConfirmedRequest) -> Result<Vec<u8>, String> {
        let now = self.now();
        let id = self.assoc.call(now, service).map_err(|e| e.to_string())?;
        self.flush()?;
        let deadline = Clock::now() + Duration::from_secs(5);
        loop {
            for (i, pdu) in self.pump(Duration::from_millis(100))? {
                if i == id {
                    if let Ok(Mms::ConfirmedError { error, .. }) = Mms::parse(&pdu, &self.limits) {
                        let e = iec61850_rs::proto::mms::ServiceError::parse(&error)
                            .map_err(|e| e.to_string())?;
                        return Err(format!("service error {} {}", e.class, e.code));
                    }
                    return Ok(pdu);
                }
            }
            if Clock::now() > deadline {
                return Err("request timed out".into());
            }
        }
    }

    fn response<T>(
        &mut self,
        service: &ConfirmedRequest,
        f: impl FnOnce(ConfirmedResponse) -> Option<T>,
    ) -> Result<T, String> {
        let pdu = self.call(service)?;
        match Mms::parse(&pdu, &self.limits) {
            Ok(Mms::ConfirmedResponse { service, .. }) => {
                f(service).ok_or_else(|| "unexpected response".to_string())
            }
            other => Err(format!("unexpected PDU {other:?}")),
        }
    }

    pub fn domains(&mut self) -> Result<Vec<String>, String> {
        self.name_list(object_class::DOMAIN, ObjectScope::VmdSpecific)
    }

    pub fn name_list(&mut self, class: i64, scope: ObjectScope) -> Result<Vec<String>, String> {
        let mut all: Vec<String> = vec![];
        loop {
            let after = all.last().cloned();
            let req = ConfirmedRequest::GetNameList {
                object_class: class,
                scope,
                continue_after: after.as_deref(),
            };
            let (names, more) = self.response(&req, |r| match r {
                ConfirmedResponse::GetNameList {
                    identifiers,
                    more_follows,
                } => Some((
                    identifiers
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect::<Vec<_>>(),
                    more_follows,
                )),
                _ => None,
            })?;
            all.extend(names);
            if !more {
                return Ok(all);
            }
        }
    }

    pub fn variables(&mut self, domain: &str) -> Result<Vec<String>, String> {
        self.name_list(
            object_class::NAMED_VARIABLE,
            ObjectScope::DomainSpecific(domain),
        )
    }

    pub fn identify(&mut self) -> Result<String, String> {
        self.response(&ConfirmedRequest::Identify, |r| match r {
            ConfirmedResponse::Identify { vendor, model, .. } => Some(format!("{vendor} {model}")),
            _ => None,
        })
    }

    /// Read named variables of `domain` (`None`: the VMD scope).
    pub fn read(
        &mut self,
        domain: Option<&str>,
        items: &[&str],
    ) -> Result<Vec<Result<Value, i64>>, String> {
        let specs = items
            .iter()
            .map(|i| VariableSpecification::Name(name(domain, i)))
            .collect();
        let req = ConfirmedRequest::Read {
            specification_with_result: false,
            access: VariableAccess::ListOfVariable(specs),
        };
        self.response(&req, |r| match r {
            ConfirmedResponse::Read { results, .. } => Some(results.iter().map(owned).collect()),
            _ => None,
        })
    }

    pub fn read_set(&mut self, domain: &str, set: &str) -> Result<Vec<Result<Value, i64>>, String> {
        let req = ConfirmedRequest::Read {
            specification_with_result: false,
            access: VariableAccess::VariableListName(name(Some(domain), set)),
        };
        self.response(&req, |r| match r {
            ConfirmedResponse::Read { results, .. } => Some(results.iter().map(owned).collect()),
            _ => None,
        })
    }

    pub fn write(
        &mut self,
        domain: &str,
        item: &str,
        v: &Value,
    ) -> Result<Result<(), i64>, String> {
        let bytes = Value::encode_all(std::slice::from_ref(v)).map_err(|e| e.to_string())?;
        let tlv = Cursor::new(&bytes)
            .next_required()
            .map_err(|e| e.to_string())?;
        let req = ConfirmedRequest::Write {
            access: VariableAccess::ListOfVariable(vec![VariableSpecification::Name(name(
                Some(domain),
                item,
            ))]),
            values: vec![tlv],
        };
        self.response(&req, |r| match r {
            ConfirmedResponse::Write(w) => Some(match w.first()? {
                WriteResult::Success => Ok(()),
                WriteResult::Failure(c) => Err(*c),
            }),
            _ => None,
        })
    }

    pub fn define_set(&mut self, domain: &str, set: &str, members: &[&str]) -> Result<(), String> {
        let variables = members
            .iter()
            .map(|m| VariableSpecification::Name(name(Some(domain), m)))
            .collect();
        let req = ConfirmedRequest::DefineNamedVariableList {
            name: name(Some(domain), set),
            variables,
        };
        self.response(&req, |r| {
            matches!(r, ConfirmedResponse::DefineNamedVariableList).then_some(())
        })
    }

    pub fn delete_set(&mut self, domain: &str, set: &str) -> Result<(u32, u32), String> {
        let req = ConfirmedRequest::DeleteNamedVariableList {
            scope: 0,
            names: vec![name(Some(domain), set)],
            domain: None,
        };
        self.response(&req, |r| match r {
            ConfirmedResponse::DeleteNamedVariableList { matched, deleted } => {
                Some((matched, deleted))
            }
            _ => None,
        })
    }

    pub fn set_members(&mut self, domain: &str, set: &str) -> Result<Vec<String>, String> {
        let req = ConfirmedRequest::GetNamedVariableListAttributes(name(Some(domain), set));
        self.response(&req, |r| match r {
            ConfirmedResponse::GetNamedVariableListAttributes { variables, .. } => Some(
                variables
                    .iter()
                    .filter_map(|v| match v.name()? {
                        ObjectName::DomainSpecific { item, .. } => Some((*item).to_string()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
    }

    /// The type of a variable, rendered as text.
    pub fn type_of(&mut self, domain: Option<&str>, item: &str) -> Result<String, String> {
        let req = ConfirmedRequest::GetVariableAccessAttributes(name(domain, item));
        self.response(&req, |r| match r {
            ConfirmedResponse::GetVariableAccessAttributes { type_spec, .. } => {
                Some(format!("{type_spec:?}"))
            }
            _ => None,
        })
    }

    /// Next_DSTransfer_Set: the name of the transfer set to configure.
    pub fn next_transfer_set(&mut self, domain: &str) -> Result<String, String> {
        let v = self
            .read(Some(domain), &["Next_DSTransfer_Set"])?
            .remove(0)
            .map_err(|c| format!("data access error {c}"))?;
        Ok(text(
            v.members()
                .and_then(|m| m.get(2))
                .ok_or("not a transfer set name")?,
        ))
    }

    /// Write a DSTransfer_Set: report `data_set` every `interval` seconds and on change, starting now when `status`.
    pub fn arm(
        &mut self,
        domain: &str,
        transfer_set: &str,
        data_set: &str,
        (interval, conditions, rbe, status): (i64, u8, bool, bool),
    ) -> Result<Result<(), i64>, String> {
        let int = Value::Integer;
        let v = Value::Structure(vec![
            Value::Structure(vec![
                Value::Integer(1),
                Value::VisibleString(domain.into()),
                Value::VisibleString(data_set.into()),
            ]),
            int(0),
            int(interval),
            int(0),
            int(0),
            int(0),
            Value::BitString {
                unused: 3,
                bytes: vec![conditions],
            },
            Value::Boolean(false),
            Value::Boolean(false),
            Value::Boolean(rbe),
            Value::Boolean(false),
            Value::Boolean(status),
            int(0),
        ]);
        self.write(domain, transfer_set, &v)
    }

    /// The next InformationReport, waiting up to `wait`.
    pub fn next_report(&mut self, wait: Duration) -> Result<Option<Report>, String> {
        let deadline = Clock::now() + wait;
        loop {
            if let Some(r) = self.reports.pop_front() {
                return Ok(Some(r));
            }
            let left = deadline.saturating_duration_since(Clock::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.pump(left.min(Duration::from_millis(100)))?;
        }
    }

    pub fn release(&mut self) -> Result<(), String> {
        let now = self.now();
        self.assoc.release(now).map_err(|e| e.to_string())?;
        self.flush()?;
        let _ = self.pump(Duration::from_millis(300));
        Ok(())
    }
}

fn name<'a>(domain: Option<&'a str>, item: &'a str) -> ObjectName<'a> {
    match domain {
        Some(domain) => ObjectName::DomainSpecific { domain, item },
        None => ObjectName::VmdSpecific(item),
    }
}
