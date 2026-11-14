# Profiling the Rust provider and field device (ww-1df.34)

One profiling pass over five workload families with `perf`, then six prototype optimizations (A-F) measured against the
unmodified binary. No product code changed; the prototype patches are in [bench/profile/patches/](../bench/profile/patches/)
and the harness in [bench/profile/](../bench/profile/). Date: 2026-11-13.

## Headlines

- Big models are bound by per-tick overhead, not physics. On the 8112-tag model `Node::eval` is 43-53 % of provider CPU
  because `Model::step` evaluates the whole expression list several times per tick, and the snapshot is rebuilt as an
  owned `Vec<(String, Value)>` every tick (malloc/free about 20 %).
- On the idle 1250-branch grid the cost is publish bookkeeping: `publish_all` (per-target `HashSet<String>` filter plus
  String clones, SipHash) and `top::publish`.
- Under a 1000-breaker storm: `Power::get` (30 % self, from one full `PowerRt::sync` per write) and dense `lu_solve` (27 %).
- Measured wins worth doing: **E** QUERY reply cache (provider CPU 43 -> 25 %), **B** one power sync per write batch
  (storm 17.3 -> 11.7 %), **C** per-target index lists (fast grid 48.7 -> 39.5 %), **A** skip redundant evals (8112 tags
  26 -> 23 %, marginal). **D** (FNV top store) is marginal; **F** (bounded LU row update) gives nothing.
- Field-device cost under a Modbus write storm is mostly kernel TCP wakeups; nothing there is worth optimizing for a range.

## Method

- **Profiler.** Linux perf 5.15 (Ubuntu 22.04 `linux-tools-generic`) in image `ffprof-perf:22.04`
  ([Dockerfile.perf](../bench/profile/Dockerfile.perf): perf, libc6-dbg, binutils, brendangregg FlameGraph, python3), run
  `--privileged --pid=host` on the default Docker Desktop daemon (kernel linuxkit, 18 vCPU, 7.7 GB). There is no hardware PMU
  in the VM, so sampling is `-e cpu-clock -F 499 -g` (frame-pointer unwinding), one `perf` per PID, recorded concurrently
  ([rec.sh](../bench/profile/rec.sh)). `perf_event_paranoid` was set to -1 for the runs and restored to 2.
- **Build.** In `ff-rust:22.04` with `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`, `CARGO_TARGET_DIR=/src/target-prof`,
  `CARGO_PROFILE_RELEASE_DEBUG=true CARGO_PROFILE_RELEASE_STRIP=false RUSTFLAGS='-C force-frame-pointers=yes'`,
  `cargo build --release --features all-protocols --bin ferrofire`, plus `bench/client` (ffbench) and `bench/control`
  (ffcontrol). opt-level stays 2 as in `Cargo.toml`. `target-prof` is excluded via `.git/info/exclude` only.
- **Workloads** run the same binary in `ffprof-perf:22.04` (glibc 2.35, same ABI as `ff-rust:22.04`) on a docker network
  `ffprof`, repo mounted at `/src`. Steady-state windows were 40-46 s (25 s for the sweep).
- **Post-processing.** `c++filt` for v0 symbols, `flamegraph.pl` for SVGs, [top.py](../bench/profile/top.py) for self/total % from
  folded stacks ([post.sh](../bench/profile/post.sh)); `lines.py` for line level.
- **Reproduce.** `bench/profile/w1a.sh`, `w1b2.sh`, `w2.sh`, `w3.sh`, `w4.sh`, `w5.sh`, `w6.sh` (workloads), then
  `post.sh WORKLOAD NAME BINARY N`; `alt.sh`/`campaign.sh`/`mm.sh` for the base/variant alternation. There is no
  `make profile` target: a run is a multi-container sequence with host paths baked into `lib.sh`, so a target would be a
  wrapper around unportable scripts. Fix the paths first, and check `docker ps` so that no other heavy container runs.

### How far to trust the numbers

- Sample shares (self %) are of one process and reliable to a point or two; the large ones are solid. Functions under
  about 1-2 % are noise at this sample count (the waterway run has only 46 samples).
- Absolute CPU % of light loads is noisy in the Docker Desktop VM, about 2x (the same 8112-tag provider read 26 % alone and
  11.5 % with a field device attached, at identical sim-time advance; probably host DVFS). Between campaigns the grid-storm
  baseline moved from 17.3 to 19.2 %. Only base/variant pairs alternated in the same block are comparable, which is how
  section "Measured ideas" was done (3 pairs each).
