//! `ferrofire field-device`: a Rust stand-in for `bennu-field-device` (docs/field-device-spec.md). The config loader,
//! datastore, provider link, logic engine and scan cycle live here; protocols are modules under `proto`.

pub mod config;
pub mod eventlog;
pub mod input;
pub mod logic;
#[cfg(any(feature = "proto-iec61850", feature = "proto-iccp"))]
pub mod mms;
pub mod net;
pub mod proto;
pub mod scan;
pub mod store;

use config::{Config, Opts};
use scan::Output;
use tokio_util::sync::CancellationToken;

/// How long a provider request waits for its reply (SPEC 3.2).
const PROVIDER_TIMEOUT_MS: i32 = 5000;

#[derive(clap::Args)]
pub struct Cli {
    /// The sceptre config.xml (an absolute path, or searched in ., .., /etc/sceptre, /usr/share, /usr/local/share)
    #[arg(long, required_unless_present = "list_protocols")]
    file: Option<String>,
    /// Refuse stock quirks and soft failures (unknown tags in mappings, `&&` truncation)
    #[arg(long)]
    strict: bool,
    /// Print the protocol modules this binary has and exit
    #[arg(long)]
    list_protocols: bool,
}

/// Entry point of the subcommand; returns the process exit code.
pub fn main(cli: Cli) -> i32 {
    if cli.list_protocols {
        print!("{}", proto::list(proto::REGISTRY));
        return 0;
    }
    let file = cli.file.unwrap_or_default();
    let cfg = match load(&file, proto::REGISTRY, &Opts { strict: cli.strict }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ERROR: bennu-field-device failed when loading the field device configuration file!\n{e}");
            return 1;
        }
    };
    match run(cfg, CancellationToken::new(), true) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ERROR: {e}");
            1
        }
    }
}

pub fn load(
    file: &str,
    registry: &[(&str, proto::Start)],
    opts: &Opts,
) -> Result<Config, config::ConfigError> {
    let path = config::find_in(file, config::SEARCH)
        .ok_or_else(|| config::ConfigError(format!("{file}: not found")))?;
    let xml = std::fs::read_to_string(&path)
        .map_err(|e| config::ConfigError(format!("{}: {e}", path.display())))?;
    Config::parse(&xml, registry, opts)
}

/// Run the device until `stop` is cancelled (and, with `signals`, until SIGINT/SIGTERM). A receiver or provider link that cannot
/// start is logged and left out; the rest keeps running.
pub fn run(cfg: Config, stop: CancellationToken, signals: bool) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut threads = vec![];
    for spec in &cfg.inputs {
        match input::spawn(spec, cfg.store.clone(), stop.clone()) {
            Ok(t) => threads.push(t),
            Err(e) => eprintln!("ERROR: input {e}"),
        }
    }
    let outputs = cfg
        .outputs
        .iter()
        .map(|o| crate::transport::requester(&o.endpoint, PROVIDER_TIMEOUT_MS).map(Output::new))
        .collect::<Result<Vec<_>, _>>()?;
    for (tag, start) in cfg.modules {
        eprintln!("{tag}: starting");
        rt.spawn(start(cfg.store.clone(), stop.clone()));
    }
    let (store, logic, cycle_ms) = (cfg.store.clone(), cfg.logic, cfg.cycle_ms);
    let stop2 = stop.clone();
    threads.push(std::thread::spawn(move || {
        scan::run(logic, outputs, store, cycle_ms, stop2)
    }));
    rt.block_on(wait(&stop, signals));
    stop.cancel();
    threads.into_iter().for_each(|t| {
        let _ = t.join();
    });
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    Ok(())
}

