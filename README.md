# rustbelt

Rust process simulator for [sceptre](https://github.com/sandialabs/sceptre-phenix-apps) / phenix ranges, with AC power flow
(a PYPOWER port) to follow. Models are data: one YAML file. No MATLAB, no PowerWorld.

Status: **phase 0** (engine core + the waterway lock as a model file). Power flow, the native bennu protocol
(REP + UDP RADIO) and the `pybennu-power-solver` drop-in are later phases; design in the range repo's `docs/provider-spec.md`.

## Use

```
rustbelt check models/waterway.yaml --steps 400 --write lowervalve.open=1@2   # offline run, prints the snapshot
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

## Build / test

No cargo needed on the host: `make test` and `make linux` use a Docker `ubuntu:22.04` image (`docker/Dockerfile`).
`make linux` writes glibc aarch64 `dist/rustbelt` and `dist/simulinksolver`. It must be glibc: the musl build hangs the
stock `bennu-simulink-provider` (different `sem_t` layout). `ipc-test.sh` checks the adapter against the stock provider
inside `bennu:arm64` (run in the Lima VM).

## Licence

MIT, Copyright (c) 2026 Ryan Adams. `NOTICE` carries the PYPOWER (BSD-3, PSERC) notice for ported code. No MATPOWER case
files are vendored. bennu/pybennu are GPL-3 and are not translated: only wire formats and observed behaviour are matched.
