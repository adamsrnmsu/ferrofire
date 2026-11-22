# ferrofire as a HELICS federate (ww-7mw.4)

`ferrofire run` with a `helics:` section joins a HELICS federation with its own engine ([docs/helics.md](../../docs/helics.md)). This
is the worked example: a tank (`example.yaml`) and a bang-bang level controller written with the HELICS Python wheel
(`controller.py`).

```
make helics            # = make dut, then sh bench/helics/run.sh
```

`run.sh` starts one `ff-physics:22.04` container (HELICS 3.6.1 wheel, `helics_broker`, pyzmq; built from
[`bench/physics/Dockerfile`](../physics/Dockerfile) when missing) holding

| process | role |
|---|---|
| `helics_broker -f 2` | the broker, two federates |
| `ferrofire run example.yaml` | federate `tank`: subscribes `ctl/valve` (the model input `valve.open`), publishes `tank/level` and `tank/high`; REP on 5555 |
| `controller.py` | federate `ctl`: opens the valve below 2.0 m, closes it above 3.0 m |

The controller prints one line per 0.1 s grant and judges the run (exit 1 on any `FAIL`):

```
t=0.1 level=2.500000 high=0 valve=0
t=0.2 level=2.468477 high=0 valve=0
...
t=7.5 level=2.957423 high=0 valve=1
ok:   REP read of tank.level (2.0523568407843906) equals the federate's next value (2.0523568407843906)
ok:   the level was updated at every one of 600 grants
ok:   tank/high agrees with the level at every grant
ok:   the level moved with the valve command 3 grants earlier (597 steps)
ok:   the controller both opened and closed the valve
ok:   the level settled in the band (min 1.935, max 3.038)
```

Values go both ways: the level and the high flag (ferrofire to the controller) and the valve command (controller to ferrofire).
The 3-grant lag is one grant for the command to reach ferrofire, one for the step it acts on, one for the new level to come back.

The REP check holds ferrofire at a grant (the controller waits half a second before asking for the next one), reads
`tank.level` over its REP socket, and compares it with the level HELICS then delivers: one engine, two interfaces.

Output goes to `target/helics/` (`controller.txt`, `ferrofire.out`, `broker.out`). Env: `FF_BIN` (default
`target-linux/debug/ferrofire`), `IMAGE`, `OUT`, `GRANTS` (600), `PFX` (container name prefix, `ff-helics`). Only the container
`<PFX>-run` is created and removed.