async fn wait(stop: &CancellationToken, signals: bool) {
    use tokio::signal::unix::{signal, SignalKind};
    let on = |k| signal(k).ok();
    let (mut int, mut term) = (on(SignalKind::interrupt()), on(SignalKind::terminate()));
    let sig = async {
        match (signals, int.as_mut(), term.as_mut()) {
            (true, Some(i), Some(t)) => tokio::select! { _ = i.recv() => (), _ = t.recv() => () },
            _ => std::future::pending().await,
        }
    };
    tokio::select! { _ = stop.cancelled() => (), _ = sig => () }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bennu::{frame, group_for};
    use std::net::UdpSocket;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn free_udp() -> u16 {
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn free_tcp() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A provider REP that ACKs everything and reports each request.
    fn provider(
        port: u16,
        stop: CancellationToken,
    ) -> (mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let rep = zmq::Context::new().socket(zmq::REP).unwrap();
        rep.set_rcvtimeo(100).unwrap();
        rep.bind(&format!("tcp://127.0.0.1:{port}")).unwrap();
        let h = std::thread::spawn(move || {
            while !stop.is_cancelled() {
                if let Ok(m) = rep.recv_bytes(0) {
                    tx.send(String::from_utf8_lossy(&m).into_owned()).unwrap();
                    rep.send("ACK=\0", 0).unwrap();
                }
            }
        });
        (rx, h)
    }

    fn device(udp: u16, tcp: u16) -> String {
        format!(
            "<SCEPTRE><field-device><name>d</name><cycle-time>30</cycle-time>\
            <tags><external-tag><name>ti</name><io>I0</io><type>analog</type></external-tag>\
            <external-tag><name>to</name><io>O0</io><type>analog</type></external-tag>\
            <external-tag><name>tf</name><io>O1</io><type>analog</type></external-tag></tags>\
            <input><endpoint>udp://127.0.0.1:{udp}</endpoint><analog><id>I0</id><name>in.p</name></analog></input>\
            <output><endpoint>tcp://127.0.0.1:{tcp}</endpoint><analog><id>O0</id><name>out.p</name></analog><analog><id>O1</id><name>fake.p</name></analog></output>\
            <comms><fake-server><tag>tf</tag></fake-server></comms>\
            <logic>to = ti * 2</logic></field-device></SCEPTRE>"
        )
    }

    #[test]
    fn provider_data_flows_through_logic_to_the_provider_and_modules_run() {
        let (udp, tcp) = (free_udp(), free_tcp());
        let stop = CancellationToken::new();
        let (reqs, prov) = provider(tcp, stop.clone());
        let cfg =
            Config::parse(&device(udp, tcp), proto::fake::REGISTRY, &Opts::default()).unwrap();
        let store = cfg.store.clone();
        let stop2 = stop.clone();
        let dev = std::thread::spawn(move || run(cfg, stop2, false));
        let ep = format!("udp://127.0.0.1:{udp}");
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (mut saw_out, mut saw_fake) = (false, false);
        let t = Instant::now();
        while !(saw_out && saw_fake) && t.elapsed() < Duration::from_secs(10) {
            let _ = sender.send_to(&frame(&group_for(&ep), b"in.p:10,\0"), ("127.0.0.1", udp));
            while let Ok(r) = reqs.recv_timeout(Duration::from_millis(50)) {
                saw_out |= r.contains("out.p:20.000000");
                saw_fake |= r.contains("fake.p:1.000000");
            }
        }
        stop.cancel();
        dev.join().unwrap().unwrap();
        prov.join().unwrap();
        assert!(saw_out, "logic result never reached the provider");
        assert!(saw_fake, "the module never wrote");
        assert_eq!(store.get_f64("ti"), Some(10.0));
        assert_eq!(store.get_f64("to"), Some(20.0));
    }

    #[test]
    fn a_bad_input_is_logged_and_the_rest_runs_and_a_bad_output_is_an_error() {
        let xml = "<field-device><name>d</name><input><endpoint>nope</endpoint><analog><name>a</name></analog></input></field-device>";
        let cfg = Config::parse(xml, &[], &Opts::default()).unwrap();
        let stop = CancellationToken::new();
        stop.cancel();
        assert_eq!(run(cfg, stop, false), Ok(()));
        let xml = "<field-device><name>d</name><output><endpoint>bogus</endpoint></output></field-device>";
        let cfg = Config::parse(xml, &[], &Opts::default()).unwrap();
        assert!(run(cfg, CancellationToken::new(), false)
            .unwrap_err()
            .contains("bogus"));
    }

    #[test]
    fn main_lists_protocols_loads_files_and_reports_errors() {
        let cli = |file: Option<&str>, list| Cli {
            file: file.map(String::from),
            strict: false,
            list_protocols: list,
        };
        assert_eq!(main(cli(None, true)), 0);
        assert_eq!(main(cli(Some("/nonexistent/fd.xml"), false)), 1);
        let dir = std::env::temp_dir().join(format!("fd-main-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.xml");
        std::fs::write(&bad, "<nope/>").unwrap();
        assert_eq!(main(cli(bad.to_str(), false)), 1);
        let e = load(bad.to_str().unwrap(), &[], &Opts::default())
            .err()
            .unwrap();
        assert!(e.0.contains("no <field-device>"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_reads_a_file() {
        let dir = std::env::temp_dir().join(format!("fd-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("ok.xml");
        std::fs::write(&ok, "<field-device><name>from-file</name></field-device>").unwrap();
        assert_eq!(
            load(ok.to_str().unwrap(), &[], &Opts::default())
                .unwrap()
                .name,
            "from-file"
        );
        std::fs::create_dir_all(dir.join("d")).unwrap(); // a directory is not readable as a file
        let e = load(dir.join("d").to_str().unwrap(), &[], &Opts::default())
            .err()
            .unwrap();
        assert!(e.0.contains("/d:"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
