//! MQTT client (`<mqtt-client>`, docs/field-device-protocols-plan.md section 2, Tier 2 row 2): publishes the listed tags to a broker
//! (retained, on change and every refresh period) and takes writes on `<prefix>/<device>/<tag>/set`, or speaks Sparkplug B
//! (NBIRTH / NDATA / NDEATH out, NCMD in) when `<sparkplug>` is present. `engine` decides what to publish and validates commands,
//! `sparkplug` is the payload codec; this file is the config and the session around rumqttc: one task, reconnecting with backoff,
//! bounded queues, never touching the scan cycle (it only reads the store and queues writes).

pub mod engine;
pub mod sparkplug;

use super::super::config::{number, require, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::store::Store;
use super::Module;
use crate::hint::show;
use engine::{Engine, Out, Point, Scheme};
use roxmltree::Node;
use rumqttc::{
    AsyncClient, Event, EventLoop, Incoming, LastWill, MqttOptions, QoS, TlsConfiguration,
    Transport,
};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// How often the session looks at the store for changes.
const TICK: Duration = Duration::from_millis(100);
/// Reconnect pause: 1 s, doubling to 30 s while the broker stays unreachable.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A packet larger than this (either way) is refused: commands are a few bytes, a Sparkplug birth of thousands of tags is not.
const MAX_PACKET: usize = 1 << 20;
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// How long a clean shutdown may take to flush the offline message.
const GOODBYE: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq)]
pub struct Broker {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

#[derive(Debug, PartialEq, Default)]
pub struct Tls {
    pub ca: Option<String>,
    pub cert: Option<String>,
    pub key: Option<String>,
}

pub struct Client {
    broker: Broker,
    client_id: String,
    login: Option<(String, String)>,
    tls: Tls,
    queue: usize,
    engine: Engine,
    log: Arc<EventLog>,
}

/// `tcp://host[:port]` (1883) or `ssl://host[:port]` (8883).
fn broker(ep: &str) -> Result<Broker, ConfigError> {
    let bad = || {
        ConfigError(format!(
            "<endpoint> `{}`: expected tcp://host:port or ssl://host:port",
            show(ep)
        ))
    };
    let (scheme, rest) = ep.split_once("://").ok_or_else(bad)?;
    let tls = match scheme {
        "tcp" => false,
        "ssl" => true,
        _ => return Err(bad()),
    };
    let default = if tls { 8883 } else { 1883 };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| bad())?),
        None => (rest, default),
    };
    if host.is_empty() || host.contains('/') {
        return Err(bad());
    }
    Ok(Broker {
        host: host.into(),
        port,
        tls,
    })
}

/// A topic level: not empty, and nothing MQTT gives a meaning to.
fn level(what: &str, v: String) -> Result<String, ConfigError> {
    if v.is_empty() || v.contains(['/', '+', '#', '\0']) {
        return Err(ConfigError(format!(
            "{what} `{}` is not a topic level (non-empty, no / + # NUL)",
            show(&v)
        )));
    }
    Ok(v)
}

fn flag(n: Node, key: &str, default: bool) -> Result<bool, ConfigError> {
    match text(n, key).as_deref() {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(v) => Err(ConfigError(format!(
            "<{key}> `{}` is not true or false",
            show(v)
        ))),
    }
}

