//! `<iec60870-5-104-client>` in bennu mode. The master's bytes and what it makes of the server's are checked against the stock
//! client's transcript (conformance/stock/client-iec104.golden, `GC`); a scripted server over a real socket checks the connection
//! rules (conformance/iec104/STOCK.md, "Client"): STARTDT and one GI at connect, and no reconnect, ever.

mod common;

use common::clients::*;
use common::unhex;
use ferrofire::fd::config::{Config, Opts};
use ferrofire::fd::proto::client::Mode;
use ferrofire::fd::proto::iec104::apci::{encode, Frame};
use ferrofire::fd::proto::iec104::asdu::{decode, Asdu};
use ferrofire::fd::proto::iec104::client::Master;
use ferrofire::fd::proto::iec104::Client;
use ferrofire::fd::proto::REGISTRY;
use ferrofire::model::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CLIENT: &str = include_str!("../conformance/stock/client-iec104.xml");

fn hex(s: &str) -> Vec<u8> {
    unhex(&s.replace(' ', ""))
}

fn asdu(s: &str) -> Asdu {
    decode(&hex(s)).unwrap()
}

fn shows(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn sample() -> Client {
    common::load_as::<Client>(CLIENT, "iec60870-5-104-client").0
}

fn master(mode: Mode, strict: bool) -> Master {
    Master::new(&sample().conns[0], mode, strict)
}

fn updates(m: &mut Master, s: &str) -> Vec<(String, Value)> {
    m.on_asdu(&asdu(s))
}

fn pairs(want: &[(&str, Value)]) -> Vec<(String, Value)> {
    want.iter().map(|(t, v)| (t.to_string(), *v)).collect()
}

fn strict() -> Opts {
    Opts {
        strict: true,
        ..Opts::default()
    }
}

#[test]
fn stock_sample_client_loads() {
    let c = Config::parse(CLIENT, REGISTRY, &strict()).unwrap();
    let names: Vec<_> = c.modules.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["iec60870-5-104-client"]);
    // the point tags are the device's own, declared at load
    assert_eq!(c.store.get("I_bus-1.gen_mw"), Some(Value::Analog(0.0)));
    assert_eq!(
        c.store.get("O_load-1_bus-1.active"),
        Some(Value::Binary(false))
    );
    let s = sample();
    assert_eq!(s.mode, Mode::Bennu);
    assert_eq!(s.cmdif.as_deref(), Some("tcp://127.0.0.1:1330"));
    assert_eq!(s.conns.len(), 1);
    let conn = &s.conns[0];
    assert_eq!(conn.endpoint, "tcp://127.0.0.1:2404");
    assert_eq!(conn.points.len(), 8);
    assert!(conn.fep.is_none());
    // GC:6, the GI: COT 6, originator 3, CA 1, QOI 20
    let gi: Vec<_> = master(Mode::Bennu, false)
        .on_start()
        .iter()
        .map(|a| shows(&a.encode()))
        .collect();
    assert_eq!(gi, ["64 01 06 03 01 00 00 00 00 14"]);
}

/// The sample server's GI in single-point mode (`conformance/stock/iec104-server.golden`) and the double-point report's
/// `M_DP_TB_1` (GC:77): a bennu-mode client ignores both; `--strict` and fep mode decode them.
#[test]
fn q12_single_points_ignored_unless_strict() {
    let single = "01 04 14 00 01 00 00 00 00 01 01 00 00 00 64 00 00 01 65 00 00 00";
    let timed = "1f 01 03 00 01 00 00 00 00 02 00 00 00 00 01 01 18";
    assert!(updates(&mut master(Mode::Bennu, false), single).is_empty());
    assert!(updates(&mut master(Mode::Bennu, false), timed).is_empty());
    let want = pairs(&[
        ("O_generator-1_bus-1.active", Value::Binary(true)),
        ("O_load-1_bus-1.active", Value::Binary(false)),
        ("I_generator-1_bus-1.active", Value::Binary(true)),
        ("I_load-1_bus-1.active", Value::Binary(false)),
    ]);
    for m in [
        &mut master(Mode::Bennu, true),
        &mut master(Mode::Fep, false),
    ] {
        assert_eq!(updates(m, single), want);
        assert_eq!(updates(m, timed), want[..1]);
    }
}

