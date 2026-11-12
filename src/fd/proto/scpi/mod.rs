//! SCPI instrument (`<scpi-server>`, docs/field-device-scpi.md): the field device presents its tags as a SCPI-99 instrument on
//! a raw TCP socket (5025, newline-terminated ASCII, the socket protocol of LXI instruments), HiSLIP (4880, `hislip`) and VXI-11
//! (portmapper and core channel, `vxi11`). `device` is the I/O-free state machine (status registers, error queue, tag routes, one
//! connection's line buffer) all three feed, `crate::scpi::syntax` the message text; this file is the config and the sockets of the
//! raw protocol: one task per connection, a connection limit shared by all three transports, an idle timeout.

pub mod device;
pub mod hislip;
pub mod vxi11;

use super::super::config::{bounded, child, number, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::net;
use super::super::store::Store;
use super::point::{points, Point};
use super::{device_name, Module};
use crate::hint::show;
use crate::model::Kind;
use crate::scpi::syntax::is_mnemonic;
use device::{Instrument, Route, Scope, Session, Shared};
use roxmltree::Node;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// A reply that cannot be written within this long ends the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_ENDPOINT: &str = "tcp://0.0.0.0:5025";
const DEFAULT_HISLIP: &str = "tcp://0.0.0.0:4880";
const DEFAULT_VXI11_CORE: &str = "tcp://0.0.0.0:4881";
/// The key value that turns a transport off.
const NONE: &str = "none";

pub struct Server {
    endpoint: String,
    log: Arc<EventLog>,
    ins: Instrument,
    max_connections: usize,
    /// A client silent for this long is dropped; `Duration::MAX` for never.
    idle: Duration,
    scope: Scope,
    /// HiSLIP's endpoint; `None` when it is off.
    hislip: Option<String>,
    /// VXI-11's portmapper and core channel endpoints; `None` when it is off.
    vxi11: Option<(String, String)>,
}

/// An endpoint key: its text, `default` when absent, `None` when it says `none`.
fn transport(node: Node, key: &str, default: &str) -> Option<String> {
    let v = text(node, key).unwrap_or_else(|| default.into());
    (v != NONE).then_some(v)
}

/// The portmapper's port is 111 for a process that may bind it, else 1111 (a client then has to be told the port).
fn default_portmapper() -> String {
    let port = if crate::sys::getuid() == 0 { 111 } else { 1111 };
    format!("tcp://0.0.0.0:{port}")
}

fn status_scope(node: Node) -> Result<Scope, ConfigError> {
    match text(node, "status-scope").as_deref() {
        None | Some("instrument") => Ok(Scope::Instrument),
        Some("connection") => Ok(Scope::Connection),
        Some(v) => Err(ConfigError(format!(
            "<status-scope> `{}` is not instrument or connection",
            show(v)
        ))),
    }
}

/// The mnemonic a tag gets in the default paths: letters, digits and `_` upper-cased, anything else becomes `_`, and a name that
/// would not start with a letter gets a leading `T`. Two tags that end up the same are a config error.
pub fn tag_mnemonic(tag: &str) -> String {
    let m: String = tag
        .chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c.to_ascii_uppercase(),
            false => '_',
        })
        .collect();
    match m.starts_with(|c: char| c.is_ascii_alphabetic()) {
        true => m,
        false => format!("T{m}"),
    }
}

/// `MEASure:VOLTage:BUS1` as its mnemonics, as written (the upper-case part of each is its short form).
fn path_of(key: &str, t: &str) -> Result<Vec<String>, ConfigError> {
    let path: Vec<String> = t.split(':').map(str::to_string).collect();
    match path.iter().all(|m| is_mnemonic(m)) {
        true => Ok(path),
        false => Err(ConfigError(format!(
            "<{key}> `{}` is not an SCPI path (mnemonics of letters, digits and _, joined with :)",
            show(t)
        ))),
    }
}

/// The path text of `<key>` when the point has it, else `default`.
fn path_or(n: Node, key: &str, default: Vec<String>) -> Result<Vec<String>, ConfigError> {
    text(n, key).map_or(Ok(default), |t| path_of(key, &t))
}

