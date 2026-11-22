# `ferrofire run` as a HELICS federate (design, ww-7mw.4)

Status: implemented in ww-7mw.4 (see "Corrections found while implementing" at the end; user documentation is docs/helics.md). The risky part (load libhelics from a Rust binary built in
`ff-rust:22.04`, join a broker, exchange values with a second federate) was proved in scratch space; results are in "Feasibility".

## Goal

Acceptance for ww-7mw.4: ferrofire joins a HELICS federation and exchanges values with a second federate, in a run we did, and the
example is documented. "ferrofire" here means its own engine (`ferrofire run`, `src/model.rs`) as the federate, not bennu's pybennu
provider (which `bench/physics` already drives in front of the field device).

## What exists

- `ferrofire run` (`src/bennu.rs`) loads a `model.yaml`, serves the REP protocol (QUERY/READ/WRITE) and publishes datagram v1. The engine
  loop is `engine()`: `tick_and_publish()` then sleep to `k * dt` (real-time paced). One tick latches queued REP writes
  (`Model::apply_writes`), takes `Model::snapshot()`, then `Model::step(dt)`.
- Model inputs (`inputs:`, plus writable power tags) are exactly what a subscription can feed: `Model::write_spec(tag)` gives the type and
  limits, `Model::writable(tag)` the test. Any published tag (`Model::tags()`, `snapshot()`) can feed a publication. Bool vs analog is
  `Kind::{Binary,Analog}`.
- `model.yaml` is `#[serde(deny_unknown_fields)]` (`struct Doc`), so a new top-level section is a deliberate schema addition.
- `unsafe` is `deny` crate-wide (`src/lib.rs`), `forbid` in `main.rs`, and allowed in exactly two modules: `src/sys.rs` (small safe
  wrappers over libc: signals, kill, fork/dup2, setsockopt, getuid, recv) and `src/simulink/ipc.rs` (SysV shm and semaphores for the
  Simulink-solver stand-in). Both are "no safe std API exists" cases with a documented invariant per block.
- `deny.toml` allows MIT, Apache-2.0, BSD-2/3, ISC, Unicode-3.0, Zlib, CC0 for crates; `libc` is already a dependency.
- `docker/Dockerfile` (`ff-rust:22.04`, Ubuntu 22.04, glibc 2.35) is the build and test image; `make test|lint|crap|dry|deny` run in it.
- `bench/physics` (`ff-physics:22.04`, helics 3.6.1 Python wheel and `helics_broker`) is the existing HELICS bench and the pattern for
  a worked example.

## Options considered

| | A. dlopen libhelics (recommended) | B. link libhelics / `helics-sys` | C. sidecar (Python federate bridging to REP/datagrams) |
|---|---|---|---|
| Crate | none new (`libc::dlopen`, already a dep) | `helics` 0.1.0 / `helics-sys` 0.1.2 on crates.io: last updated 2021-03, 1.8k downloads, bind HELICS 2.x; unmaintained. Own bindgen needs libclang and the HELICS headers at build time | none |
| Build cost | zero: default build and CI unchanged, no C++ toolchain, no libhelics at build time | needs libhelics + headers (or a full C++/Boost/ZMQ HELICS build) in every build image and on arm64 CI | none in Rust; needs Python + helics in the run image |
| Run dependency | libhelics.so only when `helics:` is used (error names how to get it otherwise) | libhelics.so always, at process start, even for models without HELICS | Python, the helics wheel, the bridge script, a second process |
| unsafe | one new small module (about 150 lines: symbol table, handles, error struct) | same, plus a build.rs | none in ferrofire |
| Lock-step with the engine | yes, the engine's own loop blocks in `helicsFederateRequestTime` | yes | no: the bridge sees only sampled REP/datagram values and cannot hold the engine at a granted time; the federate would be time-coupled to a free-running real-time engine. It joins a federation, but ferrofire's engine is not what is time-synchronised |
| Licences | HELICS BSD-3 (allowed); never linked, only loaded | BSD-3 | BSD-3 |

