//! Hostile input to the MMS server without a fuzzer (the property `fuzz/fuzz_targets/fd_iec61850.rs` checks with one): whatever
//! bytes arrive, in whatever chunks, nothing panics and a violation closes the association, not the server. Seeds are what a real
//! client (the `iec61850-rs` one: browse, read, write, report, control) sends through a recording proxy, then mutated.
#![cfg(feature = "proto-iec61850")]

mod common;

use ferrofire::fd::mms::step;
use ferrofire::fd::proto::iec61850::{Device, Server};
use ferrofire::fd::proto::Module;
use iec61850_rs::client::{Client, ControlModel, RcbSettings, TrgOps};
use iec61850_rs::common::{monotonic_now, Instant};
use iec61850_rs::proto::data::Value;
use iec61850_rs::proto::mms::association::{Association, AssociationConfig};
use iec61850_rs::Fc;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const XML: &str = r#"<SCEPTRE><field-device><name>t</name><tags>
<external-tag><name>bi</name><io>I0</io><type>binary</type></external-tag>
<external-tag><name>an</name><io>I1</io><type>analog</type></external-tag>
<external-tag><name>bo</name><io>O0</io><type>binary</type></external-tag>
<external-tag><name>ao</name><io>O1</io><type>analog</type></external-tag></tags>
<comms><iec61850-server><endpoint>tcp://127.0.0.1:PORT</endpoint><event-logging>/dev/null</event-logging>
<binary-input><tag>bi</tag></binary-input><analog-input><tag>an</tag></analog-input>
<binary-output><tag>bo</tag></binary-output><analog-output><tag>ao</tag></analog-output></iec61850-server></comms>
<input><endpoint>udp://239.0.0.1:40000</endpoint><binary><id>I0</id><name>p.b</name></binary><analog><id>I1</id><name>p.a</name></analog></input>
<output><endpoint>tcp://127.0.0.1:5555</endpoint><binary><id>O0</id><name>p.b</name></binary><analog><id>O1</id><name>p.c</name></analog></output>
</field-device></SCEPTRE>"#;

fn device() -> Device {
    let (srv, store) = common::load_as::<Server>(XML, "iec61850-server");
    srv.device(&store, None)
}

/// A forwarding proxy that remembers what the client sent, chunk by chunk.
fn proxy(to: u16, seen: Arc<Mutex<Vec<Vec<u8>>>>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", to)).unwrap();
        let mut back = s.try_clone().unwrap();
        let mut c2 = c.try_clone().unwrap();
        std::thread::spawn(move || std::io::copy(&mut back, &mut c2).map(|_| ()).unwrap_or(()));
        let mut buf = [0u8; 4096];
        while let Ok(n) = c.read(&mut buf) {
            if n == 0 || s.write_all(&buf[..n]).is_err() {
                break;
            }
            seen.lock().unwrap().push(buf[..n].to_vec());
        }
    });
    port
}

async fn record() -> Vec<Vec<u8>> {
    let sport = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (srv, store) =
        common::load_as::<Server>(&XML.replace("PORT", &sport.to_string()), "iec61850-server");
    let stop = CancellationToken::new();
    tokio::spawn(srv.run(store, stop.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let seen = Arc::new(Mutex::new(vec![]));
    let pport = proxy(sport, seen.clone());
    tokio::task::spawn_blocking(move || {
        let mut c = Client::connect(&format!("127.0.0.1:{pport}")).unwrap();
        let ld = "simpleIOGenericIO";
        for n in c.logical_device_directory(ld).unwrap().iter().take(40) {
            let _ = c.read(&format!("{ld}/{}", n.replace('$', ".")), Fc::MX);
        }
        let _ = c.read(&format!("{ld}/GGIO1.AnIn1.mag.f"), Fc::MX);
        let _ = c.enable_rcb(
            &format!("{ld}/LLN0$RP$EventsRCB01"),
            Fc::RP,
            &RcbSettings::new().with_trg_ops(TrgOps::EVENTS),
        );
        let _ = c
            .control(&format!("{ld}/GGIO1.SPCSO1"))
            .model(ControlModel::DirectNormal)
            .execute(&Value::Boolean(true));
        let _ = c.release();
    })
    .await
    .unwrap();
    stop.cancel();
    let out = seen.lock().unwrap().clone();
    out
}

/// One association over `chunks`: false if it ended (closed) before the last chunk (the release at the end closes it too).
fn drive(dev: &mut Device, chunks: &[Vec<u8>], now: Instant) -> (bool, usize) {
    let mut assoc = Association::server(AssociationConfig::default());
    let mut sent = 0;
    for (i, c) in chunks.iter().enumerate() {
        assoc.on_bytes(now, c);
        let (_, closed) = step(dev, 1, &mut assoc, now, &mut false);
        while let Some(p) = assoc.poll_transmit() {
            sent += p.len();
        }
        if closed {
            return (i + 1 >= chunks.len(), sent);
        }
    }
    (true, sent)
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..1 + rng.below(4) {
        match rng.below(5) {
            0 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            1 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] = rng.next() as u8;
            }
            2 if !b.is_empty() => b.truncate(rng.below(b.len())),
            3 => b.extend((0..rng.below(300)).map(|_| rng.next() as u8)),
            _ if b.len() > 2 => {
                let (i, j) = (rng.below(b.len()), rng.below(b.len()));
                b.swap(i, j);
            }
            _ => {}
        }
    }
    b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn mutated_client_transcripts_never_panic_and_the_server_keeps_answering() {
    let seeds = record().await;
    assert!(
        seeds.len() > 20,
        "a real client session was recorded: {}",
        seeds.len()
    );
    let mut dev = device();
    let now = monotonic_now();
    let (ok, clean) = drive(&mut dev, &seeds, now);
    assert!(
        ok && clean > 1000,
        "the unmutated session is served in full ({clean} bytes)"
    );

    let mut rng = Rng(0x61850);
    let mut closed = 0;
    for _ in 0..1500 {
        let mut chunks: Vec<Vec<u8>> = seeds
            .iter()
            .map(|s| {
                if rng.below(3) == 0 {
                    mutate(&mut rng, s)
                } else {
                    s.clone()
                }
            })
            .collect();
        match rng.below(4) {
            0 => chunks.truncate(rng.below(chunks.len())),
            1 => chunks.push((0..rng.below(2000)).map(|_| rng.next() as u8).collect()),
            _ => {}
        }
        if !drive(&mut dev, &chunks, now).0 {
            closed += 1;
        }
    }
    assert!(closed > 100, "hostile sessions are mostly closed: {closed}");
    let (ok, again) = drive(&mut dev, &seeds, now);
    assert!(
        ok && again.abs_diff(clean) < 64,
        "the same kind of answers after the abuse: {again} vs {clean}"
    );
}

#[test]
fn random_bytes_and_random_fragments_never_panic() {
    let mut dev = device();
    let mut rng = Rng(7);
    let now = monotonic_now();
    for _ in 0..2000 {
        let n = rng.below(3000);
        let mut junk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if rng.below(2) == 0 && junk.len() > 4 {
            junk[0] = 3; // a plausible TPKT header: version, reserved, length
            junk[1] = 0;
            let l = (junk.len() as u16).to_be_bytes();
            junk[2..4].copy_from_slice(&l);
        }
        let cut = rng.below(junk.len() + 1);
        let chunks = vec![junk[..cut].to_vec(), junk[cut..].to_vec()];
        let _ = drive(&mut dev, &chunks, now);
    }
}
