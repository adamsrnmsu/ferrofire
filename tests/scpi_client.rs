//! `ferrofire scpi` (the real binary) against the field device's SCPI server running in this process: one-shot commands, a script
//! from a file and from stdin, `--check` failing with the instrument's error text, `--json`, the timeout against a server that never
//! answers, and the exit codes (0 fine, 1 the instrument reported errors, 2 the run failed).

mod common;

use common::scpi::{free_port, load};
use ferrofire::fd::proto::Module;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use tokio_util::sync::CancellationToken;

const BIN: &str = env!("CARGO_BIN_EXE_ferrofire");

struct Dut {
    target: String,
    stop: CancellationToken,
    store: ferrofire::fd::store::Store,
}

async fn dut() -> Dut {
    let port = free_port();
    let (server, store) = load(&format!(
        "<endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging>"
    ));
    let stop = CancellationToken::new();
    tokio::spawn(server.run(store.clone(), stop.clone()));
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Dut {
        target: format!("127.0.0.1:{port}"),
        stop,
        store,
    }
}

/// Run the binary off the runtime's threads, so the server in this process keeps being scheduled.
async fn scpi(args: Vec<String>, stdin: &'static str) -> Output {
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(BIN)
            .arg("scpi")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

fn args(d: &Dut, rest: &[&str]) -> Vec<String> {
    std::iter::once(d.target.clone())
        .chain(rest.iter().map(|s| s.to_string()))
        .collect()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn one_shot_commands_print_the_responses_of_the_queries() {
    let d = dut().await;
    let o = scpi(
        args(
            &d,
            &[
                "*IDN?",
                "SOUR:SETPOINT 700",
                "MEAS:ANAL:LEVEL?;:MEAS:BIN:ALARM?",
            ],
        ),
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        out(&o),
        format!("ferrofire,rtu-1,0,{}\n12.5;1\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(d.store.is_queued("setpoint"));
    d.stop.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn check_fails_with_the_instruments_error_text_and_still_runs_the_rest() {
    let d = dut().await;
    let o = scpi(
        args(
            &d,
            &[
                "--check",
                "--timeout",
                "0.5",
                "SOUR:SETPOINT 9000",
                "MEAS:ANAL:NOPE?",
                "*OPC?",
            ],
        ),
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "1\n");
    let e = err(&o);
    assert!(e.contains("error: SOUR:SETPOINT 9000: -222,Data out of range;setpoint: 9000 is outside the limits"), "{e}");
    assert!(
        e.contains("error: MEAS:ANAL:NOPE?: -113,Undefined header;MEAS:ANAL:NOPE?"),
        "{e}"
    );
    let ok = scpi(args(&d, &["--check", "*CLS", "SOUR:SETPOINT 10"]), "").await;
    assert_eq!(ok.status.code(), Some(0), "{}", err(&ok));
    d.stop.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn json_prints_one_object_per_command() {
    let d = dut().await;
    let o = scpi(
        args(&d, &["--json", "--check", "MEAS:BIN:ALARM?", "FOO"]),
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        out(&o),
        "{\"command\":\"MEAS:BIN:ALARM?\",\"response\":\"1\",\"errors\":[]}\n{\"command\":\"FOO\",\"response\":null,\"errors\":[{\"code\":-113,\"message\":\"Undefined header;FOO\"}]}\n"
    );
    d.stop.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_comes_from_a_file_or_from_stdin() {
    let d = dut().await;
    let path = std::env::temp_dir().join(format!("scpi-script-{}.txt", std::process::id()));
    std::fs::write(&path, "# a script\n*CLS\n\n  MEAS:ANAL:LEVEL?  \n").unwrap();
    let o = scpi(args(&d, &["--check", "--file", path.to_str().unwrap()]), "").await;
    assert_eq!(
        (o.status.code(), out(&o)),
        (Some(0), "12.5\n".into()),
        "{}",
        err(&o)
    );
    let o = scpi(
        args(&d, &["--check", "-f", "-", "*OPC?"]),
        "MEAS:BIN:ALARM?\n# no\nSOUR:BREAKER 1\n",
    )
    .await;
    assert_eq!(
        (o.status.code(), out(&o)),
        (Some(0), "1\n1\n".into()),
        "{}",
        err(&o)
    );
    assert!(d.store.is_queued("breaker"));
    let o = scpi(args(&d, &["--file", "/nonexistent/script"]), "").await;
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("/nonexistent/script"));
    let _ = std::fs::remove_file(path);
    d.stop.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_commands_a_piped_stdin_is_a_script() {
    let d = dut().await;
    let o = scpi(args(&d, &[]), "*OPC?\n").await;
    assert_eq!(
        (o.status.code(), out(&o)),
        (Some(0), String::new()),
        "no --file, not a terminal: nothing to send is fine? {}",
        err(&o)
    );
    d.stop.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_cannot_complete_exits_2_with_a_message() {
    let d = dut().await;
    let o = scpi(args(&d, &["--timeout", "0", "*IDN?"]), "").await;
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("--timeout 0: expected a positive number of seconds"),
        "{}",
        err(&o)
    );
    let o = scpi(vec!["127.0.0.1:1".into(), "*IDN?".into()], "").await;
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("connect 127.0.0.1:1"), "{}", err(&o));
    let o = scpi(vec!["host:notaport".into(), "*IDN?".into()], "").await;
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("expected HOST[:PORT]"));
    // a server that accepts and never answers
    let mute = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let target = mute.local_addr().unwrap().to_string();
    let o = scpi(
        vec![target, "--timeout".into(), "0.3".into(), "*IDN?".into()],
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("no response to `*IDN?` within 0.3 s"),
        "{}",
        err(&o)
    );
    d.stop.cancel();
}
