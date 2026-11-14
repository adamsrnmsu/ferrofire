> Historical: NNG was dropped on 2026-11-09. Running the nng arm of this benchmark needs a checkout from before that date; the data in `docs/bench/results/` is kept.

# Transport benchmark: zmq vs nng

Compares `ferrofire run --transport zmq` (libzmq REP + UDP RADIO/DISH multicast) with `--transport nng` (NNG REP0 + PUB0 over TCP)
at 10, 50, 100 and 800 published tags, publishing at 10 Hz and 100 Hz. Results and analysis:
[docs/bench/transport-comparison.md](../docs/bench/transport-comparison.md). Raw data: [docs/bench/results/](../docs/bench/results/).

## Run it

Needs only Docker (Docker Desktop, arm64), `python3` and `uv` on the host; cargo runs in the `ff-rust:22.04` container.

```
make bench          # builds, then runs the whole matrix: 2 transports x 4 N x 2 rates x 3 reps = 48 runs, ~70 s each (~1 h)
make charts         # every comparison chart (docs/img/charts/*.svg + CSV) and docs/results.md, from the committed raw data
make bench-report   # raw -> CSV, PNG charts, docs/bench/transport-comparison.md
```

Smaller or different matrices (finished runs are skipped, so an interrupted run resumes):

```
NS=10,800 PERIODS=0.1 REPS=1 PUB_SECS=5 REQ_SECS=2 python3 bench/run.py     # smoke test, ~5 min
TRANSPORTS=nng NS=800 python3 bench/run.py                                   # one transport, one size
RAWDIR=docs/bench/results/raw-after-nng-fix NS=100,800 python3 bench/run.py   # the ww-a8n after-fix runs (separate raw dir; report.py adds the before/after table)
uv run --with matplotlib python bench/report.py                              # what make bench-report does
```

`PERIODS` are publish periods in seconds (`0.1` = 10 Hz, `0.01` = 100 Hz). Defaults: `PUB_SECS=30`, `REQ_SECS=10`.

## What runs

| Piece | Where | What |
|---|---|---|
| `gen_model.py N` | host | Model with exactly N published tags: `sim.t` (state, +1 per second), 4 writable inputs `bench.in0..3` echoed as outputs, and N-5 cheap analog expressions with long decimals. `dt` 0.01 s. |
| `ferrofire run` | container `ffb-prov` (PID 1), `--release` | The provider under test, on docker network `ffbench`. |
| `ffbench mon` | container `ffb-mon`, shares the provider's network and PID namespaces | AF_PACKET capture on the provider's `eth0`: frame bytes and packets by class (request/reply = tcp 5555; publish = tcp 5556 for nng, udp 40000 for zmq; other = ARP/IGMP), plus the provider's CPU ticks and RSS from `/proc/1`. One CSV row of cumulative counters about every 250 ms. |
| `ffbench client` | third container on the same bridge | One REQ client (zmq REQ or NNG REQ0) and one subscriber (UDP multicast DISH-equivalent, or NNG SUB0). Prints one JSON document. |
| `run.py` | host | Orchestrates the matrix; reps are the outer loop so drift hits every cell alike. |
| `report.py` | host (uv, matplotlib) | Aggregates, tables, charts, report. |
| `charts.py` | host (uv, matplotlib) | The comparison charts of [docs/results.md](../docs/results.md): reads `docs/bench/results/` (bench, live captures, OVS counters, `burger/burger.csv`), writes `docs/img/charts/*.svg` + one CSV per chart, fills `results.md.in`. Light surface baked into each SVG so it reads on GitHub light and dark. Colours: dataviz reference palette (blue `#2a78d6`, orange `#eb6834`, aqua `#1baf7a`, violet `#6250d6`, red `#e34948`, grey `#898781` for before/stock), checked with `validate_palette.js --mode light` per chart pairing: all pass except the grey, which is the de-emphasis colour and fails the chroma floor by design (it passes the contrast and neighbour-distance checks). |

