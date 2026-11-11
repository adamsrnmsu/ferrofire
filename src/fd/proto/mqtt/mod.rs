//! MQTT client (`<mqtt-client>`, docs/field-device-protocols-plan.md section 2, Tier 2 row 2): publishes the listed tags to a broker
//! (retained, on change and every refresh period) and takes writes on `<prefix>/<device>/<tag>/set`, or speaks Sparkplug B
//! (NBIRTH / NDATA / NDEATH out, NCMD in) when `<sparkplug>` is present. `engine` decides what to publish and validates commands,
//! `sparkplug` is the payload codec; this file is the config and the session around rumqttc: one task, reconnecting with backoff,
//! bounded queues, never touching the scan cycle (it only reads the store and queues writes).

pub mod engine;
pub mod sparkplug;

use super::super::config::{bounded, flag, number, require, text, ConfigError, Opts, Tags};
use super::super::eventlog::EventLog;
use super::super::store::Store;
use super::point::{points, Point};
use super::Module;
use crate::hint::{show, RateLimit};
use engine::{Engine, Out, Scheme};
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
/// The queue-full warning is logged at most this often.
const QUEUE_WARN: Duration = Duration::from_secs(5);

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

/// How to reach the broker.
pub struct Link {
    broker: Broker,
    client_id: String,
    login: Option<(String, String)>,
    tls: Tls,
    queue: usize,
}

pub struct Client {
    link: Link,
    engine: Engine,
    log: Arc<EventLog>,
    queue_warn: RateLimit,
}

/// `tcp://host[:port]` (1883) or `ssl://host[:port]` (8883).
fn broker(ep: &str) -> Result<Broker, ConfigError> {
    parse_broker(ep).ok_or_else(|| {
        ConfigError(format!(
            "<endpoint> `{}`: expected tcp://host:port or ssl://host:port",
            show(ep)
        ))
    })
}

fn parse_broker(ep: &str) -> Option<Broker> {
    let (scheme, rest) = ep.split_once("://")?;
    let tls = match scheme {
        "tcp" => false,
        "ssl" => true,
        _ => return None,
    };
    let (host, port) = host_port(rest, if tls { 8883 } else { 1883 })?;
    Some(Broker {
        host: host.into(),
        port,
        tls,
    })
}

/// `host[:port]`, the host not empty and not a path.
fn host_port(s: &str, default: u16) -> Option<(&str, u16)> {
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (s, default),
    };
    (!host.is_empty() && !host.contains('/')).then_some((host, port))
}

/// A topic level: not empty, and nothing MQTT gives a meaning to.
fn level(what: &str, v: String) -> Result<String, ConfigError> {
    if v.is_empty() || v.contains(['/', '+', '#']) {
        return Err(ConfigError(format!(
            "{what} `{}` is not a topic level (non-empty, no / + #)",
            show(&v)
        )));
    }
    Ok(v)
}

