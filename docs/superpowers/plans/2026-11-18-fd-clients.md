# Modbus client and DNP3 master (bennu and FEP modes) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `<modbus-client>` and `<dnp3-client>` field-device modules. `<mode>bennu</mode>` (the default) is a stock bennu fd-client drop-in; `<mode>fep</mode>` polls remote devices into ferrofire tags and writes local changes down.

**Architecture:** Each client is a `proto::Module`: one tokio task per connection, the store shared with the rest of the device. The codecs are ours. The Modbus client reuses the server's MBAP codec and its `Device`/`Range` maps. The DNP3 master reuses the outstation's link, transport and application layers. A shared `proto::client` module holds the mode, the FEP keys and the echo rule. The command interface (ZMTP REP) is a separate module that both clients serve.

**Tech Stack:** Rust (edition 2021, rust-version 1.99), tokio, the `zeromq` crate (already a dependency), roxmltree. Tests use cargo test, cargo-fuzz and the `conformance/` harness, which runs stock bennu in the Lima VM `phenix`.

**Spec:** `docs/superpowers/specs/2026-11-18-fd-clients-design.md`. Behaviour of stock bennu: `docs/field-device-protocols-plan.md`, rows 7, 8 and 9.

**Bead:** ww-7mw.1, in the `phenix-waterway-range` beads database (`cd ~/git/phenix-waterway-range && bd ...`). File follow-ups there with `--deps discovered-from:ww-7mw.1`.

## Global Constraints

- No new runtime dependencies. MIT licence. Code behind the existing features `proto-modbus` and `proto-dnp3`.
- `<mode>` is `bennu` (the default) or `fep`; any other value is a config error naming it, in every mode.
- `bennu` mode must equal stock on the wire and on the command interface. Each divergence is listed in `conformance/README.md`.
- In both modes, a client never blocks the scan cycle or another connection, and never panics on input.
- `fep` mode follows the echo rule: send a value only when it differs from both the last value sent and the last value polled for that point.
- TCP only. Serial endpoints keep failing the way `net.rs` fails them today.
- Branch `fd-clients` in the worktree `~/git/ferrofire-wt-fdclients`. Commit locally; **never push**.
- Do not commit while a bench runs: the pre-commit hook starts a container.

## Review Focus

