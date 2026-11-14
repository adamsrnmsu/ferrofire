# bench/findings

One script per theme of [`docs/findings.md`](../../docs/findings.md). Each recomputes its numbers from raw data,
prints them, and writes its charts to `docs/img/findings/`.

Regenerate everything (from the repo root):

```bash
for f in bench/findings/*.py; do uv run --no-project --with matplotlib python "$f"; done
```

Reruns over the same raw data are byte-identical. No docker, no build, no benchmark run.

| script | section | raw data |
|---|---|---|
| `headline.py` | 1 | `docs/bench/results/control/{ferrofire,stock-v2026.11.03,stock}` and `range/` (no v2026.11.03 there) |
| `where-time-goes.py` | 2 | `control/{ferrofire,stock-v2026.11.03[-0.1s],stock,stock-*s}`, `stock-capacity.txt`, `solve-time.txt` |
| `provider-vs-fd.py` | 3 | `control/cross-*` |
| `reliability-fixes.py` | 4 | `control/{ferrofire-before-ww-1df.32,ferrofire,stock-v2026.11.03,stock}` and `range/` |
| `stream-fidelity.py` | 5 | `docs/bench/results/{raw,raw-datagram-v1,raw-datagram-v1-d4a,live}` |
| `robustness-stock.py` | 6 | `docs/bench/results/burger` |
| `cpu-profiling.py` | 7 | `cpu-profiling-raw/` (copied from `/var/tmp/ff-prof`) |
| `field-device.py` | 8 | `docs/bench/results/fdmicro` (through `bench/fdmicro_report.py`) |
| `cpp-provider.py` | 9 | `docs/bench/results/cppprov` (bennu 6.0.0) and `cppprov-v2026.11.03` |

Colours: ferrofire blue `#2a78d6`, bennu v2026.11.03 (current stock) grey `#898781`, bennu 6.0.0 (historical stock) light grey `#c9c8c0`, as in `bench/charts.py`.

"Stock" is two bennu versions, each labelled: **bennu v2026.11.03** (current, has the upstream batched-write fix 91fd137; ww-1df.47) is the main comparison and **bennu 6.0.0 (pre-91fd137)** the historical one.
6.0.0's burst losses, reordering and 0.5 s floor on 1-2 RTUs came from a fixed 500 ms sleep per write that 91fd137 removed; the v2026.11.03 charts show they are gone (and what is new: flaps under bursts).
The robustness charts (section 6) are 6.0.0 on the range; their results have nothing to do with that fix.

## Where the recomputed numbers differed from the reports

The digest found 14 places where a report did not match the raw data. They are fixed in the reports (ww-1df.45):

| mismatch | fixed in |
|---|---|
| 1 000-flip burst, stock, 10 RTUs: 18.2 flips/s (1 rep), not "about 20/s" (the 100-flip cell, 20.1) | `control_latency.md.in` section 4 |
| "Lost" means not visible within 10 s (12 s for a generator), not dropped forever | section 7 |
| Stock's reorders are 0.99 to 10.19 s apart (median 2.06 s), not "about 1 s" | section 7 |
| ferrofire's chunk skew: stream stamps at most 0.19 ms apart in Docker (4.34 ms on the range); 3 ms was the visible inversion | section 7, "What the numbers do not say" |
| Raw `order_violations` for stock at 1 s: 1 190 = 1 105 reordered + 85 chunk skew | section 7 |
| Stock's real period at 0.05 s: 72 ms under bench load, 79 ms idle | section 8 |
| `09l-cross.csv`, `09m-cross-burst.csv` held 1 rep of stock + stock | already regenerated with 3 reps |
| Profiling idea A: 3 pairs in the raw log, median change -8.9 % | `docs/profiling.md` |
| Profiling idea C: 48.6 -> 39.3 % by medians (48.7 -> 39.5 % were means) | `docs/profiling.md` |
| Profiling W1a: `publish_all` is 48.4 % of samples | `docs/profiling.md` |
| Live hour: rtu-2 captured 35 994 datagrams, its capture starts one sequence number (100 ms, not 1 s) early; 0 missing | `results.md.in` section 6, `datagram_report.md.in` G |
| Fan-out: the 0.25 MB/s shared stream was measured with 1 listener, not 32 | `results.md.in` section 4, `datagram_report.md.in` |
| The "fixed" bennu column is three builds, one per PR (#79, #81, #82) | `results.md.in` section 8 |

One cannot be settled from raw data and both reports now say so:

- Stock fd-leak log volume: 240 MB in `results.md` 8a (from the #79 PR comment), 6.2 GB in `datagram-v1-bench.md` G6. Neither has a raw file; they may be different occurrences.