/// The name of the `<field-device>` this element sits in, the default topic level for the device.
fn device_name(n: Node) -> String {
    n.ancestors()
        .find(|a| a.has_tag_name("field-device"))
        .and_then(|d| text(d, "name"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ferrofire".into())
}

fn tls_files(node: Node, broker: &Broker) -> Result<Tls, ConfigError> {
    let tls = Tls {
        ca: text(node, "tls-ca"),
        cert: text(node, "tls-cert"),
        key: text(node, "tls-key"),
    };
    if tls.cert.is_some() != tls.key.is_some() {
        return Err(ConfigError("<tls-cert> and <tls-key> go together".into()));
    }
    match broker.tls && tls.ca.is_none() {
        true => Err(ConfigError("an ssl:// <endpoint> needs <tls-ca> (the CA file that signed the broker's certificate)".into())),
        false => Ok(tls),
    }
}

/// Sparkplug B when `<sparkplug>` is present, else the plain topic scheme.
fn scheme(node: Node, device: &str, qos: u8) -> Result<Scheme, ConfigError> {
    let Some(sp) = crate::fd::config::child(node, "sparkplug") else {
        let prefix = text(node, "topic-prefix").unwrap_or_else(|| "ferrofire".into());
        if prefix.is_empty()
            || prefix.contains(['+', '#'])
            || prefix.starts_with('/')
            || prefix.ends_with('/')
        {
            return Err(ConfigError(format!(
                "<topic-prefix> `{}` is not a topic prefix",
                show(&prefix)
            )));
        }
        return Ok(Scheme::Plain {
            prefix,
            device: device.into(),
            qos,
            retain: flag(node, "retain", true)?,
        });
    };
    Ok(Scheme::Sparkplug {
        group: level("<group-id>", require(sp, "group-id")?)?,
        edge: level(
            "<edge-node-id>",
            text(sp, "edge-node-id").unwrap_or_else(|| device.into()),
        )?,
    })
}

impl Link {
    /// `points` is how many messages one birth holds: the queue must take them all, the status message and the subscription at once.
    fn read(node: Node, device: &str, points: usize) -> Result<Link, ConfigError> {
        let broker = broker(&require(node, "endpoint")?)?;
        let client_id = text(node, "client-id").unwrap_or_else(|| format!("ferrofire-{device}"));
        if client_id.is_empty() {
            return Err(ConfigError("<client-id> is empty".into()));
        }
        Ok(Link {
            login: text(node, "username").map(|u| (u, text(node, "password").unwrap_or_default())),
            tls: tls_files(node, &broker)?,
            queue: bounded(node, "queue", 256.max(points + 8), points + 3..=65535)?,
            broker,
            client_id,
        })
    }
}

impl Module for Client {
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
        let device = level(
            "<device>",
            text(node, "device").unwrap_or_else(|| device_name(node)),
        )?;
        let qos = bounded(node, "qos", 1u8, 0..=2)?;
        let refresh = number::<u64>(node, "refresh")?.unwrap_or(60);
        let points: Vec<Point> = points(node, tags, opts)?
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        if let Some(p) = points.iter().find(|p| p.tag.contains(['+', '#'])) {
            return Err(ConfigError(format!(
                "tag `{}` cannot be a topic level",
                show(&p.tag)
            )));
        }
        Ok(Client {
            link: Link::read(node, &device, points.len())?,
            engine: Engine::new(scheme(node, &device, qos)?, points, refresh * 1000),
            log: Arc::new(EventLog::open(
                text(node, "event-logging").as_deref(),
                "mqtt-client",
            )),
            queue_warn: RateLimit::new(QUEUE_WARN),
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
        .unwrap_or_default()
        .as_millis() as u64
}

impl Client {
    fn say(&self, level: &str, event: &str, msg: &str) {
        self.log.log(level, event, msg);
        tracing::warn!("mqtt-client: {event}: {msg}");
    }

    fn options(&self, will: Out) -> Result<MqttOptions, String> {
        let b = &self.link.broker;
        let mut o = MqttOptions::new(&self.link.client_id, &b.host, b.port);
        o.set_keep_alive(KEEP_ALIVE)
            .set_clean_session(true)
            .set_max_packet_size(MAX_PACKET, MAX_PACKET)
            .set_inflight(self.link.queue.min(100) as u16)
            .set_last_will(LastWill::new(
                will.topic,
                will.payload,
                qos(will.qos),
                will.retain,
            ));
        if let Some((u, p)) = &self.link.login {
            o.set_credentials(u, p);
        }
        if let (true, Some(ca)) = (b.tls, &self.link.tls.ca) {
            let read = |p: &str| std::fs::read(p).map_err(|e| format!("{p}: {e}"));
            let client_auth = match (&self.link.tls.cert, &self.link.tls.key) {
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

    /// Queue `outs` in order. The first one that does not fit means the broker is not keeping up: stop there, and the engine
    /// republishes everything as a new birth later. The log hears of it at most once every `QUEUE_WARN`.
    fn send_all(&mut self, c: &AsyncClient, outs: Vec<Out>) {
        for out in outs {
            if c.try_publish(out.topic, qos(out.qos), out.retain, out.payload)
                .is_err()
            {
                self.engine.lost();
                if self.queue_warn.allow(Instant::now()) {
                    self.say(
                        "warn",
                        "mqtt queue",
                        "publish queue full; a new birth follows",
                    );
                }
                return;
            }
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
        let (client, mut events) = AsyncClient::new(opts, self.link.queue);
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
                    let outs = self.engine.tick(store, now, unix_ms());
                    self.send_all(&client, outs);
                }
                ev = events.poll() => match ev {
                    Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                        up = true;
                        for (t, q) in self.engine.subscriptions() {
                            let _ = client.try_subscribe(t, qos(q));
                        }
                    }
                    Ok(Event::Incoming(Incoming::Publish(p))) => {
                        // a retained command was left on the broker by someone, not sent now: it would be obeyed again on every reconnect
                        let lines = if p.retain {
                            vec![format!("retained message on `{}` ignored", show(&p.topic))]
                        } else {
                            self.engine.message(&p.topic, &p.payload, store)
                        };
                        for line in lines {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    fn tags() -> Tags {
        let mut t = Tags::default();
        t.add_internal("lvl", Kind::Analog).unwrap();
        t.add_internal("pump", Kind::Binary).unwrap();
        t
    }

    fn parse_in(device: &str, inner: &str, opts: &Opts) -> Result<Client, ConfigError> {
        let xml = format!(
            "<field-device>{device}<comms><mqtt-client><endpoint>tcp://127.0.0.1:1883</endpoint><event-logging>/dev/null</event-logging>{inner}</mqtt-client></comms></field-device>"
        );
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let node = doc
            .descendants()
            .find(|n| n.has_tag_name("mqtt-client"))
            .unwrap();
        Client::parse(node, &mut tags(), opts)
    }

    fn parse(inner: &str) -> Result<Client, ConfigError> {
        parse_in("<name>rtu-1</name>", inner, &Opts::default())
    }

    fn err(inner: &str) -> String {
        parse(inner).err().expect("an error").0
    }

    #[test]
    fn endpoints_are_tcp_or_ssl_with_default_ports() {
        let b = |h: &str, port, tls| Broker {
            host: h.into(),
            port,
            tls,
        };
        assert_eq!(
            broker("tcp://10.1.1.60").unwrap(),
            b("10.1.1.60", 1883, false)
        );
        assert_eq!(
            broker("tcp://10.1.1.60:1884").unwrap(),
            b("10.1.1.60", 1884, false)
        );
        assert_eq!(
            broker("ssl://broker.lab").unwrap(),
            b("broker.lab", 8883, true)
        );
        assert_eq!(
            broker("ssl://broker.lab:9").unwrap(),
            b("broker.lab", 9, true)
        );
        for bad in [
            "10.1.1.60",
            "http://h",
            "tcp://",
            "tcp://:1883",
            "tcp://h:x",
            "tcp://h:70000",
            "tcp://h/x",
            "mqtt://h",
        ] {
            assert!(
                broker(bad)
                    .unwrap_err()
                    .0
                    .contains("expected tcp://host:port or ssl://host:port"),
                "{bad}"
            );
        }
    }

    #[test]
    fn defaults_follow_the_device() {
        let mut c = parse("<point><tag>lvl</tag></point>").unwrap();
        assert_eq!(
            c.link.broker,
            Broker {
                host: "127.0.0.1".into(),
                port: 1883,
                tls: false
            }
        );
        assert_eq!(
            (c.link.client_id.as_str(), c.link.queue, &c.link.login),
            ("ferrofire-rtu-1", 256, &None)
        );
        assert_eq!(c.link.tls, Tls::default());
        let will = c.engine.begin_session();
        assert_eq!(will.topic, "ferrofire/rtu-1/status");
        let no_device = parse_in("", "", &Opts::default()).unwrap();
        assert_eq!(no_device.link.client_id, "ferrofire-ferrofire");
    }

    #[test]
    fn every_key_is_read() {
        let mut c = parse(
            "<device>d</device><client-id>cid</client-id><topic-prefix>site/a</topic-prefix><qos>2</qos><retain>false</retain>
             <refresh>5</refresh><queue>10</queue><username>u</username><password>p</password>
             <point><tag>pump</tag><writable>true</writable></point>",
        )
        .unwrap();
        assert_eq!((c.link.client_id.as_str(), c.link.queue), ("cid", 10));
        assert_eq!(c.link.login, Some(("u".into(), "p".into())));
        assert_eq!(c.engine.begin_session().topic, "site/a/d/status");
        let s = Store::new(crate::fd::store::Layout {
            points: vec![],
            external: vec![],
            internal: vec![
                ("lvl".into(), crate::model::Value::Analog(1.0)),
                ("pump".into(), crate::model::Value::Binary(true)),
            ],
        });
        let out = c.engine.tick(&s, 0, 0);
        assert_eq!(out[1].topic, "site/a/d/pump");
        assert_eq!((out[1].qos, out[1].retain), (2, false));
        assert_eq!(c.engine.tick(&s, 4999, 0), []);
        assert_eq!(c.engine.tick(&s, 5000, 0).len(), 1, "refresh is in seconds");
        let user_only = parse("<username>u</username>").unwrap();
        assert_eq!(user_only.link.login, Some(("u".into(), String::new())));
    }

    #[test]
    fn sparkplug_is_chosen_by_its_element_and_needs_a_group() {
        let mut c = parse("<sparkplug><group-id>g</group-id></sparkplug>").unwrap();
        assert_eq!(c.engine.begin_session().topic, "spBv1.0/g/NDEATH/rtu-1");
        let mut c =
            parse("<sparkplug><group-id>g</group-id><edge-node-id>e</edge-node-id></sparkplug>")
                .unwrap();
        assert_eq!(c.engine.begin_session().topic, "spBv1.0/g/NDEATH/e");
        assert!(err("<sparkplug/>").contains("<sparkplug> has no <group-id>"));
        assert!(
            err("<sparkplug><group-id>a/b</group-id></sparkplug>").contains("not a topic level")
        );
        assert!(
            err("<sparkplug><group-id>g</group-id><edge-node-id>#</edge-node-id></sparkplug>")
                .contains("<edge-node-id>")
        );
    }

    #[test]
    fn bad_values_are_named() {
        assert!(err("<qos>3</qos>").contains("<qos> 3 is not between 0 and 2"));
        assert!(err("<qos>x</qos>").contains("not a valid number"));
        assert!(err("<queue>0</queue>").contains("<queue> 0"));
        assert!(err("<queue>65536</queue>").contains("<queue> 65536"));
        assert!(parse("<queue>65535</queue>").is_ok());
        assert!(err("<retain>yes</retain>").contains("<retain> `yes` is not true or false"));
        assert!(err("<device>a/b</device>").contains("<device> `a/b` is not a topic level"));
        assert!(err("<device></device>").contains("not a topic level"));
        assert!(err("<client-id></client-id>").contains("<client-id> is empty"));
        for p in ["", "/x", "x/", "a+b", "a#"] {
            assert!(
                err(&format!("<topic-prefix>{p}</topic-prefix>")).contains("not a topic prefix"),
                "{p:?}"
            );
        }
        assert!(err("<tls-cert>c</tls-cert>").contains("go together"));
        assert!(err("<tls-key>k</tls-key>").contains("go together"));
        assert!(parse("<tls-cert>c</tls-cert><tls-key>k</tls-key>").is_ok());
        let e = parse_in("<name>d</name>", "", &Opts::default());
        assert!(e.is_ok());
        let doc =
            roxmltree::Document::parse("<mqtt-client><endpoint>ssl://h</endpoint></mqtt-client>")
                .unwrap();
        let e = Client::parse(doc.root_element(), &mut tags(), &Opts::default())
            .err()
            .unwrap();
        assert!(e.0.contains("ssl:// <endpoint> needs <tls-ca>"), "{e}");
        let doc = roxmltree::Document::parse("<mqtt-client/>").unwrap();
        let e = Client::parse(doc.root_element(), &mut tags(), &Opts::default())
            .err()
            .unwrap();
        assert!(e.0.contains("<mqtt-client> has no <endpoint>"), "{e}");
    }

    #[test]
    fn the_queue_must_hold_one_whole_birth() {
        // a plain birth is the status plus one message per point, and the subscription shares the queue: points + 3
        let points = "<point><tag>lvl</tag></point><point><tag>pump</tag></point>";
        assert!(err(&format!("{points}<queue>4</queue>"))
            .contains("<queue> 4 is not between 5 and 65535"));
        assert!(parse(&format!("{points}<queue>5</queue>")).is_ok());
        assert!(err("<queue>2</queue>").contains("<queue> 2 is not between 3 and 65535"));
        assert!(parse("<queue>3</queue>").is_ok());
        assert_eq!(
            parse(points).unwrap().link.queue,
            256,
            "the default has room"
        );
        let many: String = (0..300)
            .map(|i| format!("<point><tag>t{i}</tag></point>"))
            .collect();
        let mut t = Tags::default();
        for i in 0..300 {
            t.add_internal(&format!("t{i}"), Kind::Analog).unwrap();
        }
        let xml = format!("<mqtt-client><endpoint>tcp://h</endpoint>{many}</mqtt-client>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let c = Client::parse(doc.root_element(), &mut t, &Opts::default()).unwrap();
        assert_eq!(c.link.queue, 308, "the default grows with the point count");
    }

    #[test]
    fn points_are_checked_against_the_tags() {
        let c = parse(
            "<point><tag>lvl</tag><min-value>-1</min-value><max-value>5</max-value></point><point><tag>pump</tag><writable>true</writable></point>",
        )
        .unwrap();
        let mut c = c;
        let s = Store::new(crate::fd::store::Layout {
            points: vec![],
            external: vec![],
            internal: vec![
                ("lvl".into(), crate::model::Value::Analog(1.0)),
                ("pump".into(), crate::model::Value::Binary(true)),
            ],
        });
        assert!(c
            .engine
            .message("ferrofire/rtu-1/lvl/set", b"1", &s)
            .join("")
            .contains("not writable"));
        assert!(c
            .engine
            .message("ferrofire/rtu-1/pump/set", b"0", &s)
            .is_empty());
        assert!(err(
            "<point><tag>lvl</tag><min-value>5</min-value><max-value>-1</max-value></point>"
        )
        .contains("<min-value> is above"));
        assert!(parse(
            "<point><tag>lvl</tag><min-value>5</min-value><max-value>5</max-value></point>"
        )
        .is_ok());
        assert!(
            err("<point><tag>lvl</tag></point><point><tag>lvl</tag></point>")
                .contains("listed twice")
        );
        assert!(err("<point><tag>lvl</tag><writable>1</writable></point>").contains("<writable>"));
        assert!(err("<point/>").contains("<point> has no <tag>"));
        let skipped = parse("<point><tag>nope</tag></point>").unwrap();
        assert!(skipped.engine.subscriptions().len() == 1);
        let strict = parse_in(
            "<name>d</name>",
            "<point><tag>nope</tag></point>",
            &Opts { strict: true },
        );
        assert!(strict.err().unwrap().0.contains("unknown tag `nope`"));
    }

    #[test]
    fn a_tag_that_cannot_be_a_topic_level_is_refused() {
        let mut t = tags();
        t.add_internal("a+b", Kind::Analog).unwrap();
        let doc = roxmltree::Document::parse(
            "<mqtt-client><endpoint>tcp://h</endpoint><point><tag>a+b</tag></point></mqtt-client>",
        )
        .unwrap();
        let e = Client::parse(doc.root_element(), &mut t, &Opts::default())
            .err()
            .unwrap();
        assert!(e.0.contains("cannot be a topic level"), "{e}");
    }

    #[test]
    fn the_clock_is_unix_milliseconds() {
        assert!(unix_ms() > 1_700_000_000_000, "{}", unix_ms());
    }

    #[test]
    fn the_pause_doubles_to_a_cap_and_resets_after_a_good_session() {
        let s = Duration::from_secs;
        assert_eq!(backoff(s(1), false), s(2));
        assert_eq!(backoff(s(2), false), s(4));
        assert_eq!(backoff(s(16), false), s(30));
        assert_eq!(backoff(s(30), false), s(30));
        assert_eq!(backoff(s(30), true), s(1));
        assert_eq!(qos(0), QoS::AtMostOnce);
        assert_eq!(qos(1), QoS::AtLeastOnce);
        assert_eq!(qos(2), QoS::ExactlyOnce);
    }

    #[test]
    fn connect_options_carry_the_will_login_and_limits() {
        let mut c = parse("<username>u</username><password>p</password><client-id>cid</client-id>")
            .unwrap();
        let will = c.engine.begin_session();
        let o = c.options(will).unwrap();
        assert_eq!(o.client_id(), "cid");
        assert_eq!(o.broker_address(), ("127.0.0.1".to_string(), 1883));
        assert_eq!(o.keep_alive(), KEEP_ALIVE);
        assert!(o.clean_session());
        assert_eq!(o.max_packet_size(), MAX_PACKET);
        assert_eq!(o.inflight(), 100);
        let mut small = parse("<queue>5</queue>").unwrap();
        let will = small.engine.begin_session();
        assert_eq!(
            small.options(will).unwrap().inflight(),
            5,
            "never more unacknowledged than the queue holds"
        );
        let w = o.last_will().unwrap();
        assert_eq!(
            (w.topic.as_str(), &w.message[..], w.qos, w.retain),
            (
                "ferrofire/rtu-1/status",
                &b"offline"[..],
                QoS::AtLeastOnce,
                true
            )
        );
        assert_eq!(
            o.credentials().map(|l| (l.username, l.password)),
            Some(("u".into(), "p".into()))
        );
    }

    #[test]
    fn unreadable_tls_files_are_an_error_naming_the_file() {
        let doc = roxmltree::Document::parse("<mqtt-client><endpoint>ssl://h</endpoint><tls-ca>/nonexistent/ca.pem</tls-ca></mqtt-client>").unwrap();
        let mut c = Client::parse(doc.root_element(), &mut tags(), &Opts::default()).unwrap();
        let will = c.engine.begin_session();
        assert!(c
            .options(will)
            .err()
            .unwrap()
            .contains("/nonexistent/ca.pem"));
        let doc = roxmltree::Document::parse("<mqtt-client><endpoint>ssl://h</endpoint><tls-ca>/etc/hostname</tls-ca><tls-cert>/nonexistent/c</tls-cert><tls-key>/etc/hostname</tls-key></mqtt-client>").unwrap();
        let mut c = Client::parse(doc.root_element(), &mut tags(), &Opts::default()).unwrap();
        let will = c.engine.begin_session();
        assert!(c.options(will).err().unwrap().contains("/nonexistent/c"));
    }
}