fn paths(n: Node, p: &Point) -> Result<(Vec<String>, Option<Vec<String>>), ConfigError> {
    let name = tag_mnemonic(&p.tag);
    let kind = match p.kind {
        Kind::Analog => "SCALar",
        Kind::Binary => "BINary",
    };
    let read = path_or(
        n,
        "read-path",
        vec!["MEASure".into(), kind.into(), name.clone()],
    )?;
    match (p.writable, child(n, "write-path").is_some()) {
        (true, _) => Ok((
            read,
            Some(path_or(n, "write-path", vec!["SOURce".into(), name])?),
        )),
        (false, false) => Ok((read, None)),
        (false, true) => Err(ConfigError(format!(
            "<point> `{}` has a <write-path> but is not <writable>",
            show(&p.tag)
        ))),
    }
}

/// Every route's paths must differ from every other's (as the mnemonics a client may send) and stay out of SYSTem.
fn distinct(routes: &[Route]) -> Result<(), ConfigError> {
    let all: Vec<(&Route, &Vec<String>)> = routes
        .iter()
        .flat_map(|r| {
            std::iter::once(&r.read)
                .chain(r.write.as_ref())
                .map(move |p| (r, p))
        })
        .collect();
    let spell = |p: &[String]| p.iter().map(|m| m.to_ascii_uppercase()).collect::<Vec<_>>();
    for (i, (ra, a)) in all.iter().enumerate() {
        if device::path_matches(&[String::from("SYSTem")], &spell(&a[..1])) {
            return Err(ConfigError(format!(
                "the path of tag `{}` starts with SYSTem, which the instrument keeps for itself",
                show(&ra.point.tag)
            )));
        }
        for (rb, b) in &all[i + 1..] {
            if device::path_matches(a, &spell(b)) || device::path_matches(b, &spell(a)) {
                return Err(ConfigError(format!(
                    "tags `{}` and `{}` have the same SCPI path `{}` (give one a <read-path> / <write-path>)",
                    show(&ra.point.tag),
                    show(&rb.point.tag),
                    a.join(":")
                )));
            }
        }
    }
    Ok(())
}

/// An `*IDN?` field: it is comma-separated, so no comma, and one printable line.
fn idn_field(n: Node, key: &str, default: String) -> Result<String, ConfigError> {
    let v = text(n, key).unwrap_or(default);
    match !v.is_empty() && !v.contains(',') && !v.chars().any(char::is_control) {
        true => Ok(v),
        false => Err(ConfigError(format!(
            "<{key}> `{}` must not be empty, hold a comma or a control character",
            show(&v)
        ))),
    }
}

impl Server {
    pub fn instrument(&self) -> &Instrument {
        &self.ins
    }
}

/// The routes of the `<point>` elements, checked against each other.
fn routes(node: Node, tags: &Tags, opts: &Opts) -> Result<Vec<Route>, ConfigError> {
    let routes = points(node, tags, opts)?
        .into_iter()
        .map(|(n, point)| {
            let (read, write) = paths(n, &point)?;
            let default = number::<f64>(n, "default-value")?;
            Ok(Route {
                point,
                read,
                write,
                default,
            })
        })
        .collect::<Result<Vec<_>, ConfigError>>()?;
    distinct(&routes)?;
    Ok(routes)
}

/// The `*IDN?` response: manufacturer, model, serial and the ferrofire version.
fn idn(node: Node) -> Result<String, ConfigError> {
    Ok([
        idn_field(node, "manufacturer", "ferrofire".into())?,
        idn_field(node, "model", device_name(node))?,
        idn_field(node, "serial", "0".into())?,
        env!("CARGO_PKG_VERSION").to_string(),
    ]
    .join(","))
}