#[test]
fn double_and_float_points_update_tags() {
    let mut m = master(Mode::Bennu, false);
    // GC:9: OFF is false, ON true
    let dp = "03 04 14 00 01 00 00 00 00 02 01 00 00 01 64 00 00 02 65 00 00 01";
    assert_eq!(
        updates(&mut m, dp),
        pairs(&[
            ("O_generator-1_bus-1.active", Value::Binary(true)),
            ("O_load-1_bus-1.active", Value::Binary(false)),
            ("I_generator-1_bus-1.active", Value::Binary(true)),
            ("I_load-1_bus-1.active", Value::Binary(false)),
        ])
    );
    // GC:10 (the GI, COT 20) and GC:76 (the report, COT 1): every COT counts; a float32 as it came
    let x = f64::from(300.7f32);
    for cot in ["14", "01"] {
        let f = format!("0d 04 {cot} 00 01 00 02 00 00 9a 59 96 43 00 03 00 00 00 80 7a 43 00 66 00 00 9a 59 96 43 00 67 00 00 00 80 7a 43 00");
        assert_eq!(
            updates(&mut m, &f),
            pairs(&[
                ("O_bus-1.gen_mw", Value::Analog(x)),
                ("O_load-1_bus-1.mw", Value::Analog(250.5)),
                ("I_bus-1.gen_mw", Value::Analog(x)),
                ("I_load-1_bus-1.mw", Value::Analog(250.5)),
            ])
        );
    }
    // indeterminate (0) and faulty (3) are false; an IOA is cut to 16 bits (stock's `uint16_t addr`); an address with no point
    // of the kind, and the command confirmations, change nothing
    let odd = "03 04 03 00 01 00 00 00 00 00 01 00 00 03 64 00 01 02 05 00 00 02";
    assert_eq!(
        updates(&mut m, odd),
        pairs(&[
            ("O_generator-1_bus-1.active", Value::Binary(false)),
            ("O_load-1_bus-1.active", Value::Binary(false)),
            ("I_generator-1_bus-1.active", Value::Binary(true)),
        ])
    );
    for confirm in [
        "2e 01 07 03 01 00 00 00 00 01",
        "32 01 47 03 01 00 02 00 00 00 00 2a 42 00",
    ] {
        assert!(updates(&mut m, confirm).is_empty());
    }
}

/// `true`/`false` is a `C_DC_NA_1` direct execute (S/E 0) to the tag's IOA whatever its kind, and changes the binary at that
/// address locally, if there is one. GC:23, :27, :47, :50.
#[test]
fn write_binary_sends_c_dc_direct_execute() {
    let m = master(Mode::Bennu, false);
    for (tag, on, bytes, local) in [
        (
            "O_generator-1_bus-1.active",
            false,
            "2e 01 06 03 01 00 00 00 00 01",
            true,
        ),
        (
            "O_load-1_bus-1.active",
            true,
            "2e 01 06 03 01 00 01 00 00 02",
            true,
        ),
        (
            "I_load-1_bus-1.active",
            true,
            "2e 01 06 03 01 00 65 00 00 02",
            true,
        ),
        (
            "O_bus-1.gen_mw",
            true,
            "2e 01 06 03 01 00 02 00 00 02",
            false,
        ),
    ] {
        let v = Value::Binary(on);
        assert_eq!(shows(&m.command(tag, v).unwrap().encode()), bytes, "{tag}");
        let want = local.then(|| (tag.to_string(), v));
        assert_eq!(m.local(tag, v), want, "{tag}");
    }
    assert!(m.command("nope", Value::Binary(true)).is_err());
}

