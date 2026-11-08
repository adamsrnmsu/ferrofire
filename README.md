# rustbelt

Rust process simulator for [sceptre](https://github.com/sandialabs/sceptre-phenix-apps) / phenix ranges, with AC power flow
(a PYPOWER 5.1.19 port). Models are data: one YAML file. No MATLAB, no PowerWorld.

Status: **phase 1** (engine core, the waterway lock as a model file, AC/DC power flow). The native bennu protocol
(REP + UDP RADIO) and the `pybennu-power-solver` drop-in are later phases; design in the range repo's `docs/provider-spec.md`.

## Use

```
rustbelt check models/waterway.yaml --steps 400 --write lowervalve.open=1@2   # offline run, prints the snapshot
rustbelt run models/waterway.yaml [--server tcp://0.0.0.0:5555] [--publish 'udp://*;239.0.0.1:40000'] [--rate 0.1]
rustbelt simulink [-tf inf] [--model model.yaml]                                # shm adapter (see CONTRACT.md)
simulinksolver -tf inf                                                          # same adapter, selected by argv[0]
```

The adapter loads `--model`, else `./model.yaml`, else the embedded `models/waterway.yaml`. Tag order follows
`publishPoints.txt` if present, else the model's `outputs` order.

## Model format (v1, phase 0)

`version`, `dt`, `substeps`, `params` (constants), `inputs` (RTU-writable, `type: analog|binary`, `bool: true` latches
nonzero as 1), `states` (`ode: expr` integrated with RK4, or `toward: {target, rate}`; `init`, `min`, `max`),
`bodies` (1-D mover with `barriers` and `bounce`; exposes `<name>.x`, `<name>.dir`), `outputs` (`expr`, or no expr to
republish an input/state of that name), optional `publish_order`. Expressions: numbers, `+ - * / % ^`, `! -`,
comparisons (1/0), `&& ||`, `c ? a : b`, `if(c,a,b)`, `min max abs sqrt exp ln pow clamp sign step floor`;
tags containing `-` are back-quoted. See `models/waterway.yaml`.

## Power (phase 1)

A model may carry a `power:` block: `case: file.py|file.m` (relative to the model) or `inline:` buses/branches/gens,
`islands: deenergize|fail` (default `deenergize`), `dc_fallback`, `max_interval`, and `drive:` (process -> power
couplings, `"load-1_bus-3.mw": "expr"`, evaluated in order before each solve). Every PYPOWER/pybennu tag
(`bus-N.voltage`, `load-1_bus-N.mw`, `generator-1_bus-N.active`, `branch-1_F-T.active`, `bus-alert.active`, ...) is published and
readable in expressions with back-quotes; writable ones (RTU writes, `check --write`) edit the case and trigger a
re-solve on the next step. Solver: Newton-Raphson with dense LU, ported from PYPOWER (ext2int, bustypes, makeYbus,
makeSbus, dSbus_dV, newtonpf, pfsoln, makeBdc, dcpf); no `ENFORCE_Q_LIMS`, no OPF, no `.mat`.
Deliberate deviations from pybennu: load/shunt writes affect the solve; a bus keeps its PV/PQ type when `active` is
written; `deenergize` zeroes buses no reference generator reaches. `models/pump-station.yaml` couples a pump to bus 3:

```
rustbelt check models/pump-station.yaml --steps 600 --write pump.run=true@0 --write 'branch-1_2-3.active=false@30'
```

`run` is the native bennu provider: ZMQ REP (`QUERY=`/`READ=`/`WRITE=`, `ACK=`/`ERR=`, NUL-terminated) plus UDP
RADIO/DISH datagrams (`[len][group][tag:value,...\0]`, group = libstdc++ hash of the endpoint, chunked at 1400 bytes).
The zmq crate links libzmq statically from `libzmq3-dev` (jammy 4.3.4), so the binary needs no libzmq on the guest.
`tests/conformance.sh` (Lima VM) runs it next to stock `bennu-probe` and `bennu-field-device` in `bennu:arm64`.

## Build / test

No cargo needed on the host: `make test` and `make linux` use a Docker `ubuntu:22.04` image (`docker/Dockerfile`).
`make golden` generates PYPOWER 5.1.19 reference results into `target/golden` (needs `uv`; never committed;
PYPOWER 5.1.19 needs numpy<2, so Python 3.11 is pinned) and `make test` depends on it. `make linux` writes glibc aarch64 `dist/rustbelt` and `dist/simulinksolver`. It must be glibc: the musl build hangs the
stock `bennu-simulink-provider` (different `sem_t` layout). `ipc-test.sh` checks the adapter against the stock provider
inside `bennu:arm64` (run in the Lima VM).

## Licence

MIT, Copyright (c) 2026 Ryan Adams. `NOTICE` carries the PYPOWER (BSD-3, PSERC) notice for ported code. No MATPOWER case
files are vendored. bennu/pybennu are GPL-3 and are not translated: only wire formats and observed behaviour are matched.
