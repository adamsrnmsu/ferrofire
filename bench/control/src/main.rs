//! ffcontrol: the SCADA driver of the control-latency bench (ww-1df.23.3, epic ww-1df.23). One process, one clock (wall clock
//! in unix ns; the bench containers share the VM kernel clock), run in a container on the bench network.
//!
//! Per phase it issues Modbus FC5 coil writes (breaker open, generator spin-down / spin-up) through persistent per-RTU
//! connections, polls discrete inputs (and the PG input registers for generators) at --poll-ms on a second connection per RTU,
//! and listens on each RTU's multicast stream, stamping the first datagram that carries each command's new state. Per command it
//! records t_cmd (write sent), t_ack, t_echo (first read-back showing the new state), t_stream, t_visible (first read-back showing
//! it at or after t_stream), t_setpoint (generators: PG within 1% of the setpoint). t_echo is separate because the field device
//! sets a point when the provider ACKs the WRITE (queued, not applied): the read-back flips before the provider has applied it.
//! The provider's own apply/solve stamps (`ferrofire run --stamp-writes`) are joined afterwards by bench/control_join.py.
//!
//! `ffcontrol --scada R5/scada.json --provider tcp://provider:5555 --stream 'udp://*;239.0.1.{n}:40000' --cells breaker:seq:5,gen:sim:2 --out /out/run`
//! `--read-per 1 --legacy --status-only --fresh` for stock bennu: one tag per provider READ, stream bodies without the NUL ferrofire adds, generators are status flips (no PG ramp to wait for), one cell per stack so no restore afterwards. Writes `/out/run.cmds.csv` (one row per command) and `/out/run.cells.csv` (one row per phase). Exit 3: a restore failed.
mod modbus;
use ferrofire::bennu::{group_for, parse_udp};
use ferrofire::datagram::Receiver;
use ferrofire::transport::{requester, Requester};
use modbus::{bit, Mb};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
}

const CMD: usize = 0;
const ACK: usize = 1;
const ECHO: usize = 2;
const STREAM: usize = 3;
const VISIBLE: usize = 4;
const SETPOINT: usize = 5;
const STAGES: [&str; 6] = [
    "t_cmd",
    "t_ack",
    "t_echo",
    "t_stream",
    "t_visible",
    "t_setpoint",
];

/// One command and what has been seen of it. Timestamps are unix ns, 0 = not seen.
struct Cmd {
    rtu: usize,
    id: usize,
    seq: usize,
    tag: String,
    coil: u16,
    di: u16,
    /// Generators: PG input register index within the RTU's block, and the setpoint it must reach (or leave).
    pg: Option<(usize, f64)>,
    to: bool,
    t: [AtomicU64; 6],
    flaps: AtomicU32,
    /// 0 = nothing seen, 1 = last read-back was the new state, 2 = it was not.
    last: AtomicU8,
}

fn set_once(a: &AtomicU64, v: u64) {
    let _ = a.compare_exchange(0, v, Relaxed, Relaxed);
}

/// PG within 1% of the setpoint (spin-up) or at most 1% of it (spin-down).
fn pg_ok(pg: f64, sp: f64, up: bool) -> bool {
    if up {
        (pg - sp).abs() <= 0.01 * sp
    } else {
        pg <= 0.01 * sp
    }
}

/// A PG input register (16-bit over [0, hi] MW; the grid's generators are 0..100, scada.json `range`).
fn reg_mw(r: u16, hi: f64) -> f64 {
    r as f64 / 65535.0 * hi
}

/// One read-back of a command's point (and PG for a generator) at `now`.
fn observe(c: &Cmd, now: u64, di: bool, pg: Option<f64>) {
    if c.t[CMD].load(Relaxed) == 0 {
        return;
    }
    if di == c.to {
        set_once(&c.t[ECHO], now);
        let s = c.t[STREAM].load(Relaxed);
        if s != 0 && now >= s {
            set_once(&c.t[VISIBLE], now);
        }
        c.last.store(1, Relaxed);
    } else if c.last.swap(2, Relaxed) == 1 {
        c.flaps.fetch_add(1, Relaxed); // the new state was shown and then taken back
    }
    if let (Some(pg), Some((_, sp))) = (pg, c.pg) {
        if pg_ok(pg, sp, c.to) {
            set_once(&c.t[SETPOINT], now);
        }
    }
}