/// A number is a `C_SE_NC_1` direct execute (S/E 0, QL 0) of the float32. GC:31, :35, :44, :53.
#[test]
fn write_analog_sends_c_se_nc() {
    let m = master(Mode::Bennu, false);
    for (tag, x, bytes, local) in [
        (
            "O_bus-1.gen_mw",
            42.5,
            "32 01 06 03 01 00 02 00 00 00 00 2a 42 00",
            true,
        ),
        (
            "O_load-1_bus-1.mw",
            -1.25,
            "32 01 06 03 01 00 03 00 00 00 00 a0 bf 00",
            true,
        ),
        (
            "I_bus-1.gen_mw",
            5.0,
            "32 01 06 03 01 00 66 00 00 00 00 a0 40 00",
            true,
        ),
        (
            "O_generator-1_bus-1.active",
            7.0,
            "32 01 06 03 01 00 00 00 00 00 00 e0 40 00",
            false,
        ),
    ] {
        let v = Value::Analog(x);
        assert_eq!(shows(&m.command(tag, v).unwrap().encode()), bytes, "{tag}");
        let want = local.then(|| (tag.to_string(), v));
        assert_eq!(m.local(tag, v), want, "{tag}");
    }
    // a value a float32 cannot hold is kept as the float32 the wire carries (stock keeps a float)
    assert_eq!(
        m.local("O_bus-1.gen_mw", Value::Analog(0.1)),
        Some(("O_bus-1.gen_mw".into(), Value::Analog(f64::from(0.1f32))))
    );
}

#[test]
fn non_tcp_endpoint_is_an_error_as_stock() {
    for ep in ["udp://127.0.0.1:2404", "/dev/ttyS0"] {
        let xml = CLIENT.replace("tcp://127.0.0.1:2404", ep);
        let e = Config::parse(&xml, REGISTRY, &Opts::default())
            .err()
            .expect("an error");
        assert!(
            e.0.contains(&format!("Unknown endpoint protocol: {ep}")),
            "{e}"
        );
    }
}

#[test]
fn mode_sbo_and_fep_keys_are_checked_at_load() {
    let with = |head: &str, conn: &str| {
        CLIENT
            .replace(
                "<iec60870-5-104-client>",
                &format!("<iec60870-5-104-client>{head}"),
            )
            .replace(
                "<iec60870-5-104-connection>",
                &format!("<iec60870-5-104-connection>{conn}"),
            )
    };
    let err = |xml: &str, opts: &Opts| {
        Config::parse(xml, REGISTRY, opts)
            .err()
            .expect("an error")
            .0
    };
    let e = err(&with("<mode>fep2</mode>", ""), &Opts::default());
    assert!(e.contains("<mode> `fep2` is not bennu or fep"), "{e}");
    let sbo = with("", "<sbo-enabled>true</sbo-enabled>");
    assert!(err(&sbo, &Opts::default()).contains("select-before-operate is not supported"));
    assert!(Config::parse(
        &with("", "<sbo-enabled>false</sbo-enabled>"),
        REGISTRY,
        &strict()
    )
    .is_ok());
    // bennu mode: the fep keys are a warning, an error under --strict
    let keys = with("", "<scan-rate>2</scan-rate>");
    assert!(Config::parse(&keys, REGISTRY, &Opts::default()).is_ok());
    assert!(err(&keys, &strict()).contains("<scan-rate> is fep-only"));
    // fep mode reads them
    let fep = with(
        "<mode>fep</mode>",
        "<scan-rate>2</scan-rate><status-tag>up</status-tag>",
    );
    let doc = roxmltree::Document::parse(&fep).unwrap();
    let node = doc
        .descendants()
        .find(|n| n.has_tag_name("iec60870-5-104-client"))
        .unwrap();
    let mut tags = Default::default();
    let c = <Client as ferrofire::fd::proto::Module>::parse(node, &mut tags, &strict()).unwrap();
    assert_eq!(c.mode, Mode::Fep);
    let keys = c.conns[0].fep.as_ref().unwrap();
    assert_eq!(
        (keys.scan, keys.status.as_deref()),
        (Duration::from_secs(2), Some("up"))
    );
}