impl Module for Server {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let ins = Instrument {
            idn: idn(node)?,
            routes: routes(node, tags, opts)?,
            max_line: bounded(node, "max-line", 1024usize, 64..=65536)?,
        };
        let idle = bounded(node, "idle-timeout", 300u64, 0..=86400)?;
        Ok(Server {
            endpoint: text(node, "endpoint").unwrap_or_else(|| DEFAULT_ENDPOINT.into()),
            log: Arc::new(EventLog::open(
                text(node, "event-logging").as_deref(),
                "scpi-server",
            )),
            max_connections: bounded(node, "max-connections", 16usize, 1..=4096)?,
            idle: match idle {
                0 => Duration::MAX,
                s => Duration::from_secs(s),
            },
            ins,
            scope: status_scope(node)?,
            hislip: transport(node, "hislip-endpoint", DEFAULT_HISLIP),
            vxi11: transport(node, "vxi11-endpoint", &default_portmapper()).map(|pm| {
                (
                    pm,
                    text(node, "vxi11-core-endpoint").unwrap_or_else(|| DEFAULT_VXI11_CORE.into()),
                )
            }),
        })
    }

    async fn run(self, store: Store, stop: CancellationToken) {
        let (ins, log) = (Arc::new(self.ins), self.log);
        let tx = Transports {
            permits: Arc::new(Semaphore::new(self.max_connections)),
            max: self.max_connections,
            log,
            stop: stop.clone(),
        };
        let global = device::shared();
        let raw = {
            let (ins, store, stop, scope, global) = (
                ins.clone(),
                store.clone(),
                stop.clone(),
                self.scope,
                global.clone(),
            );
            let idle = self.idle;
            tx.accept(&self.endpoint, "scpi", move |s| {
                let (ins, store, stop) = (ins.clone(), store.clone(), stop.clone());
                let session = Session::sharing(scope.status(&global));
                async move { serve(s, session, &ins, &store, idle, &stop).await }
            })
        };
        let hislip = self.hislip.as_deref().map(|ep| {
            let srv = Arc::new(hislip::Server {
                ins: ins.clone(),
                store: store.clone(),
                hub: hislip::Hub::new(self.scope, global.clone()),
                idle: self.idle,
                stop: stop.clone(),
            });
            tx.accept(ep, "hislip", move |s| {
                let srv = srv.clone();
                async move { srv.serve(s).await }
            })
        });
        let vxi11 = self.vxi11.as_ref().map(|(pm, core)| {
            tx.vxi11(
                pm,
                core,
                (ins.clone(), store.clone()),
                (self.scope, global.clone()),
                self.idle,
            )
        });
        tokio::join!(
            raw,
            async {
                if let Some(h) = hislip {
                    h.await
                }
            },
            async {
                if let Some(v) = vxi11 {
                    v.await
                }
            }
        );
    }
}

/// What every transport's listener shares: the connection limit, the event log and the stop signal.
struct Transports {
    permits: Arc<Semaphore>,
    /// The size of `permits`, which VXI-11 also limits its links to.
    max: usize,
    log: Arc<EventLog>,
    stop: CancellationToken,
}

impl Transports {
    /// Bind `ep`; a failure is logged and the other transports go on.
    async fn bind(&self, ep: &str, what: &str) -> Option<TcpListener> {
        match net::listen(ep).await {
            Ok(l) => Some(l),
            Err(e) => {
                self.log.log(
                    "error",
                    &format!("{what} server init"),
                    &format!("failed to start the server: {e}"),
                );
                tracing::error!("scpi-server ({what}): {e}");
                None
            }
        }
    }

