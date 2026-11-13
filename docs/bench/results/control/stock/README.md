# Stock bennu baseline of the control-latency bench (ww-1df.23.4)

Raw data of `STACK=stock python3 bench/run_control.py` (reps 5, the two n = 1000 breaker cells 1 rep each, R = 1, 2, 5, 10). One file set per (R, rep):
`R<R>_rep<k>.cmds.csv` (a row per command), `.cells.csv` (a row per phase), `.driver.log`, `.prov.log.gz` (the provider's `Applied:` stamps), `.fd.log.gz`, `.env.txt`.
`summary.md` is `python3 bench/control_summary.py docs/bench/results/control/stock`. `pg-after-status-flip.txt` is a probe of the stock provider (below).

What ran: stock `pybennu-power-solver` (PyPower, pybennu 6.0.0, pypower 5.1.19) on the generated case, plus R stock `bennu-field-deviced` (bennu 6.0.0) with the generated `bennu/rtu-N.xml`,
from the arm64 bennu overlay of phenix-waterway-range in image `ffc-stock:22.04` (`bench/Dockerfile.stock`). The same SCADA driver (`ffcontrol`) as the ferrofire runs.

Differences from the ferrofire runs, all deliberate:
- **No ramp**: pybennu has no generator ramp. Generator cells are status flips only (coil off / on, done at `t_visible` of the status discrete input); `t_setpoint` is always 0.
  A status flip also does not restore PG: spin-down writes the generator's `mw` to 0 and spin-up leaves it at 0 (`pg-after-status-flip.txt`: 40.0 -> 0.0 -> 0.0), so the stock
  generator does not return to its setpoint without a separate setpoint write (not part of this matrix).
- **A fresh stack per cell** (`--fresh`): a stock provider that falls behind keeps applying a backlog, and restoring a 1000-breaker grid costs as long as the cell. Each cell starts on a freshly started provider and field devices.
- **Stamps**: `bench/stock_provider.py` is the stock solver with `PyPower.write` wrapped to print one `Applied:` line per tag (entry and return of the handler; stock solves before it ACKs, so `t_solved_ns` is the ACK).
  Its power-flow report (`OUT_ALL`, printed by stock on every solve) goes to /dev/null at the fd level.
- **Stream**: stock publishes the whole model (1.4 KB RADIO datagrams, no NUL terminator, noise added) once a second on ONE group for every RTU (`udp://239.0.0.1:40000`).
  `--legacy` lets the driver's Receiver take bodies without the NUL; `t_stream` is the first datagram carrying the new state.
- **Provider READ**: stock answers one tag per READ, so the post-phase check reads one tag at a time (`--read-per 1`).
- Reps: 5 for every cell except `breaker:seq:1000` and `breaker:sim:1000` (1 rep, the first). The ferrofire dir has 5 reps of every cell.

Results in one paragraph (numbers: `summary.md`, medians over all reps):
- Stock never lost, corrupted or reordered a *sequential* command: wrong_final, wrong_provider, wrong_collateral, flaps, write and poll errors are 0 in all 528 phases.
- A sequential command takes about 1.02 s to be visible whatever R is (the next publish, once a second); the provider applies it in about 30 ms. 1000 sequential flips take about 1026 s.
- Simultaneous commands are served at about one per RTU per second (the applied lag grows by 1 s per queued write of an RTU): n commands on R RTUs take about n/R s. With the 10 s limit, 50 flips are
  lost in part on R = 1 (157 of 250 over the reps) and 2 (69), 100 flips on R = 1, 2, 5 (406, 316, 36 of 500), 1000 flips on every R (981, 962, 908, 800 of 1000 on R = 1, 2, 5, 10; first rep only).
  Nothing was lost on R = 10 below n = 1000, or at n <= 10 anywhere.
- Contention: every run started with no other cargo/ff-rust container (`contended_at_start False`), but the host 1-minute load average at run start ranged 3 to 17 (see `host_load_*` in the .env.txt and `vm_load1_start_end` in the cells.csv): the Docker VM has 18 CPUs and stock uses about one.
