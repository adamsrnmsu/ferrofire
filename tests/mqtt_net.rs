//! The MQTT client against a real Mosquitto broker started as a child process (`mosquitto` is in the ff-rust image): publishing on
//! change and as a retained state, commands into the store's write queue (and what is refused), the will and the clean goodbye,
//! reconnecting when the broker comes and goes, Sparkplug B birth / data / death / rebirth / writes, and TLS with a client
//! certificate plus a password. An independent client (Paho, mosquitto_pub) and Tahu are in the conformance harness.

#![cfg(feature = "proto-mqtt")]

mod common;

use common::load_as;
use ferrofire::fd::proto::mqtt::sparkplug::{self as sp, Payload};
use ferrofire::fd::proto::mqtt::Client;
use ferrofire::fd::proto::Module;
use ferrofire::fd::store::Store;
use prost::Message;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Broker {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Broker {
    fn start() -> Broker {
        Broker::on(free_port(), "")
    }

    /// A broker on `port` (a restart reuses the port) with `extra` config lines after the listener.
    fn on(port: u16, extra: &str) -> Broker {
        let dir = std::env::temp_dir().join(format!("mqtt-net-{}-{port}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("mosquitto.conf");
        let listener = format!("user root\nlistener {port} 127.0.0.1\n");
        let anon = if extra.contains("allow_anonymous") {
            ""
        } else {
            "allow_anonymous true\n"
        };
        std::fs::write(&conf, format!("{listener}{anon}{extra}\n")).unwrap();
        let child = Command::new("mosquitto")
            .arg("-c")
            .arg(&conf)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("mosquitto on PATH (it is in the ff-rust image)");
        let broker = Broker { child, port, dir };
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return broker;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("mosquitto never listened on {port}");
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Msg {
    topic: String,
    payload: Vec<u8>,
    retain: bool,
}

/// A subscriber that records everything on the topics it asked for, and can publish.
struct Probe {
    client: AsyncClient,
    rx: mpsc::UnboundedReceiver<Msg>,
}

impl Probe {
    async fn on(port: u16, filters: &[&str]) -> Probe {
        Probe::with(
            MqttOptions::new(format!("probe-{}", free_port()), "127.0.0.1", port),
            filters,
        )
        .await
    }

    async fn with(opts: MqttOptions, filters: &[&str]) -> Probe {
        let (client, mut ev) = AsyncClient::new(opts, 100);
        for f in filters {
            client.subscribe(*f, QoS::AtLeastOnce).await.unwrap();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                match ev.poll().await {
                    Ok(Event::Incoming(Incoming::Publish(p))) => {
                        let _ = tx.send(Msg {
                            topic: p.topic,
                            payload: p.payload.to_vec(),
                            retain: p.retain,
                        });
                    }
                    Ok(_) => {}
                    Err(e) => return eprintln!("probe connection ended: {e}"),
                }
            }
        });
        Probe { client, rx }
    }

    /// The first message on `topic` whose payload satisfies `ok`, within 15 s (others are skipped).
    async fn want(&mut self, topic: &str, ok: impl Fn(&[u8]) -> bool) -> Msg {
        let find = async {
            loop {
                let m = self.rx.recv().await.expect("the probe's connection ended");
                if m.topic == topic && ok(&m.payload) {
                    return m;
                }
            }
        };
        timeout(Duration::from_secs(15), find)
            .await
            .unwrap_or_else(|_| panic!("no message on {topic} that matched"))
    }

    async fn text(&mut self, topic: &str, text: &str) -> Msg {
        self.want(topic, |p| p == text.as_bytes()).await
    }

    async fn put(&self, topic: &str, payload: impl Into<Vec<u8>>) {
        self.client
            .publish(topic, QoS::AtLeastOnce, false, payload)
            .await
            .unwrap();
    }

    async fn put_retained(&self, topic: &str, payload: impl Into<Vec<u8>>) {
        self.client
            .publish(topic, QoS::AtLeastOnce, true, payload)
            .await
            .unwrap();
    }
}

fn config(port: u16, inner: &str) -> String {
    format!(
        r#"<SCEPTRE><field-device><name>rtu-1</name><cycle-time>500</cycle-time>
<tags>
 <external-tag><name>level</name><io>I0</io><type>analog</type></external-tag>
 <external-tag><name>pump</name><io>O0</io><type>binary</type></external-tag>
 <external-tag><name>setpoint</name><io>O1</io><type>analog</type></external-tag>
 <external-tag><name>ro</name><io>O2</io><type>analog</type></external-tag>
</tags>
<comms><mqtt-client><endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging><refresh>0</refresh>
 <point><tag>level</tag></point>
 <point><tag>pump</tag><writable>true</writable></point>
 <point><tag>setpoint</tag><writable>true</writable><min-value>0</min-value><max-value>100</max-value></point>
 <point><tag>ro</tag></point>
 {inner}
</mqtt-client></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><analog><id>I0</id><name>plevel</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>ppump</name></binary><analog><id>O1</id><name>psp</name></analog><analog><id>O2</id><name>pro</name></analog></output>
</field-device></SCEPTRE>"#
    )
}

struct Running {
    store: Store,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

fn start(xml: &str) -> Running {
    let (client, store) = load_as::<Client>(xml, "mqtt-client");
    let stop = CancellationToken::new();
    let task = tokio::spawn(client.run(store.clone(), stop.clone()));
    Running { store, stop, task }
}

/// Wait until the provider-side write queue (`pending`) shows `tag`.
async fn queued(store: &Store, tag: &str) -> ferrofire::model::Value {
    for _ in 0..300 {
        if let Some((_, _, v)) = store
            .pending(1)
            .into_iter()
            .chain(store.pending(0))
            .find(|(t, _, _)| t == tag)
        {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{tag} was never queued");
}

use ferrofire::model::Value as V;

#[tokio::test]
async fn tags_are_published_retained_on_change_and_commands_reach_the_write_queue() {
    let b = Broker::start();
    let mut probe = Probe::on(b.port, &["ferrofire/rtu-1/#"]).await;
    let d = start(&config(b.port, ""));
    probe.text("ferrofire/rtu-1/status", "online").await;
    probe.text("ferrofire/rtu-1/level", "0").await;
    probe.text("ferrofire/rtu-1/pump", "false").await;
    d.store.publish("plevel", 3.5, 1);
    probe.text("ferrofire/rtu-1/level", "3.5").await;
    d.store.publish("plevel", 4.25, 2);
    probe.text("ferrofire/rtu-1/level", "4.25").await;
    // a late subscriber gets the last value as a retained message
    let mut late = Probe::on(b.port, &["ferrofire/rtu-1/level"]).await;
    assert!(late.text("ferrofire/rtu-1/level", "4.25").await.retain);

    probe.put("ferrofire/rtu-1/setpoint/set", "99999").await; // above max-value
    probe.put("ferrofire/rtu-1/ro/set", "1").await; // not writable
    probe.put("ferrofire/rtu-1/level/set", "1").await; // not writable
    probe.put("ferrofire/rtu-1/nope/set", "1").await; // unknown tag
    probe.put("ferrofire/rtu-1/pump/set", "maybe").await; // not a boolean
    probe.put("ferrofire/rtu-1/setpoint/set", "nan").await;
    probe.put("ferrofire/rtu-1/pump/set", "true").await; // the one that goes through
    assert_eq!(queued(&d.store, "pump").await, V::Binary(true));
    for t in ["setpoint", "ro", "level"] {
        assert!(!d.store.is_queued(t), "{t} must have been refused");
    }
    probe.put("ferrofire/rtu-1/setpoint/set", "42.5").await;
    assert_eq!(queued(&d.store, "setpoint").await, V::Analog(42.5));
    assert_eq!(
        d.store.get_f64("setpoint"),
        Some(0.0),
        "a command only queues"
    );
    d.stop.cancel();
}

#[tokio::test]
async fn a_clean_stop_says_offline_and_a_dropped_connection_makes_the_broker_send_the_will() {
    let b = Broker::start();
    let mut probe = Probe::on(b.port, &["ferrofire/rtu-1/status"]).await;
    let d = start(&config(b.port, ""));
    probe.text("ferrofire/rtu-1/status", "online").await;
    d.stop.cancel();
    timeout(Duration::from_secs(5), d.task)
        .await
        .expect("stops within 5 s")
        .unwrap();
    assert!(
        probe
            .text("ferrofire/rtu-1/status", "offline")
            .await
            .payload
            == b"offline"
    );

    let d = start(&config(b.port, ""));
    probe.text("ferrofire/rtu-1/status", "online").await;
    d.task.abort(); // the socket closes without a DISCONNECT: only the will can say offline
    let m = probe.text("ferrofire/rtu-1/status", "offline").await;
    assert!(!m.retain || m.payload == b"offline");
    let mut late = Probe::on(b.port, &["ferrofire/rtu-1/status"]).await;
    assert!(
        late.text("ferrofire/rtu-1/status", "offline").await.retain,
        "status is retained"
    );
}

#[tokio::test]
async fn it_connects_when_the_broker_appears_and_again_after_the_broker_restarts() {
    let port = free_port();
    let d = start(&config(port, ""));
    tokio::time::sleep(Duration::from_millis(1500)).await; // refused at least once
    d.store.publish("plevel", 7.0, 1); // the store is usable while the broker is away
    assert_eq!(d.store.get_f64("level"), Some(7.0));
    let b = Broker::on(port, "");
    let mut probe = Probe::on(port, &["ferrofire/rtu-1/#"]).await;
    probe.text("ferrofire/rtu-1/level", "7").await;
    drop(b);
    tokio::time::sleep(Duration::from_millis(500)).await;
    d.store.publish("plevel", 8.0, 2);
    let _b2 = Broker::on(port, "");
    let mut probe = Probe::on(port, &["ferrofire/rtu-1/#"]).await;
    probe.text("ferrofire/rtu-1/level", "8").await;
    d.stop.cancel();
}

#[tokio::test]
async fn a_retained_command_is_never_obeyed_not_even_on_every_reconnect() {
    let b = Broker::start();
    let filters = [
        "ferrofire/rtu-1/status",
        "spBv1.0/waterway/NBIRTH/rtu-1",
        "ferrofire/rtu-1/setpoint/set",
        "spBv1.0/waterway/NCMD/rtu-1",
    ];
    let mut probe = Probe::on(b.port, &filters).await;
    probe
        .put_retained("ferrofire/rtu-1/setpoint/set", "42")
        .await; // left on the broker by someone
    probe
        .put_retained("spBv1.0/waterway/NCMD/rtu-1", sp_write("setpoint", 43.0))
        .await;
    // the broker has stored both once the probe hears them itself; only then is the client's subscription answered with retained copies
    probe.text("ferrofire/rtu-1/setpoint/set", "42").await;
    probe.want("spBv1.0/waterway/NCMD/rtu-1", |_| true).await;
    let d = start(&config(b.port, ""));
    probe.text("ferrofire/rtu-1/status", "online").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !d.store.is_queued("setpoint"),
        "the retained command must be ignored"
    );
    probe.put("ferrofire/rtu-1/setpoint/set", "44").await; // a live one still works
    assert_eq!(queued(&d.store, "setpoint").await, V::Analog(44.0));
    d.stop.cancel();
    let d = start(&config(b.port, SPARKPLUG));
    probe.want("spBv1.0/waterway/NBIRTH/rtu-1", is_nbirth).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !d.store.is_queued("setpoint"),
        "a retained NCMD must be ignored too"
    );
    d.stop.cancel();
}

fn sp_write(name: &str, v: f64) -> Vec<u8> {
    let m = sp::Metric {
        name: Some(name.into()),
        value: Some(sp::Value::Double(v)),
        ..Default::default()
    };
    Payload {
        metrics: vec![m],
        ..Default::default()
    }
    .encode_to_vec()
}

/// A broker that accepts MQTT 3.1.1 connections and then never acknowledges or reads anything again.
fn mute_broker() -> u16 {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 512];
            let _ = s.read(&mut buf); // the CONNECT
            let _ = s.write_all(&[0x20, 2, 0, 0]); // CONNACK, accepted
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                let _ = &s;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_broker_that_stops_acknowledging_fills_the_queue_and_the_log_says_so_once_in_a_while() {
    let port = mute_broker();
    let log = std::env::temp_dir().join(format!("mqtt-net-queue-{}.log", std::process::id()));
    let xml = config(port, "")
        .replace("/dev/null", &log.display().to_string())
        .replace(
            "<refresh>0</refresh>",
            "<refresh>0</refresh><queue>8</queue>",
        );
    let d = start(&xml);
    // QoS 1 publishes pile up unacknowledged: 100 in flight, then the queue (8 here) is full
    for i in 0..400 {
        d.store.publish("plevel", f64::from(i), 1);
        tokio::time::sleep(Duration::from_millis(30)).await;
        if std::fs::read_to_string(&log).is_ok_and(|t| t.contains("publish queue full")) {
            break;
        }
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(text.contains("publish queue full"), "{text}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let n = std::fs::read_to_string(&log)
        .unwrap()
        .matches("publish queue full")
        .count();
    assert!(n <= 2, "the warning is rate limited, saw {n}");
    d.stop.cancel();
    let _ = std::fs::remove_file(&log);
}

const SPARKPLUG: &str =
    "<sparkplug><group-id>waterway</group-id><edge-node-id>rtu-1</edge-node-id></sparkplug>";

fn payload(m: &Msg) -> Payload {
    Payload::decode(m.payload.as_slice()).unwrap()
}

fn is_nbirth(p: &[u8]) -> bool {
    Payload::decode(p).is_ok_and(|p| p.seq == Some(0) && p.metrics.len() > 2)
}

#[tokio::test]
async fn sparkplug_birth_data_commands_rebirth_and_death() {
    let b = Broker::start();
    let mut probe = Probe::on(b.port, &["spBv1.0/waterway/#"]).await;
    let d = start(&config(b.port, SPARKPLUG));
    let birth = payload(&probe.want("spBv1.0/waterway/NBIRTH/rtu-1", is_nbirth).await);
    let names: Vec<_> = birth
        .metrics
        .iter()
        .filter_map(|m| m.name.clone())
        .collect();
    assert_eq!(
        names,
        [
            "bdSeq",
            "Node Control/Rebirth",
            "level",
            "pump",
            "setpoint",
            "ro"
        ]
    );
    assert_eq!(birth.metrics[2].alias, Some(1));
    let bd = birth.metrics[0].value.clone();

    d.store.publish("plevel", 12.5, 1);
    let data = probe
        .want("spBv1.0/waterway/NDATA/rtu-1", |p| {
            Payload::decode(p).is_ok_and(|p| p.seq == Some(1))
        })
        .await;
    let m = &payload(&data).metrics[0];
    assert_eq!(
        (m.alias, m.name.clone(), m.value.clone()),
        (Some(1), None, Some(sp::Value::Double(12.5)))
    );

    let ncmd = |metrics: Vec<sp::Metric>| {
        Payload {
            metrics,
            ..Default::default()
        }
        .encode_to_vec()
    };
    let write = sp::Metric {
        alias: Some(3),
        value: Some(sp::Value::Double(55.0)),
        ..Default::default()
    };
    let too_big = sp::Metric {
        name: Some("setpoint".into()),
        value: Some(sp::Value::Double(1e9)),
        ..Default::default()
    };
    probe
        .put("spBv1.0/waterway/NCMD/rtu-1", ncmd(vec![too_big]))
        .await;
    probe
        .put("spBv1.0/waterway/NCMD/rtu-1", ncmd(vec![write]))
        .await;
    assert_eq!(queued(&d.store, "setpoint").await, V::Analog(55.0));

    let rebirth = sp::Metric {
        name: Some(sp::REBIRTH.into()),
        value: Some(sp::Value::Boolean(true)),
        ..Default::default()
    };
    probe
        .put("spBv1.0/waterway/NCMD/rtu-1", ncmd(vec![rebirth]))
        .await;
    let again = payload(&probe.want("spBv1.0/waterway/NBIRTH/rtu-1", is_nbirth).await);
    assert_eq!(
        again.metrics[0].value, bd,
        "a rebirth keeps the session's bdSeq"
    );

    d.stop.cancel();
    let death = probe.want("spBv1.0/waterway/NDEATH/rtu-1", |_| true).await;
    assert_eq!(
        payload(&death).metrics[0].value,
        bd,
        "the NDEATH names the bdSeq of the NBIRTH"
    );
}

#[tokio::test]
async fn a_lost_sparkplug_session_dies_with_its_bdseq_and_the_next_one_counts_on() {
    let b = Broker::start();
    let mut probe = Probe::on(b.port, &["spBv1.0/waterway/#"]).await;
    let d = start(&config(b.port, SPARKPLUG));
    let first = payload(&probe.want("spBv1.0/waterway/NBIRTH/rtu-1", is_nbirth).await).metrics[0]
        .value
        .clone();
    assert_eq!(first, Some(sp::Value::Long(0)));
    d.task.abort();
    let death = probe.want("spBv1.0/waterway/NDEATH/rtu-1", |_| true).await;
    assert_eq!(payload(&death).metrics[0].value, first);
    let d = start(&config(b.port, SPARKPLUG));
    let second = payload(&probe.want("spBv1.0/waterway/NBIRTH/rtu-1", is_nbirth).await).metrics[0]
        .value
        .clone();
    assert_eq!(
        second,
        Some(sp::Value::Long(0)),
        "a new process starts again at 0"
    );
    d.stop.cancel();
}

fn openssl(dir: &Path, args: &[&str]) {
    let ok = Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .stderr(Stdio::null())
        .status()
        .expect("openssl")
        .success();
    assert!(ok, "openssl {args:?}");
}

/// A throwaway CA, a server certificate for 127.0.0.1 and a client certificate, all PEM in `dir`.
fn pki(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("san.ext"), "subjectAltName=IP:127.0.0.1\n").unwrap();
    std::fs::write(dir.join("cli.ext"), "extendedKeyUsage=clientAuth\n").unwrap();
    openssl(
        dir,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
            "-subj",
            "/CN=testca",
            "-days",
            "2",
        ],
    );
    for (name, ext) in [
        ("srv", &["-extfile", "san.ext"][..]),
        ("cli", &["-extfile", "cli.ext"][..]),
    ] {
        let (key, csr, crt) = (
            format!("{name}.key"),
            format!("{name}.csr"),
            format!("{name}.crt"),
        );
        openssl(
            dir,
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                &key,
                "-out",
                &csr,
                "-subj",
                "/CN=localhost",
            ],
        );
        let mut sign = vec![
            "x509",
            "-req",
            "-in",
            &csr,
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            &crt,
            "-days",
            "2",
        ];
        sign.extend(ext);
        openssl(dir, &sign);
    }
    openssl(
        dir,
        &[
            "pkcs8", "-topk8", "-nocrypt", "-in", "cli.key", "-out", "cli8.key",
        ],
    );
    for f in ["srv.key", "cli.key", "cli8.key"] {
        let _ = Command::new("chmod").arg("644").arg(dir.join(f)).status();
    }
}

#[tokio::test]
async fn tls_with_a_client_certificate_and_a_password() {
    let dir = std::env::temp_dir().join(format!("mqtt-pki-{}", std::process::id()));
    pki(&dir);
    let pw = dir.join("pw");
    assert!(Command::new("mosquitto_passwd")
        .args(["-b", "-c"])
        .arg(&pw)
        .args(["ff", "secret"])
        .status()
        .unwrap()
        .success());
    let d_ = dir.display();
    let b = Broker::on(
        free_port(),
        &format!("allow_anonymous false\npassword_file {d_}/pw\ncafile {d_}/ca.crt\ncertfile {d_}/srv.crt\nkeyfile {d_}/srv.key\nrequire_certificate true\n"),
    );
    let tls = |ca: &str| {
        format!(
            "<tls-ca>{d_}/{ca}</tls-ca><tls-cert>{d_}/cli.crt</tls-cert><tls-key>{d_}/cli8.key</tls-key><username>ff</username><password>secret</password>"
        )
    };
    let cfg = |inner: &str| config(b.port, inner).replace("tcp://", "ssl://");
    let mut opts = MqttOptions::new("tlsprobe", "127.0.0.1", b.port);
    opts.set_credentials("ff", "secret")
        .set_transport(rumqttc::Transport::Tls(rumqttc::TlsConfiguration::Simple {
            ca: std::fs::read(dir.join("ca.crt")).unwrap(),
            alpn: None,
            client_auth: Some((
                std::fs::read(dir.join("cli.crt")).unwrap(),
                std::fs::read(dir.join("cli8.key")).unwrap(),
            )),
        }));
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut probe = Probe::with(opts, &["ferrofire/rtu-1/status"]).await;
    let d = start(&cfg(&tls("ca.crt")));
    probe.text("ferrofire/rtu-1/status", "online").await;
    d.stop.cancel();

    // a wrong password never gets a session: no `online`
    let bad = tls("ca.crt").replace("secret", "wrong");
    let d = start(&cfg(&bad));
    assert!(timeout(
        Duration::from_secs(3),
        probe.text("ferrofire/rtu-1/status", "online")
    )
    .await
    .is_err());
    d.stop.cancel();
    std::fs::remove_dir_all(&dir).unwrap();
}
