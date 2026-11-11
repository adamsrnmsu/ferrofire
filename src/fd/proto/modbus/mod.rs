//! Modbus/TCP server (SPEC 4.1, `<modbus-server>`): hand-rolled MBAP, our own implementation of the stock bennu behaviour,
//! differential-tested against it (`conformance/modbus/`). `codec` frames, `device` is the I/O-free state machine (tables, scaling,
//! one connection's buffer); this file is the config and the sockets around them.

pub mod codec;
pub mod device;

use super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::net;
use super::super::store::Store;
use super::Module;
use crate::model::Kind;
use device::{Close, Conn, Device, Range, Reg, DEFAULT_RANGE};
use roxmltree::Node;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

/// A reply that cannot be written within this long ends the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Server {
    endpoint: String,
    log: Arc<EventLog>,
    dev: Device,
}

/// `<max-value>` and `<min-value>` count only together (SPEC 4.1): one alone leaves the stock default.
fn range(n: Node) -> Result<Range, ConfigError> {
    Ok(
        match (
            number::<f64>(n, "max-value")?,
            number::<f64>(n, "min-value")?,
        ) {
            (Some(max), Some(min)) => Range { min, max },
            _ => DEFAULT_RANGE,
        },
    )
}

/// The `<name>` children of a server whose tag exists with the right kind, as `(address, tag, range)`. A later element at the same
/// address replaces an earlier one.
fn table(
    node: Node,
    name: &str,
    kind: Kind,
    tags: &Tags,
    opts: &Opts,
) -> Result<BTreeMap<u16, Reg>, ConfigError> {
    let mut out = BTreeMap::new();
    for n in node.children().filter(|c| c.has_tag_name(name)) {
        let addr = number::<u16>(n, "address")?
            .ok_or_else(|| ConfigError(format!("<{name}> has no <address>")))?;
        let tag = require(n, "tag")?;
        if tags.mapped(&tag, kind, opts)? {
            out.insert(
                addr,
                Reg {
                    tag,
                    range: range(n)?,
                },
            );
        }
    }
    Ok(out)
}

fn names(t: BTreeMap<u16, Reg>) -> BTreeMap<u16, String> {
    t.into_iter().map(|(a, r)| (a, r.tag)).collect()
}

impl Server {
    /// The tables of this server, as every connection shares them.
    pub fn device(&self) -> Device {
        self.dev.clone()
    }
}

impl Module for Server {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let endpoint = require(node, "endpoint")?;
        let log = Arc::new(EventLog::open(
            text(node, "event-logging").as_deref(),
            "modbus-server",
        ));
        let dev = Device {
            coils: names(table(node, "coil", Kind::Binary, tags, opts)?),
            discretes: names(table(node, "discrete-input", Kind::Binary, tags, opts)?),
            holding: table(node, "holding-register", Kind::Analog, tags, opts)?,
            input: table(node, "input-register", Kind::Analog, tags, opts)?,
        };
        Ok(Server { endpoint, log, dev })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let listener = match net::listen(&self.endpoint).await {
            Ok(l) => l,
            Err(e) => {
                self.log.log(
                    "error",
                    "modbus TCP server init",
                    &format!("failed to start the server: {e}"),
                );
                tracing::error!("modbus-server: {e}");
                return;
            }
        };
        let (dev, log) = (Arc::new(self.dev), self.log);
        let on_conn = |stream: TcpStream| {
            tokio::spawn(serve(
                stream,
                dev.clone(),
                store.clone(),
                log.clone(),
                stop.clone(),
            ));
        };
        let report = |e: &std::io::Error| {
            log.log(
                "error",
                "modbus accept",
                &format!("accept failed: {e}; retrying"),
            )
        };
        net::accept_loop(&listener, &stop, on_conn, report).await;
    }
}