Decision: **A**. B buys nothing over A except link-time symbol checking and costs a build dependency on every platform; the only
Rust crates are stale 2.x bindings. C is what bennu's own Helics provider is, and it is the right tool for the bench, but it cannot
satisfy "ferrofire's own engine exchanges values as a federate" in lock-step. C stays the fallback if the unsafe policy is vetoed
(it needs no ferrofire change, only a bridge script, and gives up time coupling).

## Feasibility (prototype, scratch only: `/private/tmp/ff-wf-hx/`, not in any repo)

A 70-line Rust file using `extern "C" { dlopen, dlsym, dlerror }` and ~20 `dlsym`'d C API functions, built with plain `rustc` in
`ff-rust:22.04` (arm64), `ldd` shows only libc and libgcc_s (no libhelics link). It `dlopen`s `libhelics.so`, creates a federate
(`zmq` core, `--broker_address=tcp://ff-wf-hx-broker`, period 1.0), registers publication `ferrofire/echo` (double) and subscription
`py/ctl`, enters executing mode and loops `RequestTime(t+1)`, reading `py/ctl` and publishing `2 * py/ctl`.

Peer: `helics_broker -f 2` and a ten-line Python federate (`helics` 3.6.1) in `ff-physics:22.04`, on a private docker network.
Observed, both ways:

```
libhelics 3.6.1 (2025-02-24)
ferrofire t=1 updated=1 py/ctl=10      py t=2.0 echo=20.0
ferrofire t=2 updated=1 py/ctl=11      py t=3.0 echo=22.0
ferrofire t=3 updated=1 py/ctl=12      py t=4.0 echo=24.0
ferrofire t=4 updated=1 py/ctl=13      PY OK / OK
```

