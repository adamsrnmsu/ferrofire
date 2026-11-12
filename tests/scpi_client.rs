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
        "<endpoint>tcp://127.0.0.1:{port}</endpoint><event-logging>/dev/null</event-logging>
         <hislip-endpoint>none</hislip-endpoint><vxi11-endpoint>none</vxi11-endpoint>"
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
                "MEAS:SCAL:LEVEL?;:MEAS:BIN:ALARM?",
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
                "MEAS:SCAL:NOPE?",
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
        e.contains("error: MEAS:SCAL:NOPE?: -113,Undefined header;MEAS:SCAL:NOPE?"),
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
    std::fs::write(&path, "# a script\n*CLS\n\n  MEAS:SCAL:LEVEL?  \n").unwrap();
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

/// Any instrument that offers the socket service, not only ours: this stub answers with CRLF, has its own `SYST:ERR?` text, and
/// shows the client needs nothing from ferrofire beyond lines of ASCII.
#[tokio::test(flavor = "multi_thread")]
async fn it_works_against_a_foreign_responder_that_ends_lines_with_crlf() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let target = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let (s, _) = l.accept().unwrap();
        let mut w = s.try_clone().unwrap();
        for line in BufReader::new(s).lines() {
            let reply: &[u8] = match line.unwrap().trim() {
                "*IDN?" => b"ACME,STUB,1,1.0\r\n",
                "SYST:ERR?" => b"-113,\"Undefined header\"\r\n",
                "BAD?" => b"",
                _ => b"+1.234E+00\r\n",
            };
            w.write_all(reply).unwrap();
        }
    });
    let o = scpi(vec![target, "*IDN?".into(), "MEAS:X?".into()], "").await;
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ACME,STUB,1,1.0\n+1.234E+00\n");
}

// ---- HiSLIP and VXI-11 resources ---------------------------------------------------------------------------------------------

/// A device with the raw socket, HiSLIP and VXI-11 (portmapper and core on free ports) up.
struct All {
    raw: u16,
    hislip: u16,
    pm: u16,
    stop: CancellationToken,
}

async fn all(inner: &str) -> All {
    let (raw, hislip, pm, core) = (free_port(), free_port(), free_port(), free_port());
    let (server, store) = load(&format!(
        "<endpoint>tcp://127.0.0.1:{raw}</endpoint><hislip-endpoint>tcp://127.0.0.1:{hislip}</hislip-endpoint>
         <vxi11-endpoint>tcp://127.0.0.1:{pm}</vxi11-endpoint><vxi11-core-endpoint>tcp://127.0.0.1:{core}</vxi11-core-endpoint>
         <event-logging>/dev/null</event-logging>{inner}"
    ));
    let stop = CancellationToken::new();
    tokio::spawn(server.run(store, stop.clone()));
    for port in [raw, hislip, pm, core] {
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
    All {
        raw,
        hislip,
        pm,
        stop,
    }
}

impl All {
    /// The same instrument as each resource string names it.
    fn resources(&self) -> [(String, Vec<String>); 4] {
        let pm = vec!["--portmapper".to_string(), self.pm.to_string()];
        [
            (format!("TCPIP::127.0.0.1::{}::SOCKET", self.raw), vec![]),
            (format!("127.0.0.1:{}", self.raw), vec![]),
            (
                format!("TCPIP::127.0.0.1::hislip0,{}::INSTR", self.hislip),
                vec![],
            ),
            ("TCPIP::127.0.0.1::INSTR".to_string(), pm),
        ]
    }
}

fn run(target: &str, extra: &[String], rest: &[&str]) -> Vec<String> {
    std::iter::once(target.to_string())
        .chain(extra.iter().cloned())
        .chain(rest.iter().map(|s| s.to_string()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_resource_string_reaches_the_same_instrument() {
    let d = all("").await;
    for (target, extra) in d.resources() {
        let o = scpi(
            run(&target, &extra, &["*IDN?", "MEAS:SCAL:LEVEL?", "*CLS"]),
            "",
        )
        .await;
        assert!(o.status.success(), "{target}: {}", err(&o));
        assert_eq!(
            out(&o),
            format!("ferrofire,rtu-1,0,{}\n12.5\n", env!("CARGO_PKG_VERSION")),
            "{target}"
        );
        let o = scpi(
            run(
                &target,
                &extra,
                &["--check", "--json", "SOUR:SETPOINT 100", "MEAS:SCAL:LEVEL?"],
            ),
            "",
        )
        .await;
        assert!(o.status.success(), "{target}: {}", err(&o));
        assert!(
            out(&o).contains("\"response\":\"12.5\""),
            "{target}: {}",
            out(&o)
        );
        let o = scpi(run(&target, &extra, &["--check", "SOUR:SETPOINT 5000"]), "").await;
        assert_eq!(o.status.code(), Some(1), "{target}");
        assert!(
            err(&o).contains("-222,Data out of range"),
            "{target}: {}",
            err(&o)
        );
    }
    d.stop.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_query_under_check_does_not_cost_the_timeout() {
    let d = all("").await;
    for (target, extra) in d.resources() {
        let t0 = std::time::Instant::now();
        let o = scpi(
            run(
                &target,
                &extra,
                &["--check", "--timeout", "30", "NOPE:NOPE?"],
            ),
            "",
        )
        .await;
        assert_eq!(o.status.code(), Some(1), "{target}: {}", err(&o));
        assert!(
            err(&o).contains("-113,Undefined header;NOPE:NOPE?"),
            "{target}: {}",
            err(&o)
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "{target}: {:?}",
            t0.elapsed()
        );
        let o = scpi(
            run(
                &target,
                &extra,
                &[
                    "--check",
                    "--timeout",
                    "30",
                    "MEAS:SCAL:LEVEL?;*CLS",
                    "*IDN?;*IDN?",
                ],
            ),
            "",
        )
        .await;
        assert_eq!(o.status.code(), Some(0), "{target}: {}", err(&o));
        assert_eq!(out(&o).lines().count(), 2, "{target}");
    }
    d.stop.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_that_cannot_be_reached_or_parsed_fail_with_exit_2() {
    let d = all("").await;
    let closed = free_port();
    let o = scpi(
        vec![format!("TCPIP::127.0.0.1::hislip0,{closed}::INSTR")],
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let o = scpi(
        vec![
            "TCPIP::127.0.0.1::INSTR".into(),
            "--portmapper".into(),
            closed.to_string(),
            "*IDN?".into(),
        ],
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(2));
    let o = scpi(
        vec![
            "TCPIP::127.0.0.1::gpib0,2::INSTR".into(),
            "--portmapper".into(),
            d.pm.to_string(),
            "*IDN?".into(),
        ],
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("no such device `gpib0,2`"), "{}", err(&o));
    let o = scpi(
        vec![
            "TCPIP::127.0.0.1::hislip9::INSTR".replace("::INSTR", &format!(",{}::INSTR", d.hislip)),
            "*IDN?".into(),
        ],
        "",
    )
    .await;
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("no such HiSLIP instrument"), "{}", err(&o));
    let o = scpi(vec!["TCPIP::127.0.0.1".into(), "*IDN?".into()], "").await;
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("TCPIP::host::INSTR"), "{}", err(&o));
    d.stop.cancel();
}
