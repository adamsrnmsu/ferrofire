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
| `headline.py` | 1 | `docs/bench/results/control/{ferrofire,stock}` and `range/` |
| `where-time-goes.py` | 2 | `control/{ferrofire,stock,stock-*s}`, `stock-capacity.txt`, `solve-time.txt` |
| `provider-vs-fd.py` | 3 | `control/cross-*` (bench still running: a rerun redraws with newer data) |
| `reliability-fixes.py` | 4 | `control/{ferrofire-before-ww-1df.32,ferrofire,stock}` and `range/` |
| `stream-fidelity.py` | 5 | `docs/bench/results/{raw,raw-datagram-v1,raw-datagram-v1-d4a,live}` |
| `robustness-stock.py` | 6 | `docs/bench/results/burger` |
| `cpu-profiling.py` | 7 | `cpu-profiling-raw/` (copied from `/var/tmp/ff-prof`) |

Colours: ferrofire blue `#2a78d6`, stock bennu grey `#898781`, as in `bench/charts.py`.

## Where the recomputed numbers differ from the reports

- 1 000-flip burst, stock, 10 RTUs: 18.2 flips/s (1 rep). The "about 20/s" in `control-latency.md` is the 100-flip cell (20.1).
- "Lost" in the reports means not visible within 10 s (12 s for a generator), not dropped forever. The charts and page say "missed".
- Stock's reorders are 0.99 to 10.19 s apart (median 2.06 s), not "about 1 s".
- ferrofire's chunk skew: stream stamps at most 0.19 ms apart in Docker (4.34 ms on the range). The 3 ms in the report is the visible inversion.
- Raw `order_violations` for stock at 1 s is 1 190 = 1 105 reordered + 85 chunk skew.
- Stock's real period at 0.05 s: 79 ms idle (`stock-capacity.txt`), 72 ms under bench load. Both reproduce.
- `docs/img/charts/09l-cross.csv` and `09m-cross-burst.csv` hold 1 rep of stock + stock; raw now has 3 (1029/1027/1024/1021 ms, 81/63/9/0 %).
- Profiling idea A: the raw log has 3 pairs, not 4; median change -8.9 %, not about -11 %.
- Profiling idea C: 48.6 -> 39.3 % by medians; the report's 48.7 -> 39.5 % are means.
- Profiling W1a: `publish_all` is 48.4 % of samples, not 48.6 %.
- Live hour: rtu-2 captured 35 994 datagrams (its capture starts one sequence number early); 0 missing.
- Fan-out: the 0.25 MB/s shared stream was measured with 1 listener, not 32.
- Stock fd-leak log volume: 240 MB in `results.md`, 6.2 GB in `datagram-v1-bench.md`. Neither has a raw file.
- The "fixed" bennu column is three builds, one per PR (#79, #81, #82), not one combined build.
