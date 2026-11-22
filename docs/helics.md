# `ferrofire run` as a HELICS federate

A model with a `helics:` section makes `ferrofire run` join a [HELICS](https://helics.org) federation as a value federate. The
engine itself is the federate: one engine tick is one slice of a granted time step, and `ferrofire run` blocks in
`helicsFederateRequestTime` between steps, so a federation can run faster than real time and deterministically. The REP socket and
datagram publishing keep working beside it (one engine feeds both worlds).

This is not what bennu's pybennu HELICS provider does: that provider is a separate process that bridges HELICS to a *provider's*
REP socket (`bench/physics` runs it in front of the field device). Here ferrofire's own model is the federate. Message endpoints,
filters and iterative time are not supported (see "Not yet").

## The `helics:` section

```yaml
version: 1
dt: 0.1
inputs:
  valve.open: {type: analog, default: 0, min: 0, max: 1}
states:
  tank.level: {ode: "0.5 * valve.open - 0.2 * sqrt(max(tank.level, 0))", init: 2.5, min: 0, max: 10}
outputs:
  tank.level: {type: analog}
  tank.high:  {type: binary, expr: "tank.level > 3"}
helics:
  name: tank                    # federate name (default: the model file's name without .yaml)
  core: zmq                     # zmq (default), tcp, udp, ...: given to HELICS as the core type
  broker: tcp://127.0.0.1:23404 # given to HELICS as --broker_address (default: HELICS's own, localhost)
  period: 0.1                   # seconds of federate time per grant; default dt; must be a multiple of dt
  pace: lockstep                # lockstep (default): step as fast as grants arrive | wall: also hold to the wall clock
  library: /opt/helics/lib64/libhelics.so   # optional, see "libhelics"
  subscribe:                    # a peer's publication key -> a model input (or writable power tag)
    - {key: ctl/valve, tag: valve.open}
  publish:                      # a model tag -> a global publication key
    - {tag: tank.level, key: tank/level}
    - {tag: tank.high,  key: tank/high}
```

- A subscription is judged like a REP write: its tag's type and `min`/`max` apply, and an out-of-range value is dropped with one
  `Warn:` line per subscription. Only updated values are written.
- Analog tags travel as HELICS `double`, binary tags as `boolean`.
- Tags that do not exist, subscription targets that are not writable, and a `period` that is not a multiple of `dt` are errors when
  the model loads (`ferrofire check` does not look at the section). `period` larger than `dt` runs several model steps per grant.
- `ferrofire run` flags override the section: `--helics-broker ADDR`, `--helics-name NAME`, `--helics-lib PATH`. They need the
  section (it holds the subscriptions and publications).

## Time

Federates are created uninterruptible with `period` as their time period, so a grant is exactly the time asked for. In each
grant `ferrofire run`:

1. reads the subscriptions that changed and queues them as writes (after any REP writes, so HELICS wins on the same tick),
2. publishes the state of the model at this instant,
3. steps the model `period / dt` times (the first step applies the writes),
4. asks for the next grant and blocks until every federate is ready.

HELICS delivers a value at the receiver's next grant, so there is one grant of latency each way: the level a controller reads at
`t` is the state published at `t - period`, and its valve command acts on the step after it arrives. `bench/helics` asserts this.

`SIGINT`/`SIGTERM` end the run even while it waits for peers or a grant. HELICS does not tell a federate that its peers have left
(it free-runs once it is alone), so a federate run ends on a signal, or on the federation closing down. If a signal interrupts a
blocked HELICS call, the process leaves with `_exit` (libhelics cannot shut that state down), after everything ferrofire owns is
stopped.

## libhelics

libhelics is loaded with `dlopen` when a run starts; ferrofire does not link it, so builds and models without `helics:` need
nothing. It is looked for in `helics: library:` / `--helics-lib`, then `$FERROFIRE_HELICS_LIB`, then the loader's search for
`libhelics.so.3` and `libhelics.so`. Any HELICS 3.x works (3.6.1 and 3.7.1 were run, together). Get one from:

- the release tarball `Helics-shared-<version>-Linux-<arch>.tar.gz` at <https://github.com/GMLC-TDC/HELICS/releases>
  (`lib64/libhelics.so`, with a libzmq beside it; needs only glibc), or
- the Python wheel (`pip download helics`, `helics/install/lib/libhelics.so`).

The `ff-rust` build image has the 3.7.1 tarball in `/opt/helics` (sha256 checked per architecture) and sets
`FERROFIRE_HELICS_LIB`, which is how `make test` runs `tests/helics.rs` (it is skipped, loudly, without the variable).

The code is feature `helics` (default on; `--no-default-features` removes it, and a model with a `helics:` section then fails to
load). The only `unsafe` is `src/helics/ffi.rs`, a thin layer behind the `Federate` trait; the grant loop is safe code tested with
a scripted fake federate.

## Worked example

`bench/helics` (`make helics`, see [its README](../bench/helics/README.md)): `helics_broker`, `ferrofire run example.yaml` as the
federate `tank`, and a Python federate `ctl` that runs a bang-bang controller on `tank/level`. It prints the values each side
exchanges and checks that the level followed the valve command, and that a `READ` over REP equals the value the controller
receives next.

## Not yet

Message endpoints (pybennu's HELICS provider sends writes as endpoint messages), `RequestTimeIterative`, filters, translators,
federate-info JSON, `only_transmit_on_change`. Each is a follow-up.
