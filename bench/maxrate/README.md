# Maximum publish rate, ferrofire provider -> ferrofire field device (ww-1df.43)

How fast can `ferrofire run` publish to `ferrofire field-device`, and what limits it. Report: [docs/bench/max-rate.md](../../docs/bench/max-rate.md); raw: `docs/bench/results/maxrate/`.

```
make bench-maxrate          # = python3 bench/maxrate/run.py: builds ferrofire and the observer in ff-rust, runs the matrix (about 25 min a rep, 3 reps), finished cells are skipped
make bench-maxrate-control  # the control-grid flip latency at each dt (bench/run_control.py with DT_MS; about 1 h)
make charts                 # docs/bench/max-rate.md and docs/img/charts/13*.svg from the raw files
REPS=1 MODELS=S RS=1 DTS=10 DECOUPLED= SECS=5 OUTDIR=target-linux/maxrate/smoke python3 bench/maxrate/run.py     # a smoke run into a scratch dir
```

Needs `ff-rust:22.04` on the **default** docker daemon; cargo runs with `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`; never start it next to another heavy container. Env knobs: top of `run.py`.

## Parts

- `gen.py MODEL R DT_MS PUB_MS DIR`: the provider model and R field-device configs of one cell. Models S (15 tags), M (800), L (8 112): every value changes every tick
  (`(bench.k * 0.1 + n * 0.37) % 1000`, the worst case for the wire and the device), dealt round-robin over R publish slices (one multicast group per device).
  G: the control grid of `bench/gen_grid.py` (250 buses, 1 250 breakers, 10 generators) with its own device configs. All carry a tick counter `bench.cnt` (the register of RTU 1 at IR 39000 is the integer sent).
- `src/main.rs` (`ffmaxrate`, a standalone crate like `bench/fdmicro`): the observer. Nothing is scripted: a REAL provider and R REAL devices run in their own containers; the observer (`--pid host`) joins every
  group (one socket per group), reads cpu, RSS, `Udp InDatagrams / RcvbufErrors` and socket drops of every process from /proc, and polls RTU 1's counter over Modbus.
  Window A (10 s, nobody polls): achieved publish and tick rate (the provider's own `_ff.t` / `_ff.cyc` and the counter), intervals above 1.5 periods (a tick that overran), datagrams sent (`_ff.seq` span) against `InDatagrams` at each device, cpu, RSS.
  Window B (10 s, tight Modbus poll): stream to register = the poll that first shows counter value c minus the `_ff.t` of the datagram that carried c, and the device's cpu while polled.
- `run.py`: one fresh provider, R devices and the observer per cell; reps are the outer loop.
- Flip latency and simulation-lost of the grid: `bench/run_control.py` with `DT_MS` (engine tick = publish period) and the cells `breaker:seq:100,breaker:sim:100`.