(One grant of latency each way: a value published at grant `t` is visible to a peer at `t + period`; the first read is the
"no value yet" sentinel `-1e49`, so the subscription's `IsUpdated` must gate the write.)

Where libhelics comes from, and what was checked:

| source | result |
|---|---|
| pip wheel `helics==3.6.1` (manylinux_2_27 aarch64, and x86_64) | `libhelics.so` + `libzmq.so.5` + `helics.h`; works in `ff-physics:22.04` and in `ff-rust:22.04` (via bind mount) |
| GitHub release `Helics-shared-3.7.1-Linux-aarch64.tar.gz` (4.3 MB; x86_64 4.6 MB) | `lib64/libhelics.so` + bundled `libzmq.so.5`; needs only glibc 2.25 and libstdc++ is static-linked, so `ldd` resolves in `ff-rust:22.04` with nothing installed. Loaded and ran the prototype against the 3.6.1 peer (minor-version mix works) |
| sha256 | aarch64 `32e8fbf63525c6f00ff580bd6281fa5372683ca8194dd328f1a0c3a0c01a1120`, x86_64 `6f4ad54f1330ecdec951661be7c3851fa055febdf7cb9e6d3753cbff9b4d1a8d` (from `Helics-v3.7.1-SHA-256.txt`, aarch64 re-checked locally) |
| apt / conda | not used; the release tarball is simpler and pinned |

Not proved: an in-process broker (`helicsCreateBroker` with the `inproc` core) for the Rust-only integration test, and
`RequestTimeIterative`. Both are in the test plan below with a fallback.

Gotcha found: `helics_broker` binds loopback by default; across containers it needs `--local_interface=0.0.0.0` or federates time out
with "zmq broker connection timed out".

## Design

### Config: a `helics:` section in `model.yaml`

```yaml
version: 1
dt: 0.1
inputs:
  pump.run:   {type: binary, default: false}
  valve.open: {type: analog, default: 0.5, min: 0, max: 1}
outputs:
  tank.level: {type: analog}
helics:
  name: tank                    # federate name (default: the model file's stem)
  core: zmq                     # zmq (default) | tcp | udp | inproc ...; passed to HELICS as-is
  broker: tcp://broker:23404    # default: HELICS's own default (localhost)
  period: 0.1                   # seconds of federate time per grant; default: model dt (must be a multiple of dt)
  pace: lockstep                # lockstep (default): step as fast as grants arrive | wall: also hold to wall clock
  library: libhelics.so         # optional; also $FERROFIRE_HELICS_LIB; default tries libhelics.so.3, libhelics.so
  subscribe:                    # peer publication key -> model input tag
    - {key: ctl/valve, tag: valve.open}
    - {key: ctl/run,   tag: pump.run}
  publish:                      # model tag -> publication key (global)
    - {tag: tank.level, key: tank/level}
```

- `subscribe[].tag` must be `Model::writable`; the type and limits come from `write_spec` (a value outside `min`/`max` is rejected like a
  REP write, with the same rate-limited warning). `publish[].tag` is any published tag; HELICS type is `double` for analog,
  `boolean` for binary. Unknown tag or a non-writable subscription target is a load error naming the tag (`did_you_mean` hint).
- No `helics:` section = no libhelics, no behaviour change. Config check happens in `Model::load` (pure, unit-testable); the library
  is only touched in `run`.
- `ferrofire run` flags (override the section; no section needed for these three): `--helics-broker ADDR`, `--helics-name NAME`,
  `--helics-lib PATH`. `ferrofire check` ignores the section; `ferrofire doctor` gains a "libhelics found, version" line when the
  model has a `helics:` section.

### Time stepping

One HELICS grant = one engine tick, in the engine thread (HELICS calls are made from that thread only; REP and datagrams keep their
own threads):

```
helics_enter_executing_mode()
loop until STOP:
    T = model.time
    for s in subs if input_updated(s): writes.push((tag, value))
    apply REP-queued writes, then HELICS writes (HELICS wins on the same tick)      # existing engine_tick path
    snapshot -> datagram publish (unchanged), and publish[] -> helics_publish at T
    model.step(dt)                                                                   # existing
    granted = request_time(T + period)                                               # blocks until every peer is ready
    model.time = granted (error if granted != T + period unless period > dt: then step floor((g-T)/dt) times)
```

`pace: wall` additionally sleeps to `t0 + k*dt` (so a live RTU on the datagram feed sees real time while a slow federate holds the
model back). `lockstep` is what makes a federation run faster than real time and deterministic. `period` > `dt` runs several model
steps per grant. Termination: on `STOP` or error, `helicsFederateFinalize` then free; HELICS errors other than a peer finalising are
fatal and printed as `ferrofire run: helics: <message>` with exit 1 (the existing `exit_on_err`).

Out of scope for the first cut (follow-up beads): message endpoints (bennu's HELICS provider sends WRITEs as endpoint messages; an
endpoint section `endpoints: [{name, tag}]` would map them to model writes), iterative time (`RequestTimeIterative`), filters,
translators, federate-info JSON passthrough (`helicsCreateValueFederateFromConfig`), min-delta/`only_transmit_on_change`.

### Where the code goes

- `src/helics/mod.rs` (safe): config structs (`HelicsDef`, serde with `deny_unknown_fields`), validation against a `Model`, the
  `Federate` trait (`enter_exec`, `read_updated`, `publish`, `request_time`, `finalize`) and the grant loop `run_federate(model, fed,
  ...)` that uses the trait only.
- `src/helics/ffi.rs` (the third unsafe module, `#![allow(unsafe_code)]`, `#![deny(unsafe_op_in_unsafe_fn)]`, like `sys.rs`): the
  `dlopen`/`dlsym` table (`Lib`), `HelicsError` struct, the `Federate` impl over raw handles, each block with a `// SAFETY:`
  invariant (handles are valid until `Drop`, one thread, strings are NUL-terminated `CString`s, error struct is checked after every call).
  Everything public is safe. `src/lib.rs` comment becomes "allowed only in `sys`, `simulink::ipc` and `helics::ffi`".
- `src/model.rs`: `Doc.helics: Option<HelicsDef>`, exposed as `Model.helics`.
- `src/bennu.rs`: `engine()` becomes a choice between the existing sleep loop and `helics::run_federate`; the tick body is
  `tick_and_publish` (reused as-is).
- Cargo feature `helics`, **default-on** (the gates build default features only, so crap/mutate/deny cover the code, as with
  `proto-mqtt`). With no new crates and no build-time libhelics it costs nothing; `--no-default-features` removes it. Without the
  feature, a model with `helics:` is a load error ("built without the helics feature").
- `deny.toml`: no change (no crate added). `NOTICE`: one line that HELICS (BSD-3-Clause, GMLC-TDC) is loaded at run time and not
  distributed with ferrofire.

### Build and image changes

- `docker/Dockerfile` (`ff-rust:22.04`): install `Helics-shared-3.7.1-Linux-$(uname -m)` to `/opt/helics` after a pinned sha256 check
  (the two values above), `ENV FERROFIRE_HELICS_LIB=/opt/helics/lib64/libhelics.so`. +4 MB, so `make test` can run the federate
  tests with no network. Rebuild once (`docker build -t ff-rust:22.04 docker`).
- Release binaries are unchanged and do not bundle libhelics; the run image supplies it (the physics image already has the wheel's
  copy; `FERROFIRE_HELICS_LIB` points at it).

### Tests

1. Unit, pure (no library): config parse and validation (unknown tag, non-writable target, `period` not a multiple of `dt`, bad
   type), and `run_federate` driven by a fake `Federate` (scripted subscription values, recorded publishes): updated-only writes,
   limit rejection, grant arithmetic, HELICS-wins-over-REP order, `period` > `dt` multi-step, finalize on STOP. These hold the CRAP
   budget (the FFI file is thin: one-line wrappers, branches live in the safe loop).
2. Integration (`tests/helics.rs`, needs libhelics: skipped with a visible message when `FERROFIRE_HELICS_LIB` is unset, run in
   `make test` because the image has it): a second federate made with the same `ffi::Lib` in a thread, broker from
   `helicsCreateBroker` with the `inproc` core (fallback: `zmq` on 127.0.0.1, not proved yet). Asserts the two-way exchange and the
   one-grant latency, plus `ferrofire run` as a child process against a broker for the CLI path.
3. Worked example, `bench/helics/` (pattern of `bench/physics`): `run.sh` starts `helics_broker -f 2 --local_interface=0.0.0.0` and a
   Python federate (`helics` wheel) in `ff-physics:22.04` containers on a private network (prefix `ff-wf-`), and `ferrofire run
   example.yaml` (binary from `make dut`, `FERROFIRE_HELICS_LIB` pointed at the wheel's `libhelics.so`) as the third container. The
   Python federate plays a controller: it reads `tank.level` and publishes `valve.open` (a bang-bang law with hysteresis) so the
   tank level visibly settles. `check.py` asserts from the federate's transcript that (a) the level was received, (b) after the
   controller opened the valve the level fell, (c) the same tags are read over REP (`ferrofire probe` / READ) equal the federate's last
   values, i.e. one engine feeds both worlds. Output in `target/helics/`; `make helics`; a section in `README.md` and
   `bench/helics/README.md` (this is the "example documented" half of the acceptance).
4. The existing gates: `make fmt-check lint test crap dry deny`.

## Risks and open points

- **Unsafe policy**: a third unsafe module is a policy change, though smaller in blast radius than `simulink::ipc` (no shared memory;
  failure is a clean error or a crash in a call). Mitigation: thin FFI, all logic in safe code behind a trait. If vetoed, fall back to C.
- **ABI drift**: the used C API subset (about 20 functions) is stable across HELICS 3.x; `Lib::open` checks `helicsGetVersion` major is
  3 and every symbol resolves, naming the missing one. Tested with 3.6.1 and 3.7.1 libraries against a 3.6.1 peer.
- **Dependent library in the field**: dlopen failure message must say where to get libhelics (release tarball URL, `pip download helics`)
  and how to point at it. libhelics needs libzmq: the wheel and shared tarball bundle one (rpath), a distro libhelics pulls its own.
- **Blocking on a stuck peer**: `RequestTime` blocks until all federates grant; STOP (SIGINT/SIGTERM) must still exit. The signal handler
  only sets the atomic; the grant loop therefore uses a per-federate `helics` timeout (`--helics-timeout`, default off) or a
  `helicsFederateRequestTimeAsync` plus poll. Decide in the implementation plan after measuring (the async pair is in the API).
- **HELICS core threads and fork**: `double_fork_detach` in `sys.rs` forks, so the federate is created after that, never before.
- **CI on arm64**: the aarch64 release tarball and the wheel both exist; the Dockerfile picks by `uname -m`. No new CI job, only the image
  rebuild.

## Follow-ups (to file as beads under ww-7mw)

1. Implement `helics:` section, `src/helics/` and the feature (tasks: config + validation; fake-federate loop tests; ffi module;
   engine hookup and flags; doctor line).
2. `ff-rust` image: libhelics tarball, sha256 pins.
3. `bench/helics` worked example, `make helics`, README section.
4. Message endpoints (pybennu provider parity) and `RequestTimeIterative`.
5. Stuck-peer shutdown behaviour (async request plus signal poll).

## Corrections found while implementing (ww-7mw.4)

The design held; these details changed:

- **Stuck peer / SIGTERM** (the open point above): the grant wait is `helicsFederateRequestTimeAsync` plus a 200 us poll of
  `helicsFederateIsAsyncOperationCompleted` that checks the stop flag; `EnterExecutingMode` is waited for the same way (startup also
  blocks until every federate joined). A stop during either leaves the federate abandoned: `helicsFederateFinalize` and libhelics'
  exit handlers then block forever (measured: the process printed `Info: stopped` and never exited), so `ffi` skips finalize and free
  and `helics::finish()` ends the process with `_exit` (new `sys::exit_now`). No `--helics-timeout` flag.
- **Federation end**: HELICS does not return `HELICS_TIME_MAXTIME` to a federate whose peers have finalized; it free-runs. A run ends
  on SIGINT/SIGTERM (the grant loop still stops on a MAXTIME grant, for a federation that closes down). The integration test and
  `bench/helics` stop ferrofire with SIGTERM after the peer finishes.
- **Loop order**: read subscriptions, publish the state at this instant (before the tick's writes), tick(s), request. HELICS writes
  are queued behind REP writes so they win. Federates are uninterruptible so a grant equals the request.
- **No `ferrofire doctor` line**: `doctor` probes a provider over REP and does not load a model; `--helics-*` flags override the
  section and need it (they do not stand in for one).
- **Tests**: `tests/helics.rs` runs the broker (`helicsCreateBroker`, zmq on 127.0.0.1) and the second federate (same `ffi::Fed`)
  in the test process and `ferrofire run` as a child process (the CLI path, SIGTERM, and the stuck-peer exit), plus the same exchange with the
  engine in the test process (`helics::run_engine`), because coverage is not collected from a child that libhelics exits; no `inproc` core (not needed once the
  child process is the federate under test). Broker ports are scanned below the ephemeral range (a busy suite fills it). The tests share one lock: two federations in one libhelics process cross-talk
  ("duplicate federate name").
- **Worked example**: one `ff-physics:22.04` container (broker, ferrofire, controller on loopback), not three containers on a private
  network; `make helics`. libhelics for ferrofire there is the image's `/usr/local/lib/libhelics.so` (3.6.1), controller on the
  same 3.6.1; the Python controller needs `LD_LIBRARY_PATH=/usr/local/lib` for pyzmq. The REP check holds ferrofire at a grant
  and reads `tank.level`, comparing it with the value the controller receives next (the engine's REP snapshot at the held grant
  is the state HELICS delivers one grant later).
- **Dockerfile**: the HELICS layer is appended last, so a rebuild of `ff-rust:22.04` that hits the layer cache is a one-layer build
  (verified as `FROM ff-rust:22.04` plus exactly those lines; a from-scratch rebuild was not repeated).