    /// Accept on `ep` and run `serve` for each connection, within the connection limit: at it a new client is accepted and closed
    /// at once (it sees a reset), the others keep their service.
    async fn accept<F, Fut>(&self, ep: &str, what: &str, serve: F)
    where
        F: Fn(TcpStream) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if let Some(l) = self.bind(ep, what).await {
            self.accept_on(&l, what, serve).await;
        }
    }

    async fn accept_on<F, Fut>(&self, l: &TcpListener, what: &str, serve: F)
    where
        F: Fn(TcpStream) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let on_conn = |stream: TcpStream| {
            if let Ok(permit) = self.permits.clone().try_acquire_owned() {
                let fut = serve(stream);
                tokio::spawn(async move {
                    fut.await;
                    drop(permit);
                });
            }
        };
        let report = |e: &std::io::Error| {
            self.log.log(
                "error",
                &format!("{what} accept"),
                &format!("accept failed: {e}; retrying"),
            )
        };
        net::accept_loop(l, &self.stop, on_conn, report).await;
    }

    /// VXI-11: the core channel on `core`, the portmapper on `pm` (TCP and UDP) telling clients where it is.
    async fn vxi11(
        &self,
        pm: &str,
        core: &str,
        (ins, store): (Arc<Instrument>, Store),
        (scope, global): (Scope, Shared),
        idle: Duration,
    ) {
        let Some(core_l) = self.bind(core, "vxi11 core").await else {
            return;
        };
        let port = core_l.local_addr().map_or(0, |a| a.port());
        let svc = Arc::new(vxi11::Vxi11::new(
            ins,
            store,
            (scope, global),
            port,
            self.max,
        ));
        let serve = |svc: Arc<vxi11::Vxi11>, stop: CancellationToken| {
            move |s: TcpStream| {
                let (svc, stop) = (svc.clone(), stop.clone());
                async move { vxi11::serve_tcp(s, &svc, idle, &stop).await }
            }
        };
        let portmapper = async {
            if let Some(l) = self.bind(pm, "vxi11 portmapper").await {
                self.accept_on(
                    &l,
                    "vxi11 portmapper",
                    serve(svc.clone(), self.stop.clone()),
                )
                .await;
            }
        };
        let udp = async {
            if let Some(sock) = self.udp(pm).await {
                vxi11::serve_udp(sock, &svc, &self.stop).await;
            }
        };
        tokio::join!(
            self.accept_on(&core_l, "vxi11 core", serve(svc.clone(), self.stop.clone())),
            portmapper,
            udp
        );
    }

    /// The portmapper's UDP socket: the same address as its TCP listener.
    async fn udp(&self, ep: &str) -> Option<UdpSocket> {
        let bound = match net::parse(ep) {
            Ok(net::Endpoint::Tcp(addr)) => UdpSocket::bind(addr).await.map_err(|e| e.to_string()),
            _ => Err(format!("endpoint `{}` is not tcp://ip:port", show(ep))),
        };
        bound
            .map_err(|e| {
                self.log.log(
                    "error",
                    "vxi11 portmapper init",
                    &format!("no UDP portmapper: {e}"),
                );
            })
            .ok()
    }
}

