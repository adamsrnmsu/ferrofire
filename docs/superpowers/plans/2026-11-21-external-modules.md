# External protocol modules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A documented ffmod/1 boundary so a protocol can be a separate program (stdio, socket or bennu channels), with an IEC 104 server and client in Python on `c104` as the first module, drop-in for stock configs.

**Architecture:** A new compiled module `external` (`src/fd/proto/external/`) implements the existing `Module` trait. It owns the child process and speaks ffmod/1 (JSON lines) over a transport; one transport-agnostic session loop diffs a store snapshot each scan cycle and turns module writes into the same store calls compiled modules make. A manifest lookup lets stock `<comms>` element names start an installed module.

**Tech Stack:** Rust (tokio, `tokio::process`, serde_json, zeromq crate already used by `cmdif`), Python 3.10 + `c104==2.2.1` for the module, the existing `conformance/` harness (tshark, Zeek).

**Spec:** `docs/superpowers/specs/2026-11-21-external-modules-design.md` (approved 2026-11-21). Bead: ww-7mw.3 (phenix-waterway-range).

## Global Constraints

- ferrofire stays MIT; `modules/iec104/` is GPL-3.0 with its own `LICENSE`; `NOTICE` says so. No GPL code in the Rust crate.
- Gates pass: `make fmt-check lint test crap dry deny` (CRAP < 10, dry < 0.8). Run them through the `IN` wrapper; retry a run that fails with E0460/E0463 up to 3 times.
- No new lock or work on the scan loop's hot path; compiled modules unchanged in behaviour and timing.
- A module that hangs, dies or misbehaves never takes the device down, and never makes device stop take longer than 5 s.
- ffmod/1 is JSON, one object per line, `\n`-terminated, field `t` = type; unknown fields ignored; a message with an unknown `t` from a module is logged and ignored.
- A stock `<iec60870-5-104-server>` / `<iec60870-5-104-client>` element loads and serves with no edits.
- `ff-rust:22.04` has no Python: Rust tests use `sh` scripts as fake modules. Python runs in the module image (Task 7).
- Commit locally only; nothing is pushed. Conventional Commit messages, normal English, ending with the attribution trailer.

## Review Focus

1. A module that writes garbage, a partial line, or a line over 64 KiB: the line is dropped and logged, the session goes on (Task 2 test `garbage_and_long_lines_are_dropped`).
2. A module that never reads its stdin: `values` must not block the session, the store or other modules; the writer keeps only the latest pending diff, and device stop still completes in 5 s (Task 3 test `a_module_that_never_reads_does_not_block_stop`).
3. A command that is missing, not executable, or exits at once: logged, restarted on the 1 s to 30 s backoff, no busy loop (Task 3 test `a_crashing_module_backs_off`).
4. A module that ignores SIGTERM: killed at 5 s; device stop bounded (Task 3 test `stop_escalates_to_sigkill`).
5. An analog value that is NaN or infinite (JSON cannot carry it): encoded as `null`; a module keeps its previous value. A `write` with `null` or a string value is rejected (Task 1 test `non_finite_values_are_null`).

---

### Task 0: c104 spike (gate)

**Files:**
- Create: `modules/iec104/spike/` (throwaway; deleted in Task 7 or kept as notes in the README)

- [ ] **Step 1:** Build a scratch image `ff-mod104:22.04` from `ff-physics:22.04` plus `pip install c104==2.2.1`.
- [ ] **Step 2:** Write a 60-line c104 server with the points of the stock sample 104 config (`~/git/sceptre-bennu/data/configs/ep`, the `<iec60870-5-104-server>` element; read-only, do not modify that checkout), and run `conformance/stock/probe_104.py` against it and against the stock server (`ffc-stock-2026.11.03:22.04`, `bennu-field-device`).
- [ ] **Step 3:** Compare what a master sees (spec "Fidelity bar"): GI with QOI 20 (ACT_CON, data COT 20 with all points incl. outputs, ACT_TERM), negative ACT_CON for another QOI, periodic COT 1 sends at `rpoll-rate`, single vs double mode (types 1/3/31), commands 45/46/50 and their replies, CA 1, IOA from `address`.
- [ ] **Step 4: Decide.** If any of those cannot be matched, stop here, write the findings to the bead (`bd update ww-7mw.3 --notes ...`) and report back; do not start Task 1. Otherwise list the differences that remain (grouping, timers, queues) for the README and continue.

### Task 1: ffmod/1 messages

**Files:**
- Create: `src/fd/proto/external/mod.rs` (module root, `pub mod msg;`), `src/fd/proto/external/msg.rs`
- Modify: `src/fd/proto/mod.rs` (`pub mod external;` behind feature `external`), `Cargo.toml` (feature `external = ["dep:serde_json"]`, added to `default`; tokio gains `"process"`)