/// A datagram at `now` carrying `tag = v`: the first one with the new state, after the command was sent, is t_stream.
fn on_pair(c: &Cmd, now: u64, v: f64) {
    let sent = c.t[CMD].load(Relaxed);
    if sent != 0 && now >= sent && (v != 0.0) == c.to {
        set_once(&c.t[STREAM], now);
    }
}

/// How long after t_cmd a command may take: the timeout, plus the ramp allowance for a generator.
fn limit(c: &Cmd, timeout_ns: u64, gen_extra_ns: u64) -> u64 {
    timeout_ns + if c.pg.is_some() { gen_extra_ns } else { 0 }
}

/// When the command finished (t_visible, and t_setpoint for a generator, whichever is later); 0 while it has not.
fn finished_at(c: &Cmd) -> u64 {
    c.t[VISIBLE].load(Relaxed).max(c.t[SETPOINT].load(Relaxed))
}

/// Done, and within its limit: one that completes later than that (during the tail wait or the settle) is lost, not ok.
fn done_in_time(c: &Cmd, timeout_ns: u64, gen_extra_ns: u64) -> bool {
    done(c) && finished_at(c) <= c.t[CMD].load(Relaxed) + limit(c, timeout_ns, gen_extra_ns)
}

fn done(c: &Cmd) -> bool {
    c.t[VISIBLE].load(Relaxed) != 0 && (c.pg.is_none() || c.t[SETPOINT].load(Relaxed) != 0)
}

// ---- the register map and the commands to issue -----------------------------------------------------------------

struct Pt {
    id: usize,
    tag: String,
    coil: u16,
    di: u16,
    pg: Option<(usize, f64)>,
}

struct Rtu {
    host: String,
    breakers: Vec<Pt>,
    gens: Vec<Pt>,
}

struct Scada {
    rtus: Vec<Rtu>,
    flip_order: Vec<usize>,
    gen_order: Vec<usize>,
    sp: f64,
    port: u16,
}

fn u(v: &Value) -> usize {
    v.as_u64().expect("number in scada.json") as usize
}

fn load_scada(path: &str) -> Scada {
    let j: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("scada.json"))
        .expect("scada.json is JSON");
    let sp = j["gen_setpoint"].as_f64().expect("gen_setpoint");
    let list = |v: &Value| {
        v.as_array()
            .expect("array")
            .iter()
            .map(u)
            .collect::<Vec<_>>()
    };
    let pts = |v: &Value, key: &str, gen: bool| -> Vec<Pt> {
        v.as_array()
            .expect("array")
            .iter()
            .map(|p| Pt {
                id: u(&p["idx"]),
                tag: p[key].as_str().expect("point").to_string(),
                coil: u(&p["coil"]) as u16,
                di: u(&p["di"]) as u16,
                pg: gen.then(|| (u(&p["ir"]) - 30000, p["range"][1].as_f64().expect("range"))),
            })
            .collect()
    };
    Scada {
        rtus: j["rtus"]
            .as_array()
            .expect("rtus")
            .iter()
            .map(|r| Rtu {
                host: r["host"].as_str().expect("host").to_string(),
                breakers: pts(&r["breakers"], "point", false),
                gens: pts(&r["gens"], "status_point", true),
            })
            .collect(),
        flip_order: list(&j["flip_order"]),
        gen_order: list(&j["gen_order"]),
        sp,
        port: u(&j["port"]) as u16,
    }
}

/// `n` ids of `order`, spread evenly over the RTUs: first the earliest ids with no RTU over ceil(n / R), then, if some RTU has
/// fewer than that to give, the earliest unused ids of the others. Returns per-RTU lists in `order` order.
fn pick(order: &[usize], owner: &HashMap<usize, usize>, nr: usize, n: usize) -> Vec<Vec<usize>> {
    let quota = n.div_ceil(nr);
    let mut count = vec![0; nr];
    let mut chosen = std::collections::HashSet::new();
    for id in order {
        if chosen.len() < n && count[owner[id]] < quota {
            count[owner[id]] += 1;
            chosen.insert(*id);
        }
    }
    chosen.extend(
        order
            .iter()
            .filter(|id| !chosen.contains(id))
            .take(n - chosen.len())
            .copied()
            .collect::<Vec<_>>(),
    );
    let mut by: Vec<Vec<usize>> = vec![vec![]; nr];
    for id in order.iter().filter(|id| chosen.contains(id)) {
        by[owner[id]].push(*id);
    }
    by
}

