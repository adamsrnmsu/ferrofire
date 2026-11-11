//! Nothing a config, a logic line or a datagram says makes the field device panic: seeded mutations of a real config,
//! random bytes, and random datagrams. A failure repeats (fixed seed).
use ferrofire::datagram::Receiver;
use ferrofire::fd::config::{Config, Opts};
use ferrofire::fd::input;
use ferrofire::fd::store::{Layout, Store};
use ferrofire::model::Kind;

const RTU1: &str = include_str!("conformance/rtu-1.xml");

/// xorshift64*: a seeded generator, so a failure repeats.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn mutate(rng: &mut Rng, base: &str) -> String {
    let mut b = base.as_bytes().to_vec();
    for _ in 0..1 + rng.below(6) {
        if b.is_empty() {
            break;
        }
        let i = rng.below(b.len());
        match rng.below(4) {
            0 => b[i] = rng.next() as u8,
            1 => {
                b.remove(i);
            }
            2 => b.insert(i, b"<>/&=\"'(),*%#"[rng.below(13)]),
            _ => b.truncate(i.max(1)),
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

#[test]
fn mutated_configs_never_panic() {
    let mut rng = Rng(0x5eed);
    let with_logic = RTU1.replace("</field-device>", "<logic>uvalve = lock_h >= 3 && lgate == 1, delay:2\nlgate = (lock_h + 1) * 2 ** 3 % 5</logic></field-device>");
    for base in [RTU1, with_logic.as_str()] {
        for _ in 0..3000 {
            let xml = mutate(&mut rng, base);
            let _ = Config::parse(&xml, &[], &Opts::default());
            let _ = Config::parse(&xml, &[], &Opts { strict: true });
        }
    }
}

#[test]
fn random_text_never_panics_the_loader() {
    let mut rng = Rng(7);
    for _ in 0..3000 {
        let n = rng.below(200);
        let s: String = (0..n)
            .map(|_| char::from(rng.next() as u8 % 96 + 32))
            .collect();
        let _ = Config::parse(&s, &[], &Opts::default());
        let _ = Config::parse(
            &format!("<field-device><name>d</name><logic>{s}</logic></field-device>"),
            &[],
            &Opts::default(),
        );
    }
}

#[test]
fn random_datagrams_never_panic_the_receiver() {
    let store = Store::new(Layout {
        points: vec![
            ("a".into(), Kind::Analog, None),
            ("b".into(), Kind::Binary, None),
        ],
        external: vec![("ta".into(), "a".into()), ("tb".into(), "b".into())],
        internal: vec![],
    });
    let mut rx = Receiver::new("g", ["a", "b"]);
    let mut rng = Rng(11);
    let good: &[u8] = b"\x01ga:1,b:true,_ff.seq:1,\0";
    for _ in 0..20_000 {
        let mut d = good.to_vec();
        for _ in 0..rng.below(5) {
            let i = rng.below(d.len());
            d[i] = rng.next() as u8;
        }
        d.truncate(rng.below(d.len() + 1));
        input::apply(&mut rx, &store, &d, 1);
    }
    let n = store.snapshot().len();
    assert_eq!(n, 2);
}