/// The name of the `<field-device>` this element sits in, the default topic level for the device.
fn device_name(n: Node) -> String {
    n.ancestors()
        .find(|a| a.has_tag_name("field-device"))
        .and_then(|d| text(d, "name"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ferrofire".into())
}

fn points(node: Node, tags: &Tags, opts: &Opts) -> Result<Vec<Point>, ConfigError> {
    let mut out: Vec<Point> = vec![];
    for n in node.children().filter(|c| c.has_tag_name("point")) {
        let tag = require(n, "tag")?;
        let Some(kind) = tags.kind(&tag) else {
            opts.warn(format!("mapping for unknown tag `{}` skipped", show(&tag)))?;
            continue;
        };
        if out.iter().any(|p| p.tag == tag) {
            return Err(ConfigError(format!(
                "<point> tag `{}` is listed twice",
                show(&tag)
            )));
        }
        let (min, max) = (
            number::<f64>(n, "min-value")?,
            number::<f64>(n, "max-value")?,
        );
        if min.zip(max).is_some_and(|(a, b)| a > b) {
            return Err(ConfigError(format!(
                "<point> `{}`: <min-value> is above <max-value>",
                show(&tag)
            )));
        }
        let writable = flag(n, "writable", false)?;
        out.push(Point {
            tag,
            kind,
            writable,
            min,
            max,
        });
    }
    Ok(out)
}

impl Module for Client {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let broker = broker(&require(node, "endpoint")?)?;
        let device = level(
            "<device>",
            text(node, "device").unwrap_or_else(|| device_name(node)),
        )?;
        let client_id = text(node, "client-id").unwrap_or_else(|| format!("ferrofire-{device}"));
        if client_id.is_empty() {
            return Err(ConfigError("<client-id> is empty".into()));
        }
        let login = text(node, "username").map(|u| (u, text(node, "password").unwrap_or_default()));
        let tls = Tls {
            ca: text(node, "tls-ca"),
            cert: text(node, "tls-cert"),
            key: text(node, "tls-key"),
        };
        if tls.cert.is_some() != tls.key.is_some() {
            return Err(ConfigError("<tls-cert> and <tls-key> go together".into()));
        }
        if broker.tls && tls.ca.is_none() {
            return Err(ConfigError("an ssl:// <endpoint> needs <tls-ca> (the CA file that signed the broker's certificate)".into()));
        }
        let qos = number::<u8>(node, "qos")?.unwrap_or(1);
        if qos > 2 {
            return Err(ConfigError(format!("<qos> {qos} is not 0, 1 or 2")));
        }
        let queue = number::<usize>(node, "queue")?.unwrap_or(256);
        if !(1..=65535).contains(&queue) {
            return Err(ConfigError(format!(
                "<queue> {queue} is not between 1 and 65535"
            )));
        }
        let refresh = number::<u64>(node, "refresh")?.unwrap_or(60);
        let scheme = match crate::fd::config::child(node, "sparkplug") {
            Some(sp) => Scheme::Sparkplug {
                group: level("<group-id>", require(sp, "group-id")?)?,
                edge: level(
                    "<edge-node-id>",
                    text(sp, "edge-node-id").unwrap_or_else(|| device.clone()),
                )?,
            },
            None => {
                let prefix = text(node, "topic-prefix").unwrap_or_else(|| "ferrofire".into());
                if prefix.is_empty()
                    || prefix.contains(['+', '#', '\0'])
                    || prefix.starts_with('/')
                    || prefix.ends_with('/')
                {
                    return Err(ConfigError(format!(
                        "<topic-prefix> `{}` is not a topic prefix",
                        show(&prefix)
                    )));
                }
                Scheme::Plain {
                    prefix,
                    device,
                    qos,
                    retain: flag(node, "retain", true)?,
                }
            }
        };
        let points = points(node, tags, opts)?;
        for p in points.iter().filter(|p| p.tag.contains(['+', '#', '\0'])) {
            return Err(ConfigError(format!(
                "tag `{}` cannot be a topic level",
                show(&p.tag)
            )));
        }
        let log = Arc::new(EventLog::open(
            text(node, "event-logging").as_deref(),
            "mqtt-client",
        ));
        Ok(Client {
            broker,
            client_id,
            login,
            tls,
            queue,
            engine: Engine::new(scheme, points, refresh * 1000),
            log,
        })
    }

    async fn run(mut self, store: Store, stop: CancellationToken) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let start = Instant::now();
        let mut pause = BACKOFF_FIRST;
        while !stop.is_cancelled() {
            match self.session(&store, &stop, start).await {
                Ended::Stopped => return,
                Ended::Lost { was_up } => {
                    pause = backoff(pause, was_up);
                    tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(pause) => {} }
                }
            }
        }
    }
}

enum Ended {
    Stopped,
    Lost { was_up: bool },
}