**Interfaces:**
- Produces:
  - `pub enum ToModule { Hello { device: String, strict: bool, config: String, points: Vec<(String, Value)> }, Values(Vec<(String, Value)>), Ack(u64), Reject(u64, String), Stop }`
  - `pub enum FromModule { Ready { role: Role, uses: Vec<Use> }, Write { id: u64, tag: String, value: Value }, Log { level: String, msg: String }, Error(String) }`
  - `pub enum Role { Server, Client }`; `pub struct Use { pub tag: String, pub kind: Kind, pub dir: Dir }`; `pub enum Dir { In, Out }`
  - `pub fn encode(m: &ToModule) -> String` (one line, with `\n`); `pub fn decode(line: &str) -> Result<FromModule, String>`
  - `pub const MAX_LINE: usize = 65536;`
- Wire shapes are exactly the spec's ("ffmod/1 messages"): `hello` has `v:1`, `opts:{"strict":..}`, `points:[{"tag","kind","value"}]`; `values` has `changes:[[tag,value],...]`; kinds are `"binary"`/`"analog"`; `dir` is `"in"`/`"out"`.

- [ ] **Step 1: Write the failing tests** in `msg.rs`:
  - `hello_matches_the_spec`: `encode(&ToModule::Hello{..})` parses (serde_json) to `{"t":"hello","v":1,"device":"rtu-1","opts":{"strict":false},"config":"<x/>","points":[{"tag":"a","kind":"binary","value":true},{"tag":"b","kind":"analog","value":2.5}]}`.
  - `values_ack_reject_stop`: `{"t":"values","changes":[["b",2.6]]}`, `{"t":"ack","id":7}`, `{"t":"reject","id":7,"err":"unknown tag"}`, `{"t":"stop"}`.
  - `decode_ready_write_log_error`: each module message of the spec decodes to its variant; `"dir":"in"` → `Dir::In`.
  - `non_finite_values_are_null`: `Values(vec![("b", Analog(f64::NAN))])` encodes `["b",null]`; `decode` of a `write` with `"value":null` or `"value":"x"` is `Err`.
  - `unknown_fields_are_ignored_unknown_type_is_err`: an extra field decodes; `{"t":"nope"}` is `Err`.
- [ ] **Step 2:** Run `make test` (or `cargo test --lib external::msg` through `IN`); expected FAIL (module missing).
- [ ] **Step 3:** Implement with `serde_json::Value` building and matching (no derive needed if it is shorter; either is fine).
- [ ] **Step 4:** Tests pass; `make fmt-check lint`.
- [ ] **Step 5:** Commit `feat(external): ffmod/1 message codec (ww-7mw.3)`.

### Task 2: Session loop (transport-agnostic)

**Files:**
- Create: `src/fd/proto/external/session.rs`

**Interfaces:**
- Consumes: Task 1 types; `Store::{snapshot, kind, write, is_queued, get}`; `proto::client::{input_polled, Out}`.
- Produces:
  - `pub struct Spec { pub name: String, pub device: String, pub strict: bool, pub config: String, pub cycle: Duration, pub cmdif: Option<String> }`
  - `pub enum End { Stopped, Broken, Refused(String) }` (`Refused`: module sent `error`, or `--strict` refused its tags; no restart)
  - `pub async fn session<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin + Send + 'static>(r: R, w: W, spec: &Spec, store: Store, stop: CancellationToken) -> End`
  - `pub const READY_TIMEOUT: Duration = Duration::from_secs(10);`
- Behaviour: send `hello` (all `store.snapshot()` tags with `store.kind`); wait for `ready` up to `READY_TIMEOUT` (timeout → `Broken`); check each `use` with `store.kind` (unknown or wrong kind: warn, or `Refused` under strict); then loop: every `spec.cycle` diff `snapshot()` against the last sent and send `values` if non-empty; for `role: client` the diff of `out` tags goes through one `Out` per tag (its Echo rule decides what is sent) and `write`s of `in` tags go through `input_polled`; for `role: server` a `write` is `store.write` → `ack` or `reject` (`UnknownTag` → `"unknown tag"`, `WrongKind` → `"wrong kind"`). `log` → `tracing` at its level with the module name. `error` → `Refused`. EOF → `Broken`. `stop` → send `stop`, return `Stopped`. If `spec.cmdif` is set and role is client, spawn `cmdif::serve(endpoint, TagPoints::new(store, uses as (tag, dir == Out)), stop.child_token())` after `ready`.
- Writer: a task fed by a `tokio::sync::watch` (latest pending diff wins) plus an mpsc for acks/stop, so a module that never reads cannot block the session.