- Sampling is `cpu-clock`: no cycles, IPC or cache counters. libc has no frame pointers, so the caller of a leaf libc
  function (malloc, memcmp) is sometimes skipped by unwinding and mis-attributed.
- A hotspot share overstates the gain: C removed what looked like 48 % of samples (`publish_all`) and gained 19 %.

## Workloads: CPU and hotspots

Flamegraphs are in [img/profiling/](img/profiling/) (open the SVG in a browser to search and zoom). Self % is of the process.

### W1a. Provider on the control-bench grid, idle ([svg](img/profiling/w1a-grid-idle.svg))

`bench/gen_grid.py` R10: 1250 branches, 10 generators, dt 0.05, 10 field devices, 10 per-RTU slices published every 0.1 s.
Provider 10.9-11.4 % of a core (about 5 ms of publish work per tick, no solves).

| Symbol | Self % | Note |
|---|---|---|
| `sip::Hasher::write` | 15.6 | SipHash of tag-name Strings in the `HashSet`/`HashMap` lookups (`publish_all`'s `contains`, plus `top`'s older path) |
| `hash_one::<&String>` | 10.8 (24.2 total) | one per tag per publish |
| `top::publish` (src/top.rs:150) | 10.8 (21.3 total) | per-tag lookup in a `BTreeMap` under one mutex, `name.clone()` on first insert |
| `memcmp` | 9.1 | key compares |
| `_int_malloc` / `_int_free` / `malloc_consolidate` | 8.9 / 5.2 / 2.9 | `owned_snapshot` Strings each tick, `publish_all` slice builders |
| `publish_all` (bennu.rs:550) | 0 (48.6 total) | per-slice `HashSet` filter plus cloned snapshot parts for each of 10 slices |
| `Radio::publish` -> `send_all` | 0 (13 total) | UDP `sendmsg`, `nft_do_chain` from the bridge netfilter (a Docker artifact) |

Correction to the first-pass reading: SipHash here comes from `publish_all`'s `HashSet::contains`, not from `top::publish`
(whose store is a `BTreeMap`; see D below).

### W1b. Provider on the grid under a breaker storm ([svg](img/profiling/w1b-grid-storm.svg))

ffcontrol, `breaker:sim:1000` repeated 400 times, `--settle-s 0.05` (1000 simultaneous flips then restore, about 3 cells/s),
Modbus polling at 2 ms. Provider 15.7 % of a core; 1000/1000 cells ok, 0 lost.

| Symbol | Self % | Note |
|---|---|---|
| `Power::get` (power/mod.rs:530) | 30.4 | `PowerRt::sync` calls it for every slot; `apply_write` runs `sync` once per write (model.rs:810), so about 1000 x 2900 slots; `bus_gen_sum` filters all gens per bus |
| `pf::lu_solve` (pf.rs:258) | 26.7 | dense O(n^3) LU of a roughly 500-unknown Jacobian per Newton iteration |
| `sip::Hasher::write` | 5.3 | as idle |
| `Model::apply_writes` | 4.5 (31.5 total) | storm-specific O(writes x tags) |
| `top::publish` | 3.8 (9.2 total) | as idle |
| `jacobian` closures, `matvec` | 1.6 + 1.5, 0.7 | dense assembly |

### W2a. Waterway model ([svg](img/profiling/w2a-waterway.svg))

`models/waterway.yaml`, 15 tags, dt 0.05, 10 Hz. 0.3 % of a core, 46 samples: indicative only. About 40 % is kernel
timer/epoll/multicast paths of a nearly idle process. Nothing to optimize.

### W2b. Provider on the 8112-tag model ([svg](img/profiling/w2b-big8112.svg))

`bench/gen_model.py 8112`, dt 0.01, publish 10 Hz. 26-27 % of a core alone (25.7-27.2 % over four runs).

| Symbol | Self % | Note |
|---|---|---|
| `expr::Node::eval` | 52.8 | tree walk over 8107 expressions; `Model::step` evaluates the list about 7 times per tick (apply_writes, step start, rk4 k1..k4, step end) for an ODE (`sim.t`) that needs 1 |
| `_int_malloc` / `_int_free` / `malloc` / `malloc_consolidate` | 8.2 / 5.7 / 3.4 / 2.3 | `owned_snapshot` (bennu.rs:530) allocates 8112 Strings per tick, plus `lock(sh).snap = snap.clone()` |
| `top::publish` | 2.2 | publish ticks only |
| `format_shortest_opt` | 1.8 | f64 text in `fmt_value` |