/// Serve one client until it goes, misbehaves or the device stops. The socket closes when this returns.
async fn serve(
    mut stream: TcpStream,
    dev: Arc<Device>,
    store: Store,
    log: Arc<EventLog>,
    stop: CancellationToken,
) {
    net::keepalive(&stream);
    let mut conn = Conn::new();
    let mut buf = [0u8; 512];
    loop {
        let n = tokio::select! {
            _ = stop.cancelled() => break,
            r = stream.read(&mut buf) => match r { Ok(0) | Err(_) => break, Ok(n) => n },
        };
        match conn.feed(&dev, &store, &log, &buf[..n]) {
            Err(Close(why)) => {
                log.log(
                    "error",
                    "modbus connection",
                    &format!("closing the connection: {why}"),
                );
                break;
            }
            Ok(bytes) if bytes.is_empty() => {}
            Ok(bytes) => {
                if !matches!(
                    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(&bytes)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> Tags {
        let mut t = Tags::default();
        t.add_internal("bi", Kind::Binary).unwrap();
        t.add_internal("an", Kind::Analog).unwrap();
        t
    }

    fn parse(inner: &str, opts: &Opts) -> Result<Server, ConfigError> {
        let xml = format!(
            "<modbus-server><endpoint>tcp://127.0.0.1:1</endpoint><event-logging>/dev/null</event-logging>{inner}</modbus-server>"
        );
        let doc = roxmltree::Document::parse(&xml).unwrap();
        Server::parse(doc.root_element(), &mut tags(), opts)
    }

    fn err(inner: &str) -> String {
        parse(inner, &Opts::default()).err().expect("an error").0
    }

    fn reg(addr: u16, extra: &str) -> String {
        format!(
            "<holding-register><address>{addr}</address><tag>an</tag>{extra}</holding-register>"
        )
    }

    #[test]
    fn the_four_tables_are_read_from_their_elements() {
        let s = parse(
            "<coil><address>1</address><tag>bi</tag></coil>
             <discrete-input><address>2</address><tag>bi</tag></discrete-input>
             <holding-register><address>3</address><tag>an</tag></holding-register>
             <input-register><address>4</address><tag>an</tag></input-register>",
            &Opts::default(),
        )
        .unwrap();
        let d = s.device();
        assert_eq!(s.endpoint, "tcp://127.0.0.1:1");
        assert_eq!(d.coils.keys().collect::<Vec<_>>(), [&1]);
        assert_eq!(d.discretes.keys().collect::<Vec<_>>(), [&2]);
        assert_eq!(d.holding.keys().collect::<Vec<_>>(), [&3]);
        assert_eq!(d.input.keys().collect::<Vec<_>>(), [&4]);
        assert_eq!(d.holding[&3].tag, "an");
    }

    #[test]
    fn a_range_needs_both_keys_else_the_stock_default() {
        let range = |extra: &str| {
            parse(&reg(3, extra), &Opts::default())
                .unwrap()
                .device()
                .holding[&3]
                .range
        };
        assert_eq!(
            range("<max-value>1000</max-value><min-value>0</min-value>"),
            Range {
                min: 0.0,
                max: 1000.0
            }
        );
        assert_eq!(
            range("<min-value>-5.5</min-value><max-value>7.25</max-value>"),
            Range {
                min: -5.5,
                max: 7.25
            }
        );
        assert_eq!(range("<max-value>1000</max-value>"), DEFAULT_RANGE);
        assert_eq!(range("<min-value>0</min-value>"), DEFAULT_RANGE);
        assert_eq!(range(""), DEFAULT_RANGE);
        assert_eq!(
            DEFAULT_RANGE,
            Range {
                min: -1000.0,
                max: 2000.0
            }
        );
        assert!(
            err(&reg(3, "<max-value>x</max-value><min-value>0</min-value>"))
                .contains("not a valid number")
        );
    }

    #[test]
    fn missing_keys_are_named() {
        let doc = roxmltree::Document::parse("<modbus-server/>").unwrap();
        let e = Server::parse(doc.root_element(), &mut tags(), &Opts::default())
            .err()
            .unwrap();
        assert!(e.0.contains("<modbus-server> has no <endpoint>"), "{e}");
        assert!(err("<coil><tag>bi</tag></coil>").contains("<coil> has no <address>"));
        assert!(err("<coil><address>1</address></coil>").contains("<coil> has no <tag>"));
        assert!(err("<coil><address>70000</address><tag>bi</tag></coil>")
            .contains("not a valid number"));
    }

    #[test]
    fn unknown_tags_are_skipped_wrong_kinds_refused_and_a_later_element_replaces_an_earlier() {
        let skip = "<coil><address>1</address><tag>nope</tag></coil>";
        assert!(parse(skip, &Opts::default())
            .unwrap()
            .device()
            .coils
            .is_empty());
        assert!(parse(skip, &Opts { strict: true }).is_err());
        assert!(err("<coil><address>1</address><tag>an</tag></coil>")
            .contains("is Analog but its mapping needs Binary"));
        assert!(
            err(&reg(3, "").replace("an", "bi")).contains("is Binary but its mapping needs Analog")
        );
        let twice = "<coil><address>1</address><tag>bi</tag></coil><coil><address>1</address><tag>bi</tag></coil>";
        assert_eq!(
            parse(twice, &Opts::default()).unwrap().device().coils.len(),
            1
        );
        let a = format!(
            "{}{}",
            reg(3, "<max-value>1</max-value><min-value>0</min-value>"),
            reg(3, "")
        );
        assert_eq!(
            parse(&a, &Opts::default()).unwrap().device().holding[&3].range,
            DEFAULT_RANGE
        );
    }
}