- [ ] **Step 1: Write the failing tests** (in-memory `tokio::io::duplex` pairs; a test helper plays the module):
  - `hello_then_values_on_change`: store with `a` binary, `b` analog; module answers `ready`; after `store.publish("b", 3.0, ..)` the next line is `{"t":"values","changes":[["b",3.0]]}`; with no change no `values` line arrives within 3 cycles.
  - `server_write_is_acked_or_rejected`: `write` id 1 `b` 4.0 → `ack` 1 and `store.queued("b") == Some(Analog(4.0))`; `write` id 2 `zz` → `reject` 2 `"unknown tag"`; `write` id 3 `a` 1.5 → `reject` 3 `"wrong kind"`.
  - `strict_refuses_unknown_tags`: `strict: true`, `ready` uses `zz` → `End::Refused`; `strict: false` → session continues.
  - `no_ready_in_time_is_broken`: no `ready` → `End::Broken` after `READY_TIMEOUT` (use `tokio::time::pause`).
  - `client_role_uses_the_client_rules`: role client, `in` tag write of the value already in the store queues nothing (`!store.is_queued`); an `out` tag change is sent once, and not echoed back after the module writes the same value.
  - `garbage_and_long_lines_are_dropped` (Review Focus 1): a non-JSON line, a 70 000-byte line, then a valid `write` → only the `write` is acted on.
- [ ] **Step 2:** Run; expected FAIL.
- [ ] **Step 3:** Implement `session`. Read lines with a `MAX_LINE` cap (read until `\n` or cap; over cap, discard to the next `\n`).
- [ ] **Step 4:** Tests pass; `make crap dry` stay green (split helpers if CRAP ≥ 10).
- [ ] **Step 5:** Commit `feat(external): ffmod/1 session loop (ww-7mw.3)`.

### Task 3: `<external-module>`, transport A (stdio) and lifecycle

**Files:**
- Modify: `src/fd/proto/external/mod.rs`, `src/fd/proto/mod.rs` (REGISTRY line `("external-module", start::<external::External>)` and `KNOWN` entry), `src/fd/mod.rs` (fatal check), `src/main.rs` or the CLI module (nothing yet; Task 4 adds the flag)

**Interfaces:**
- Consumes: Task 2 `session`, `Spec`, `End`; `proto::{backoff, BACKOFF_FIRST, device_name, Module}`.
- Produces:
  - `pub struct External` implementing `Module`. `parse` reads `<name>` (required, `name_in` rule: non-empty), `<command>` (required for stdio), `<transport>` (default `stdio`; `tcp://`, `unix://`, `bennu` are Task 5/6), `<config>` (inner XML text verbatim, from `node.document().input_text()[range]` of its first element child; empty allowed), `<command-interface>` (optional). The cycle is the device's `cycle-time` (read from the `<field-device>` ancestor as `config.rs:327` does, default 1000 ms).
  - `pub fn from_element(node: Node, cmd: &str, opts: &Opts) -> Result<External, ConfigError>`: the manifest path (Task 4) builds an `External` whose config is `node` itself.
  - `pub fn fatal(msg: String, stop: &CancellationToken)` and `pub fn take_fatal() -> Option<String>` (a `static Mutex<Option<String>>`); `fd::run` returns `Err(msg)` after shutdown when set.
- Behaviour: command split on ASCII whitespace (no shell, no quoting; documented); env `FERROFIRE_MODULE=<name>`; stdin/stdout piped, stderr lines logged at `info` with the name; `kill_on_drop(true)`. `End::Broken` or spawn error → sleep `backoff` (reset to first after a session that reached `ready`) and restart; `End::Refused(m)` → log error, no restart, and under `--strict` `fatal(m)`. Stop: session sends `stop`, drop stdin, wait 2 s, SIGTERM, wait 3 s more, SIGKILL.

- [ ] **Step 1: Write the failing tests** in `tests/external.rs` (spawns the real binary like `tests/helics.rs` does, or calls `External::run` in-process; fake modules are `sh` scripts written to a temp dir):
  - `stdio_module_round_trip`: script reads `hello`, prints a `ready` using a device tag, then a `write`; the device's provider REP (reuse the helper pattern in `src/fd/mod.rs` tests) receives that write.
  - `a_crashing_module_backs_off` (Review Focus 3): command `false` and a missing path: within 4 s at most 3 spawns are logged; the device keeps serving its other modules.
  - `stop_escalates_to_sigkill` (Review Focus 4): script `trap "" TERM; read x; sleep 60`: device stop returns within 6 s and the child is gone.
  - `a_module_that_never_reads_does_not_block_stop` (Review Focus 2): script prints `ready` then `sleep 60` without reading; with a store tag changing every cycle for 3 s, device stop still returns within 6 s.
  - `strict_error_exits_nonzero`: under `--strict`, a script that prints `{"t":"error","msg":"no rpoll-rate"}` makes `ferrofire field-device` exit non-zero with that text; without `--strict` it keeps running.
