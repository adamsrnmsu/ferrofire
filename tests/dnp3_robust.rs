//! Hostile input to the DNP3 outstation without a fuzzer (the same property `fuzz/fuzz_targets/fd_dnp3.rs` checks with one):
//! whatever bytes arrive, in whatever chunks, nothing panics, memory stays bounded and a violation is a `Close`.
//! Seeds are the requests of the recorded stock transcripts, mutated.

mod common;

use ferrofire::fd::proto::dnp3::outstation::{Close, Conn, Device, Sink};
use std::time::Duration;

struct Null;
impl Sink for Null {
    fn binary(&mut self, _: &str, _: u16, _: bool) {}
    fn analog(&mut self, _: &str, _: u16, _: f32) {}
}

/// xorshift: the tests need to be repeatable, not strong.
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

fn device() -> Device {
    let xml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/conformance/dnp3/diff-server.xml"
    ))
    .unwrap();
    common::load(&xml.replace("dnp3-diff.log", "/dev/null"))
        .0
        .device()
}

fn seeds() -> Vec<Vec<u8>> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/conformance/dnp3/stock-diff.trace"
    ))
    .unwrap();
    text.lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|v| v[0] == "send")
        .map(|v| common::unhex(v[1].as_str().unwrap()))
        .collect()
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

/// Feed `data` in random chunks; a `Close` ends the connection and a new one starts, as the socket layer would.
fn drive(dev: &mut Device, conn: &mut Conn, rng: &mut Rng, data: &[u8], now: &mut Duration) {
    let mut rest = data;
    while !rest.is_empty() {
        let n = 1 + rng.below(rest.len().min(64));
        *now += Duration::from_millis(rng.below(900) as u64);
        match conn.feed(dev, &mut Null, &rest[..n], *now) {
            Ok(out) => assert!(out.len() < 1 << 20, "a reply of {} bytes", out.len()),
            Err(Close(_)) => {
                conn.cancel(dev);
                *conn = Conn::new();
            }
        }
        conn.tick(dev, *now);
        rest = &rest[n..];
    }
}

#[test]
fn mutated_transcripts_never_panic() {
    let (seeds, mut rng) = (seeds(), Rng(0x9E3779B97F4A7C15));
    assert!(seeds.len() > 100);
    let (mut dev, mut conn, mut now) = (device(), Conn::new(), Duration::ZERO);
    for _ in 0..6000 {
        let seed = &seeds[rng.below(seeds.len())];
        let data = mutate(&mut rng, seed);
        drive(&mut dev, &mut conn, &mut rng, &data, &mut now);
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0xD1B54A32D192ED03);
    let (mut dev, mut conn, mut now) = (device(), Conn::new(), Duration::ZERO);
    for _ in 0..3000 {
        let n = rng.below(600);
        let mut data: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if rng.below(2) == 0 && data.len() > 2 {
            data[0] = 0x05;
            data[1] = 0x64;
        }
        drive(&mut dev, &mut conn, &mut rng, &data, &mut now);
    }
}

/// Valid frames around random application bytes: the link layer accepts them, so the application layer sees the garbage.
#[test]
fn random_application_fragments_never_panic() {
    use ferrofire::fd::proto::dnp3::{link, transport};
    let mut rng = Rng(0x2545F4914F6CDD1D);
    let (mut dev, mut conn, mut now, mut seq) = (device(), Conn::new(), Duration::ZERO, 0u8);
    let valid = link::encode(0xC0, 10, 1, &[]);
    assert!(conn.feed(&mut dev, &mut Null, &valid, now).is_ok());
    for _ in 0..6000 {
        let n = rng.below(120);
        let mut app: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if app.len() >= 2 {
            app[0] = 0xC0 | (rng.next() as u8 & 0x0F);
            app[1] = [1u8, 2, 3, 4, 5, 6, 0, 23, rng.next() as u8][rng.below(9)];
        }
        let mut wire = vec![];
        for seg in transport::segment(&app, &mut seq) {
            wire.extend(link::encode(0xC4, 10, 1, &seg));
        }
        now += Duration::from_millis(300);
        match conn.feed(&mut dev, &mut Null, &wire, now) {
            Ok(_) => conn.tick(&mut dev, now),
            Err(Close(_)) => panic!("valid frames must not close the connection"),
        }
    }
}