Source: `bench/client/` is a standalone cargo package (not part of ferrofire's build, tests or `make check`); `bench/` touches nothing in `src/`.

## Timeline of one run (about 65 s)

1. 3 s warm-up (connect, subscribe, QUERY to check the tag count).
2. 30 s **publish only**: subscriber receives, no requests. Publish metrics, publish traffic, idle CPU and RSS come from here.
3. 10 s **READ** loop (random tag per request), 10 s **QUERY** loop, 10 s **WRITE then READ-back** loop. Closed loop, one request in flight.
   The subscriber keeps running throughout (publish stats are also recorded per phase in the JSON).

## Method notes

- Latency is the client's `Instant` around send+recv, in the client process; percentiles over every request in the phase.
- WRITE: a `WRITE=bench.in0:k` is acknowledged when queued; the value shows in `READ` only after the next engine tick (`dt` = 10 ms).
  So the loop is write, then READ until the new value appears: reported as write RTT, read RTT, and write-to-visible.
- Publish cycle: a chunk starting `sim.t:` begins a cycle. Complete = all N tags received. `spread` = first to last chunk of a cycle.
  `age above floor` = last-chunk receipt time minus (`sim.t` + E0), E0 set so the smallest age in the run is 0: freshness variation,
  not an absolute one-way latency (see the report for why an absolute one is not available without instrumenting the provider).
- Loss = 1 - complete cycles / expected cycles (window / period); good to about one cycle at the window edge.
- Wire bytes are AF_PACKET frame lengths: Ethernet header (14 B) + IP + TCP/UDP + payload, no preamble or FCS.
  TSO/GSO/GRO are switched off with `ethtool` in both network namespaces so a captured frame is a real wire frame (max frame is recorded in `runs.csv`).
- Clocks: all containers share the Docker Desktop VM kernel, so the client's wall clock stamps and the monitor's agree.

## Datagram v1 matrix (ww-otj.18)

`python3 bench/run_dg.py` (cells A, C, D, E, F; see its docstring) -> `docs/bench/results/raw-datagram-v1/`, then
`python3 bench/dg_report.py` -> [docs/bench/datagram-v1-bench.md](../docs/bench/datagram-v1-bench.md). The earlier matrix above and its
raw data are untouched (the new report only reads them for the "before" age numbers).

- `ffbench dg` is the client: several streams side by side against one provider, each with the real `ferrofire::datagram::Receiver`.
  `--whole` = one multicast stream with every tag, `--slices R --k K` = R per-RTU multicast groups (ports 41000+i), `--pull R --k K` =
  R real `ferrofire pull-bridge` processes republishing on client-side loopback (42000+i). `--restart-at S --restart-gap S` makes
  subscribers leave/rejoin and respawns the bridges; `--events` stores every complete-cycle arrival time (epoch ms) for post-hoc analysis.
- `ffbench inject --rate 100 --secs S` sends seven kinds of bad datagram into the multicast group; `ffbench req REQUEST...` sends requests.
- Cycles are assembled by `_ff.cyc` (not by finding `sim.t`), so a lost first chunk does not merge two cycles. A cycle is complete when every
  tag of the stream has arrived. `staleness` = gap between consecutive complete cycles (with the lead-in and tail of the window); `probe_gap` =
  arrival gaps of 8 tags spread over the chunks. `datagrams.lost` = `_ff.seq` span minus distinct seqs seen (order independent; a reordered
  datagram is not loss, though the Receiver still drops it as stale). `transit` = arrival minus `_ff.t` (one clock: all containers share the VM).
- `tc netem` runs in the provider container (`--cap-add NET_ADMIN`) on its `eth0` egress; the monitor counts frames before the qdisc.
  Reply blackhole = a `prio` qdisc with a u32 filter on source port 5555 sending to a `netem loss 100%` band.
- Fault cells run the provider as an exec'd process (PID 1 is `sleep`) so it can be killed and restarted without losing the network namespace;
  event timestamps come from `date` inside that container, the same clock the client stamps with.

## Live range runs (ww-otj.19, ww-2s0)

`bench/live/` drives the `waterway-arm64` range (provider VM, rtu-1, rtu-2 with stock bennu) from the Mac through minimega `cc` (no host route to the guests):
`rng.sh` (run a snippet on VMs), `deploy.sh`/`fetch.sh` (files to `/root/ff`, summaries back into `docs/bench/results/live/<host>/`), `cap.sh` + `cap.py` + `seqgap.py` (tcpdump
summary, `_ff.seq` gaps), `trk.py` (Modbus vs provider READ), `ovsb.sh` (OVS port counters), `fault.py`/`inject.py`/`misspell.sh`/`fdlog.sh` (fault injection and device-log health),
`provrtu.sh`/`rtudg.sh` (per-RTU groups, restore), `rtu2.sh` (pull-bridge), `matrix.sh` + `matrix_table.py` (the 4 x 3 push/pull matrix). Results: section G of
`docs/bench/datagram-v1-bench.md`. Pollers must keep one Modbus connection open (stock bennu leaks an fd per closed connection, ww-1p6).

## Control-latency grid (ww-1df.23.1)

`python3 bench/gen_grid.py OUTDIR` builds the synthetic grid of the breaker/generator control bench: 250 buses, 1250 breakers (branches),
10 commandable generators plus the slack, partitioned round-robin over R = 1, 2, 5, 10 RTUs. It writes `OUTDIR/case.py` (PYPOWER, shared by
ferrofire and stock pybennu-power-solver), `OUTDIR/R<R>/model.yaml` (ferrofire, per-RTU publish slices), `config.ini` (stock), `ferrofire/rtu-N.xml` and
`bennu/rtu-N.xml` (field-device configs) and `scada.json` (Modbus register map and the safe `flip_order`; see the script's docstring for the
islanding rule). `python3 bench/test_gen_grid.py` checks sizes, partition, determinism and config consistency. `bench/solve_time.sh BIN GRIDDIR`
measures the re-solve time at 1250 branches: [docs/bench/results/control/solve-time.txt](../docs/bench/results/control/solve-time.txt)
(about 9.7 ms per solve, ferrofire in a container on a contended host; pybennu is not installed offline, so its solve time is not measured).

## Control-latency harness (ww-1df.23.3)

`make bench-control STACK=ferrofire` (`bench/run_control.py`, about 2 h on a quiet host) runs breaker flips {1,5,10,50,100,1000} and generator spin-down + spin-up
{1,2,5,10} across R = 1, 2, 5, 10 RTUs, in sequential and simultaneous mode, 5 reps, in docker on the default daemon (needs the grid of `gen_grid.py`, which the
script generates into `target-linux/control-grid/`). Per (rep, R): a provider container (`ferrofire run --stamp-writes`), R `ferrofire field-device` containers
and one `ffcontrol` container (`bench/control/`, a standalone cargo package: the SCADA driver, Modbus/TCP plus a stream listener, one process and one wall clock)
on the network `ffcontrol`. Raw output per run in `docs/bench/results/control/<stack>/R<R>_rep<k>.*`: `cmds.csv` (a row per command), `cells.csv` (a row per phase),
`driver.log`, `prov.log.gz` (the provider's `Applied:` stamps), `fd.log.gz`, `env.txt` (commit, binary hash, host load, other containers, contention).
`bench/control_join.py` adds the provider stamps to the command rows. Knobs: env of `run_control.py`; `ffcontrol --help` is the source docstring of `bench/control/src/main.rs`.

Per command (unix ns, one clock): `t_cmd` write sent, `t_ack` write answered, `t_echo` first read-back (discrete input, polled every 2 ms on its own connection) showing
the new state, `t_stream` first datagram carrying the new state on the RTU's multicast group, `t_visible` first read-back showing it at or after `t_stream`,
`t_setpoint` (generators) PG input register within 1% of the setpoint, and from the provider `t_apply_ns` (engine applied the write) and `t_solved_ns` (the step that re-solved it finished).
`t_echo` is kept apart because the field device (like the stock one, SPEC 3.2) sets a point when the provider ACKs the WRITE, which is before the provider has applied it:
the read-back flips early and a still-old datagram can flip it back (`flaps`). A command is *done* at `t_visible` (and `t_setpoint` for a generator); sequential mode issues the
next command when the previous is done. Spread over RTUs: n commands, no RTU over ceil(n/R) (a shortfall is taken from the others), issued round-robin over the RTUs in
sequential mode, one concurrent Modbus client per RTU in simultaneous mode.

- **Lost**: not done within 10 s of `t_cmd` (12 s for a generator, which also has to ramp). 10 s is above the field device's 5 s provider timeout, so one retried WRITE shows as tail latency, not loss.
- **Wrong**, per phase: `wrong_final` = commands that were done but whose point (or PG) is not at the target after a 1 s settle; `wrong_provider` = done commands whose tag the provider READs back
  at a different value; `wrong_collateral` = points nobody commanded that changed; `order_violations` = adjacent commands of one RTU that became visible in the opposite order (the raw driver column, which includes same-publish chunk skew; the report splits it into `reordered` and `chunk_skew` from t_stream, threshold half the publish period); `flaps` = read-backs that showed the new state and then took it back.
- **Generator**: spin-down (status off after PG ramps 40 MW to 0 at 40 MW/s, `power.ramp` in the generated model) then spin-up (status on, PG ramps back), each a measured phase; breakers: open the first n of `flip_order`, then restore (unmeasured, checked).
- The provider stamp joins the first unused `Applied:` line of the command's tag and value at or after `t_cmd`; a provider other than ferrofire only has to log lines of that shape.

## Stock bennu at faster publish intervals (ww-1df.35)

Stock publishes once a second and that is not configurable: pybennu 6.0.0 `PyPower.periodic_publish` (`solvers/python_power.py`) ends in `time.sleep(1)`, and no `config.ini` key reaches it. **Deviation from stock as shipped, bench only**:
`bench/stock_provider.py` (the stock provider plus the `Applied:` stamps) replaces `PyPower.periodic_publish` by a verbatim copy of the 6.0.0 method with `time.sleep(1)` -> `time.sleep(PERIOD)`, `PERIOD` from the env var `STOCK_PUBLISH_S`, and timing counters that print one
`Publish-stats: ... publishes= pack_ms_mean= send_ms_mean= cpu_s=` line every 10 s (the pack / send / CPU numbers of the report). With `STOCK_PUBLISH_S` unset the method is stock's own. Nothing in the image, the field devices, the grid or the driver changes.

```
for P in 0.5 0.2 0.1 0.05; do STOCK_PUBLISH_S=$P STACK=stock REPS=5 SLOW=breaker:seq:1000,breaker:sim:1000 SLOW_REPS=1 OUTDIR=docs/bench/results/control/stock-${P}s python3 bench/run_control.py; done   # ~100 min each, Docker, one heavy container at a time
bash bench/stock_capacity.sh > docs/bench/results/control/stock-capacity.txt   # idle provider, 55 s per interval: pack/send ms, effective period, CPU
for P in 0.5 0.2 0.1 0.05; do python3 bench/control_summary.py docs/bench/results/control/stock-${P}s > docs/bench/results/control/stock-${P}s/summary.md; done
```

Same matrix, grid, driver and fresh-stack-per-cell as the 1 s baseline (`results/control/stock/`), 5 reps (an interval takes under 2.5 h), R = 1, 2, 5, 10; Docker only (the range matched Docker for both stacks). The report is section 8 of `docs/bench/control-latency.md` (`bench/charts.py`: `chart_stock_intervals`; `control_report.py`: `provider_stats`).

## Provider vs field device: the 2x2 cross matrix (ww-1df.40)

`PROVIDER=ferrofire|stock FD=ferrofire|stock python3 bench/run_control.py` pairs any provider with any field device (either variable set = cross mode; the stack is `cross-<provider>-<fd>`, results in
`docs/bench/results/control/cross-<provider>-<fd>[-<s>s]/`, the suffix for `PROVIDER=stock` with a tuned `STOCK_PUBLISH_S`). Same grid, `ffcontrol` driver and raw layout as every other stack. A mixed pairing starts the
field device from a copy of its generated config whose stream endpoint is the provider's (`target-linux/control-grid/R<R>/cross-<fd>-<provider>/`; the two generated configs differ only in that endpoint); the driver's flags follow the
provider (stock: `--read-per 1 --legacy --status-only`) and the stack is fresh per cell whenever either side is stock. Run rep by rep over the pairings so host drift hits all alike:

```
CELLS=breaker:seq:100,breaker:sim:10,breaker:sim:50,breaker:sim:100,breaker:sim:1000,gen:seq:5,gen:sim:5 RS=1,2,5,10 NOBUILD=1
for k in 1 2 3; do   # SLOW=breaker:sim:1000 SLOW_REPS=1 for the stock-provider pairings
  PROVIDER=ferrofire FD=ferrofire REPS=$k python3 bench/run_control.py; PROVIDER=ferrofire FD=stock REPS=$k python3 bench/run_control.py
  PROVIDER=stock FD=stock REPS=$k python3 bench/run_control.py; PROVIDER=stock FD=stock STOCK_PUBLISH_S=0.1 REPS=$k python3 bench/run_control.py
done
```

`PROVIDER=stock FD=ferrofire` does not run: ferrofire's field-device receiver drops every datagram of the stock provider (no NUL terminator, `Receiver::body_of`), see section 9 of the report.

## Control-latency on the live range (ww-1df.23.5)

The same driver, grid, cells and raw layout on real VMs: `bench/range/run_range.py` (host, stdlib) + `bench/range/vm.sh` (inside the VMs). Topology: `topologies/control-bench-arm64`
of phenix-waterway-range (provider VM that also runs `ffcontrol`, so every timestamp is on the provider's one clock; `rtu-1..5`; one flat ICS VLAN `10.1.1.0/24`, no router; 4 vCPU / 3 GB provider,
2 vCPU / 1 GB RTUs; all from `bennu-ferrofire-arm64.qc2`, which holds stock pybennu + `bennu-field-deviced` and ferrofire, so the stock stack runs only stock binaries). No scenario: nothing
autostarts and the bench drives the VMs through minimega `cc`. The flat VLAN has no default route, so `vm.sh setup` adds `224.0.0.0/4` (multicast join and RADIO need a route) and `/etc/hosts` names.

```
python3 bench/range/run_range.py up      # phenix exp create/start, boot batch 1 (provider, rtu-1, rtu-2), then batch 2 (rtu-3..5), wait for 6 cc clients, ship the bundle (grid, ferrofire, ffcontrol)
python3 bench/range/run_range.py run     # RS=1,2,5 REPS=3 STACKS=ferrofire,stock CELLS=breaker:{seq,sim}:{1,10,100},gen:{seq,sim}:{1,5}; finished runs are skipped
python3 bench/range/run_range.py down    # exp stop + delete, then `namespace vm info` must be empty
```

Raw output: `docs/bench/results/control/range/<stack>/R<R>_rep<k>.*`, the same files as the docker runs (`control_join.py`, `control_summary.py`, `control_check.py` read them unchanged).
Ferrofire: one stack per (rep, R) runs every cell; stock: a fresh stack per cell (a stock provider that falls behind keeps a backlog), as in docker. `env.txt` records host load,
guest load and whether anything else was running. Gotchas found: phenix cannot boot a `do_not_boot` VM later (`vm restart` / `vm redeploy` do not find it in minimega), so `up` replays
the experiment's own mm-script stanza for rtu-3..5; `cc send` never overwrites a file the guest already has (each ship uses a new file name); stock `bennu-field-deviced` needs a relative
`--file` path and leaves a pid file that `vm.sh stop` removes.