/// What the client logs, from every thread: the master runs on the runtime's workers.
fn logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Arc::new(Mutex::new(vec![]));
        let to = logs.clone();
        let writer = move || Sink(to.clone());
        let sub = tracing_subscriber::fmt().with_writer(writer).finish();
        tracing::subscriber::set_global_default(sub).unwrap();
        logs
    })
    .clone()
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A scripted server on a real socket: it counts the connections it accepts and keeps what the master sent; each connection
/// gets `script`'s frames once the master's GI is in, and is closed when `close` is cancelled.
struct Scripted {
    port: u16,
    accepted: Arc<AtomicUsize>,
    got: Arc<Mutex<Vec<u8>>>,
    close: tokio_util::sync::CancellationToken,
}

async fn scripted(answer: Vec<Vec<u8>>) -> Scripted {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let s = Scripted {
        port: l.local_addr().unwrap().port(),
        accepted: Default::default(),
        got: Default::default(),
        close: Default::default(),
    };
    let (accepted, got, close) = (s.accepted.clone(), s.got.clone(), s.close.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut c, _)) = l.accept().await else {
                return;
            };
            accepted.fetch_add(1, Ordering::SeqCst);
            let (got, close, answer) = (got.clone(), close.clone(), answer.clone());
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let mut answered = false;
                loop {
                    let n = tokio::select! {
                        _ = close.cancelled() => return,
                        r = c.read(&mut buf) => match r { Ok(n @ 1..) => n, _ => return },
                    };
                    got.lock().unwrap().extend_from_slice(&buf[..n]);
                    // STARTDT act (6 bytes) and the GI (16) are in
                    if !answered && got.lock().unwrap().len() >= 22 {
                        answered = true;
                        let _ = c.write_all(&answer.concat()).await;
                    }
                }
            });
        }
    });
    s
}

fn client_xml(port: u16, cmdif: u16) -> String {
    CLIENT
        .replace("tcp://127.0.0.1:2404", &format!("tcp://127.0.0.1:{port}"))
        .replace("tcp://127.0.0.1:1330", &format!("tcp://127.0.0.1:{cmdif}"))
}

async fn says(cmdif: u16, req: &str) -> String {
    shown(&command(cmdif, req).await)
}

/// The server's answer to the GI, GC:7-11, as I-frames numbered from 0, each acking the master's GI.
fn gi_answer() -> Vec<Vec<u8>> {
    let mut out = vec![hex("68 04 0b 00 00 00")];
    for (ns, a) in [
        "64 01 07 03 01 00 00 00 00 14",
        "03 04 14 00 01 00 00 00 00 02 01 00 00 01 64 00 00 02 65 00 00 01",
        "0d 04 14 00 01 00 02 00 00 9a 59 96 43 00 03 00 00 00 80 7a 43 00 66 00 00 9a 59 96 43 00 67 00 00 00 80 7a 43 00",
        "64 01 0a 03 01 00 00 00 00 14",
    ]
    .iter()
    .enumerate()
    {
        out.push(encode(&Frame::I {
            ns: ns as u16,
            nr: 1,
            asdu: hex(a),
        }));
    }
    out
}

