# ferrofire vs Sandia's C++ bennu-simulink-provider, with a stub model (ww-1df.42)

**Plumbing only: there is no solver in this comparison.** Neither side runs a power flow or a Simulink model. It measures what the provider and the transport add (shared-memory copy and publish on one side, the engine tick and publish on the other), with the same field devices and the same SCADA driver as the [control-latency bench](control-latency.md).
It says nothing about solve time, and a real Simulink step would add its own compute time and step size on top of every figure below.

Raw files: [results/cppprov/](results/cppprov/) (`summary.md` is the output of `bench/cppprov/report.py`). Method and commands: [bench/README.md](../../bench/README.md#provider-vs-sandias-c-bennu-simulink-provider-stub-model-ww-1df42). Everything ran on the default Docker daemon (Docker Desktop VM, 18 vCPU, 7.7 GB), linux/arm64, one heavy container set at a time (the only other container was the stale `elegant_zhukovsky`).

## What was compared

| | C++ side | ferrofire side |
|---|---|---|
| Provider | `bennu-simulink-provider`, **unmodified**, `--publish-rate` 0.1 / 0.05 | `ferrofire run model.yaml --stamp-writes`, `publish_rate` 0.1 / 0.05 |
| "Model" | `bench/cppprov/stub_model.c`: a C program that speaks the `modelHooks` contract (SysV shm 10610/10613, POSIX semaphores `publish_sem` / `updates_sem`, FIFO `/tmp/updates_fifo`, 256-byte `tag:TYPE:value` slots). N points `brk-NNNN.active` (BOOLEAN, start closed), fixed step 0.05 s on an absolute clock, `step()` in the order of `modelHooks.c` (publish state, then apply FIFO updates), a write sets the point and the next step publishes it | `bench/cppprov/gen.py` `model.yaml`: N binary inputs `brk-NNNN.active` (default true) republished by same-named outputs, no `power:` block, `dt` 0.05 s, one publish slice per RTU |
| Field devices | stock `bennu-field-deviced` and `ferrofire field-device`, the same config file for both (shape of `gen_grid.py`'s `fd_xml`), 10 RTUs | same |
| Driver | `ffcontrol`, the binary of the control bench, `--fresh` stack per cell | same (`--read-per 100` instead of `--read-per 1 --legacy --status-only`, as for pybennu) |

Points: N = 100 and 1 250. Publish interval: 0.1 s and 0.05 s. 3 reps of every cell, pooled in the tables. Cells: sequential flips of 10 and 50 breakers (`seq`, pooled), 10 and 100 simultaneous flips (`sim:10`, and the burst at 100 points), 1 000 simultaneous flips at 1 250 points (the burst there).
Four pairings, provider + field device: C++ + stock, ferrofire + stock, C++ + ferrofire, ferrofire + ferrofire. Latency is `t_visible - t_cmd`, a Modbus write to the first read-back (2 ms poll) that shows the new state at or after the stream datagram that carries it. "applied" is the provider-side stamp (`Applied:` line, printed by the stub for the C++ provider) minus `t_cmd`.

### Deviations and things to know

- **The C++ provider was not rebuilt for this run.** It is `/usr/local/bin/bennu-simulink-provider` of the phenix-waterway-range `bennu-arm64` overlay (sha256 prefix in every `env.txt`), built 2026-11-07 by `images/docker/bennu-arm64.Dockerfile` from a clean `git archive HEAD` of sceptre-bennu (HEAD `c9c2cf6`; the working tree has uncommitted, non-compiling WIP, so any rebuild would export HEAD again).
  The provider's source and its `distributed` library last changed in 2025 (`6f771a4`, `1761191`), long before the build; the uncommitted diff of `main.cpp` is whitespace. It runs in the `ffc-stock:22.04` image (Ubuntu 22.04 with the overlay's `/usr/local`). The stub is compiled in the `ff-rust` container with `gcc -O2`.
- **10 RTUs, not 1.** With one RTU the stock field device forwards about one write per 0.5 s (control-latency.md, section 2 and 8), which hides every provider difference; at 10 RTUs it is not the limit for sequential flips.
- **A burst of 1 000 through the stock field device is the device's limit, not the provider's:** both providers lose 80% (the device forwards about 2 writes per second per RTU and never sends the rest; the provider logs show 240 WRITE requests, 0 FIFO errors). The pairings with the ferrofire field device isolate the provider.
- `wrong_provider` of the driver is meaningless for the C++ provider (its READ replies `ACK=false`, without the `tag:` the driver parses); `wrong_final` was 0 everywhere and the provider's logged READs agree. The column is not reported.
- The ferrofire field device normally drops datagrams of stock pybennu (no NUL terminator). The C++ provider's publisher appends the NUL to every chunk, so C++ + ferrofire FD works; it was run to isolate the provider.
- Stub step 0.05 s is a choice (ferrofire's `dt` is the same). It is also the knob behind the burst loss below. The stub omits the solver's compute time and `modelHooks.c`'s per-point linear search of the CAPI signal table.
- Idle rate/cpu: a 20 s window after 8 s of start-up. `rate.py` listens on RTU 1's group only: publishes per second and the interval are the provider's; datagrams per publish and bytes per datagram are of that group (the C++ provider publishes all N points in one stream cut into chunks, ferrofire one slice per RTU). CPU is `/proc` ticks of the provider processes (C++: provider plus stub), resolution 0.05% of a core; RSS is the peak (`VmHWM`) over the run.
- Not measured: R = 1, other poll intervals, a real Simulink model.

## Results

### Write to visible, ms (p50 / p95 / max over all reps)

| points | publish s | stack | seq p50 | seq p95 | seq max | sim:10 p50 | sim:10 p95 | burst p50 | burst p95 | burst max | seq applied p50 | burst applied p50 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + stock FD | 166 | 201 | 205 | 146 | 192 | 2606 | 4653 | 4729 | 49 | 2529 |
| 100 | 0.1 | ferrofire + stock FD | 99 | 101 | 115 | 84 | 112 | 2514 | 4605 | 4638 | 47 | 2513 |
| 100 | 0.1 | C++ provider + ferrofire FD | 150 | 201 | 204 | 143 | 186 | 131 | 189 | 190 | 49 | 36 |
| 100 | 0.1 | ferrofire + ferrofire FD | 99 | 101 | 104 | 89 | 144 | 66 | 81 | 125 | 47 | 27 |
| 100 | 0.05 | C++ provider + stock FD | 127 | 152 | 200 | 99 | 145 | 2606 | 4647 | 4696 | 49 | 2530 |
| 100 | 0.05 | ferrofire + stock FD | 49 | 99 | 103 | 49 | 98 | 2508 | 4556 | 4607 | 47 | 2505 |
| 100 | 0.05 | C++ provider + ferrofire FD | 148 | 152 | 156 | 93 | 143 | 88 | 138 | 138 | 50 | 34 |
| 100 | 0.05 | ferrofire + ferrofire FD | 49 | 99 | 101 | 41 | 72 | 52 | 70 | 80 | 47 | 51 |
| 1250 | 0.1 | C++ provider + stock FD | 103 | 205 | 208 | 124 | 193 | 5102 | 9627 | 9708 | 46 | 6005 |
| 1250 | 0.1 | ferrofire + stock FD | 99 | 102 | 105 | 87 | 94 | 5017 | 9517 | 9649 | 47 | 6015 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 103 | 205 | 207 | 87 | 189 | 165 | 207 | 210 | 45 | 59 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 99 | 102 | 127 | 90 | 94 | 89 | 142 | 147 | 46 | 44 |
| 1250 | 0.05 | C++ provider + stock FD | 151 | 157 | 163 | 124 | 144 | 5085 | 9587 | 9643 | 53 | 6019 |
| 1250 | 0.05 | ferrofire + stock FD | 49 | 98 | 102 | 50 | 78 | 5017 | 9519 | 9592 | 46 | 6015 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 118 | 159 | 169 | 99 | 149 | 91 | 141 | 144 | 49 | 33 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 49 | 99 | 103 | 35 | 66 | 54 | 98 | 103 | 47 | 51 |

### Burst: simultaneous writes, one Modbus client per RTU (10 RTUs), done within 10 s

| points | publish s | stack | burst size | issued | done | lost | lost % |
|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.1 | ferrofire + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 1000 | 3000 | 1415 | 1585 | 52.8 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.05 | ferrofire + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 1000 | 3000 | 1520 | 1480 | 49.3 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |

### Publish rate and cost of the provider, idle (nothing driving it)

| points | publish s | stack | publishes/s | interval mean ms | p99 ms | datagrams per publish | bytes per datagram | provider cpu % of a core | provider peak RSS MB | 10 field devices cpu % | field device peak RSS MB (largest of the 10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + stock FD | 9.73 | 103.1 | 110.4 | 2.0 | 1068 | 0.9 | 4.7 | 3.0 | 6.4 |
| 100 | 0.1 | ferrofire + stock FD | 10.05 | 100.0 | 108.6 | 1.0 | 277 | 1.2 | 7.1 | 2.1 | 6.3 |
| 100 | 0.1 | C++ provider + ferrofire FD | 9.48 | 106.0 | 110.6 | 2.0 | 1068 | 0.9 | 4.6 | 1.0 | 6.4 |
| 100 | 0.1 | ferrofire + ferrofire FD | 10.03 | 100.0 | 109.7 | 1.0 | 277 | 1.1 | 7.1 | 0.8 | 6.0 |
| 100 | 0.05 | C++ provider + stock FD | 18.98 | 52.8 | 60.3 | 2.0 | 1068 | 1.6 | 4.7 | 4.4 | 6.4 |
| 100 | 0.05 | ferrofire + stock FD | 20.05 | 50.0 | 58.7 | 1.0 | 277 | 1.9 | 7.1 | 2.8 | 6.3 |
| 100 | 0.05 | C++ provider + ferrofire FD | 18.12 | 55.4 | 60.5 | 2.0 | 1068 | 1.4 | 4.6 | 1.2 | 6.4 |
| 100 | 0.05 | ferrofire + ferrofire FD | 20.05 | 50.0 | 59.4 | 1.0 | 277 | 1.8 | 7.1 | 0.9 | 5.9 |
| 1250 | 0.1 | C++ provider + stock FD | 9.60 | 104.8 | 112.5 | 17.9 | 1475 | 5.1 | 5.2 | 10.2 | 7.5 |
| 1250 | 0.1 | ferrofire + stock FD | 10.05 | 100.0 | 109.0 | 3.0 | 943 | 3.2 | 7.8 | 2.9 | 7.5 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 9.35 | 107.7 | 112.8 | 17.9 | 1475 | 4.6 | 5.6 | 3.3 | 6.7 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 10.03 | 100.0 | 109.5 | 3.0 | 943 | 3.0 | 7.8 | 1.3 | 7.0 |
| 1250 | 0.05 | C++ provider + stock FD | 18.10 | 55.4 | 62.5 | 17.9 | 1475 | 9.1 | 5.3 | 17.7 | 7.5 |
| 1250 | 0.05 | ferrofire + stock FD | 20.05 | 50.0 | 59.0 | 3.0 | 943 | 5.5 | 7.8 | 4.0 | 7.5 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 17.45 | 57.5 | 62.7 | 18.0 | 1475 | 8.0 | 5.6 | 5.6 | 7.0 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 20.05 | 50.0 | 59.7 | 3.0 | 943 | 5.4 | 7.8 | 2.2 | 6.7 |

## What it shows

- **Write to visible.** With one unsynchronised timer each, ferrofire shows a write at the next tick-plus-publish: p50 99 ms at a 0.1 s publish interval and 49 ms at 0.05 s, p95 101 / 99 ms. The C++ chain has two unsynchronised timers, the stub's 50 ms step (a write applied in step *k* is published by step *k+1*; "applied" is about 45-55 ms after the write) and the provider's own `publish-rate` sleep, so latency is bimodal: p50 103-166 ms and p95 about 201 ms at 0.1 s, p50 118-151 ms at 0.05 s. At 0.05 s ferrofire is 49 ms and C++ 118-151 ms (2.4-3x). **Three reps do not pin the C++ figure**: the stub's step and the provider's sleep run unsynchronised and keep their phase for a whole run, so each rep sits at one point of the bimodal distribution (per-rep p50, 0.1 s: 198 / 100 / 199 ms at 100 points; 102 / 104 / 201 ms at 1 250 points, where the pooled 103 ms is within 4 ms of ferrofire's 99), and the pooled p50 is closer to which phases three runs happened to draw than to a stable median. The robust statements are that ferrofire's latency is the same in every rep (98-99 / 49 ms) and the C++ chain's ranges from ferrofire's value up to about two publish intervals; the factor 2.4-3x is indicative only. The C++ provider's worth also depends on the stub step you pick; a faster step would pull its figure toward `publish-rate`.
- **Publish rate achieved.** ferrofire publishes at the configured interval (10.05 and 20.05 per s). The C++ provider sleeps `publish-rate` *after* each publish, so the period is the interval plus the work: 103-108 ms at 0.1 s and 53-58 ms at 0.05 s (9.4-9.7 and 17.5-19.0 per s), and it grows with N (104.8 ms at 1 250 points, 0.1 s; 55.4 ms at 0.05 s).
- **Burst losses, ferrofire field device (isolates the provider).** ferrofire: 3 000 of 3 000 writes visible at 1 250 points; no loss. C++ provider: 1 000 simultaneous writes lose 49-53% (1 585 and 1 480 of 3 000 lost). Cause (`prov.log.gz`): the provider's `write()` puts each tag in the non-blocking `updates_fifo`; a Linux pipe holds 64 KiB = 256 of the 256-byte messages, the stub drains it once per 50 ms step,
  so about 256 writes per step get in and the rest answer `ERR=Problem writing tag in simulink provider` ("Problem sending update message to FIFO" in the log), which the field device logs as "provider rejected" and does not retry. The contract is the Simulink one, the 0.05 s step is mine: a slower real model loses more, a faster one fewer. 100 simultaneous writes lose none on either side.
- **Provider cost, idle.** CPU (percent of one core, C++ = provider plus stub): 0.9 vs 1.1 at 100 points and 0.1 s, 5.1 vs 3.2 at 1 250 points, 9.1 vs 5.5 at 1 250 points and 0.05 s: ferrofire is cheaper once there is real data (the C++ provider re-parses every shm slot into `std::string`s per publish and holds a lock over it). Peak RSS: C++ provider 4.6-5.6 MB (+1.7 MB stub) vs ferrofire 7.1-7.8 MB.
- **Field devices.** Behind the C++ provider the stock field device costs more CPU than behind ferrofire (10.2 vs 2.9 percent across 10 devices at 1 250 points, 0.1 s; 17.7 vs 4.0 at 0.05 s), because each of the 10 devices receives and parses the whole 1 250-point stream (18 chunks) where ferrofire publishes it a 125-point slice (3 datagrams) per RTU; ferrofire's own field device is the cheapest everywhere (1.3 / 2.2 percent).
- **Correctness.** `wrong_final` (a done command whose point is not at the target after a 1 s settle) was 0 in every cell of every pairing.

Caveat for all of the above: one host, one VM, three reps; sequential-latency differences of tens of ms are about the size of the publish interval.