/// RTU 0's first, RTU 1's first, ... then the seconds: the order the sequential mode issues in.
fn interleave(by: &[Vec<usize>]) -> Vec<(usize, usize)> {
    let longest = by.iter().map(Vec::len).max().unwrap_or(0);
    (0..longest)
        .flat_map(|i| {
            by.iter()
                .enumerate()
                .filter_map(move |(r, l)| l.get(i).map(|&id| (r, id)))
        })
        .collect()
}

fn cmd(s: &Scada, r: usize, id: usize, seq: usize, gen: bool, to: bool) -> Cmd {
    let p = if gen {
        &s.rtus[r].gens
    } else {
        &s.rtus[r].breakers
    }
    .iter()
    .find(|p| p.id == id)
    .expect("point of the RTU");
    Cmd {
        rtu: r,
        id,
        seq,
        tag: p.tag.clone(),
        coil: p.coil,
        di: p.di,
        pg: p.pg.map(|(i, _)| (i, s.sp)),
        to,
        t: Default::default(),
        flaps: AtomicU32::new(0),
        last: AtomicU8::new(0),
    }
}

/// The commands of a phase: `n` breakers (to `to`) or `n` generators, spread evenly over the RTUs, in issue order.
fn plan(s: &Scada, gen: bool, n: usize, to: bool) -> Vec<Cmd> {
    let (order, owner): (&[usize], HashMap<usize, usize>) = if gen {
        (
            s.gen_order.as_slice(),
            s.rtus
                .iter()
                .enumerate()
                .flat_map(|(r, t)| t.gens.iter().map(move |p| (p.id, r)))
                .collect(),
        )
    } else {
        (
            s.flip_order.as_slice(),
            s.rtus
                .iter()
                .enumerate()
                .flat_map(|(r, t)| t.breakers.iter().map(move |p| (p.id, r)))
                .collect(),
        )
    };
    let by = pick(order, &owner, s.rtus.len(), n);
    interleave(&by)
        .into_iter()
        .enumerate()
        .map(|(seq, (r, id))| cmd(s, r, id, seq, gen, to))
        .collect()
}

// ---- running a phase --------------------------------------------------------------------------------------------

struct Ctx {
    s: Scada,
    writers: Vec<Mb>,
    pollers: Vec<Mb>,
    prov: Box<dyn Requester>,
    /// Tags per provider READ (--read-per; 100, or 1 for stock bennu).
    read_per: usize,
    /// Stream bodies without the NUL terminator (stock bennu; --legacy).
    legacy: bool,
    /// One cell per stack (--fresh): no restore afterwards.
    fresh: bool,
    socks: Vec<(String, UdpSocket)>,
    poll: Duration,
    timeout_ns: u64,
    settle: Duration,
    gen_extra_ns: u64,
}

#[derive(Default)]
struct Errs {
    write: AtomicU32,
    poll: AtomicU32,
}