/// Serve one client until it goes, goes quiet or the device stops. The socket closes when this returns.
async fn serve(
    mut stream: TcpStream,
    mut session: Session,
    ins: &Instrument,
    store: &Store,
    idle: Duration,
    stop: &CancellationToken,
) {
    net::keepalive(&stream);
    let mut buf = [0u8; 4096];
    loop {
        let read = tokio::select! {
            _ = stop.cancelled() => return,
            r = tokio::time::timeout(idle, stream.read(&mut buf)) => r,
        };
        let (out, last) = match read {
            Ok(Ok(n)) if n > 0 => (session.feed(ins, store, &buf[..n]), false),
            Ok(Ok(_)) => (session.eof(ins, store), true),
            _ => return,
        };
        let sent = tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(&out)).await;
        if last || !matches!(sent, Ok(Ok(()))) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    fn tags() -> Tags {
        let mut t = Tags::default();
        t.add_internal("level", Kind::Analog).unwrap();
        t.add_internal("pump.run", Kind::Binary).unwrap();
        t.add_internal("1st-stage", Kind::Analog).unwrap();
        t.add_internal("Pump_Run", Kind::Binary).unwrap();
        t
    }

    fn parse(inner: &str, opts: &Opts) -> Result<Server, ConfigError> {
        let xml = format!("<field-device><name>rtu-9</name><comms><scpi-server>{inner}</scpi-server></comms></field-device>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let node = doc
            .descendants()
            .find(|n| n.has_tag_name("scpi-server"))
            .unwrap();
        Server::parse(node, &mut tags(), opts)
    }

    fn err(inner: &str) -> String {
        parse(inner, &Opts::default()).err().expect("an error").0
    }

    fn point(tag: &str, extra: &str) -> String {
        format!("<point><tag>{tag}</tag>{extra}</point>")
    }

    #[test]
    fn tags_become_mnemonics() {
        assert_eq!(tag_mnemonic("level"), "LEVEL");
        assert_eq!(tag_mnemonic("pump.run"), "PUMP_RUN");
        assert_eq!(tag_mnemonic("load-1_bus-1.mw"), "LOAD_1_BUS_1_MW");
        assert_eq!(tag_mnemonic("1st"), "T1ST");
        assert_eq!(tag_mnemonic("_x"), "T_X");
        assert_eq!(tag_mnemonic("é"), "T_");
        assert!(is_mnemonic(&tag_mnemonic("a b/c")));
    }

    #[test]
    fn defaults_and_every_key_are_read() {
        let s = parse("", &Opts::default()).unwrap();
        assert_eq!(s.endpoint, "tcp://0.0.0.0:5025");
        assert_eq!(
            s.ins.idn,
            format!("ferrofire,rtu-9,0,{}", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!((s.max_connections, s.ins.max_line), (16, 1024));
        assert_eq!(s.idle, Duration::from_secs(300));
        assert!(s.ins.routes.is_empty());
        assert_eq!(s.scope, Scope::Instrument);
        assert_eq!(s.hislip.as_deref(), Some("tcp://0.0.0.0:4880"));
        let pm = if crate::sys::getuid() == 0 { 111 } else { 1111 };
        assert_eq!(
            s.vxi11,
            Some((format!("tcp://0.0.0.0:{pm}"), "tcp://0.0.0.0:4881".into()))
        );
        let s = parse(
            "<endpoint>tcp://127.0.0.1:1</endpoint><manufacturer>Acme</manufacturer><model>M1</model><serial>S/9</serial>
             <max-connections>3</max-connections><max-line>200</max-line><idle-timeout>7</idle-timeout><event-logging>/dev/null</event-logging>",
            &Opts::default(),
        )
        .unwrap();
        assert_eq!(s.endpoint, "tcp://127.0.0.1:1");
        assert_eq!(
            s.ins.idn,
            format!("Acme,M1,S/9,{}", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            (s.max_connections, s.ins.max_line, s.idle),
            (3, 200, Duration::from_secs(7))
        );
        assert_eq!(
            parse("<idle-timeout>0</idle-timeout>", &Opts::default())
                .unwrap()
                .idle,
            Duration::MAX
        );
        assert_eq!(s.instrument().idn, s.ins.idn);
    }

    #[test]
    fn transports_and_the_status_scope_are_configured_or_switched_off() {
        let s = parse(
            "<status-scope>connection</status-scope><hislip-endpoint>tcp://127.0.0.1:4</hislip-endpoint>
             <vxi11-endpoint>tcp://127.0.0.1:5</vxi11-endpoint><vxi11-core-endpoint>tcp://127.0.0.1:6</vxi11-core-endpoint>",
            &Opts::default(),
        )
        .unwrap();
        assert_eq!(s.scope, Scope::Connection);
        assert_eq!(s.hislip.as_deref(), Some("tcp://127.0.0.1:4"));
        assert_eq!(
            s.vxi11,
            Some(("tcp://127.0.0.1:5".into(), "tcp://127.0.0.1:6".into()))
        );
        let s = parse(
            "<status-scope>instrument</status-scope><hislip-endpoint>none</hislip-endpoint><vxi11-endpoint>none</vxi11-endpoint>",
            &Opts::default(),
        )
        .unwrap();
        assert_eq!(
            (s.scope, s.hislip, s.vxi11),
            (Scope::Instrument, None, None)
        );
        assert!(err("<status-scope>session</status-scope>")
            .contains("<status-scope> `session` is not instrument or connection"));
    }

    #[test]
    fn bad_values_are_named() {
        for (inner, want) in [
            (
                "<max-connections>0</max-connections>",
                "<max-connections> 0 is not between 1 and 4096",
            ),
            (
                "<max-connections>4097</max-connections>",
                "is not between 1 and 4096",
            ),
            (
                "<max-line>63</max-line>",
                "<max-line> 63 is not between 64 and 65536",
            ),
            ("<max-line>65537</max-line>", "is not between 64 and 65536"),
            (
                "<idle-timeout>86401</idle-timeout>",
                "<idle-timeout> 86401 is not between 0 and 86400",
            ),
            (
                "<manufacturer>a,b</manufacturer>",
                "<manufacturer> `a,b` must not be empty",
            ),
            ("<model></model>", "<model> `` must not be empty"),
            ("<serial>a\nb</serial>", "<serial>"),
            ("<max-line>x</max-line>", "not a valid number"),
        ] {
            assert!(err(inner).contains(want), "{inner}: {}", err(inner));
        }
    }

    #[test]
    fn a_point_gets_the_default_paths_unless_it_names_its_own() {
        let s = parse(
            &[
                point("level", ""),
                point("pump.run", "<writable>true</writable><default-value>1</default-value>"),
                point("1st-stage", "<read-path>MEASure:VOLTage:S1</read-path><writable>true</writable><write-path>SOURce:VOLTage:S1</write-path><min-value>-1</min-value><max-value>9</max-value>"),
            ]
            .concat(),
            &Opts::default(),
        )
        .unwrap();
        let r = &s.instrument().routes;
        assert_eq!(r[0].read, ["MEASure", "SCALar", "LEVEL"]);
        assert_eq!(r[0].write, None);
        assert_eq!(r[1].read, ["MEASure", "BINary", "PUMP_RUN"]);
        assert_eq!(
            r[1].write,
            Some(vec!["SOURce".to_string(), "PUMP_RUN".into()])
        );
        assert_eq!((r[0].default, r[1].default), (None, Some(1.0)));
        assert_eq!(r[2].read, ["MEASure", "VOLTage", "S1"]);
        assert_eq!(
            r[2].write,
            Some(vec!["SOURce".to_string(), "VOLTage".into(), "S1".into()])
        );
        assert_eq!((r[2].point.min, r[2].point.max), (Some(-1.0), Some(9.0)));
    }

    #[test]
    fn paths_must_be_scpi_paths_that_are_distinct_and_not_system() {
        for bad in ["", ":A", "A:", "A::B", "1A", "A-B", "A B"] {
            let e = err(&point("level", &format!("<read-path>{bad}</read-path>")));
            assert!(e.contains("is not an SCPI path"), "{bad:?}: {e}");
        }
        assert!(err(&point("level", "<write-path>A</write-path>"))
            .contains("has a <write-path> but is not <writable>"));
        let e = err(&[point("pump.run", ""), point("Pump_Run", "")].concat());
        assert!(
            e.contains("have the same SCPI path `MEASure:BINary:PUMP_RUN`"),
            "{e}"
        );
        let e = err(&[
            point("level", "<read-path>MEASure:X</read-path>"),
            point("pump.run", "<read-path>MEAS:X1</read-path>"),
        ]
        .concat());
        assert!(e.contains("same SCPI path"), "{e}");
        let fine = [
            point("level", "<read-path>MEASure:X</read-path>"),
            point("pump.run", "<read-path>MEASure:Y</read-path>"),
        ]
        .concat();
        assert!(parse(&fine, &Opts::default()).is_ok());
        assert!(err(&point("level", "<read-path>SYSTem:ERRor</read-path>"))
            .contains("starts with SYSTem"));
        assert!(
            err(&point("level", "<read-path>syst:x</read-path>")).contains("starts with SYSTem")
        );
        assert!(
            err(&point("level", "<read-path>SYSTEM</read-path>")).contains("starts with SYSTem")
        );
        let e = err(&point(
            "level",
            "<writable>true</writable><write-path>MEAS:SCAL:LEVEL</write-path>",
        ));
        assert!(
            e.contains("same SCPI path"),
            "a set path must not shadow a read path: {e}"
        );
    }

    #[test]
    fn unknown_tags_are_skipped_and_wrong_numbers_refused() {
        let skip = point("nope", "");
        assert!(parse(&skip, &Opts::default())
            .unwrap()
            .ins
            .routes
            .is_empty());
        assert!(parse(&skip, &Opts { strict: true }).is_err());
        assert!(
            err(&point("level", "<default-value>x</default-value>")).contains("not a valid number")
        );
        assert!(err(&point(
            "level",
            "<min-value>5</min-value><max-value>1</max-value>"
        ))
        .contains("<min-value> is above"));
    }
}