- [ ] **Step 2:** Run; expected FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Tests pass; all six gates.
- [ ] **Step 5:** Commit `feat(external): <external-module> over stdio with restart and stop (ww-7mw.3)`.

### Task 4: Manifests (drop-in stock element names)

**Files:**
- Create: `src/fd/proto/external/manifest.rs`
- Modify: `src/fd/proto/mod.rs::start_module` (registry miss → manifest lookup → KNOWN error only on a miss), the CLI (`--modules-dir`), `docs/` comes in Task 8

**Interfaces:**
- Produces:
  - Manifest file `<dir>/<module>/module.json`: `{"serves":["iec60870-5-104-server","iec60870-5-104-client"],"command":"python3 module.py"}`; `command` runs with the module directory as working directory. (JSON, not the spec's TOML: serde_json is already a dependency and TOML would add one. My call.)
  - `pub fn set_dir(p: PathBuf)` (a `OnceLock`, set once from the CLI); `pub fn dir() -> PathBuf` (`--modules-dir`, else `FERROFIRE_MODULES`, else `/opt/ferrofire/modules`)
  - `pub fn lookup(tag: &str) -> Option<(PathBuf, String)>` (module dir, command); scans `dir()/*/module.json` once and caches; a bad manifest is a warning naming the file.
- Precedence: a compiled `REGISTRY` entry always wins; then a manifest; then the `KNOWN` "needs the `proto-…` feature" error, worded `<tag> needs the proto-… feature or an installed module (see docs/modules.md)`.

- [ ] **Step 1: Write the failing tests**: `lookup_finds_a_served_tag` (temp dir with two manifests), `bad_manifest_is_a_warning_not_an_error`, `registry_wins_over_a_manifest`, and in `config.rs` tests `stock_104_element_starts_the_module` (a stock `<iec60870-5-104-server>` element with a manifest present yields one module runner; with none, the existing feature error text with the new wording).
- [ ] **Step 2:** Run; FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Pass; gates.
- [ ] **Step 5:** Commit `feat(external): module manifests serve stock <comms> elements (ww-7mw.3)`.

### Task 5: Transport B (socket)

**Files:**
- Modify: `src/fd/proto/external/mod.rs` (or `transport.rs` if `mod.rs` passes 300 lines)

**Interfaces:**
- Consumes: Task 2 `session`, Task 3 lifecycle.
- Behaviour: `<transport>tcp://HOST:PORT</transport>` or `unix:///path`. ferrofire binds; a non-loopback TCP host logs a warning, and under `--strict` is a config error. One connection at a time: a new connection cancels the running session and starts a fresh one (new `hello`). With `<command>`, also spawn it (lifecycle as Task 3) with `FERROFIRE_MODULE_ADDR` set to the transport text; without it, only listen. A unix socket file left over is removed before bind and on stop.

- [ ] **Step 1: Write the failing tests**: `tcp_module_connects_and_gets_hello`, `a_second_connection_replaces_the_first` (first sees EOF, second gets `hello`), `unix_socket_round_trip`, `non_loopback_is_refused_under_strict`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** Pass; gates.
- [ ] **Step 5:** Commit `feat(external): socket transport for ffmod/1 (ww-7mw.3)`.

### Task 6: Transport C (bennu channels)

**Files:**
- Create: `src/fd/proto/external/bennu.rs`

**Interfaces:**
- Consumes: `cmdif::{serve, Points}`, `bennu::{frame, group_for}`, `Store`.
- Produces: `pub struct StorePoints(Store)` implementing `Points` over every device tag (`names` = all tags in name order; `read` = `store.get`; `write` = `store.write`, errors worded as `cmdif` expects).
- Behaviour: `<transport>bennu</transport>` with `<command-interface>` (required) and `<publish-group>` (required, `udp://ADDR:PORT`). Serve `StorePoints` on the REP; every cycle send the snapshot diff as one datagram-v1 body (`tag:value,` per point, `true`/`false` for binaries, `cmdif::float` formatting for analogs) framed with `bennu::frame(group_for(group), body)`, chunked under 1 400 bytes per datagram as pybennu does. Optional `<command>` spawned with the Task 3 lifecycle but no ffmod/1 (stdin null).

- [ ] **Step 1: Write the failing tests**: `query_lists_every_device_tag` (reply equals `ACK=` + names + `,` in name order), `write_goes_through_the_store`, `changes_are_multicast_as_datagram_v1` (a UDP listener on the group receives a frame that ferrofire's own `datagram` receiver parses back to the changed points), `a_large_change_set_is_chunked`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** Pass; gates.
- [ ] **Step 5:** Commit `feat(external): bennu-channel transport, device-level command interface and change datagrams (ww-7mw.3)`.

### Task 7: The IEC 104 module

**Files:**
- Create: `modules/iec104/module.py`, `module.json`, `requirements.txt` (`c104==2.2.1`), `LICENSE` (GPL-3.0 text), `README.md`, `Dockerfile` (FROM `ff-physics:22.04`; pip install the requirements; copy the module to `/opt/ferrofire/modules/iec104`; image tag `ff-mod104:22.04`), `test_module.py`
- Modify: `Makefile` (`mod104-image:` builds it; `mod104-test:` runs pytest in it), `NOTICE` (the directory is GPL-3.0 and imports c104)

**Interfaces:**
- Consumes: ffmod/1 (Task 1 shapes), the spike's findings (Task 0).
- Behaviour: read `hello`, parse `config` (one `<iec60870-5-104-server>` or `<iec60870-5-104-client>` element, stock keys of field-device-spec.md 4.3 and protocols plan row 10), send `error` for what stock refuses at load (no `rpoll-rate` or `subtype` on a server, a non-`tcp://` endpoint), else `ready` with role and `uses` (server: inputs `out`, outputs `in`; client: inputs `in`, outputs `out`). Server: the 4.3 behaviour within the fidelity bar. Client: one GI at connect, then the server's sends; decodes `M_ME_NC_1` and `M_DP_NA_1`, plus `M_SP_NA_1` when `opts.strict`; `values` of `out` tags become `C_DC_NA_1` (select) / `C_SE_NC_1`. Logs to stderr only; stdout is ffmod/1 only.

- [ ] **Step 1: Write the failing tests** (`test_module.py`, pytest, module driven through its stdin/stdout with a scripted device): `stock_server_config_gives_ready`, `missing_rpoll_rate_is_an_error`, `server_command_becomes_a_write` (a c104 client in the test sends `C_SE_NC_1` IOA 102 value 7.5 → module prints `write` of that tag 7.5), `values_reach_a_gi` (after `values`, a GI from a c104 client returns the new value), `client_polls_into_writes` (module as client against a c104 server in the test).
- [ ] **Step 2:** `make mod104-test`: FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(modules): IEC 104 server and client on c104, GPL-3.0 (ww-7mw.3)`.

### Task 8: Conformance, docs and wiring

**Files:**
- Create: `conformance/iec104/` and `conformance/iec104-client/` (copy `conformance/dnp3/` and `conformance/modbus-client/`; `proto.env` uses `DUT_IMAGE=ff-mod104:22.04` for `DUT=rust`), `docs/modules.md`
- Modify: `README.md` (protocol table rows for IEC 104 via module; a "Modules" line), `docs/field-device-protocols-plan.md` rows 5 and 10, `conformance/README.md` (divergences from Task 0/7)

- [ ] **Step 1:** `conformance/iec104/check.sh`: `want_field` on the GI ACT_CON (type 100 COT 7), data COT 20 for each configured IOA, ACT_TERM (COT 10), a periodic COT 1 send, a `C_SE_NC_1` echo COT 7; `zeek.sh` on the ICSNPP iec104 logs. Run `make conformance PROTO=iec104` (stock oracle) and `DUT=rust make conformance PROTO=iec104`: both PASS.
- [ ] **Step 2:** Differential: `conformance/differential.sh iec104` (stock vs ours, same config, `probe_104.py` transcript) equal except the listed divergences. Same for `iec104-client` against the stock 104 server.
- [ ] **Step 3:** `docs/modules.md`: ffmod/1 (every message, with the spec's examples), the three transports and their config, lifecycle numbers, manifests and `--modules-dir`, the worked example (run the IEC 104 module from a stock config in `ff-mod104:22.04`), and a 30-line "write your own" example in `sh` or Python that the Task 3 test also runs (so the doc example is tested).
- [ ] **Step 4:** All six gates; `make physics` still PASS (no regression of the providers path).
- [ ] **Step 5:** Commit `docs(external): ffmod/1 reference, IEC 104 conformance (ww-7mw.3)`. Then update the bead with the commits and results; do not close it (the main session closes it after review).
