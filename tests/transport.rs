//! Transport selection (provider-spec 11): flag > model/config > zmq, and a transport the binary lacks is a startup error.
use ferrofire::transport::{pick, select, COMPILED};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/models/waterway.yaml");
const BOTH: &[&str] = &["zmq", "nng"];

#[test]
fn flag_beats_configured_beats_zmq() {
    for (flag, conf, want) in [
        (None, None, "zmq"),
        (None, Some("nng"), "nng"),
        (Some("zmq"), Some("nng"), "zmq"),
        (Some("nng"), Some("zmq"), "nng"),
        (Some("nng"), None, "nng"),
    ] {
        assert_eq!(pick(flag, conf, BOTH), Ok(want), "{flag:?} {conf:?}");
    }
}

#[test]
fn unknown_and_missing_transports_are_named_errors() {
    let e = pick(Some("carrier-pigeon"), None, BOTH).unwrap_err();
    assert!(e.contains("unknown transport `carrier-pigeon`") && e.contains("zmq or nng"));
    // a bad value in the model or config is refused too, even when the flag would not have been
    assert!(pick(None, Some("x"), BOTH).unwrap_err().contains("`x`"));
    for (missing, kept) in [("nng", "zmq"), ("zmq", "nng")] {
        let e = pick(Some(missing), None, &[kept]).unwrap_err();
        assert!(
            e.contains(&format!("`{missing}` cargo feature"))
                && e.contains(&format!("--features {missing}")),
            "{e}"
        );
    }
    // the default is zmq: a nng-only build must be told to ask for nng
    assert!(pick(None, None, &["nng"])
        .unwrap_err()
        .contains("--features zmq"));
}

#[test]
fn compiled_lists_exactly_the_enabled_features() {
    assert_eq!(COMPILED.contains(&"zmq"), cfg!(feature = "zmq"));
    assert_eq!(COMPILED.contains(&"nng"), cfg!(feature = "nng"));
    assert!(select(Some("bogus"), None).is_err());
}

fn run_stderr(extra: &[&str]) -> (Option<i32>, String) {
    let o = Command::new(BIN)
        .args(["run", MODEL])
        .args(extra)
        .output()
        .unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stderr).into())
}

#[test]
fn run_refuses_a_transport_it_was_not_built_with() {
    let (code, e) = run_stderr(&["--transport", "bogus"]);
    assert_eq!(code, Some(1));
    assert!(e.contains("unknown transport `bogus`") && e.contains("usage: ferrofire run"));
    for (name, built) in [
        ("zmq", cfg!(feature = "zmq")),
        ("nng", cfg!(feature = "nng")),
    ] {
        if !built {
            let (code, e) = run_stderr(&["--transport", name]);
            assert_eq!(code, Some(1));
            assert!(e.contains(&format!("`{name}` cargo feature")), "{e}");
        }
    }
}

#[test]
fn pybennu_power_solver_validates_the_flag_and_the_config_key() {
    let dir = std::env::temp_dir().join(format!("ff-tr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("pybennu-power-solver");
    let _ = std::os::unix::fs::symlink(BIN, &bin);
    let case = concat!(env!("CARGO_MANIFEST_DIR"), "/target/golden/case9.py");
    let conf = |t: &str| {
        let f = dir.join(format!("{}.ini", t.len()));
        std::fs::write(&f, format!("[power-solver-service]\nsolver-type = PyPower\n{t}server-endpoint = tcp://127.0.0.1:1\npublish-endpoint = udp://127.0.0.1:2\ncase-file = {case}\n")).unwrap();
        f
    };
    let start = |c: &std::path::Path, extra: &[&str]| {
        let o = Command::new(&bin)
            .args(["start", "-c", c.to_str().unwrap(), "-e", "trtest"])
            .args(extra)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    assert!(start(&conf("transport = bogus\n"), &[]).contains("unknown transport `bogus`"));
    assert!(start(&conf(""), &["--transport", "bogus"]).contains("unknown transport `bogus`"));
    // the flag wins over the file, so a bad file value is ignored when the flag is bad too (flag is reported)
    assert!(start(&conf("transport = nng\n"), &["--transport", "bogus"]).contains("`bogus`"));
}