/// GC:63 to the end: with the server gone, reads answer the last values and writes are ACKed and set the local value; no `ERR`
/// comes from the server being away, and the client never connects again. With no server at start, the same from zero values.
#[tokio::test(flavor = "multi_thread")]
async fn server_away_gives_stock_err_and_never_reconnects() {
    let s = scripted(gi_answer()).await;
    let cmdif = free_port();
    let _client = modules(&client_xml(s.port, cmdif));
    until("the GI's data", Duration::from_secs(5), || {
        s.got.lock().unwrap().len() >= 22
    })
    .await;
    // STARTDT act and the GI back to back, before the STARTDT con (GC:5-6)
    let start = "68 04 07 00 00 00 68 0e 00 00 00 00 64 01 06 03 01 00 00 00 00 14";
    assert_eq!(shows(&s.got.lock().unwrap()[..22]), start);
    let mut gi = String::new();
    for _ in 0..300 {
        gi = says(cmdif, "READ=I_bus-1.gen_mw").await;
        if gi != "ACK=I_bus-1.gen_mw:0.000000\\x00" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(gi, "ACK=I_bus-1.gen_mw:300.700012\\x00");
    assert_eq!(
        says(cmdif, "READ=O_generator-1_bus-1.active").await,
        "ACK=O_generator-1_bus-1.active:true\\x00"
    );
    // a write goes on the wire as an I-frame N(S) 1, N(R) 4 (the four I-frames received)
    let before = s.got.lock().unwrap().len();
    assert_eq!(
        says(cmdif, "WRITE=O_bus-1.gen_mw:42.5").await,
        "ACK=Wrote tag O_bus-1.gen_mw -- 42.5\\x00"
    );
    until("the command", Duration::from_secs(2), || {
        s.got.lock().unwrap().len() >= before + 20
    })
    .await;
    let sent = s.got.lock().unwrap()[before..].to_vec();
    let cmd = "68 12 02 00 08 00 32 01 06 03 01 00 02 00 00 00 00 2a 42 00";
    assert!(shows(&sent).contains(cmd), "{}", shows(&sent));
    // the server goes away; the listener stays, and is never connected to again
    s.close.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    for (req, want) in [
        ("READ=I_bus-1.gen_mw", "ACK=I_bus-1.gen_mw:300.700012\\x00"),
        (
            "WRITE=O_bus-1.gen_mw:9",
            "ACK=Wrote tag O_bus-1.gen_mw -- 9\\x00",
        ),
        ("READ=O_bus-1.gen_mw", "ACK=O_bus-1.gen_mw:9.000000\\x00"),
        (
            "WRITE=O_load-1_bus-1.active:true",
            "ACK=Wrote tag O_load-1_bus-1.active -- true\\x00",
        ),
        (
            "READ=O_load-1_bus-1.active",
            "ACK=O_load-1_bus-1.active:true\\x00",
        ),
        (
            "READ=nope",
            "ERR=Client does not have a mapping for tag 'nope'\\x00",
        ),
        // stock crashes on a request with no `=` (STOCK.md); ours answers
        (
            "nope",
            "ERR=Malformed request (must be QUERY=|READ=tag|WRITE=tag:value)\\x00",
        ),
    ] {
        assert_eq!(says(cmdif, req).await, want, "{req}");
    }
    // no server at start: one refused connect, the zero values, and no other attempt once a server listens
    let port = free_port();
    let cmdif2 = free_port();
    let logs = logs();
    let _late = modules(&client_xml(port, cmdif2));
    // the refused connect has happened before anything listens there; stock then prints `Exiting.` (MOD/ClientConnection.cpp:55)
    let refused = format!("RTU Connection: tcp://127.0.0.1:{port}");
    until("the refused connect", Duration::from_secs(2), || {
        let text = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
        text.find(&refused)
            .is_some_and(|at| text[at..].contains("Exiting."))
    })
    .await;
    assert_eq!(
        says(cmdif2, "READ=I_bus-1.gen_mw").await,
        "ACK=I_bus-1.gen_mw:0.000000\\x00"
    );
    assert_eq!(
        says(cmdif2, "WRITE=O_bus-1.gen_mw:1").await,
        "ACK=Wrote tag O_bus-1.gen_mw -- 1\\x00"
    );
    let l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let late = tokio::time::timeout(Duration::from_secs(2), l.accept()).await;
    assert!(late.is_err(), "the client connected again");
    assert_eq!(s.accepted.load(Ordering::SeqCst), 1);
}
