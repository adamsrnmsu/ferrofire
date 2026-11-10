# Quality gates (ub-mode)

The gates come from [ub-mode](https://github.com/unclebob/ub-mode): [crapper](https://github.com/unclebob/crapper)
(CRAP per function), [dryer](https://github.com/unclebob/dryer) (duplication) and
[mutator](https://github.com/unclebob/mutator) (mutation testing). They run in the `ff-rust:22.04` container at the
commits pinned in `docker/Dockerfile`, scoped to `src` (the Python conformance probes are not Rust code).

```
make check    # fmt-check, clippy -D warnings, tests, crap (< 10 per function), dry (< 0.8)
make mutate   # slow: every covered mutation site reruns the suite; exit 3 if a mutant survives
```

Thresholds are fixed in the `Makefile` (`CRAP_MAX := 9.99`, `DRY_MIN := 0.8`) and were not relaxed to pass. Nothing is
excluded: tests (`tests/`, `mod tests`) are the only code the tools skip, as ub-mode allows.

## Before (commit 960db25, 146 functions)

| Gate | Result |
|---|---|
| CRAP | 36 of 146 functions at or above 10; worst `bennu::run` 1122 (CC 33, 0% covered) |
| Dryer | 1 pair at 0.89: the REP request wrapper in `bennu.rs:146-150` and `pypower.rs:109-113` |
| Mutation (whole tree) | 701 killed, 0 survived, 95 uncovered sites (796 sites) |

CRAP of 10 or more:

| Function | CC | Coverage | CRAP |
|---|---|---|---|
| `bennu::run` | 33 | 0.0% | 1122.0 |
| `power::Power::set` | 28 | 31.2% | 282.8 |
| `crate::check` | 13 | 0.0% | 182.0 |
| `simulink::main` | 10 | 0.0% | 110.0 |
| `model::Model::load_in` | 71 | 89.9% | 76.1 |
| `simulink::ipc::Shm::create` | 6 | 0.0% | 42.0 |
| `power::Power::get` | 37 | 88.1% | 39.3 |
| `expr::Node::eval` | 38 | 93.5% | 38.4 |
| `bennu::udp_sender` | 8 | 22.2% | 38.1 |
| `expr::lex` | 32 | 93.3% | 32.3 |
| `simulink::load_model` | 5 | 0.0% | 30.0 |
| `power::pf::runpf` | 25 | 100.0% | 25.0 |
| `pypower::cli` | 24 | 89.6% | 24.7 |
| `expr::P::atom` | 19 | 91.2% | 19.2 |
| `crate::main` | 8 | 46.7% | 17.7 |
| `pypower::reply` | 17 | 90.7% | 17.2 |
| `power::pf::newtonpf` | 17 | 96.5% | 17.0 |
| `power::case::parse_py` | 16 | 88.9% | 16.4 |
| `power::Power::build_tags` | 16 | 89.7% | 16.3 |
| `expr::binding` | 16 | 100.0% | 16.0 |
| `power::pf::ext2int` | 14 | 85.2% | 14.6 |
| `power::pf::pfsoln` | 13 | 92.7% | 13.1 |
| `expr::Func::lookup` | 13 | 93.3% | 13.1 |
| `model::Model::apply_writes` | 13 | 96.2% | 13.0 |
| `power::case::parse_m` | 13 | 97.6% | 13.0 |
| `model::Model::body_step` | 12 | 95.7% | 12.0 |
| `power::pf::dc` | 12 | 98.2% | 12.0 |
| `pypower::serve` | 12 | 100.0% | 12.0 |
| `simulink::ipc::Fifo::create` | 3 | 0.0% | 12.0 |
| `simulink::ipc::Fifo::drain` | 3 | 0.0% | 12.0 |
| `simulink::ipc::Sem::open_locked` | 3 | 0.0% | 12.0 |
| `simulink::ipc::Sem::wait` | 3 | 0.0% | 12.0 |
| `power::case::Inline::to_case` | 11 | 94.5% | 11.0 |
| `pypower::load_cfg` | 11 | 96.6% | 11.0 |
| `power::pf::lu_solve` | 11 | 100.0% | 11.0 |
| `power::Power::new` | 10 | 98.0% | 10.0 |

The big ones were `bennu::run`, `simulink::main`, `check` and the IPC wrappers (no test ran them), the two 36-arm matches
in `Power::get`/`set`, `Model::load_in` (one 270-line function), and the lexer, parser and evaluator in `expr`.

## What changed

- **Split by purpose, same arithmetic.** `Model::load_in` is declare, build_power, compile_states/bodies/outputs,
  build_runtime and build_published. `runpf`, `newtonpf`, `pfsoln`, `dc` and `ext2int` are one-purpose helpers; the
  numeric expressions were moved, not rewritten, so the PYPOWER goldens still match.
- **Data instead of case statements.** Power tags are `(Tab, column)` accessors or a `Flag` with const field tables; the
  expression operators and functions are const tables of `fn` pointers. `get` went from CC 37 to 7.
- **One decision in one place.** `serve_rep`, `install_signals`, `sleep_until`, `with_nul` and `usage_exit` are shared by
  `ferrofire run`, the pybennu drop-in and the simulink solver (this is what the dryer pair was).
- **Behaviour tests where nothing ran.** `tests/cli.rs` runs the real `ferrofire run` (REP, UDP, SIGTERM) and `check`;
  `tests/simulink.rs` runs `simulinksolver` against real SysV shm, POSIX semaphores and the FIFO; `tests/power_tags.rs`
  round-trips every tag. No behaviour changed: the waterway golden trace and the PYPOWER goldens passed after every commit.

## After

| Gate | Result |
|---|---|
| CRAP | 0 of 271 functions at or above 10; worst 9.1 |
| Dryer | No duplicate candidates >= 0.8 |
| Mutation (whole tree, fresh run) | 686 killed, 0 survived, 1 uncovered site (687 sites); `mutator` exits 0 |

Highest CRAP now:

| Function | CC | Coverage | CRAP |
|---|---|---|---|
| `pypower::parse_cli` | 9 | 90.9% | 9.1 |
| `simulink::contract::decode_update` | 9 | 92.3% | 9.0 |
| `bennu::reply` | 9 | 93.3% | 9.0 |
| `model::build_published` | 9 | 95.5% | 9.0 |
| `expr::Node::eval` | 9 | 100.0% | 9.0 |
| `model::order_outputs` | 9 | 100.0% | 9.0 |

## Mutation notes

- Command: `mutator --max-workers 6 src` in the container, whole tree, from a clean checkout (about 40 minutes: each of
  the 686 mutants reruns the ~25 s suite). `make mutate` runs the same thing with one worker per core.
- Site count fell from 796 to 687 because the refactor deleted code (two 36-arm matches, duplicated loops, an unused
  argument, `Rng::seeded`'s fallback literal), not because anything was excluded.
- The one uncovered site is `pypower.rs:261`, the `fork() > 0` of the second (intermediate) daemon process. That
  process exits straight after forking, and its coverage counters are not recorded by the instrumentation. The
  behaviour is exercised by `daemon_verbs_pidfile_logs_and_feed` (start, status, restart, stop all go through it).
- Survivors found on the way and fixed: `main.rs` sliced `args[1..]` where `args[0..]` behaved the same (equivalent
  mutant, removed with `split_first`), and the `1e-9 s` write tolerance in `check` had no boundary test (now
  `a_write_due_exactly_at_the_tolerance_applies_once_in_the_first_step`).
- The baseline run predates a test-isolation fix. The IPC tests use machine-global names (SysV keys, POSIX semaphore
  names, `/tmp/updates_fifo`) and the daemon test used a fixed port, so mutation workers running the suite in
  parallel could fail for the wrong reason and count as kills. Tests now take a file lock and use free ports; the
  after numbers come from a run with that fixed. Treat the baseline's 0 survivors as an upper bound on its quality.

## Rerunning

```
make check     # fmt, clippy, tests, crap, dry (about 2 minutes after the image is built)
make mutate    # mutation testing; exit 3 means a mutant survived
```

`.metrics/` (crap.edn, dry.edn, mutate/) is generated and git-ignored.

## After the transport work (zmq and nng, ww-1df.17; historical, NNG dropped 2026-11-09)

| Gate | Result |
|---|---|
| `make check` | exit 0 (fmt, clippy -D warnings, tests, CRAP, dryer) |
| CRAP | 0 of 290 functions at or above 10; worst 9.0 (`bennu::reply`, `model::build_published`, `expr::Node::eval`, ...) |
| Dryer | No duplicate candidates >= 0.8 |
| Mutation (whole tree, `make mutate`) | 690 killed, 0 survived, 1 uncovered site (691 sites); exit 0. The uncovered site is the daemon fork in `pypower.rs`, as before |
| `make variants` | clippy + tests for `--features zmq` and `--features nng` pass; the nng-only binary links no libzmq |

`pypower::parse_cli` was split (`Cli::set`, `VALUE_FLAGS`) when the `--transport` flag pushed it to CRAP 10.1.