1. A remote device that accepts the TCP connection and never answers. Each request has a deadline (stock's timeout in `bennu` mode, 1000 ms in `fep` mode). `<status-tag>` goes false, and the other connections keep polling. Test: Task 5 (Modbus), Task 8 (DNP3).
2. A short, malformed or mismatched response (wrong transaction id, wrong function code, wrong DNP3 sequence). It is discarded and logged, no tag changes, and the session goes on or reconnects. Test: Tasks 4 and 7, plus the fuzz targets in Task 9.
3. A local write while the remote device is down, in `fep` mode. Only the newest value is kept, and it is sent after reconnect. The echo rule still holds. Test: Tasks 5 and 8.
4. A NaN or out-of-range analog value on its way to a register. It is clamped as `Range::scale` clamps it, and NaN is never sent (WARN). Test: Task 5.
5. Configs from the generator (`fd_client.mako`) and the bennu samples in `~/git/sceptre-bennu/data/configs/ep/{modbus,dnp3}-client.xml` load unchanged. Test: Task 3.

---

### Task 1: Stock client goldens

**Files:**
- Modify: `conformance/stock/run-stock.sh` (add the cases `modbus-client` and `dnp3-client`)
- Create: `conformance/stock/probe_cmdif.py`, `conformance/stock/client-{modbus,dnp3}.golden`, `conformance/stock/client-{modbus,dnp3}.pcap`
- Modify: `docs/field-device-protocols-plan.md` rows 7–9 (correct anything the wire contradicts)

**Interfaces:**
- Produces: the golden transcripts Tasks 2, 4 and 7 must match.
  - Each line is `request<TAB>reply` from the command interface, followed by the wire bytes in hex.
  - The cases cover: QUERY; READ and WRITE of each point type; a READ with the server stopped; a Modbus exception; an unknown command; a mixed-case op.

- [ ] **Step 1:** In the container from `run-stock.sh`, run a stock `bennu-field-device` with the sample `modbus-server.xml` and a second one with the sample `modbus-client.xml`, pointed at it. Do the same for DNP3. Drive each client's command interface with `probe_cmdif.py`, and capture with tcpdump.
- [ ] **Step 2:** Record each reply and the client's bytes. Also record:
  - the timeout and the reply when the server is stopped;
  - whether the connection is persistent or opened per command;
  - for DNP3: the startup sequence (integrity poll, unsolicited mask, enable unsolicited), CROB SBO against direct, and AnalogOutputFloat32.
- [ ] **Step 3:** Where the source-derived rows differ from the wire, rewrite the rows to match the wire, and quote the golden line.
- [ ] **Step 4:** Commit: `test(conformance): stock fd-client goldens for Modbus and DNP3`.

### Task 2: Command interface

**Files:**
- Create: `src/fd/cmdif.rs`
- Modify: `src/fd/mod.rs` (add `pub mod cmdif;`)
- Test: unit tests in `cmdif.rs`, and `tests/cmdif_net.rs`

**Interfaces:**
- Produces:
  - `pub trait Points: Send + Sync { fn names(&self) -> Vec<String>; fn read(&self, tag: &str) -> impl Future<Output = Result<Value, String>> + Send; fn write(&self, tag: &str, v: Value) -> impl Future<Output = Result<(), String>> + Send; }` (an object-safe boxed-future form is fine if needed)
  - `pub async fn reply(line: &str, p: &impl Points) -> Vec<u8>`: the whole reply, NUL included.
  - `pub async fn serve(endpoint: &str, p: impl Points + 'static, stop: CancellationToken)`: ZMTP REP via `zeromq::RepSocket`. A bind failure logs and returns.

- [ ] **Step 1: Write the failing tests.** Use a fake `Points` with tags `a` (analog 1.5) and `b` (binary true):
  - `reply("QUERY=")` gives `b"ACK=a,b,\0"`;
  - `reply("read=a")` gives `b"ACK=a:1.500000\0"`;
  - `reply("READ=b")` gives `b"ACK=b:true\0"`;
  - `reply("WRITE=a:2")` gives `b"ACK=Wrote tag a -- 2\0"`, with the value text as Task 1's golden shows it;
  - `reply("FOO=x")` gives `b"ERR=Unknown command type (must be QUERY|READ|WRITE)\0"`;
  - a read error replies exactly as the golden shows.
- [ ] **Step 2:** Run `cargo test -p ferrofire cmdif`. Expected: FAIL (the module is missing).
- [ ] **Step 3:** Implement `reply` and `serve`.
- [ ] **Step 4:** Run the tests: PASS. `tests/cmdif_net.rs` uses a `zeromq::ReqSocket` against `serve` on 127.0.0.1 to check a round trip and that the reply is NUL-terminated.
- [ ] **Step 5:** Commit: `feat(fd): bennu command interface (ZMTP REP)`.

### Task 3: Shared client config: mode, FEP keys, echo rule

**Files:**
- Create: `src/fd/proto/client.rs`
- Modify: `src/fd/proto/mod.rs` (add `pub mod client;`)
- Test: unit tests in `client.rs`; `src/fd/config.rs` tests for the samples

**Interfaces:**
- Produces:
  - `pub enum Mode { Bennu, Fep }`
  - `pub fn mode(n: Node) -> Result<Mode, ConfigError>`: the error is `<mode> \`x\` is not bennu or fep`.
  - `pub struct Fep { pub scan: Duration, pub status: Option<String> }`
  - `pub fn fep_keys(conn: Node, mode: Mode, tags: &mut Tags, opts: &Opts) -> Result<Option<Fep>, ConfigError>`:
    - `<scan-rate>` in seconds, default 1;
    - `<status-tag>` is added as a binary internal tag;
    - in `Bennu` mode, either key is `opts.warn("<scan-rate> is fep-only, ignored")`, and the function returns `Ok(None)`.
  - `pub fn cycle(n: Node) -> Duration`: the `<cycle-time>` of the `<field-device>` ancestor, with the field device's default when absent.
  - `pub fn point_tag(tags: &mut Tags, tag: &str, kind: Kind, opts: &Opts) -> Result<bool, ConfigError>`: declares a device-local tag. A tag already declared by a connection or an `<output>` is `opts.warn` and returns `false`; the first declaration wins.
  - `pub struct Echo<V>`, with `last_sent`, `last_polled` and `pending`:
    - `polled(&mut self, v: V) -> bool`: returns false while a local write is pending; that poll result is ignored.
    - `due(&mut self, now: V) -> Option<V>`: returns `Some` when `now` differs from both `last_sent` and `last_polled`, and nothing is pending.
    - `sent_ok(&mut self, v: V)` and `sent_failed(&mut self)`.
    - Use `store::same` for equality.

- [ ] **Step 1: Write the failing tests:**
  - `mode` of no `<mode>` is `Bennu`; of `fep` is `Fep`; of `FEP2` is an error containing `` `FEP2` ``.
  - In `fep_keys`, `<scan-rate>2</scan-rate>` under `Bennu` warns and is an error under `--strict`; under `Fep` it is 2 s.
  - Echo:
    - after `polled(5)`, `due(5)` is `None`;
    - `due(6)` is `Some(6)`;
    - after `sent_ok(6)`, `due(6)` is `None`;
    - a `polled(7)` while a write is pending returns false;
    - after `sent_failed`, `due(6)` is `Some(6)` again.
  - Both stock sample client configs, and the generator's mismatched close tag (which `config::normalise` already handles), parse with no error.
- [ ] **Step 2:** Run `cargo test client::`: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run the tests: PASS.
- [ ] **Step 5:** Commit: `feat(fd): client mode, fep keys and the echo rule`.

### Task 4: Modbus client, `bennu` mode

**Files:**
- Create: `src/fd/proto/modbus/client.rs`
- Modify:
  - `src/fd/proto/modbus/codec.rs`: client-side request and response codec;
  - `src/fd/proto/modbus/mod.rs` (add `pub mod client;`);
  - `src/fd/proto/mod.rs`: add `("modbus-client", start::<modbus::client::Client>)` to `REGISTRY`.
- Test: unit tests; `tests/modbus_client.rs`

**Interfaces:**
- Consumes: `codec::{parse, frame, Parse}`, `device::{Device, Reg, Range}`, `client::{mode, point_tag}`, `cmdif::{Points, serve}`.
- Produces:
  - `pub fn read_req(fc: u8, addr: u16, qty: u16) -> [u8; 5]`
  - `pub fn write_req(fc: u8, addr: u16, value: u16) -> [u8; 5]`, for FC5 and FC6. FC5's value is `0xFF00` or `0`.
  - `pub fn response(fc: u8, qty: u16, pdu: &[u8]) -> Result<Vec<u16>, Reply>`, where `enum Reply { Exception(u8), Bad(&'static str) }`. Bit tables give 0 or 1 per item.
  - `pub struct Client { conns: Vec<Conn>, cmdif: Option<String>, log: EventLog }` implementing `Module`.
  - `pub struct Conn { endpoint: SocketAddr, unit: u8, map: Device, fep: Option<Fep> }`.
  - The connection-handling details stay inside the module.

- [ ] **Step 1: Write the failing tests:**
  - `read_req(3, 10, 2) == [3, 0, 10, 0, 2]`.
  - `write_req(5, 1, 0xFF00) == [5, 0, 1, 0xFF, 0]`.
  - `response(3, 2, &[3, 4, 0, 1, 0, 2])` gives `Ok(vec![1, 2])`.
  - `response(3, 1, &[0x83, 2])` gives `Err(Exception(2))`.
  - A byte count that disagrees with `qty`, or a function code that is neither `fc` nor `fc | 0x80`, gives `Err(Bad(..))`.
  - A response whose transaction id does not match is discarded (Review Focus 2).
  - `tests/modbus_client.rs`: a ferrofire `<modbus-server>` and a `bennu`-mode client in one process. Driven through the command interface, every line of `client-modbus.golden` replies as stock does.
- [ ] **Step 2:** Run `cargo test modbus::client` and `cargo test --test modbus_client`: FAIL.
- [ ] **Step 3:** Implement.
  - A `READ` is one live request. A `WRITE` is FC5 or FC6, scaled with `Range::scale`.
  - Timeout, connection handling and error replies follow Task 1's golden.
  - Tags hold the last value read or written (spec, "Tags").
- [ ] **Step 4:** Run the tests: PASS. Then cross-pair against stock: `DUT=rust make conformance PROTO=modbus-client`. Create `conformance/modbus-client/` by copying `conformance/scpi-client/`; the peer is a stock `modbus-server`. Expected: exit 0, and the transcript equals the golden.
- [ ] **Step 5:** Commit: `feat(fd): Modbus client, bennu mode`.

### Task 5: Modbus client, `fep` mode

**Files:**
- Modify: `src/fd/proto/modbus/client.rs`
- Test: unit tests; `tests/fd_fep.rs` (create)

**Interfaces:**
- Consumes: `client::{Fep, Echo, cycle}`, Task 4's codec.
- Produces:
  - `pub fn plan_reads(map: &Device, split: &BTreeSet<(u8, u16)>) -> Vec<(u8, u16, u16)>`: `(fc, start, qty)`. It merges contiguous addresses per table, at most 125 registers or 2,000 bits per request, and gives every address in `split` (table function code and address) its own request.

- [ ] **Step 1: Write the failing tests:**
  - Planning:
    - holding registers 0, 1, 2 and 10 give `[(3,0,3),(3,10,1)]`;
    - 130 contiguous registers give `[(3,0,125),(3,125,5)]`;
    - 2,001 coils give two requests;
    - with `(3,1)` in `split`, registers 0–2 give three requests.
  - Unscaling: a raw `32767` over `[0,100]` gives about 50.0. A binary tag on a holding register reads non-zero as true. `min-value == max-value` is a config error.
  - `tests/fd_fep.rs`:
    - **Gateway:** an RTU device (a ferrofire `<modbus-server>`) and a FEP device (a `fep` client polling it, plus its own `<modbus-server>`). A value set on the RTU appears on the FEP's server within `scan-rate` plus one cycle.
    - **Write-down:** a FC6 write to the FEP's server reaches the RTU within one cycle. No second write follows it, per the echo rule: count the RTU's FC6 requests.
    - **Peer down:** with the RTU stopped, `<status-tag>` becomes false and polling backs off. With the RTU restarted, the session recovers and status becomes true.
    - **Silent peer** (Review Focus 1): a listener that accepts and never answers. Status goes false within 1 s plus one cycle, and a second connection to a live RTU keeps updating.
    - **Write while down** (Review Focus 3): set 1, then 2, while the RTU is down. After restart the RTU receives only 2.
    - **NaN and out of range** (Review Focus 4): NaN is not sent and a WARN is logged; 1e9 is sent clamped to 65535.
    - **Exception 02:** on a merged read, the next poll splits that block.
    - **Command interface in `fep`:**
      - A `READ` answers from the tag, with no request on the wire.
      - A `WRITE` reaches the RTU through the echo rule and replies `ACK=Wrote tag t -- v`.
      - `<status-tag>` is false before the first connection.
- [ ] **Step 2:** Run `cargo test --test fd_fep`: FAIL.
- [ ] **Step 3:** Implement.
  - Poll every `scan`; outputs are polled too (spec, "`fep` also polls outputs").
  - Write polled values with `store.write`.
  - Once per `cycle(n)`, run `Echo::due` for each output tag and send through FC5/FC6.
  - A local write wins a race with a poll.
  - Per-request deadline 1000 ms. Use `proto::backoff`. Set `<status-tag>` as the spec says.
- [ ] **Step 4:** Run the tests: PASS.
- [ ] **Step 5:** Commit: `feat(fd): Modbus client fep mode: polling, write-down, status`.

### Task 6: DNP3 master core: requests and response decode

**Files:**
- Create: `src/fd/proto/dnp3/master.rs`
- Modify: `src/fd/proto/dnp3/mod.rs` (add `pub mod master;`), `src/fd/proto/dnp3/app.rs` (the request encoder)
- Test: unit tests against the bytes in `client-dnp3.golden`

**Interfaces:**
- Consumes: `link::{encode, parse, Frame}`, `transport::{segment, Reassembler}`, `app::{read_headers, Header}`.
- Produces:
  - `pub fn request(seq: u8, func: u8, objects: &[u8]) -> Vec<u8>`: the application request, with FIR, FIN and the sequence set.
  - `pub fn class_read(classes: &[u8]) -> Vec<u8>`: the object headers for a class poll (group 60).
  - `pub fn unsol(enable: bool, classes: &[u8]) -> Vec<u8>`
  - `pub fn crob(idx: u16, on: bool) -> Vec<u8>`: group 12 variation 1.
  - `pub fn ao_f32(idx: u16, v: f32) -> Vec<u8>`: group 41 variation 3.
  - `pub fn output_status() -> Vec<u8>`: groups 10 and 40, all, used in FEP only.
  - `pub enum Seen { Binary(u16, bool), Analog(u16, f64) }`
  - `pub fn decode(fragment: &[u8]) -> Result<(u8 /*seq*/, u16 /*iin*/, bool /*unsol*/, Vec<Seen>), Malformed>`: decodes groups 1, 2, 3, 10, 30, 32 and 40. Other groups are skipped, as the stock SOE skips them.

- [ ] **Step 1: Write the failing tests:**
  - Each builder's output equals the request bytes of the matching golden line: the startup integrity poll, the unsolicited mask, the CROB SelectAndOperate and DirectOperate, and the AnalogOutputFloat32.
  - `decode` of the golden's integrity response gives the expected `Seen` list.
  - A truncated object gives `Err`, and so does a `qty` past the end (Review Focus 2).
- [ ] **Step 2:** Run `cargo test dnp3::master`: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run the tests: PASS.
- [ ] **Step 5:** Commit: `feat(fd): DNP3 master request and response codec`.

### Task 7: DNP3 master, `bennu` mode

**Files:**
- Modify:
  - `src/fd/proto/dnp3/master.rs`: the `Client` module and the session;
  - `src/fd/proto/mod.rs`: add `("dnp3-client", start::<dnp3::master::Client>)` to `REGISTRY`.
- Test: `tests/dnp3_master.rs`; `conformance/dnp3-client/`

**Interfaces:**
- Produces: `pub struct Client` implementing `Module`. It parses the row 9 schema: `address`, `scan-rate`, `command-interface`, and `dnp3-connection*` with `endpoint`, `address`, the four point lists with `sbo`, and `class-scan-rates`.

- [ ] **Step 1: Write the failing tests.** In `tests/dnp3_master.rs`, a ferrofire `<dnp3-server>` and a `bennu`-mode master run in one process:
  - The startup sequence's bytes equal the golden.
  - A binary change on the outstation reaches the master's tag through a class scan and through an unsolicited response; the unsolicited one is confirmed.
  - A command-interface WRITE of a `sbo` binary output is Select then Operate. A non-sbo one is DirectOperate. An analog output is group 41 variation 3. The tag is updated optimistically.
  - Each line of `client-dnp3.golden` replies as stock does.
  - A sequence mismatch is discarded with no tag change (Review Focus 2).
- [ ] **Step 2:** Run `cargo test --test dnp3_master`: FAIL.
- [ ] **Step 3:** Implement. Follow row 9 as Task 1 corrected it.
- [ ] **Step 4:** Run the tests: PASS. Then `DUT=rust make conformance PROTO=dnp3-client`, with a stock outstation as the peer: exit 0. Bytes against the stock-master golden: equal.
- [ ] **Step 5:** Commit: `feat(fd): DNP3 master, bennu mode`.

### Task 8: DNP3 master, `fep` mode

**Files:**
- Modify: `src/fd/proto/dnp3/master.rs`
- Test: `tests/fd_fep.rs` (extend)

**Interfaces:**
- Consumes: `client::{Fep, Echo, cycle}`, `master::output_status`.

- [ ] **Step 1: Write the failing tests.** These mirror Task 5's, over DNP3:
  - a gateway through the FEP's own `<dnp3-server>`;
  - a write-down within one cycle, with no echo: count the outstation's operates;
  - an outstation that is down, and one that is silent (Review Focus 1);
  - a write while down (Review Focus 3);
  - output-status reads (groups 10 and 40) seen on the wire only in `fep` mode.
- [ ] **Step 2:** Run `cargo test --test fd_fep dnp3`: FAIL.
- [ ] **Step 3:** Implement:
  - the `bennu` session, plus `output_status()` in each scan;
  - scan results go into tags through `store.write`;
  - output tags are checked once per cycle through `Echo`;
  - `<status-tag>` and backoff.
- [ ] **Step 4:** Run the tests: PASS.
- [ ] **Step 5:** Commit: `feat(fd): DNP3 master fep mode`.

### Task 9: Fuzz, docs, regression and close-out

**Files:**
- Create:
  - `fuzz/fuzz_targets/fd_modbus_client.rs` (feeds `codec::parse`, then `client::response`);
  - `fuzz/fuzz_targets/fd_dnp3_master.rs` (feeds `link::parse`, then `Reassembler`, then `master::decode`);
  - `docs/field-device-clients.md`.
- Modify:
  - `fuzz/Cargo.toml`: two `[[bin]]` entries, in the `fd_scpi` style;
  - `conformance/README.md`: divergences, if any;
  - `docs/field-device-protocols-plan.md`: mark rows 7–9 built, with the bead id.

- [ ] **Step 1:** Run each fuzz target for 10 minutes: `cargo +nightly fuzz run fd_modbus_client -- -max_total_time=600`, then the same for `fd_dnp3_master`. Expected: no crash. A crash becomes a regression test plus a fix.
- [ ] **Step 2:** Write `docs/field-device-clients.md`:
  - the schema;
  - `<mode>`, `<scan-rate>` and `<status-tag>`;
  - the echo rule;
  - a FEP gateway example config.
- [ ] **Step 3:** Regression:
  - `cargo test`, `cargo clippy --all-targets -- -D warnings` (no new warnings), and `make conformance PROTO=modbus` and `PROTO=dnp3`. All pass.
  - Run the fdmicro S profile for ferrofire on both protocols. Numbers within noise of `docs/bench/results/fdmicro-cmp/ferrofire_S_*`.
- [ ] **Step 4:** Commit: `docs(fd): clients page, fuzz targets, plan rows 7-9 built`.
- [ ] **Step 5:** File follow-ups, with `--deps discovered-from:ww-7mw.1`:
  - serial endpoints (F6);
  - Modbus FC15/FC16 write-down;
  - IEC 104 and BACnet clients.
  Then close ww-7mw.1 with a reason naming the commits.
