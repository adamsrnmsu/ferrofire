# Results at a glance

What ferrofire has been measured to do, on a Docker bench and on a live phēnix range (aarch64 KVM guests on an
Apple Silicon Mac, stock bennu field devices). Every number links to the section with its method and raw data.

## Live range: one hour, stock bennu RTUs

`waterway-arm64`, ferrofire as the provider, two stock `bennu-field-device` RTUs, one OVS bridge, 10 Hz
([section G](bench/datagram-v1-bench.md#g-live-range-waterway-arm64-across-ovs-ww-otj19-ww-2s0)).

| What | Result |
|---|---|
| Datagrams in 1 h (provider / rtu-1 / rtu-2) | 35 993 / 35 993 / 35 994 |
| Lost, duplicated or reordered | **0** at every capture point |
| RTU Modbus values matching the provider | **100 %** of 325 samples (max error 0.5 m on a 300 m transit) |
| Stock bennu affected by the extra `_ff.*` tags | No: no errors, no restarts, 1.1 % of a core |
| Per-RTU groups, 10 min, snooping off and on | 0 lost, 100 % tracking |
| Bystander NICs with OVS multicast snooping off / on | 4.3 KB/s (15.6 MB/h each) / 36-116 B/s |
| Provider `kill -9`, restarted after 5 s or 20 s; `SIGSTOP` 10 s | RTUs tracking again at the first sample (0.2 s); no RTU wedged; writes go through after recovery |
| 11 041 garbage datagrams injected | stock bennu kept running (values flickered while it lasted; ferrofire's own receiver drops them) |
| Misspelled tag in a `WRITE`/`READ` | `ERR` with a "did you mean" suggestion, nothing else changes |

## Freshness: publish right after each engine tick

800 tags, Docker bench ([section A](bench/datagram-v1-bench.md#a-age-at-800-tags-before-vs-after-publishing-synchronously-after-the-engine-tick)).

| | age p50 before | after | age p99 before | after |
|---|---|---|---|---|
| 10 Hz | 9.5 ms | **4.1 ms** | 14.5 ms | **5.3 ms** |
| 100 Hz | 6.5 ms | **3.2 ms** | ~14 ms | **4.9 ms** |

## Loss and reordering (tc netem)

Sequence numbers measure loss exactly: injected 0.1 / 1 / 5 % read as 0.10-0.17 / 1.07-1.34 / 5.07-5.14 %
([sections B and C](bench/datagram-v1-bench.md#b-and-c-datagram-loss-by-_ffseq-cycle-loss-and-staleness-under-tc-netem)).

Complete cycles with jitter (`delay 2ms 1ms`) after the order-tolerant receiver fix (ww-d4a):

| loss | 100 tags, 10 Hz | 100 tags, 100 Hz | 800 tags, 10 Hz |
|---|---|---|---|
| 0 % (before the fix) | 17 % | 17 % | 0 % |
| 0 % | **100 %** | **100 %** | **100 %** |
| 1 % | 97.1 % | 96.9 % | 83.9 % |
| 5 % | 86.6 % | 85.8 % | 39.8 % |

A cycle of D datagrams is complete with probability (1-p)^D; each tag holds its last value until a newer cycle
arrives (per-tag p99 staleness 2-3 periods at 1 % loss, 3-9 at 5 %). Per-RTU slices make D small.

## Faults on the bench

| Fault | Result |
|---|---|
| Provider `kill -9`, restart 3 s later | multicast subscribers fresh 3-4 ms after restart; pull-bridge 140 ms; nothing wedged |
| Every REP reply dropped for 4 s | multicast untouched; pull-bridge fresh 260 ms after replies return, socket recreated, no wedge |
| 7 800 bad datagrams at 100/s | all counted by reason, 100 % of cycles still delivered, no crash |
| ~3 000 random requests, 10-12 MB payloads, non-UTF-8, `nan`/`1e999`, unknown tags | every request answered, provider stayed up |

## Fan-out: per-RTU groups vs pull vs one stream

32 RTUs x 100 tags at 10 Hz ([section D](bench/datagram-v1-bench.md#d-per-rtu-groups-vs-one-whole-model-stream-vs-pull-bridges)):

| mode | provider wire | provider CPU |
|---|---|---|
| one 800-tag multicast stream | 0.25 MB/s | 4 % |
| per-RTU multicast groups | 1.0 MB/s | 8 % |
| pull-bridge (REQ/REP) | 1.4 MB/s | 10 % |

The single stream is cheapest for the provider and costliest for every RTU (each receives all of it).

## Tools on the live range

`waterway-arm64`, ferrofire provider, stock bennu RTUs ([captures](live-tools/README.md)).

| Tool | Result |
|---|---|
| `probe write lowervalve.open=1,lowergate.open=0` | reached rtu-1's Modbus registers within 577 ms |
| `probe play` (3 timed writes at 0, 2, 4 s) | writes landed at 0, 2 012 and 4 023 ms |
| `probe record`, 60 s | 601 rows, cycles 2249..2849, no gaps, no stale flags |
| `probe diff` against both RTUs (22 registers) | 20 clean cycles; a holding register written straight onto rtu-1 gave 9 mismatches and exit 1, then clean |
| `ferrofire top`, provider stopped | all 15 tags `STALE` at 4.4 s (stale-after 2 s), back to `good` on resume |
| `ferrofire doctor`, OVS snooping off/on | caught a wrong multicast route on a multihomed host, a member starved after snooping was enabled late, and a 1.0 s clock skew between VMs |
| `ferrofire log` | `on` x3 printed "already logging to" twice with one file; `off` x2 printed "already off"; `level debug --for 10s` reverted by itself |
| Binary on a VM without libzmq | runs (pure-Rust zeromq; glibc dynamic, needs only libc/libm/libgcc_s) |

## Earlier: ZeroMQ vs NNG (historical)

NNG was evaluated and dropped: no multicast (N RTUs = N copies, 19.6 MB/s vs 2.4 MB/s at 8 RTUs, 100 Hz), about
1.8x the request CPU and bimodal latency ([transport-comparison.md](bench/transport-comparison.md)).

## Code quality gates

Every function CRAP < 10, no duplicate code above 0.8 similarity, mutation testing with 0 surviving mutants
([quality.md](quality.md)).