fn poll_rtu(
    mb: &mut Mb,
    cmds: &[Cmd],
    mine: &[usize],
    every: Duration,
    stop: &AtomicBool,
    errs: &Errs,
) {
    let n_di = mine.iter().map(|&i| cmds[i].di).max().map_or(0, |d| d + 1);
    let n_ir = mine
        .iter()
        .filter_map(|&i| cmds[i].pg)
        .map(|(k, _)| k as u16 + 1)
        .max();
    while !stop.load(Relaxed) {
        let t0 = Instant::now();
        let got = mb
            .read_bits(0, n_di)
            .and_then(|b| Ok((b, n_ir.map(|n| mb.read_regs(30000, n)).transpose()?)));
        match got {
            Ok((bits, regs)) => {
                let now = now_ns();
                for &i in mine {
                    let c = &cmds[i];
                    let pg =
                        c.pg.zip(regs.as_ref())
                            .map(|((k, _), r)| reg_mw(r[k], 100.0));
                    observe(c, now, bit(&bits, c.di as usize), pg);
                }
            }
            Err(_) => {
                errs.poll.fetch_add(1, Relaxed);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        std::thread::sleep(every.saturating_sub(t0.elapsed()));
    }
}

fn listen(
    sock: &UdpSocket,
    group: &str,
    cmds: &[Cmd],
    by_tag: &HashMap<&str, usize>,
    stop: &AtomicBool,
    legacy: bool,
) {
    let mut rx = Receiver::new(group, by_tag.keys().copied());
    let mut buf = vec![0u8; 65536];
    while !stop.load(Relaxed) {
        let Ok(n) = sock.recv(&mut buf) else { continue };
        let now = now_ns();
        // stock bennu's RADIO bodies carry no terminating NUL (ferrofire's do): add it so the Receiver takes them
        let n = if legacy && n < buf.len() && buf[n - 1] != 0 {
            buf[n] = 0;
            n + 1
        } else {
            n
        };
        for (tag, v) in rx.accept(&buf[..n]).unwrap_or_default() {
            if let Some(&i) = by_tag.get(tag.as_str()) {
                on_pair(&cmds[i], now, v);
            }
        }
    }
}

fn issue(mb: &mut Mb, c: &Cmd, errs: &Errs) {
    c.t[CMD].store(now_ns(), Relaxed);
    match mb.write_coil(c.coil, c.to) {
        Ok(()) => c.t[ACK].store(now_ns(), Relaxed),
        Err(_) => {
            errs.write.fetch_add(1, Relaxed);
        }
    }
}

/// Wait until every issued command is done or past its own timeout (a command never issued counts as finished).
fn wait_all(cmds: &[Cmd], to_ns: &dyn Fn(&Cmd) -> u64) {
    while cmds
        .iter()
        .any(|c| !done(c) && now_ns() <= c.t[CMD].load(Relaxed) + to_ns(c))
    {
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn run_phase(ctx: &mut Ctx, cmds: &[Cmd], sim: bool) -> (u32, u32) {
    let Ctx {
        s,
        writers,
        pollers,
        socks,
        legacy,
        poll,
        timeout_ns,
        settle,
        gen_extra_ns,
        ..
    } = ctx;
    let by_tag: HashMap<&str, usize> = cmds
        .iter()
        .enumerate()
        .map(|(i, c)| (c.tag.as_str(), i))
        .collect();
    let by_rtu: Vec<Vec<usize>> = (0..s.rtus.len())
        .map(|r| (0..cmds.len()).filter(|&i| cmds[i].rtu == r).collect())
        .collect();
    let (stop, errs) = (AtomicBool::new(false), Errs::default());
    let go = std::sync::Barrier::new(by_rtu.iter().filter(|m| !m.is_empty()).count());
    let to = |c: &Cmd| limit(c, *timeout_ns, *gen_extra_ns);
    std::thread::scope(|sc| {
        for (mb, mine) in pollers
            .iter_mut()
            .zip(&by_rtu)
            .filter(|(_, m)| !m.is_empty())
        {
            let (stop, errs, every) = (&stop, &errs, *poll);
            sc.spawn(move || poll_rtu(mb, cmds, mine, every, stop, errs));
        }
        for (ep, sock) in socks.iter() {
            let (stop, by_tag, group, legacy) = (&stop, &by_tag, group_for(ep), *legacy);
            sc.spawn(move || listen(sock, &group, cmds, by_tag, stop, legacy));
        }
        std::thread::sleep(Duration::from_millis(100)); // pollers and listeners are up before the first command
        if sim {
            let hs: Vec<_> = writers
                .iter_mut()
                .zip(&by_rtu)
                .filter(|(_, m)| !m.is_empty())
                .map(|(mb, mine)| {
                    let (errs, go) = (&errs, &go);
                    sc.spawn(move || {
                        go.wait();
                        mine.iter().for_each(|&i| issue(mb, &cmds[i], errs));
                    })
                })
                .collect();
            hs.into_iter().for_each(|h| h.join().expect("writer"));
        } else {
            for c in cmds {
                issue(&mut writers[c.rtu], c, &errs);
                wait_all(std::slice::from_ref(c), &to);
            }
        }
        wait_all(cmds, &to);
        std::thread::sleep(*settle); // pollers keep watching: a late revert shows as a flap or a wrong final state
        stop.store(true, Relaxed);
    });
    (errs.write.load(Relaxed), errs.poll.load(Relaxed))
}

/// Every RTU's breaker and generator status bits and its generators' PG (MW), read on the polling connections.
fn snapshot(ctx: &mut Ctx) -> Vec<(Vec<bool>, Vec<f64>)> {
    let Ctx { s, pollers, .. } = ctx;
    s.rtus
        .iter()
        .zip(pollers.iter_mut())
        .map(|(r, mb)| {
            let (nb, ng) = (r.breakers.len(), r.gens.len());
            let bits = mb
                .read_bits(0, (nb + ng) as u16)
                .expect("snapshot: read discrete inputs");
            let pg = if ng > 0 {
                mb.read_regs(30000, ng as u16)
                    .expect("snapshot: read registers")
            } else {
                vec![]
            };
            (
                (0..nb + ng).map(|i| bit(&bits, i)).collect(),
                pg.iter().map(|&x| reg_mw(x, 100.0)).collect(),
            )
        })
        .collect()
}

/// READ in requests of `per` tags (`READ=a,b,`); stock bennu answers one tag per READ, so `per` = 1 sends `READ=a`.
fn prov_read(p: &mut dyn Requester, tags: &[&str], per: usize) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for chunk in tags.chunks(per) {
        let r = p
            .call(format!("READ={}{}", chunk.join(","), if per > 1 { "," } else { "" }).as_bytes())
            .expect("provider READ");
        let r = String::from_utf8_lossy(&r)
            .trim_end_matches('\0')
            .to_string();
        let body = r
            .strip_prefix("ACK=")
            .unwrap_or_else(|| panic!("provider READ: {r}"));
        out.extend(
            body.split(',')
                .filter_map(|p| p.split_once(':'))
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
    }
    out
}

fn baseline_ok(s: &Scada, snap: &[(Vec<bool>, Vec<f64>)]) -> bool {
    s.rtus.iter().zip(snap).all(|(r, (bits, pg))| {
        bits.iter().all(|&b| b)
            && r.gens
                .iter()
                .enumerate()
                .all(|(i, g)| g.pg.is_none() || pg_ok(pg[i], s.sp, true))
    })
}

fn await_baseline(ctx: &mut Ctx) -> bool {
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        let snap = snapshot(ctx);
        if baseline_ok(&ctx.s, &snap) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

// ---- results ----------------------------------------------------------------------------------------------------

/// Adjacent commands of one RTU (issue order) that became visible in the opposite order.
fn order_violations(cmds: &[Cmd]) -> usize {
    let nr = cmds.iter().map(|c| c.rtu + 1).max().unwrap_or(0);
    (0..nr)
        .map(|r| {
            let v: Vec<u64> = cmds
                .iter()
                .filter(|c| c.rtu == r)
                .map(|c| c.t[VISIBLE].load(Relaxed))
                .filter(|&t| t != 0)
                .collect();
            v.windows(2).filter(|w| w[1] < w[0]).count()
        })
        .sum()
}

fn load1() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split(' ').next().map(String::from))
        .unwrap_or_default()
}

struct Meta {
    stack: String,
    r: usize,
    rep: String,
    label: String,
    mode: &'static str,
    kind: &'static str,
}

fn run_cell(
    ctx: &mut Ctx,
    m: &Meta,
    n: usize,
    cmds_csv: &mut impl Write,
    cells_csv: &mut impl Write,
) -> bool {
    let gen = m.kind == "gen";
    let phases: &[(&str, bool)] = if gen {
        &[("down", false), ("up", true)]
    } else {
        &[("open", false)]
    };
    let sim = m.mode == "sim";
    for &(dir, to) in phases {
        let before = snapshot(ctx);
        let cmds = plan(&ctx.s, gen, n, to);
        let (t_start, load_start) = (now_ns(), load1());
        let (werr, perr) = run_phase(ctx, &cmds, sim);
        let (t_end, after) = (now_ns(), snapshot(ctx));
        let tags: Vec<&str> = cmds.iter().map(|c| c.tag.as_str()).collect();
        let prov = prov_read(&mut *ctx.prov, &tags, ctx.read_per);
        let (mut ok, mut lost, mut wrong, mut wrong_prov, mut flaps) = (0, 0, 0, 0, 0);
        let mut touched: Vec<Vec<bool>> =
            before.iter().map(|(b, _)| vec![false; b.len()]).collect();
        for c in &cmds {
            touched[c.rtu][c.di as usize] = true;
            let f = c.flaps.load(Relaxed);
            flaps += f;
            let d = done_in_time(c, ctx.timeout_ns, ctx.gen_extra_ns);
            if d {
                ok += 1;
                let fin = after[c.rtu].0[c.di as usize] == c.to
                    && c.pg
                        .is_none_or(|(k, sp)| pg_ok(after[c.rtu].1[k], sp, c.to));
                wrong += (!fin) as u32;
                wrong_prov += (prov.get(&c.tag).map(String::as_str)
                    != Some(c.to.to_string().as_str())) as u32;
            } else {
                lost += 1;
            }
            let t: Vec<String> = c.t.iter().map(|a| a.load(Relaxed).to_string()).collect();
            writeln!(
                cmds_csv,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                m.stack,
                m.r,
                m.rep,
                m.label,
                m.kind,
                dir,
                c.seq,
                c.rtu,
                c.id,
                c.tag,
                c.to as u8,
                t.join(","),
                f,
                d as u8
            )
            .unwrap();
        }
        let collateral = before
            .iter()
            .zip(&after)
            .enumerate()
            .map(|(r, (b, a))| {
                (0..b.0.len())
                    .filter(|&i| !touched[r][i] && b.0[i] != a.0[i])
                    .count()
            })
            .sum::<usize>();
        let wall = (t_end - t_start) as f64 / 1e9;
        let first = cmds
            .iter()
            .map(|c| c.t[CMD].load(Relaxed))
            .filter(|&t| t != 0)
            .min()
            .unwrap_or(t_start);
        let last = cmds.iter().map(finished_at).max().unwrap_or(0);
        let span = if last > first {
            (last - first) as f64 / 1e9
        } else {
            0.0
        };
        let load = format!("{load_start}/{}", load1());
        writeln!(
            cells_csv,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.3},{:.3},{},{},{}",
            m.stack,
            m.r,
            m.rep,
            m.label,
            m.mode,
            m.kind,
            dir,
            n,
            cmds.len(),
            ok,
            lost,
            wrong,
            wrong_prov,
            collateral,
            order_violations(&cmds),
            flaps,
            werr,
            span,
            wall,
            if span > 0.0 {
                format!("{:.2}", ok as f64 / span)
            } else {
                String::new()
            },
            perr,
            load
        )
        .unwrap();
        cmds_csv.flush().unwrap();
        cells_csv.flush().unwrap();
        eprintln!("{} {dir}: {ok}/{} done, lost {lost}, wrong {wrong}/{wrong_prov}, flaps {flaps}, {span:.2} s", m.label, cmds.len());
    }
    if ctx.fresh {
        return true; // the stack is thrown away after this cell: nothing to restore
    }
    // restore: every commanded point back to its baseline (breaker closed, generator on at the setpoint), unrecorded
    let cmds = plan(&ctx.s, gen, n, true);
    run_phase(ctx, &cmds, true);
    cmds.iter().all(done) && await_baseline(ctx)
}

fn connect_all(s: &Scada) -> Vec<Mb> {
    s.rtus
        .iter()
        .map(|r| {
            (0..50)
                .find_map(|_| {
                    Mb::connect(&r.host, s.port).ok().or_else(|| {
                        std::thread::sleep(Duration::from_millis(200));
                        None
                    })
                })
                .unwrap_or_else(|| panic!("cannot connect to {}", r.host))
        })
        .collect()
}

fn open_stream(ep: &str) -> UdpSocket {
    let (_, addr) = parse_udp(ep).expect("stream endpoint");
    let sock = UdpSocket::bind(addr).expect("bind stream group");
    if let std::net::IpAddr::V4(ip) = addr.ip() {
        sock.join_multicast_v4(&ip, &Ipv4Addr::UNSPECIFIED)
            .expect("join stream group");
    }
    sock.set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    sock
}

fn arg(args: &[String], flag: &str, default: &str) -> String {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map_or(default.to_string(), |w| w[1].clone())
}

/// `breaker:seq:5` -> (kind, mode, n)
fn parse_cell(c: &str) -> (&'static str, &'static str, usize) {
    let p: Vec<&str> = c.split(':').collect();
    let kind = match p[0] {
        "breaker" => "breaker",
        "gen" => "gen",
        k => panic!("cell kind {k}"),
    };
    let mode = match p[1] {
        "seq" => "seq",
        "sim" => "sim",
        m => panic!("cell mode {m}"),
    };
    (kind, mode, p[2].parse().expect("cell count"))
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut s = load_scada(&arg(&a, "--scada", "scada.json"));
    if a.iter().any(|x| x == "--status-only") {
        // generators are status flips only: no PG register is polled or required (stock pybennu has no ramp and keeps PG at 0 after a spin-down)
        s.rtus.iter_mut().flat_map(|r| &mut r.gens).for_each(|g| g.pg = None);
    }
    let nr = s.rtus.len();
    let stream = arg(&a, "--stream", "udp://*;239.0.1.{n}:40000");
    let eps: BTreeMap<String, ()> = (0..nr)
        .map(|r| (stream.replace("{n}", &(r + 1).to_string()), ()))
        .collect();
    let secs = |f: &str, d: &str| (arg(&a, f, d).parse::<f64>().expect(f) * 1e9) as u64;
    let mut ctx = Ctx {
        writers: connect_all(&s),
        pollers: connect_all(&s),
        prov: requester(&arg(&a, "--provider", "tcp://provider:5555"), 5000)
            .expect("provider endpoint"),
        legacy: a.iter().any(|x| x == "--legacy"),
        fresh: a.iter().any(|x| x == "--fresh"),
        read_per: arg(&a, "--read-per", "100").parse().expect("--read-per"),
        socks: eps.keys().map(|e| (e.clone(), open_stream(e))).collect(),
        poll: Duration::from_micros(
            (arg(&a, "--poll-ms", "2").parse::<f64>().expect("--poll-ms") * 1000.0) as u64,
        ),
        timeout_ns: secs("--timeout-s", "10"),
        settle: Duration::from_secs_f64(arg(&a, "--settle-s", "1").parse().expect("--settle-s")),
        gen_extra_ns: secs("--gen-extra-s", "2"),
        s,
    };
    let out = arg(&a, "--out", "run");
    let mut cmds_csv = std::fs::File::create(format!("{out}.cmds.csv")).expect("cmds csv");
    let mut cells_csv = std::fs::File::create(format!("{out}.cells.csv")).expect("cells csv");
    writeln!(
        cmds_csv,
        "stack,R,rep,cell,kind,dir,seq,rtu,id,tag,to,{},flaps,done",
        STAGES.join(",")
    )
    .unwrap();
    writeln!(cells_csv, "stack,R,rep,cell,mode,kind,dir,n,issued,ok,lost,wrong_final,wrong_provider,wrong_collateral,order_violations,flaps,write_errors,span_s,wall_s,per_s,poll_errors,vm_load1_start_end").unwrap();
    if !await_baseline(&mut ctx) {
        eprintln!(
            "baseline never reached: every breaker closed, every generator on at its setpoint"
        );
        std::process::exit(2);
    }
    for cell in arg(&a, "--cells", "").split(',').filter(|c| !c.is_empty()) {
        let (kind, mode, n) = parse_cell(cell);
        let m = Meta {
            stack: arg(&a, "--stack", "ferrofire"),
            r: nr,
            rep: arg(&a, "--rep", "1"),
            label: cell.to_string(),
            mode,
            kind,
        };
        if !run_cell(&mut ctx, &m, n, &mut cmds_csv, &mut cells_csv) {
            eprintln!("restore after {cell} failed: the stack is dirty");
            std::process::exit(3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scada(nr: usize) -> Scada {
        let pt = |id: usize, gen: bool| Pt {
            id,
            tag: format!("{}{id}", if gen { "g" } else { "b" }),
            coil: id as u16,
            di: id as u16,
            pg: gen.then_some((id, 100.0)),
        };
        Scada {
            rtus: (0..nr)
                .map(|r| Rtu {
                    host: format!("rtu-{r}"),
                    breakers: (r..20).step_by(nr).map(|i| pt(i, false)).collect(),
                    gens: (r..4).step_by(nr).map(|i| pt(i, true)).collect(),
                })
                .collect(),
            flip_order: vec![3, 4, 7, 2, 1, 6, 5, 8, 9, 10],
            gen_order: vec![0, 1, 2, 3],
            sp: 40.0,
            port: 502,
        }
    }

    #[test]
    fn pick_spreads_evenly_and_stops_at_n() {
        let owner: HashMap<usize, usize> = HashMap::from([
            (3, 1),
            (7, 1),
            (1, 1),
            (5, 1),
            (9, 1),
            (11, 1),
            (0, 0),
            (2, 0),
        ]);
        assert_eq!(
            pick(&[3, 7, 1, 5, 9, 11, 0, 2], &owner, 2, 4),
            [vec![0, 2], vec![3, 7]]
        );
        assert_eq!(
            pick(&[0, 1, 2], &HashMap::from([(0, 0), (1, 0), (2, 0)]), 1, 2),
            [vec![0, 1]]
        );
    }

    #[test]
    fn interleave_alternates_rtus() {
        assert_eq!(
            interleave(&[vec![0, 2, 4], vec![1]]),
            [(0, 0), (1, 1), (0, 2), (0, 4)]
        );
    }

    #[test]
    fn plan_orders_commands_round_robin_over_rtus() {
        let s = scada(2);
        let p = plan(&s, false, 4, false);
        assert_eq!(
            p.iter().map(|c| (c.rtu, c.id, c.seq)).collect::<Vec<_>>(),
            [(0, 4, 0), (1, 3, 1), (0, 2, 2), (1, 7, 3)]
        );
        assert!(p.iter().all(|c| !c.to && c.pg.is_none()));
        let g = plan(&s, true, 3, true);
        assert_eq!(
            g.iter().map(|c| (c.rtu, c.id)).collect::<Vec<_>>(),
            [(0, 0), (1, 1), (0, 2)]
        );
        assert!(g
            .iter()
            .all(|c| c.to && c.pg.map(|(_, sp)| sp) == Some(40.0)));
    }

    fn c(to: bool, pg: bool) -> Cmd {
        cmd(&scada(1), 0, if pg { 0 } else { 1 }, 0, pg, to)
    }

    #[test]
    fn observation_stages_follow_the_rules() {
        let k = c(false, false);
        observe(&k, 10, false, None); // not sent yet
        assert_eq!(k.t[ECHO].load(Relaxed), 0);
        k.t[CMD].store(20, Relaxed);
        observe(&k, 30, false, None); // echo before any datagram: not yet visible
        assert_eq!(
            (k.t[ECHO].load(Relaxed), k.t[VISIBLE].load(Relaxed)),
            (30, 0)
        );
        on_pair(&k, 25, 1.0); // old state: no
        on_pair(&k, 15, 0.0); // before the command: no
        assert_eq!(k.t[STREAM].load(Relaxed), 0);
        on_pair(&k, 40, 0.0);
        on_pair(&k, 45, 0.0);
        assert_eq!(k.t[STREAM].load(Relaxed), 40);
        observe(&k, 50, false, None);
        assert_eq!((k.t[VISIBLE].load(Relaxed), done(&k)), (50, true));
        observe(&k, 60, true, None); // taken back
        observe(&k, 70, false, None);
        assert_eq!((k.flaps.load(Relaxed), k.t[VISIBLE].load(Relaxed)), (1, 50));
    }

    #[test]
    fn a_generator_is_done_only_when_pg_is_at_the_setpoint() {
        let up = c(true, true);
        up.t[CMD].store(1, Relaxed);
        up.t[STREAM].store(2, Relaxed);
        observe(&up, 5, true, Some(10.0));
        assert!(!done(&up));
        observe(&up, 6, true, Some(39.7));
        assert_eq!((up.t[SETPOINT].load(Relaxed), done(&up)), (6, true));
        assert!(
            pg_ok(0.3, 40.0, false)
                && !pg_ok(0.5, 40.0, false)
                && !pg_ok(39.5, 40.0, true)
                && pg_ok(39.7, 40.0, true)
        );
    }

    #[test]
    fn a_command_finishing_after_its_limit_is_not_in_time() {
        let k = c(false, false);
        k.t[CMD].store(100, Relaxed);
        k.t[VISIBLE].store(600, Relaxed);
        k.t[STREAM].store(500, Relaxed);
        assert!(done_in_time(&k, 500, 0) && !done_in_time(&k, 499, 0));
        let g = c(true, true);
        g.t[CMD].store(100, Relaxed);
        g.t[VISIBLE].store(150, Relaxed);
        g.t[SETPOINT].store(1100, Relaxed);
        assert_eq!(finished_at(&g), 1100);
        assert!(
            !done_in_time(&g, 500, 400)
                && done_in_time(&g, 500, 500)
                && !done_in_time(&c(true, false), 5, 5)
        );
    }

    #[test]
    fn order_violations_count_inverted_visible_pairs_per_rtu() {
        let mk = |rtu: usize, v: u64| {
            let k = cmd(&scada(2), rtu, rtu, 0, false, false);
            k.t[VISIBLE].store(v, Relaxed);
            k
        };
        assert_eq!(
            order_violations(&[mk(0, 5), mk(1, 3), mk(0, 4), mk(1, 9), mk(0, 0)]),
            1
        );
        assert_eq!(parse_cell("gen:sim:10"), ("gen", "sim", 10));
        assert_eq!(reg_mw(65535, 100.0), 100.0);
    }
}