### W3. Field device on the 8112-tag stream under a Modbus write storm ([svg](img/profiling/w3-fd-modbus-storm.svg))

fd XML from `gen_fd.py` (8112 stream tags), `mbload.py`: 4 FC6 writers plus 4 FC3/FC4 pollers every 2 ms for 62 s, DNP3
integrity polls (`dnpload.py`, 50 ms) in the 10 Hz variant. Field device 68-70 % of a core (about 33k writes/s, 0 errors),
provider 12-14 %, python load generator 161 %.

| Symbol | Self % | Note |
|---|---|---|
| `__wake_up_sync_key` + `try_to_wake_up` (kernel) | 26.8 | one TCP receive wakeup per loopback write; syscall-bound, not user-space |
| `__kernel_clock_gettime` | 4.2 | `now_secs()`/`Instant` per request |
| `EventLog::log` (fd/eventlog.rs:73) | 0.5 (8.7 total) | formats stamp, `format!`, `clean()` per request even when the sink is `io::sink` |
| `Device::handle` (modbus/device.rs:131) | 0.9 (19 total) | decode, store, reply |
| `Store::Inner::get` | 1.1 (2.2 total) | SipHash lookup by name |

Stream decode of 8112 tags at 20 Hz is cheap (2.2 % of the fd). A real range polls near 1 Hz, so this is a synthetic ceiling.

### W4. Datagram and REQ/REP at 800 tags, 100 Hz ([provider svg](img/profiling/w4a-datagram-provider.svg), [REQ/REP svg](img/profiling/w4b-reqrep-provider.svg))

`bench/models/m800.yaml --rate 0.01`. 4a: provider 9.5 %, `ffbench dg` receiver 4.5 %. 4b: provider 30.4 % under about 16.7k
READ/s (p50 59 us), 11.4k QUERY/s (9.6 KB replies, p50 83 us) and 16.9k WRITE/s, while still publishing.

| Side | Symbol | Self % |
|---|---|---|
| 4a provider | `Node::eval` | 16.6 |
| 4a provider | `format_shortest_opt` (16.6 attributed to `fmt_value`) | 9.2 |
| 4a provider | `top::publish` | 7.0 |
| 4a provider | malloc/free family | 14.3 (20 total) |
| 4a receiver | `memchr_aligned` (line splitting in `Receiver::accept`) | 15.3 |
| 4a receiver | SipHash | 14.4 |
| 4a receiver | `parse_number`, UTF-8 checks | 12.0 |
| 4b provider | `__wake_up_sync_key` | 12.2 |
| 4b provider | `format_inner` (QUERY reply, per-tag `format!`) | 2.1 (22 total) |
| 4b provider | realloc/memcpy/finish_grow | 8.0 (10.8 total) |

The ffbench client CPU line for 4b is invalid (the client exited before the window ended).

### W5. ICCP/TASE.2 data-set polling of the field device ([svg](img/profiling/w5-fd-iccp.svg))

Scratch client (path-includes `tests/common/iccp_client.rs`): ReadSet of 100 members plus Read of 20 items, closed loop, about
8k each per second, plus a 1 s transfer set. Field device 52 % of a core, client 36 %, provider 12 %.

| Symbol | Total % | Note |
|---|---|---|
| `mms::flush` | 18.6 | socket send, wakeups (`__wake_up_sync_key` 8.1 self) |
| `mms::encode` / `Object::encode` | 14.8 / 12.2 | per-member allocation |
| `Device::request` | 8.0 | set lookup by name |
| read_set mapping closure | 27.9 | per-member name -> value |
| malloc/free family | 16 self (20 total) | |
| SipHash | 6.4 self | |

The IEC 61850 MMS server was not profiled (its client lives on the Lima daemon, which this work may not use).

### W6. Rate sweep (provider CPU, % of one core, 25 s windows)

| Workload | Setting | CPU % |
|---|---|---|
| Grid idle, dt / publish | 0.05 / 0.1, 0.02 / 0.05, 0.01 / 0.02, 0.005 / 0.01 | 10.9, 19.4, 31.2, 48.7 |
| Grid, 1000-flip storm | same four | 17.4, 23.8, 41.3, 59.6 |
| 8112 tags (dt 0.01), publish | 0.1, 0.05, 0.02, 0.01 s | 25.3, 28.1, 33.6, 35.9 |
| 800 tags, publish | 0.1, 0.05, 0.02, 0.01 s | 3.7, 4.4, 5.9, 8.2 |

