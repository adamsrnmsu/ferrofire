//! `ferrofire field-device` as an operator runs it: --list-protocols, load errors, and a running device that SIGTERM stops.
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");

fn fd(args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("field-device")
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn write_cfg(name: &str, xml: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("ff-fd-{}-{name}.xml", std::process::id()));
    std::fs::write(&p, xml).unwrap();
    p
}

#[test]
fn list_protocols_prints_the_registry() {
    let o = fd(&["--list-protocols"]);
    assert!(o.status.success());
    let out = text(&o.stdout);
    assert!(out.starts_with("compiled:\n"), "{out}");
    assert!(out.contains("not built:\n"), "{out}");
    assert!(
        out.contains("dnp3-server (feature proto-dnp3)")
            || out.contains("compiled:\n  dnp3-server"),
        "{out}"
    );
}

#[test]
fn file_is_required_unless_listing() {
    let o = fd(&[]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("--file"), "{}", text(&o.stderr));
}

#[test]
fn a_missing_or_broken_config_exits_1_with_the_stock_message() {
    let o = fd(&["--file", "/nonexistent/config.xml"]);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    assert!(
        err.contains(
            "Error: bennu-field-device failed when loading the field device configuration file!"
        ),
        "{err}"
    );
    assert!(err.contains("not found"), "{err}");
    let p = write_cfg("broken", "<SCEPTRE><field-device></SCEPTRE>");
    let o = fd(&["--file", p.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("not valid XML"));
    std::fs::remove_file(p).unwrap();
}

#[test]
fn strict_makes_an_unknown_comms_element_fatal() {
    let p = write_cfg(
        "strict",
        "<SCEPTRE><field-device><name>d</name><comms><mystery/></comms></field-device></SCEPTRE>",
    );
    let o = fd(&["--file", p.to_str().unwrap(), "--strict"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("unknown <comms> element <mystery>"));
    std::fs::remove_file(p).unwrap();
}

#[test]
fn a_running_device_exits_cleanly_on_sigterm() {
    let p = write_cfg("run", "<SCEPTRE><field-device><name>d</name><cycle-time>50</cycle-time><comms><mystery/></comms></field-device></SCEPTRE>");
    for sig in [libc::SIGTERM, libc::SIGINT] {
        let mut c = Command::new(BIN)
            .args(["field-device", "--file", p.to_str().unwrap()])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(c.try_wait().unwrap().is_none(), "device exited early");
        unsafe { libc::kill(c.id() as i32, sig) };
        let t = Instant::now();
        let status = c.wait().unwrap();
        assert!(status.success(), "signal {sig}: {status}");
        assert!(t.elapsed() < Duration::from_secs(5));
    }
    std::fs::remove_file(p).unwrap();
}
