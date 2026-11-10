# Live tool check on the waterway range (2026-11-10)

Every ferrofire tool of the tools workflow (`probe`, `doctor`, `log`, `top`) run for real on the `waterway-arm64` experiment (Lima VM `phenix`, minimega,
aarch64 KVM guests): provider VM `waterway-provider` (172.16.1.2) running `ferrofire run waterway.yaml` from `make linux` in place of the stock
`bennu-simulink-provider`, and the **stock** `bennu-field-deviced` on `rtu-1` (172.16.1.101 / mgmt 10.1.1.101) and `rtu-2` (172.16.1.102 / 10.1.1.102).
`scada-server` (172.16.2.71, same L2 segment, no libzmq) is the bystander host. Commands and output are verbatim in `captures/`; the scripts that
produced them are next to this file. Driven through minimega `cc` (no host route to the guests), as in `docs/bench/datagram-v1-bench.md` G.

Binary: `dist/ferrofire` md5 `ab37b842fd4ae783eb7609f3bff13de7` (main at 03e97ee), copied to `/root/ff` on provider, rtu-1 and scada-server.

| capture | what it proves |
|---|---|
| `01-probe-query-read.txt` | `probe query` lists the 15 tags; `read` by name and by glob (`*.open`), `--json`, a typo gets the provider's "did you mean" and exit 1. |
| `02-probe-write-rtu-follows.txt` | `probe write lowervalve.open=1,lowergate.open=0` at +0.5 s: rtu-1's Modbus input registers (polled every 50 ms) go 30003 0 -> 65 and 30002 65 -> 0 at +577 ms, i.e. the write reaches the stock RTU through the multicast stream within one publish period. The stock RTU logic then recomputes both outputs from its sensors and puts them back (+993 ms / +1515 ms): a manual write on these four tags lives about a second on this range. Refused writes (`=5` on a boolean, unknown tag) and `--dry-run`. |
| `03-probe-play.txt` | `probe play` of a 3-step script (`at: 0, 2, 4`): the audit lines land at 0 ms, 2012 ms, 4023 ms; rtu-2's registers (polled every 50 ms) follow each step; `--dry-run` first. |
| `04-probe-watch-stale.txt` | `probe watch` for 16 s with the provider SIGSTOPped for 5 s. **Finding:** it prints only when a datagram arrives, so the 5.07 s silence shows as a gap and no tag is ever printed STALE (bead filed). `top` does this (09). |
| `05-probe-record-60s.csv` | `probe record --duration 60`: 601 rows, cycles 2249..2849, 0 gaps, 15 tags with their `_stale` columns, no stale flag set. |
| `06-probe-diff.txt` | `probe diff --map rtus.yaml` (`rtus.yaml`: both RTUs, 22 registers, from their `config.xml`): 20 cycles, no output, exit 0. Then FC6 writes of 40001 = 65535 straight onto rtu-1 while diff polls every 50 ms: 9 mismatches `modbus=1000 provider=0`, exit 1; clean again afterwards. The stock device applies the forged write on its 1 s scan and its logic overwrites it ~50 ms later (`mbq.py watchh`), so the window is short and a slow diff would miss it. The forged value also shows in input register 30003 (it mirrors the same tag), hence two lines per hit. |
| `07-doctor-ovs-snooping.txt` | `ferrofire doctor` with OVS multicast snooping off, on, and after a re-join (bridge `phenix` in the minimega container, restored to default at the end: `false`, `{}`). It catches the wrong multicast route on a multihomed host (FAIL datagrams), a healthy path (41 datagrams in 4 s, no gaps, no bystanders), and, with snooping on and no querier, a host that joined before snooping was enabled and is now starved (FAIL datagrams; rtu-1's Modbus register frozen at 195.99 while the boat moves 5 m/s), recovering after a re-join (the bridge's `mdb/show` lists the port). It also warns of the 1.0 s clock skew between the VMs. The stock device holds :40000 without SO_REUSEADDR, so doctor cannot run on an RTU (bead filed). |
| `08-log-idempotent.txt` | `ferrofire log` on the running provider: `on` x3 to one file prints "already logging to <path>" twice and leaves one file with one line (the rejected WRITE, logged once: the provider rate-limits it to one per 10 s); `on --file OTHER` switches and closes the first; `on` again appends; `level debug --for 10s` reverts by itself; `tail`; `off` x2 prints "already off"; every command prints `ferrofire run (pid N): logging to <destination>, level L`. |
| `09-top-live.txt` | `ferrofire top` in two tmux panes (attached to the local process, and on the multicast stream) captured as text while the boat moves, with the provider SIGSTOPped 5 s (all 15 tags STALE at +4 s in both views, age 4.4 s), and 1.5 s after SIGCONT (0 stale). The log pane shows a rejected WRITE. `--once` text and `--json` dumps. A `--stream --once` on a silent stream prints nothing for 10 s (bead filed). |
| `10-static-no-libzmq.txt` | The aarch64 binary on `scada-server`, which has no libzmq (0 entries in ldconfig, no zmq package): `ldd` shows libgcc_s, libm, libc only; `probe read` and `query` against the provider work. It is dynamic glibc, not static: fully static builds are ww-otj.41. |
| `11-stock-rtus-vs-zeromq-provider.txt`, `11b-stock-rtus-track.txt` | The stock libzmq `bennu-probe` on rtu-1 reads, writes and gets the ERR for an unknown tag from the `zeromq`-crate REP provider; the stock devices' own REQ writes (their logic opens the lock valves as the boat arrives: `lock.height` 5 -> 15 m between 15 s and 35 s) and their Modbus registers track the provider (boat within 0.5 m, lock within 0.08 m, both RTUs). |

## Tools

- `on.sh HOST SECS CMD` / `cap.sh NAME HOST SECS CMD` : run a command on a range VM through minimega `cc` (uses `bench/live/rng.sh`) and save it to `captures/`.
- `prov.sh` : start `ferrofire run` as the provider. `mbq.py` : minimal Modbus/TCP client (FC3/4/6, 50 ms `watch`). `rtus.yaml` : register map for `probe diff`.
- `doctor-ovs.sh` : the snooping off/on/re-join run (Mac side, restores the bridge). `restart-rtu.sh` : restart a stock device the way the image's startup script does.
  Do not `pkill -f bennu-field-deviced`: it also kills the watcher (its arguments name the binary) and leaves a stale `/run/default-bennu-field-deviced.pid` that stops the next one starting.
- `top-live.sh` : the tmux capture of `top`.

## State after the run

Both RTUs run their original stock `bennu-field-deviced` with the untouched `/etc/sceptre/config.xml` (rtu-1's device was restarted several times, rtu-2's never); OVS snooping is back to
its default (off, `other_config` empty); the experiment was stopped. Guests are rebuilt from the image on the next `exp start`, so `/root/ff` is gone with them.
