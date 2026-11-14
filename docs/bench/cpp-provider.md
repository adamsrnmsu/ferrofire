# ferrofire vs Sandia's C++ bennu-simulink-provider, with a stub model (ww-1df.42)

**Plumbing only: there is no solver in this comparison.** Neither side runs a power flow or a Simulink model. It measures what the provider and the transport add (shared-memory copy and publish on one side, the engine tick and publish on the other), with the same field devices and the same SCADA driver as the [control-latency bench](control-latency.md).
It says nothing about solve time, and a real Simulink step would add its own compute time and step size on top of every figure below.

**Two runs, labelled by the stock bennu in them (ww-1df.47):** the first (ww-1df.42) used bennu 6.0.0 (pre-91fd137) for both the stock field device and the C++ `bennu-simulink-provider`; the rerun uses bennu v2026.11.03 (the current release) for both. Results for both are below, the rerun is the current one.

Raw files: [results/cppprov-v2026.11.03/](results/cppprov-v2026.11.03/) (bennu v2026.11.03) and [results/cppprov/](results/cppprov/) (bennu 6.0.0; the ferrofire + ferrofire FD pairing is only here); `summary.md` in each is the output of `bench/cppprov/report.py`. Method and commands: [bench/README.md](../../bench/README.md#provider-vs-sandias-c-bennu-simulink-provider-stub-model-ww-1df42). Everything ran on the default Docker daemon (Docker Desktop VM, 18 vCPU, 7.7 GB), linux/arm64, one heavy container set at a time (the only other container was the stale `elegant_zhukovsky`).

## What was compared

| | C++ side | ferrofire side |
|---|---|---|
| Provider | `bennu-simulink-provider`, **unmodified**, `--publish-rate` 0.1 / 0.05 | `ferrofire run model.yaml --stamp-writes`, `publish_rate` 0.1 / 0.05 |
| "Model" | `bench/cppprov/stub_model.c`: a C program that speaks the `modelHooks` contract (SysV shm 10610/10613, POSIX semaphores `publish_sem` / `updates_sem`, FIFO `/tmp/updates_fifo`, 256-byte `tag:TYPE:value` slots). N points `brk-NNNN.active` (BOOLEAN, start closed), fixed step 0.05 s on an absolute clock, `step()` in the order of `modelHooks.c` (publish state, then apply FIFO updates), a write sets the point and the next step publishes it | `bench/cppprov/gen.py` `model.yaml`: N binary inputs `brk-NNNN.active` (default true) republished by same-named outputs, no `power:` block, `dt` 0.05 s, one publish slice per RTU |
| Field devices | stock `bennu-field-deviced` and `ferrofire field-device`, the same config file for both (shape of `gen_grid.py`'s `fd_xml`), 10 RTUs | same |
| Driver | `ffcontrol`, the binary of the control bench, `--fresh` stack per cell | same (`--read-per 100` instead of `--read-per 1 --legacy --status-only`, as for pybennu) |

Points: N = 100 and 1 250. Publish interval: 0.1 s and 0.05 s. 3 reps of every cell, pooled in the tables. Cells: sequential flips of 10 and 50 breakers (`seq`, pooled), 10 and 100 simultaneous flips (`sim:10`, and the burst at 100 points), 1 000 simultaneous flips at 1 250 points (the burst there).
Four pairings, provider + field device: C++ + stock, ferrofire + stock, C++ + ferrofire, ferrofire + ferrofire (the last has no bennu program and was not rerun). Latency is `t_visible - t_cmd`, a Modbus write to the first read-back (2 ms poll) that shows the new state at or after the stream datagram that carries it. "applied" is the provider-side stamp (`Applied:` line, printed by the stub for the C++ provider) minus `t_cmd`.

### Deviations and things to know

- **The C++ provider was not rebuilt for the first (6.0.0) run.** It is `/usr/local/bin/bennu-simulink-provider` of the phenix-waterway-range `bennu-arm64` overlay (sha256 prefix in every `env.txt`), built 2026-11-07 by `images/docker/bennu-arm64.Dockerfile` from a clean `git archive HEAD` of sceptre-bennu (HEAD `c9c2cf6`; the working tree has uncommitted, non-compiling WIP, so any rebuild would export HEAD again).
  The provider's source and its `distributed` library last changed in 2025 (`6f771a4`, `1761191`), long before the build; the uncommitted diff of `main.cpp` is whitespace. It runs in the `ffc-stock:22.04` image (Ubuntu 22.04 with the overlay's `/usr/local`). The stub is compiled in the `ff-rust` container with `gcc -O2`.
  **For the v2026.11.03 rerun** the provider and the field device are the programs of `ffc-stock-2026.11.03:22.04` (sceptre-bennu tag `v2026.11.03`, built by `bench/stock_image.sh`, `STOCK_IMAGE=ffc-stock-2026.11.03:22.04`); the sha256 prefix of the provider (`9a0de9e25217978c`, against `c56564bc721e550d` for 6.0.0) is in every `env.txt`. The same stub and configs, the same driver and cells.
- **10 RTUs, not 1.** With one RTU the bennu 6.0.0 field device forwarded about one write per 0.5 s (control-latency.md, sections 8 and 9), which hides every provider difference; at 10 RTUs it is not the limit for sequential flips. (v2026.11.03 has no such floor; 10 RTUs stayed for comparability.)
- **A burst of 1 000 through the bennu 6.0.0 field device was the device's limit, not the provider's:** both providers lost 80% (the device forwarded about 2 writes per second per RTU and never sent the rest; the provider logs show 240 WRITE requests, 0 FIFO errors). That was bennu 6.0.0 (before the upstream batched-write fix 91fd137, release v2026.11.03, which removed a fixed 500 ms sleep per request in the field device's `Client::send`). **With v2026.11.03 nothing is lost in a burst of 1 000**, behind any provider (table below). The pairings with the ferrofire field device isolate the provider.
- `wrong_provider` of the driver is meaningless for the C++ provider (its READ replies `ACK=false`, without the `tag:` the driver parses); `wrong_final` was 0 everywhere and the provider's logged READs agree. The column is not reported.
- The ferrofire field device normally drops datagrams of stock pybennu (no NUL terminator). The C++ provider's publisher appends the NUL to every chunk, so C++ + ferrofire FD works; it was run to isolate the provider.
- Stub step 0.05 s is a choice (ferrofire's `dt` is the same). It is also the knob behind the burst loss below. The stub omits the solver's compute time and `modelHooks.c`'s per-point linear search of the CAPI signal table.
- Idle rate/cpu: a 20 s window after 8 s of start-up. `rate.py` listens on RTU 1's group only: publishes per second and the interval are the provider's; datagrams per publish and bytes per datagram are of that group (the C++ provider publishes all N points in one stream cut into chunks, ferrofire one slice per RTU). CPU is `/proc` ticks of the provider processes (C++: provider plus stub), resolution 0.05% of a core; RSS is the peak (`VmHWM`) over the run.
- Not measured: R = 1, other poll intervals, a real Simulink model.

## Results

Two runs of the same bench, labelled by the stock bennu in them (ww-1df.47). The **bennu v2026.11.03** run is the current one; the **bennu 6.0.0 (pre-91fd137)** run (ww-1df.42) is kept as history. In the first, "stock" is 6.0.0 in both the stock field device and the C++ `bennu-simulink-provider`; in the rerun both come from the v2026.11.03 image. The ferrofire + ferrofire FD pairing has no bennu program in it, was not rerun and is shared (it is in the 6.0.0 tables).

### bennu v2026.11.03 (current)

#### Write to visible, ms (p50 / p95 / max over all reps)

| points | publish s | stack | seq p50 | seq p95 | seq max | sim:10 p50 | sim:10 p95 | burst p50 | burst p95 | burst max | seq applied p50 | burst applied p50 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 101 | 201 | 202 | 128 | 185 | 137 | 180 | 181 | 44 | 32 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 99 | 100 | 101 | 108 | 119 | 107 | 129 | 129 | 47 | 55 |
| 100 | 0.1 | C++ provider + ferrofire FD | 102 | 201 | 208 | 113 | 170 | 108 | 200 | 201 | 46 | 55 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 102 | 151 | 160 | 108 | 147 | 89 | 142 | 143 | 45 | 32 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 49 | 99 | 101 | 53 | 86 | 55 | 82 | 83 | 47 | 53 |
| 100 | 0.05 | C++ provider + ferrofire FD | 148 | 153 | 159 | 129 | 157 | 99 | 168 | 169 | 52 | 41 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 103 | 203 | 206 | 135 | 200 | 168 | 189 | 194 | 47 | 63 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 99 | 102 | 107 | 111 | 130 | 96 | 114 | 117 | 47 | 48 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 199 | 203 | 207 | 109 | 113 | 132 | 189 | 191 | 51 | 61 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 149 | 155 | 160 | 114 | 144 | 121 | 154 | 159 | 51 | 55 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 49 | 99 | 101 | 64 | 78 | 48 | 91 | 101 | 47 | 45 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 150 | 155 | 204 | 101 | 144 | 114 | 129 | 162 | 51 | 44 |

#### Burst: simultaneous writes, one Modbus client per RTU (10 RTUs), done within 10 s

| points | publish s | stack | burst size | issued | done | lost | lost % |
|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |

#### Flaps: the read-back showed the new state, then the old, then the new again (all cells pooled)

| points | publish s | stack | commands | flaps | flaps % |
|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 510 | 267 | 52.4 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 510 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 510 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 510 | 490 | 96.1 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 510 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 510 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 3510 | 2161 | 61.6 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 3510 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 3510 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 3510 | 3493 | 99.5 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 3510 | 104 | 3.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 3510 | 0 | 0.0 |

#### Publish rate and cost of the provider, idle (nothing driving it)

| points | publish s | stack | publishes/s | interval mean ms | p99 ms | datagrams per publish | bytes per datagram | provider cpu % of a core | provider peak RSS MB | 10 field devices cpu % | field device peak RSS MB (largest of the 10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 9.72 | 103.5 | 110.2 | 2.0 | 1068 | 0.9 | 5.3 | 3.0 | 7.0 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 10.05 | 100.0 | 109.2 | 1.0 | 277 | 1.2 | 7.1 | 2.2 | 6.9 |
| 100 | 0.1 | C++ provider + ferrofire FD | 9.48 | 105.9 | 110.5 | 2.0 | 1068 | 0.8 | 5.3 | 0.9 | 6.6 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 18.95 | 52.9 | 60.2 | 2.0 | 1068 | 1.5 | 5.3 | 4.4 | 7.0 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 20.05 | 50.0 | 59.0 | 1.0 | 277 | 1.9 | 7.1 | 2.7 | 6.9 |
| 100 | 0.05 | C++ provider + ferrofire FD | 18.15 | 55.3 | 60.4 | 2.0 | 1068 | 1.3 | 5.3 | 1.3 | 6.5 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 9.65 | 104.3 | 111.4 | 18.9 | 1399 | 3.6 | 5.8 | 9.0 | 8.1 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 10.05 | 100.0 | 109.5 | 3.0 | 943 | 3.1 | 7.8 | 2.7 | 8.1 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 9.42 | 106.8 | 111.8 | 18.9 | 1399 | 3.5 | 5.8 | 3.5 | 7.3 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 18.53 | 54.1 | 61.5 | 19.0 | 1399 | 6.6 | 5.8 | 15.9 | 8.1 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 20.03 | 50.0 | 58.9 | 3.0 | 943 | 5.4 | 7.9 | 3.9 | 8.1 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 17.88 | 56.0 | 61.7 | 18.9 | 1399 | 6.0 | 5.8 | 6.2 | 6.9 |

### bennu 6.0.0 (pre-91fd137, the first run)

#### Write to visible, ms (p50 / p95 / max over all reps)

| points | publish s | stack | seq p50 | seq p95 | seq max | sim:10 p50 | sim:10 p95 | burst p50 | burst p95 | burst max | seq applied p50 | burst applied p50 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu 6.0.0 FD | 166 | 201 | 205 | 146 | 192 | 2606 | 4653 | 4729 | 49 | 2529 |
| 100 | 0.1 | ferrofire + bennu 6.0.0 FD | 99 | 101 | 115 | 84 | 112 | 2514 | 4605 | 4638 | 47 | 2513 |
| 100 | 0.1 | C++ provider + ferrofire FD | 150 | 201 | 204 | 143 | 186 | 131 | 189 | 190 | 49 | 36 |
| 100 | 0.1 | ferrofire + ferrofire FD | 99 | 101 | 104 | 89 | 144 | 66 | 81 | 125 | 47 | 27 |
| 100 | 0.05 | C++ provider + bennu 6.0.0 FD | 127 | 152 | 200 | 99 | 145 | 2606 | 4647 | 4696 | 49 | 2530 |
| 100 | 0.05 | ferrofire + bennu 6.0.0 FD | 49 | 99 | 103 | 49 | 98 | 2508 | 4556 | 4607 | 47 | 2505 |
| 100 | 0.05 | C++ provider + ferrofire FD | 148 | 152 | 156 | 93 | 143 | 88 | 138 | 138 | 50 | 34 |
| 100 | 0.05 | ferrofire + ferrofire FD | 49 | 99 | 101 | 41 | 72 | 52 | 70 | 80 | 47 | 51 |
| 1250 | 0.1 | C++ provider + bennu 6.0.0 FD | 103 | 205 | 208 | 124 | 193 | 5102 | 9627 | 9708 | 46 | 6005 |
| 1250 | 0.1 | ferrofire + bennu 6.0.0 FD | 99 | 102 | 105 | 87 | 94 | 5017 | 9517 | 9649 | 47 | 6015 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 103 | 205 | 207 | 87 | 189 | 165 | 207 | 210 | 45 | 59 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 99 | 102 | 127 | 90 | 94 | 89 | 142 | 147 | 46 | 44 |
| 1250 | 0.05 | C++ provider + bennu 6.0.0 FD | 151 | 157 | 163 | 124 | 144 | 5085 | 9587 | 9643 | 53 | 6019 |
| 1250 | 0.05 | ferrofire + bennu 6.0.0 FD | 49 | 98 | 102 | 50 | 78 | 5017 | 9519 | 9592 | 46 | 6015 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 118 | 159 | 169 | 99 | 149 | 91 | 141 | 144 | 49 | 33 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 49 | 99 | 103 | 35 | 66 | 54 | 98 | 103 | 47 | 51 |

#### Burst: simultaneous writes, one Modbus client per RTU (10 RTUs), done within 10 s

| points | publish s | stack | burst size | issued | done | lost | lost % |
|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu 6.0.0 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + bennu 6.0.0 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + bennu 6.0.0 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + bennu 6.0.0 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + bennu 6.0.0 FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.1 | ferrofire + bennu 6.0.0 FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 1000 | 3000 | 1415 | 1585 | 52.8 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu 6.0.0 FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.05 | ferrofire + bennu 6.0.0 FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 1000 | 3000 | 1520 | 1480 | 49.3 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |

#### Flaps: the read-back showed the new state, then the old, then the new again (all cells pooled)

| points | publish s | stack | commands | flaps | flaps % |
|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu 6.0.0 FD | 510 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + bennu 6.0.0 FD | 510 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 510 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + ferrofire FD | 510 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + bennu 6.0.0 FD | 510 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + bennu 6.0.0 FD | 510 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 510 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + ferrofire FD | 510 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + bennu 6.0.0 FD | 3510 | 0 | 0.0 |
| 1250 | 0.1 | ferrofire + bennu 6.0.0 FD | 3510 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 3510 | 0 | 0.0 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 3510 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu 6.0.0 FD | 3510 | 0 | 0.0 |
| 1250 | 0.05 | ferrofire + bennu 6.0.0 FD | 3510 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 3510 | 0 | 0.0 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 3510 | 0 | 0.0 |

#### Publish rate and cost of the provider, idle (nothing driving it)

| points | publish s | stack | publishes/s | interval mean ms | p99 ms | datagrams per publish | bytes per datagram | provider cpu % of a core | provider peak RSS MB | 10 field devices cpu % | field device peak RSS MB (largest of the 10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu 6.0.0 FD | 9.73 | 103.1 | 110.4 | 2.0 | 1068 | 0.9 | 4.7 | 3.0 | 6.4 |
| 100 | 0.1 | ferrofire + bennu 6.0.0 FD | 10.05 | 100.0 | 108.6 | 1.0 | 277 | 1.2 | 7.1 | 2.1 | 6.3 |
| 100 | 0.1 | C++ provider + ferrofire FD | 9.48 | 106.0 | 110.6 | 2.0 | 1068 | 0.9 | 4.6 | 1.0 | 6.4 |
| 100 | 0.1 | ferrofire + ferrofire FD | 10.03 | 100.0 | 109.7 | 1.0 | 277 | 1.1 | 7.1 | 0.8 | 6.0 |
| 100 | 0.05 | C++ provider + bennu 6.0.0 FD | 18.98 | 52.8 | 60.3 | 2.0 | 1068 | 1.6 | 4.7 | 4.4 | 6.4 |
| 100 | 0.05 | ferrofire + bennu 6.0.0 FD | 20.05 | 50.0 | 58.7 | 1.0 | 277 | 1.9 | 7.1 | 2.8 | 6.3 |
| 100 | 0.05 | C++ provider + ferrofire FD | 18.12 | 55.4 | 60.5 | 2.0 | 1068 | 1.4 | 4.6 | 1.2 | 6.4 |
| 100 | 0.05 | ferrofire + ferrofire FD | 20.05 | 50.0 | 59.4 | 1.0 | 277 | 1.8 | 7.1 | 0.9 | 5.9 |
| 1250 | 0.1 | C++ provider + bennu 6.0.0 FD | 9.60 | 104.8 | 112.5 | 17.9 | 1475 | 5.1 | 5.2 | 10.2 | 7.5 |
| 1250 | 0.1 | ferrofire + bennu 6.0.0 FD | 10.05 | 100.0 | 109.0 | 3.0 | 943 | 3.2 | 7.8 | 2.9 | 7.5 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 9.35 | 107.7 | 112.8 | 17.9 | 1475 | 4.6 | 5.6 | 3.3 | 6.7 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 10.03 | 100.0 | 109.5 | 3.0 | 943 | 3.0 | 7.8 | 1.3 | 7.0 |
| 1250 | 0.05 | C++ provider + bennu 6.0.0 FD | 18.10 | 55.4 | 62.5 | 17.9 | 1475 | 9.1 | 5.3 | 17.7 | 7.5 |
| 1250 | 0.05 | ferrofire + bennu 6.0.0 FD | 20.05 | 50.0 | 59.0 | 3.0 | 943 | 5.5 | 7.8 | 4.0 | 7.5 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 17.45 | 57.5 | 62.7 | 18.0 | 1475 | 8.0 | 5.6 | 5.6 | 7.0 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 20.05 | 50.0 | 59.7 | 3.0 | 943 | 5.4 | 7.8 | 2.2 | 6.7 |

## What it shows

### With bennu v2026.11.03 (current)

- **Write to visible.** Ferrofire is the same as before: p50 99 ms at a 0.1 s publish interval and 49 ms at 0.05 s, p95 100-102 / 99 ms, identical in every rep. The C++ chain is still bimodal (the stub's 50 ms step and the provider's `publish-rate` sleep, two unsynchronised timers, keep their phase for a whole run): behind the ferrofire field device its p50 is 102 / 199 ms at 0.1 s (100 / 1 250 points; per rep 102 / 101 / 197 and 199 / 102 / 200) and 148 / 150 ms at 0.05 s, p95 about 153-203 ms. At 0.05 s that is **3.0 times** ferrofire's 49 ms on both sizes; at 0.1 s anything from 1.0 to 2.0 times depending on which phases three runs drew, so only the 0.05 s factor is stable and even it is indicative (the stub's step is a choice). With the bennu v2026.11.03 field device behind it the numbers are 101-103 ms at 0.1 s and 102 / 149 ms at 0.05 s. Bennu 6.0.0's C++ chain was in the same range (p50 103-166 ms at 0.1 s, 127 / 151 ms at 0.05 s): the release did not change the C++ provider's latency.
- **Publish rate achieved.** Ferrofire publishes at the configured interval (10.05 and 20.03-20.05 per s); the C++ provider sleeps `publish-rate` *after* each publish, so its period is the interval plus the work: 103.5-106.8 ms at 0.1 s and 52.9-56.0 ms at 0.05 s (9.4-9.7 and 17.9-19.0 per s), as with 6.0.0.
- **Burst losses: none.** All 1 000 simultaneous writes at 1 250 points are visible within 10 s in every pairing: C++ + v2026.11.03 field device 3 000 of 3 000, C++ + ferrofire field device 3 000 of 3 000, ferrofire + v2026.11.03 field device 3 000 of 3 000 (the provider logs of the C++ runs have no FIFO errors). In the 6.0.0 run the field device lost 80% (its 500 ms sleep, removed by 91fd137) and the C++ provider itself lost 49-53% behind the ferrofire field device (its 256-message updates FIFO filled between two 50 ms steps): the release sizes that FIFO to hold a full write batch (`e59691f`, 1 MiB = 4 096 records), which is why the C++ chain no longer loses a burst here. **So the burst advantage ferrofire showed over the C++ chain, and over the stock field device, is gone against current bennu.** 100 simultaneous writes lost none on either run.
- **Flaps: new, and large behind the C++ provider.** The read-back showed the new state, then the old, then the new again for 6 411 of 8 040 commands of C++ + v2026.11.03 field device (99% of the commands at 0.05 s, about half at 0.1 s: 2 996 of 3 000 burst writes at 0.05 s), for 104 of 8 040 of ferrofire + v2026.11.03 field device (all in the 0.05 s cells at 1 250 points) and for none of C++ + ferrofire field device. Bennu 6.0.0 had none in any pairing. The v2026.11.03 field device exposes the written value as soon as the provider ACKs, before the provider has applied and published it, and the next datagrams (published before the apply: the stub applies a write in a step and publishes it in the next) still carry the old value; a ferrofire field device holds an ACKed point against such stale values (ww-1df.32). That reading fits every row but was not traced in the bennu source (follow-up bead). A SCADA polling at 2 ms sees these flaps; the driver counts a command as done only when the new state is also seen on the stream.
- **Provider cost, idle.** CPU (percent of one core, C++ = provider plus stub): 0.9 vs 1.2 at 100 points and 0.1 s (C++ vs ferrofire), 3.6 vs 3.1 at 1 250 points and 0.1 s, 6.6 vs 5.4 at 1 250 points and 0.05 s: the C++ provider is a little more expensive once there is real data and cheaper on small models, with the stock field device behind it (6.0.0: 5.1 vs 3.2 and 9.1 vs 5.5). Peak RSS: C++ provider 5.3-5.8 MB (+1.7 MB stub) vs ferrofire 7.1-7.9 MB.
- **Field devices.** Behind the C++ provider the bennu v2026.11.03 field device costs more CPU than behind ferrofire (9.0 vs 2.7 percent across 10 devices at 1 250 points, 0.1 s; 15.9 vs 3.9 at 0.05 s) for the reason of the first run: each of the 10 devices receives and parses the whole 1 250-point stream (19 datagrams of 1 399 bytes) where ferrofire publishes a 125-point slice (3 datagrams) per RTU. The ferrofire field device behind the C++ provider costs 3.5 / 6.2 percent, the cheapest being ferrofire + ferrofire (first run: 1.3 / 2.2).
- **Correctness.** `wrong_final`, `wrong_collateral`, write errors and lost commands were 0 in every cell of every pairing of the rerun, as in the first run. Contention: no other container was running at the start of any rerun run (`other_containers none`).

### What the first run (bennu 6.0.0) showed

- **Write to visible.** With one unsynchronised timer each, ferrofire shows a write at the next tick-plus-publish: p50 99 ms at a 0.1 s publish interval and 49 ms at 0.05 s, p95 101 / 99 ms. The C++ chain has two unsynchronised timers, the stub's 50 ms step (a write applied in step *k* is published by step *k+1*; "applied" is about 45-55 ms after the write) and the provider's own `publish-rate` sleep, so latency is bimodal: p50 103-166 ms and p95 about 201 ms at 0.1 s, p50 118-151 ms at 0.05 s. At 0.05 s ferrofire is 49 ms and C++ 118-151 ms (2.4-3x). **Three reps do not pin the C++ figure**: the stub's step and the provider's sleep run unsynchronised and keep their phase for a whole run, so each rep sits at one point of the bimodal distribution (per-rep p50, 0.1 s: 198 / 100 / 199 ms at 100 points; 102 / 104 / 201 ms at 1 250 points, where the pooled 103 ms is within 4 ms of ferrofire's 99), and the pooled p50 is closer to which phases three runs happened to draw than to a stable median. The robust statements are that ferrofire's latency is the same in every rep (98-99 / 49 ms) and the C++ chain's ranges from ferrofire's value up to about two publish intervals; the factor 2.4-3x is indicative only. The C++ provider's worth also depends on the stub step you pick; a faster step would pull its figure toward `publish-rate`.
- **Publish rate achieved.** ferrofire publishes at the configured interval (10.05 and 20.05 per s). The C++ provider sleeps `publish-rate` *after* each publish, so the period is the interval plus the work: 103-108 ms at 0.1 s and 53-58 ms at 0.05 s (9.4-9.7 and 17.5-19.0 per s), and it grows with N (104.8 ms at 1 250 points, 0.1 s; 55.4 ms at 0.05 s).
- **Burst losses, ferrofire field device (isolates the provider).** ferrofire: 3 000 of 3 000 writes visible at 1 250 points; no loss. C++ provider: 1 000 simultaneous writes lose 49-53% (1 585 and 1 480 of 3 000 lost). Cause (`prov.log.gz`): the provider's `write()` puts each tag in the non-blocking `updates_fifo`; a Linux pipe holds 64 KiB = 256 of the 256-byte messages, the stub drains it once per 50 ms step,
  so about 256 writes per step get in and the rest answer `ERR=Problem writing tag in simulink provider` ("Problem sending update message to FIFO" in the log), which the field device logs as "provider rejected" and does not retry. The contract is the Simulink one, the 0.05 s step is mine: a slower real model loses more, a faster one fewer. 100 simultaneous writes lose none on either side.
- **Provider cost, idle.** CPU (percent of one core, C++ = provider plus stub): 0.9 vs 1.1 at 100 points and 0.1 s, 5.1 vs 3.2 at 1 250 points, 9.1 vs 5.5 at 1 250 points and 0.05 s: ferrofire is cheaper once there is real data (the C++ provider re-parses every shm slot into `std::string`s per publish and holds a lock over it). Peak RSS: C++ provider 4.6-5.6 MB (+1.7 MB stub) vs ferrofire 7.1-7.8 MB.
- **Field devices.** Behind the C++ provider the stock field device costs more CPU than behind ferrofire (10.2 vs 2.9 percent across 10 devices at 1 250 points, 0.1 s; 17.7 vs 4.0 at 0.05 s), because each of the 10 devices receives and parses the whole 1 250-point stream (18 chunks) where ferrofire publishes it a 125-point slice (3 datagrams) per RTU; ferrofire's own field device is the cheapest everywhere (1.3 / 2.2 percent).
- **Correctness.** `wrong_final` (a done command whose point is not at the target after a 1 s settle) was 0 in every cell of every pairing.

Caveat for all of the above: one host, one VM, three reps; sequential-latency differences of tens of ms are about the size of the publish interval. The rerun ran in a later session than the first run, on the same VM and harness.
