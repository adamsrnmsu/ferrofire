//! `<scpi-client>` against `<scpi-server>`: two ferrofire field devices in one process, a controller and an instrument, over the raw
//! socket and HiSLIP. `<logic>` on the controller drives sets that reach the instrument's store, a query tag on the controller tracks a
//! value written on the instrument, the default commands pair the two by tag name alone, and a restarted instrument gets every set
//! again although nothing on the controller changed.

#![cfg(feature = "proto-scpi")]

mod common;

use common::scpi::free_port;
use ferrofire::fd::config::{Config, Opts};
use ferrofire::fd::proto::REGISTRY;
use ferrofire::fd::store::Store;
use ferrofire::model::Value;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// A whole field device (modules, logic, scan cycle) on its own thread, as `ferrofire field-device` runs it.
struct Device {
    store: Store,
    stop: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

impl Device {
    fn start(xml: &str) -> Device {
        let cfg = Config::parse(xml, REGISTRY, &Opts::default()).unwrap();
        let (store, stop) = (cfg.store.clone(), CancellationToken::new());
        let s = stop.clone();
        let thread = std::thread::spawn(move || ferrofire::fd::run(cfg, s, false).unwrap());
        Device {
            store,
            stop,
            thread: Some(thread),
        }
    }

    /// Stop and wait until its sockets are closed.
    fn halt(&mut self) {
        self.stop.cancel();
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// The instrument: `volt` and `out` can be set, `level` is read, all device-local (no provider), starting at `level`.
fn instrument(port: u16, hislip: u16, level: f64) -> String {
    format!(
        r#"<field-device><name>psu</name><cycle-time>50</cycle-time>
<tags>
 <internal-tag><name>volt</name><value>0</value></internal-tag>
 <internal-tag><name>out</name><status>false</status></internal-tag>
 <internal-tag><name>level</name><value>{level}</value></internal-tag>
</tags>
<comms><scpi-server><endpoint>tcp://127.0.0.1:{port}</endpoint><hislip-endpoint>tcp://127.0.0.1:{hislip}</hislip-endpoint>
 <vxi11-endpoint>none</vxi11-endpoint><event-logging>/dev/null</event-logging>
 <point><tag>volt</tag><writable>true</writable></point>
 <point><tag>out</tag><writable>true</writable></point>
 <point><tag>level</tag></point>
</scpi-server></comms></field-device>"#
    )
}

/// The controller: it has no `<tags>`; the client declares them. `elements` are the `<query>` and `<set>` elements.
fn controller(endpoint: &str, log: &Path, elements: &str) -> String {
    format!(
        r#"<field-device><name>ctl</name><cycle-time>50</cycle-time>
<comms><scpi-client><endpoint>{endpoint}</endpoint><interval>50</interval><timeout>1000</timeout>
 <event-logging>{}</event-logging>{elements}</scpi-client></comms>
<logic>out = level &lt; 80
volt = level * 2</logic></field-device>"#,
        log.display()
    )
}

/// Tag names only: the client uses the paths the server gives the same names.
const BY_NAME: &str =
    "<query><tag>level</tag></query><set><tag>out</tag><type>binary</type></set><set><tag>volt</tag></set>";

const COMMANDS: &str = "<query><tag>level</tag><command>MEASure:SCALar:LEVEL?</command></query>
 <set><tag>out</tag><type>binary</type><command>SOUR:OUT {}</command></set>
 <set><tag>volt</tag><command>SOURce:VOLT {}</command></set>";

fn wait_for(what: &str, ok: impl Fn() -> bool) {
    let t = Instant::now();
    while !ok() {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn log_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("scpi-controller-{}-{name}.log", std::process::id()))
}

/// Logic on the controller sets `out` and `volt` on the instrument from the `level` it reads there.
fn control_loop(endpoint: impl Fn(u16, u16) -> String, elements: &str, name: &str) {
    let (port, hislip) = (free_port(), free_port());
    let mut ins = Device::start(&instrument(port, hislip, 0.0));
    let log = log_path(name);
    let mut ctl = Device::start(&controller(&endpoint(port, hislip), &log, elements));
    wait_for("out set from level 0", || {
        ins.store.get("out") == Some(Value::Binary(true))
    });
    ins.store.write("level", Value::Analog(50.0)).unwrap();
    wait_for("the controller reads level 50", || {
        ctl.store.get_f64("level") == Some(50.0)
    });
    wait_for("volt set to 100", || {
        ins.store.get_f64("volt") == Some(100.0)
    });
    ins.store.write("level", Value::Analog(90.5)).unwrap();
    wait_for("out cleared at level 90.5", || {
        ins.store.get("out") == Some(Value::Binary(false))
    });
    wait_for("volt set to 181", || {
        ins.store.get_f64("volt") == Some(181.0)
    });
    ctl.halt();
    ins.halt();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        !text.contains("scpi error"),
        "the instrument refused something:\n{text}"
    );
    assert_eq!(text.matches(",connected").count(), 1, "{text}");
    std::fs::remove_file(&log).unwrap();
}

#[test]
fn logic_drives_an_instrument_over_the_raw_socket() {
    control_loop(|port, _| format!("127.0.0.1:{port}"), COMMANDS, "raw");
}

#[test]
fn two_ferrofire_devices_pair_by_tag_name_over_hislip() {
    control_loop(
        |_, hislip| format!("TCPIP::127.0.0.1::hislip0,{hislip}::INSTR"),
        BY_NAME,
        "hislip",
    );
}

#[test]
fn a_restarted_instrument_gets_every_set_again() {
    let (port, hislip) = (free_port(), free_port());
    let mut ins = Device::start(&instrument(port, hislip, 50.0));
    let log = log_path("restart");
    let mut ctl = Device::start(&controller(&format!("127.0.0.1:{port}"), &log, BY_NAME));
    wait_for("the first instrument is set", || {
        ins.store.get("out") == Some(Value::Binary(true))
            && ins.store.get_f64("volt") == Some(100.0)
    });
    ins.halt();
    wait_for("the controller sees it go", || {
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("reconnecting")
    });
    // the same level, so nothing the controller computes changes: only the reconnect can make it send the sets again
    let mut ins = Device::start(&instrument(port, hislip, 50.0));
    wait_for("the restarted instrument is set", || {
        ins.store.get("out") == Some(Value::Binary(true))
            && ins.store.get_f64("volt") == Some(100.0)
    });
    assert_eq!(ctl.store.get_f64("level"), Some(50.0));
    ctl.halt();
    ins.halt();
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.matches(",connected").count(), 2, "{text}");
    std::fs::remove_file(&log).unwrap();
}

#[test]
fn the_documented_example_loads() {
    let doc = include_str!("../docs/field-device-scpi.md");
    let section = &doc[doc.find("## The controller").unwrap()..];
    let xml = section
        .split("```xml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    // the example has no <event-logging>; the default file would land in the working directory
    let xml = xml.replace(
        "<scpi-client>",
        "<scpi-client><event-logging>/dev/null</event-logging>",
    );
    let cfg = Config::parse(&xml, REGISTRY, &Opts::default()).unwrap();
    assert_eq!(cfg.modules.len(), 1);
    assert_eq!(cfg.store.get("psu.out"), Some(Value::Binary(false)));
    assert_eq!(cfg.store.get_f64("tank.level"), Some(0.0));
}