/// The pause after a failed session: back to the first once a session got as far as being connected, else doubled up to the cap.
fn backoff(pause: Duration, was_up: bool) -> Duration {
    if was_up {
        BACKOFF_FIRST
    } else {
        (pause * 2).min(BACKOFF_MAX)
    }
}

fn qos(q: u8) -> QoS {
    match q {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Client {
    fn say(&self, level: &str, event: &str, msg: &str) {
        self.log.log(level, event, msg);
        tracing::warn!("mqtt-client: {event}: {msg}");
    }

    fn options(&self, will: Out) -> Result<MqttOptions, String> {
        let b = &self.broker;
        let mut o = MqttOptions::new(&self.client_id, &b.host, b.port);
        o.set_keep_alive(KEEP_ALIVE)
            .set_clean_session(true)
            .set_max_packet_size(MAX_PACKET, MAX_PACKET)
            .set_last_will(LastWill::new(
                will.topic,
                will.payload,
                qos(will.qos),
                will.retain,
            ));
        if let Some((u, p)) = &self.login {
            o.set_credentials(u, p);
        }
        if let (true, Some(ca)) = (b.tls, &self.tls.ca) {
            let read = |p: &str| std::fs::read(p).map_err(|e| format!("{p}: {e}"));
            let client_auth = match (&self.tls.cert, &self.tls.key) {
                (Some(c), Some(k)) => Some((read(c)?, read(k)?)),
                _ => None,
            };
            o.set_transport(Transport::Tls(TlsConfiguration::Simple {
                ca: read(ca)?,
                alpn: None,
                client_auth,
            }));
        }
        Ok(o)
    }

    /// Queue `out`; a full queue means the broker is not keeping up, so the engine republishes everything as a new birth later.
    fn send(&mut self, c: &AsyncClient, out: Out) {
        if c.try_publish(out.topic, qos(out.qos), out.retain, out.payload)
            .is_err()
        {
            self.engine.lost();
            self.say(
                "warn",
                "mqtt queue",
                "publish queue full; a new birth follows",
            );
        }
    }

    async fn session(&mut self, store: &Store, stop: &CancellationToken, start: Instant) -> Ended {
        let will = self.engine.begin_session();
        let opts = match self.options(will) {
            Ok(o) => o,
            Err(e) => {
                self.say(
                    "error",
                    "mqtt tls",
                    &format!("cannot read the TLS files: {e}"),
                );
                return Ended::Lost { was_up: false };
            }
        };
        let (client, mut events) = AsyncClient::new(opts, self.queue);
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut up = false;
        loop {
            tokio::select! {
                _ = stop.cancelled() => {
                    self.farewell(&client, &mut events, up).await;
                    return Ended::Stopped;
                }
                _ = tick.tick(), if up => {
                    let now = start.elapsed().as_millis() as u64;
                    for out in self.engine.tick(store, now, unix_ms()) {
                        self.send(&client, out);
                    }
                }
                ev = events.poll() => match ev {
                    Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                        up = true;
                        for (t, q) in self.engine.subscriptions() {
                            let _ = client.try_subscribe(t, qos(q));
                        }
                    }
                    Ok(Event::Incoming(Incoming::Publish(p))) => {
                        for line in self.engine.message(&p.topic, &p.payload, store) {
                            self.say("warn", "mqtt command", &line);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.say("error", "mqtt connection", &format!("{e}; reconnecting"));
                        return Ended::Lost { was_up: up };
                    }
                }
            }
        }
    }

    /// Clean shutdown: say offline (a will is only sent when the connection drops), disconnect, and let the eventloop flush both.
    async fn farewell(&mut self, c: &AsyncClient, events: &mut EventLoop, up: bool) {
        if !up {
            return;
        }
        let bye = self.engine.goodbye();
        let _ = c.try_publish(bye.topic, qos(bye.qos), bye.retain, bye.payload);
        let _ = c.try_disconnect();
        let _ = tokio::time::timeout(GOODBYE, async {
            while let Ok(ev) = events.poll().await {
                if matches!(ev, Event::Outgoing(rumqttc::Outgoing::Disconnect)) {
                    break;
                }
            }
        })
        .await;
    }
}