The grid is roughly linear in 1/dt (about 10 % of a core per 0.005 s step), so one core saturates near dt 0.002-0.003. The big
model is already eval-bound at dt 0.01, so a faster publish adds little. The per-cell perf data exist but were not
post-processed into tables; `post.sh` renders any of them.

## Measured ideas

Each idea was prototyped in a throwaway worktree, built with the profile flags, and run alternately base/variant 3 times per
workload (one container at a time, provider CPU, no perf). Patches: [bench/profile/patches/](../bench/profile/patches/).

| | Idea | Before -> after | Verdict |
|---|---|---|---|
| E | Cache the QUERY reply in `Meta` (800 tags, 9.6 KB) | QUERY phase CPU 42.9/43.1/43.5 -> 24.2/24.7/24.8 %; 11.7k -> 15.4k/s; p50 79 -> 58 us, p99 about 150 -> 95 us. READ phase (control) unchanged at about 24.5 % | **Worth doing** |
| B | One power `sync` per write batch, not per write (model.rs) | Storm dt 0.05: 16.9/17.3/17.6 -> 11.7/11.7/11.7 % (-32 %). Fast storm (dt 0.005): 63.0/59.4/57.4 -> 55.8/56.0/56.7 %. 1000-flip span p50 0.081-0.089 -> 0.072 s at fast dt; at dt 0.05 it stays 0.120 s (tick-bound). All cells 1000/1000 | **Worth doing** |
| C | Per-target snapshot index lists plus `write!` `snapshot_msg` | Fast grid idle: 48.8/48.6/48.6 -> 40.2/38.9/39.3 % (-19 %). Fast storm about 60 -> 47 % (4 of 6 runs read cleanly). m800 at publish 0.01: 8.1/8.5/7.9 -> 7.3/7.5/7.3 % (-9 %) | **Worth doing**; the prototype caches indices and assumes a fixed snapshot order, so resolve at target construction and add an old-vs-new byte-equality test |
| A | Skip redundant full evals (`fresh` flag: apply_writes, step top, rk4 k1) | 8112 tags: 25.6/25.9/27.3/25.4 -> 22.3/23.6/23.8/22.4 % (about -11 %). 7 evals -> 4 per tick; allocator churn remains. A differential test (2000 steps, random writes, 3 models) is bit-identical | **Marginal to worth doing**. The ODE-cone variant was not built |
| D | `top::publish` with FNV `HashMap`, sorted on read | Fast grid idle: 48.8/50.4/50.2 -> 43.2/44.4/45.2 % (about -11 %, separate campaigns) | **Marginal**, not worth the complexity alone |
| F | Bounded row update in `lu_eliminate` (bit-identical) | Storm: 19.2/18.8/19.5 -> 19.6/18.7/18.4 %; fast storm 60.0 vs 60.4 | **Not worth it**: fill-in is dense, so the trailing-zero bound does nothing. A real win needs sparse LU with ordering |

Tests run on the variants: tests/bennu (24, E and C), lib model (A: 12; B: 9), power, power_ramp, power_tags, pypower golden
(F), lib top and tests/gt (D).

### Claims in the first-pass idea list that did not hold

- "Skip `top::publish` unless a `top` client is attached" is wrong: `src/gt.rs` reads `top::tags()` for the ground-truth page.
- "`top::publish` causes the SipHash cost" is wrong: its store is a `BTreeMap`; the hashing is `publish_all`'s `HashSet::contains`.
  `top::publish` is 10.8 % inclusive on the idle grid, not 21 %.
- Per-idea gains read off sample shares were all too high (see above).

### Not measured

- Sparse LU, chord Newton and the ODE-dependency-cone eval were not prototyped (solver work is size L; F shows the cheap rung is
  useless).
- Snapshot as `Vec<Value>` with interned names (medium refactor overlapping A and C), `sendmmsg` (kernel UDP and netfilter
  dominate, and the netfilter cost is a Docker-bridge artifact), the REP-loop plumbing, and the field-device ideas (EventLog,
  store, tokio worker count, ICCP set resolution): skipped because kernel TCP wakeups dominate a synthetic 33k writes/s load and
  the ICCP load needs a custom client; any win would be within noise at range rates.
- The IEC 61850 MMS server, hardware counters, and any cross-campaign absolute comparison.
- C's index cache has no old-vs-new byte-for-byte test beyond the existing tests/bennu.
