# Field-device microbench (ww-1df.41)

ferrofire `field-device` vs stock bennu `bennu-field-deviced` (bennu 6.0.0, the phenix-waterway-range overlay image `ffc-stock:22.04`) with **no real provider**: one scripted
provider process stands in for everything around the device, so only the device differs. Report: [docs/bench/field-device.md](../../docs/bench/field-device.md); raw: `docs/bench/results/fdmicro/`.

```
make bench-fdmicro                                  # = python3 bench/fdmicro/run.py: builds the harness in ff-rust, runs the whole matrix (about 2 h), finished runs are skipped
REPS=1 PROFILES=S PROTOS=mb OUTDIR=target-linux/fdmicro/smoke SECS=2 FLIPS=10 python3 bench/fdmicro/run.py     # a smoke run into a scratch dir
make charts                                         # docs/bench/field-device.md and docs/img/charts/12*.svg from the raw files (uv + matplotlib)
```

Needs the ferrofire release binary (`make linux`, `target-linux/release/ferrofire`) and the images `ff-rust:22.04` and `ffc-stock:22.04` (`bench/Dockerfile.stock`) on the **default** docker daemon.
cargo runs with `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`; never start it next to another heavy container. Env knobs are listed at the top of `run.py`.

## Parts

- `gen.py`: the configs. Per profile (S 15 tags, M 800, L 8112) one `<P>-mb.xml` (modbus-server) and one `<P>-dnp3.xml` (dnp3-server), **the same file for both devices**, plus `<P>.json` (the tag layout the harness reads).
  Same stream endpoint `udp://*;239.0.0.1:40000`, same provider endpoint `tcp://provider:5555`, `cycle-time` 50 ms. Input tags: `probe` (binary, DI 0, flipped on a schedule), counters `cnt0..` (analog, input registers
  30000+, range 0..65535 so the register is the integer the provider sent), then a quarter binary (discrete inputs) and three quarters analog (input registers, range 0..1000). Outputs: 4 coils and 2 holding registers to the REP.
- `src/main.rs` (`fdmicro`, a standalone crate like `bench/control`): the scripted provider and every client, one process and one clock (CLOCK_MONOTONIC).
  - Provider thread: RADIO-framed datagrams at a fixed rate, the whole tag set every cycle in 1 400-byte chunks (ferrofire datagram v1 framing: whole points, `_ff.seq/_ff.cyc/_ff.t` trailer, NUL; stock bennu ignores the trailer pseudo-tags and accepts the NUL).
    Analog values change every cycle. **Every datagram starts with a counter point `cntK:<cycle>`** (K = the datagram's index in its cycle): a counter that skips a number is a chunk whose value was never visible. The probe flips on a
    jittered schedule (`every` to `2 * every` cycles apart, so it never lines up with the device's 50 ms cycle or stock's 1 s refresh) and the send instant of the datagram that carries each flip is recorded.
  - REP thread: ferrofire's `transport::serve` (zeromq crate), every `WRITE=` answered `ACK=` at once, arrival stamped inside the handler.
  - Clients: Modbus/TCP (`bench/control/src/modbus.rs`, shared by `#[path]`) and a DNP3 master (ferrofire's own link layer encoder, class-0 READ g60v1, no confirm).
  - Device cpu/RSS/UDP counters: the harness container runs with `--pid container:<device>`, so it reads `/proc/<pid>/stat`, `status` and `net/snmp` (UDP `InDatagrams`, `RcvbufErrors`) of the device itself.
- `run.py`: one fresh device container (alias `rtu`) and one harness container (alias `provider`) per (rep, profile, protocol, stack); the two stacks run one after the other, who goes first alternating with the rep.

## Phases (per run)

Modbus (`-mb`): idle (10 s, no stream), stream only (10 s), **stream-to-register** (tight FC2 poll of the probe bit, about 100 flips; S also at 1 and 100 Hz), **read latency and throughput** (1, 4, 16 clients in tight loops, FC4 of 1 register and of
min(100, all) registers, 5 s each), **write-forward** (FC5 on a coil to the `WRITE=` arriving at the REP; spaced: 60 writes 1.2 s apart, rapid: 100 writes 60-150 ms apart), **drops** (30 s of full-cycle stream with every counter read in a tight loop,
then every register compared with the last cycle). DNP3 (`-dnp3`): idle, stream only, class-0 poll latency (300 polls, 20 ms apart), back-to-back polls, stream-to-DNP3-visible (about 30 flips).
Main rates: S (15 tags) 10 Hz, M (800 tags) 100 Hz, L (8112 tags) 10 Hz.

Output per run: `<stack>_<P>_<proto>_rep<k>.metrics.csv` (`phase,variant,metric,value`), `.s2r.csv` and `.wf.csv` (every flip's / write's delay, -1 = never seen), `.driver.log`, `.env.txt` (commit, binary hash, image id, config hash,
host load, other containers), `.fd.log.gz` (the device's own log).
