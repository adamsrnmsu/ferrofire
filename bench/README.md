> Historical: NNG was dropped on 2026-11-09. Running the nng arm of this benchmark needs a checkout from before that date; the data in `docs/bench/results/` is kept.

# Transport benchmark: zmq vs nng

Compares `ferrofire run --transport zmq` (libzmq REP + UDP RADIO/DISH multicast) with `--transport nng` (NNG REP0 + PUB0 over TCP)
at 10, 50, 100 and 800 published tags, publishing at 10 Hz and 100 Hz. Results and analysis:
[docs/bench/transport-comparison.md](../docs/bench/transport-comparison.md). Raw data: [docs/bench/results/](../docs/bench/results/).

## Run it

Needs only Docker (Docker Desktop, arm64), `python3` and `uv` on the host; cargo runs in the `ff-rust:22.04` container.

```
make bench          # builds, then runs the whole matrix: 2 transports x 4 N x 2 rates x 3 reps = 48 runs, ~70 s each (~1 h)
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
