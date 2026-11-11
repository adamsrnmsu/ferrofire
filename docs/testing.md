# How ferrofire is tested

This page says, for each component and protocol, what is checked, against what, how to run it, what is not covered, and what
could cover it. It is written so the gaps are as visible as the coverage. Where a claim rests on a committed file, the file is
named. Numbers come from [results.md](results.md) and the raw data under `docs/bench/results/`.

Terms. An **oracle** is something other than our own code that decides whether our output is right (stock bennu, PYPOWER, a
third-party client, a packet dissector). A **peer** is an independent program that talks to ours. A test that only compares our
code with our own reading of a standard shows consistency, not correctness; the table marks which is which.

## Summary

| Component | Strategy | Independent oracle | Status |
|---|---|---|---|
| Provider engine, waterway model | Unit tests; trace golden; closed-loop transit test | None: the golden was captured from ferrofire's own earlier `physics.rs`. Live: stock bennu RTUs read the values over Modbus | Covered; the physics itself is not validated against a reference |
| Power flow | Goldens from PYPOWER 5.1.19 (case9, 14, 30, 57, 118, 300; DC; a branch outage); properties; drop-in test against stock pybennu | PYPOWER (stock pybennu's solver) | Covered for AC and DC power flow; PYPOWER itself is the only reference |
| Datagram v1 | Wire-spec test vector; receiver rules; `tc netem` bench; one hour on the live range; Wireshark dissector | Stock bennu RTUs consume it; tshark with `ferrofire_datagram.lua`; the hash vector is checked in Rust and in Python | Covered; no real network loss measured |
| Request/reply (REP) robustness | Every-request-gets-one-reply tests; fuzz target; fault injection on the bench and range | libzmq client in the interop tests; stock bennu clients on the range | Covered; fuzz is a smoke run |
| Tools (`probe`, `doctor`, `top`, `log`, `explain`) | insta snapshot tests of text and `--json`; integration tests against a real `run`; live captures | The live range (stock RTUs, OVS) | Covered; snapshots pin format, not correctness |
| Field-device core | Unit and robustness tests; fuzz targets for config and datagram; the shared quality gates | Stock bennu (config semantics, from the spec) | Covered by tests; no dedicated differential for the core |
| DNP3 outstation | Byte-for-byte replay of recorded stock transcripts; opendnp3 master; tshark expert info; Zeek ICSNPP; fuzz | Stock bennu outstation, opendnp3, tshark, Zeek ICSNPP | Strongest of the protocols |
| IEC 61850 MMS + GOOSE | libiec61850 client examples and GOOSE subscriber; tshark `mms`/`goose`; in-process client; fuzz | libiec61850 (GPL, test peers only), tshark. No stock oracle: bennu has no 61850 server | Covered against one independent stack; the server library is young |
| ICCP / TASE.2 server | `mms_utility` browse and read; our thin ICCP client; tshark `mms`; fuzz; igs-iec-tase2 and telcon-tase2 as clients and as servers | Two independent, young TASE.2 stacks (igs, telcon) plus two real captures (checked by the parsers); libiec61850 and tshark for the MMS layer | Better: three readings of the standard agree on most of block 1; known disagreements are marked (below). Still no oracle |
| ICCP parsers (Zeek, Wireshark) | btest baselines and field goldens over `tests/pcaps/iccp`; the two real captures when fetched | tshark's own MMS dissector underneath; the Wireshark wiki `mms.pcap` and the chaugan capture (real ICCP) | Tested on synthetic captures and, locally, on two real ones |
| Whole system | Burger range boards, live range runs | Stock bennu, phēnix, OVS | Covers stock bennu fixes and ferrofire runs; see below |

Everything below is in the repo and runs from `make`. Make targets that need docker run in the `ff-rust:22.04` container or
in the `ferrofire-testpeers` image (`make testpeers`, about 10 minutes the first time).

## Provider engine and power flow

**Checked.**
- Expression parser, model loading, shm contract, wire framing (`cargo test`; unit tests inside `src/` and `tests/*.rs`).
- Waterway: `tests/waterway.rs` replays 600 s of the closed loop against `tests/golden/waterway_trace.csv` (every 5 s, 15 tags)
  and runs a full boat transit with the RTU logic.
- Power flow: `tests/power.rs` compares bus voltage and angle, generator P and Q and branch flows with PYPOWER 5.1.19 on case9,
  14, 30, 57, 118 and 300 (AC), case9 and case30 (DC) and case9 with branch 4-5 out. Tolerances in the test: VM 1e-6, everything
  else 1e-4. `tests/power_tags.rs` round-trips every tag of case30 through set and get. Properties: per-bus power balance,
  barrier behaviour, write latching, a tank against its closed form.
- Drop-in: `tests/pypower-conformance.sh` installs ferrofire as `pybennu-power-solver` in a `bennu:arm64` container and compares
  it with the stock pybennu `PyPower` class on case9 and the soap case300 (needs a Lima VM; `make linux` first). Also
  `tests/conformance.sh`.
- Binaries: `tests/cli.rs` drives `ferrofire run` (REP, UDP, SIGTERM) and `check`; `tests/simulink.rs` drives `simulinksolver`
  against real SysV shm, POSIX semaphores and the updates FIFO.

**Oracle.** PYPOWER 5.1.19, generated at test time by `tests/golden/gen_pypower.py` (`make golden`, needs `uv`; goldens are never
committed). Stock pybennu for the drop-in test.

**Run.** `make golden`, `make test`. The drop-in script is run by hand in the Lima VM.

**Gaps.**
- The waterway trace golden was captured from ferrofire's earlier hand-written `physics.rs`. It proves the YAML model reproduces
  that code, not that the physics is right.
- PYPOWER is a port of MATPOWER. Agreeing with it shows the solver matches that implementation on those cases, not that the
  model matches a real grid. Only Newton AC and the DC approximation are compared; no OPF.
- The drop-in test is manual and needs the Lima VM, so it is not in `make check`.

**Ideas.** Cross-check the same cases against pandapower or MATPOWER/Octave, which are independent code bases. Compare a
waterway transit against the original Simulink model output where it exists.

## Datagram v1

**Checked.**
- Wire format: the test vector in [datagram-v1.md](datagram-v1.md) (endpoint, `H(endpoint)`, group, one complete datagram in hex)
  is asserted in `tests/bennu.rs` and, with a separate Python hash, in `tools/wireshark/test.sh`.
- Receiver rules (`ferrofire::datagram::Receiver`), `tests/robust.rs`: garbage, truncated, wrong-group, duplicate and old
  datagrams are dropped, reordered chunks of one cycle all apply, a late old cycle does not overwrite newer values, a restarted
  provider is detected after reordering, shuffled chunks give the state of in-order delivery, random datagrams never panic.
- Sender: UDP send errors are counted and never block the tick.
- Netem bench ([datagram-v1-bench.md](bench/datagram-v1-bench.md), 117 runs in `docs/bench/results/raw-datagram-v1/`): sequence
  numbers against injected loss (0.1 / 1 / 5 % read as 0.10-0.17 / 1.07-1.34 / 5.07-5.14 %), cycle completeness against the
  (1-p)^D model, jitter and reordering, per-RTU fan-out, restarts, faults, pull-bridge.
- Live range, section G: one hour, two stock bennu RTUs, one OVS bridge: 35 993 / 35 993 / 35 994 datagrams, 0 lost, duplicated
  or reordered, 100 % of 325 RTU Modbus samples matching the provider; snooping on and off; `kill -9` and `SIGSTOP` of the
  provider; 11 041 garbage datagrams injected.
- Dissector: `tools/wireshark/ferrofire_datagram.lua` decodes the test vector under tshark.

**Oracle.** Stock bennu RTUs (they accept the datagrams and publish the values over Modbus, which a Modbus client reads); `tc
netem` for loss (the injected rate is the reference for the measured one); tshark.

**Run.** `make test` (vector, receiver); `make wireshark` (dissector; host tshark, not in `check`); `make bench-dg` (about 1.5 h,
then `make bench-dg-report` writes the report from the raw files); the live range runs are described in section G of the bench
report, with captures in `docs/live-tools/`.

**Gaps.**
- netem loss is independent per packet; a congested switch loses in bursts. Loss on a real network was not measured.
- The bench is one VM with one clock, so transit times from `_ff.t` are real only there. Two live RTUs only.
- The receiver and the bench client are ours. The only third-party consumer is stock bennu's reader, which ignores what it
  does not subscribe to; it does not check sequence numbers.
- The finding that the first receiver dropped reordered chunks (ww-d4a) came from the bench, not from a unit test: the unit
  tests had not modelled reordering.

**Ideas.** Run the same streams across a physical switch and a second host. Replay a bursty-loss trace instead of uniform loss.
Add a second independent receiver (a few lines of Python from the spec alone) and run both over the same pcap; any difference
is a spec ambiguity.

## Request/reply robustness

**Checked.** Every request gets exactly one reply: bad requests get `ERR` naming the problem, panicking handlers still reply,
multipart and oversize requests get one `ERR`, a client survives a lost reply, mutex poisoning is survived, misspelt tags and
keys get a "did you mean" (`tests/robust.rs`, 19 tests). Fuzz targets under `fuzz/fuzz_targets/` (`request`, `datagram`, `expr`,
`model`, and `fd_*` for the field device) run on random bytes. On the bench and range: provider killed and restarted, all
replies dropped for 4 s, about 3 000 random requests of up to 12 MB, non-UTF-8, `nan` and `1e999`: every request answered,
provider up ([results.md](results.md)).

**Oracle.** The property is "no panic and a reply", which needs no oracle. Interop tests use a libzmq client against the
pure-Rust `zeromq` crate; stock bennu clients talk to it on the range.

**Run.** `make test`; `make fuzz-smoke` (every target for `FUZZ_SECS`, default 10 s); `make fuzz T=request` until stopped. CI runs
`make check` then `make fuzz-smoke`. A fuzz crash is turned into a test in `tests/robust.rs` before it is fixed.

**Gaps.**
- Fuzzing needs the nightly toolchain (the container has it); the rest of the project is on the pinned stable one.
- The CI smoke run is 10 s per target. The longest committed run was 90 s per target (commit 3f82213). That finds shallow
  crashes, not deep ones.
- Fuzz checks "no panic", not "the right reply".

**Ideas.** A scheduled long fuzz run with a persisted corpus. Differential check of replies against stock bennu's REP handler
for the commands both implement.

## Tools

**Checked.** `probe`, `doctor`, `explain`, `log` and `top --once` text and `--json` output are pinned by insta snapshots in
`tests/snapshots/` (`tests/snapshots.rs`, `tests/log.rs`); `tests/probe.rs`, `doctor.rs`, `top.rs`, `log.rs` and `explain.rs` run the
real commands against a real `run` process, and `top` is also driven through a pty. Live captures in
[live-tools/README.md](live-tools/README.md) (`docs/live-tools/captures/`): writes reaching rtu-1's Modbus registers within
577 ms, timed `play`, 60 s `record` (601 rows, no gaps), `diff` catching a register written straight onto an RTU, `top` showing
`STALE` when the provider stops, `doctor` catching a wrong multicast route, a starved member and a 1.0 s clock skew.

**Oracle.** Snapshots have none: they pin our own output, so a format change fails the test and a human decides. The live runs
use stock RTUs read over Modbus as the independent view.

**Run.** `make test`; accept intended snapshot changes with `INSTA_UPDATE=always make test`. Live: the scripts in
`docs/live-tools/`.

**Gaps.** A snapshot proves the output did not change, not that it was right the first time. Live captures are one run on one
range; they are not repeated in CI. `doctor` was shown on the three faults above, not on a catalogue of network faults.

**Ideas.** A scripted fault matrix for `doctor` (one fault per case, expected finding per case) run on the range.

## Field-device core

The core is the config loader, datastore, logic, provider link and module registry (`src/fd`), shared by all protocol modules.

**Checked.** `tests/fielddevice.rs`, `tests/fd_robust.rs` (hostile configs and inputs), fuzz targets `fd_config` and `fd_datagram`,
and the gates below. Semantics come from [field-device-spec.md](field-device-spec.md), which records what the stock bennu field
device does, with probes in `conformance/stock/` (`probe_modbus.py`, `probe_modbus_overflow.py`, `probe_dnp3.py`, `probe_104.py`,
`logic_corpus.xml`) run against `bennu:arm64` by `conformance/stock/run-stock.sh`.

**Gaps.** The core has no byte-level differential of its own; its stock comparison is through the DNP3 transcripts. There is no
Rust Modbus or IEC 104 server in ferrofire yet, so the Modbus and 104 probes record stock behaviour and are not run against ours.

**Ideas.** Run the logic corpus (`conformance/stock/logic_corpus.xml`) through the stock device and ours and compare the
datastore after each step, as the DNP3 transcripts do for the wire.

## DNP3 outstation

**Checked.**
- Differential, `conformance/dnp3/`: about 230 requests (`probe_diff.py`: link layer, reads by group and variation, events and
  their order, IIN, every control function, routing, CloseNew) and 130 + 130 + 450 points (`probe_big.py`: event overflow,
  multi-fragment responses, two-octet ranges) were recorded against the stock bennu outstation: transcript, machine-readable
  trace and event log (`stock-*.golden`, `.trace`, `.logs`). `tests/dnp3_golden.rs` replays the traces against our state machine
  on a virtual clock and requires identical link frames, application bytes and event-log lines. It runs in `cargo test`, so it is
  also in mutation testing. Two things a wall clock decides are masked: provider time inside time-bearing objects and IIN1.4
  NEED_TIME.
- Real sockets (`tests/dnp3_net.rs`): CROB and analog commands reach the write queue, CloseNew, bad CRC closes, 300-connection
  churn leaves descriptors flat, bind failure, serial endpoints. `tests/dnp3_accept.rs`: `accept()` failing with EMFILE is
  retried. `tests/dnp3_robust.rs` and fuzz target `fd_dnp3`: mutated transcripts, random bytes, random fragments.
- Acceptance, `conformance/dnp3/`: opendnp3's master (and our scriptable `ff-dnp3-master`) does an integrity poll, a CROB, an
  SBO analog output and a direct analog output against stock and against ours; tshark must find 0 malformed frames and no
  expert Error or Warn (a bad checksum is only a Warn, so Warns count); Zeek's ICSNPP `dnp3.log` and `dnp3_control.log` rows are
  asserted. `conformance/dnp3/smoke.sh` runs the stock `dnp3-client` against ours and reads `brkr:true` and `load-power:400.5`.
- Findings recorded as tests (things the stock device does that the spec did not say): two-octet ranges whenever a type has a
  point above index 255; events before static data, in the order they happened; CON on every non-final fragment; NEED_TIME 30 s
  after a time write; a count qualifier on static data is NO_FUNC_CODE_SUPPORT; a request with any unknown object is refused whole.

**Oracle.** The stock bennu outstation (byte for byte), opendnp3 3.1.2, tshark, Zeek with CISA icsnpp-dnp3.

**Run.** `make test` (replay); `make testpeers`, then `make conformance PROTO=dnp3` against the stock device or `DUT=rust make
conformance PROTO=dnp3` against ours; `make diff-dnp3` (live, in containers, build `target-linux/debug/ferrofire` with `make
test` first); `make differential PROTO=dnp3 WHO=stock SET=diff` to re-record. `make conformance-selftest` proves the harness
fails on a damaged and on an empty capture. Differential runs must not overlap: every device joins 239.0.0.1:40000 on the
same bridge, and one probe's publishes land on the other (this contaminated the first goldens).

**Gaps.**
- The reference is the stock device's behaviour, including its quirks, not the IEEE 1815 text. Where stock deviates from the
  standard, so do we.
- Only the stock configuration shapes in the probes are covered; other object groups, unsolicited responses and secure
  authentication are not.
- The testpeers image has been built for arm64 only. The publish workflow builds amd64 too, but it runs only on a `v*` tag and
  none exists yet, so an amd64 build has not been exercised.

**Ideas.** Run opendnp3's own outstation test suite or a DNP3 conformance tool against ours (the DNP3 Users Group publishes
conformance test procedures; whether a free tool exists was not checked). Build and run the testpeers image on amd64.

## IEC 61850 (MMS and GOOSE)

**Checked.** `tests/iec61850_net.rs` (in-process and socket clients), `tests/iec61850_robust.rs` (mutated transcripts),
`tests/iec61850_accept.rs` (`accept()` failure), fuzz target `fd_iec61850` (bytes through the OSI stack and the ACSI server).
`conformance/iec61850/`: libiec61850's unmodified example clients (read, control, reporting) and GOOSE subscriber run against
our server on `lo`; `check.sh` asserts tshark's `mms` Initiate, GetNameList, Read, Write, InformationReport and `goose` fields
(appid, gocbRef, data set, entries) and what the peers printed. Values are not asserted, because the device's inputs come from
whatever publishes on the multicast group. 0 malformed frames and no expert warnings.

**Oracle.** libiec61850 v1.6.2 (GPL-3.0, built and run only as test peers, never linked), tshark. No stock oracle: bennu never
loads an IEC 61850 server. Zeek has no MMS or GOOSE parser, so that step prints SKIP.

**Run.** `cargo test`; `make testpeers` (rebuild after the Dockerfile changed: images built before 2026-11-10 lack the control
client and GOOSE subscriber); `make conformance PROTO=iec61850`; `cargo +nightly fuzz run fd_iec61850` in `fuzz/`.

**Gaps.**
- The server is built on `iec61850-rs` 0.4.0, a pre-1.0 crate from a single author (first release 2026-09-05; every minor
  version breaks; pinned with `=0.4.0`). Its claims of libiec61850 interop are its own; ours is one stack, one version.
- One independent peer. An agreement between `iec61850-rs` and ferrofire's use of it is not independent of the library.
- GOOSE: the subscriber is libiec61850's, which does not deliver frames to `01-0C-CD-01-00-00` (observed on `lo`), so the default
  destination is `...-00-01`. No test on a physical L2 segment. No subscriber in ferrofire. No TLS (62351-3), 62351-6
  authentication or R-GOOSE.
- Values and control semantics (select-before-operate state machines) are exercised, not asserted against a reference.

**Ideas.** Differential fuzzing: feed the same mutated MMS PDUs to libiec61850's server and to ours and compare the response
class (reply, error, reject, close); a crash or a hang on either is a finding, and so is any disagreement worth reading.
Run a second client stack (OpenMUC's Java OpenIEC61850, or a vendor client such as Triangle MicroWorks' test harness) against
ours. Run an IED simulator's SCL through our SCL loader. Attend an interoperability event (IEC 61850 UCA International Users
Group runs them) and bring the server.

## ICCP / TASE.2 server

**Checked.** `tests/iccp_net.rs` (socket tests with our thin client, `tests/common/iccp_client.rs`, also `examples/iccp-client`),
a hostile-bytes test, fuzz target `fd_iccp`. `make conformance PROTO=iccp` (`conformance/iccp/`) runs four legs against our server and the
peers' servers in one capture, then judges it with tshark (0 malformed, expert info clean apart from the allow-list in `proto.env`, MMS
decodes), `tools/wireshark/iccp.lua` and the Zeek analyzer:

1. libiec61850's `mms_utility` (domains, identity, directory, three reads) and our thin client (browse, read, write, data set,
   transfer set, three reports) against our server. As before.
2. **igstase2-client** ([aklira/igs-iec-tase2](https://github.com/aklira/igs-iec-tase2) with igs-iso9506-mms, Apache-2.0) against our
   server: associates with a Bilateral Table (it checks `Bilateral_Table_ID`, `TASE2_Version` 2000.08 and the length of
   `Supported_Features` itself), GetNameList, read of data values and a type, write of a plain real, create / delete a data set, and a
   client whose table id differs. Concludes.
3. **tase2-client** ([janfietz/telcon-tase2](https://github.com/janfietz/telcon-tase2), `tase2-test-tools`, MIT OR Apache-2.0) daemon against
   our server: `blt-id`, name lists, reads of plain real, state and Q real, write, define / read / delete a data set.
4. Our thin client, one named check per step (`ICCP_STEPS=1`), against **igstase2-server** (port 10102, `igs-server.toml`; calls with an
   AP-title, `ICCP_CALLING_AP_TITLE`) and telcon's **tase2-server** (port 10103): association, Identify, browse, `Bilateral_Table_ID` /
   `TASE2_Version` / `Supported_Features`, reads, write, data set define / members / read / delete, `Next_DSTransfer_Set`, a transfer
   set armed with interval and with RBE and its reports, Conclude.

Every expectation is `ok` (must pass), `xfail` (a known disagreement or peer limit: the run fails if it starts to pass, so the mark
cannot go stale) or `skip` (printed with the reason). Wire checks on top: the negotiated nesting level never exceeds the proposed one
(igs proposes 2) and a proposal of 2 is answered 2; object-non-existent reaches igs as ServiceError access 2; `Supported_Features` is 12
bits and `TASE2_Version` {2000;8} on every read.

**What still fails, and why** (all marked `xfail`):

| Check | Peer | Why | Bead |
|---|---|---|---|
| igs `start` of a transfer set (interval, RBE) against our server | igs | igs neither defines the data set nor reads `Next_DSTransfer_Set`, and each CLI call is its own association; ours has no preconfigured data sets or transfer-set names | ww-otj.71 |
| telcon `read --kind real_q_time_tag` / `state_q_time_tag` | telcon | telcon decodes the QTimeTag structures as {value, flags, time}; ours and igs's are {time, value, flags}; no real capture holds one | ww-otj.69 |
| telcon `transfer-set` against our server; our `arm` against telcon's server | telcon | telcon wants a 2-member `Transfer_Set_Name`; both real captures and ours have 3 (scope, domain, name) | ww-otj.70 |
| our GetNameList / variables against igs's server | igs | the server does not serve GetNameList (its docs) | peer limit |
| our Identify, `Bilateral_Table_ID`, `TASE2_Version`, `Supported_Features`, data set members against telcon's server | telcon | the test daemon does not serve them (its own `blt-id` fails) | peer limit |

Skipped with a reason: igs writing a `Data_State` (its CLI sends a boolean), telcon reading `TASE2_Version` / `Supported_Features` (no
command). Smoke findings not fixed here and not exercised by a passing check: the Transfer_Set_Name scope as `unsigned` (ww-otj.64), a
12-member DSTransferSet, Buffer_Time, and data sets whose first members are `Transfer_Set_Name` / `DSConditions_Detected` (ww-otj.65).
Observed, not marked: after a Conclude response our stack closes the TCP connection, and igs sends an ACSE release request after the
Conclude that meets an RST (tshark: "Connection reset", an undecoded zero-byte MMS PDU and OID; allowed in `proto.env`).
`Bilateral_Table_ID` is domain-scoped on the wire in both real captures and in igs; we serve it in the VMD scope as well (a superset).

**The real captures.** `tests/pcaps/iccp/fetch-real.sh` downloads the Wireshark wiki `mms.pcap` (ICCP between two utilities, 2005,
TASE2_Version {1996;8}) and chaugan/wireshark-iccp-dissector's `iccp_with_switchover_anon.pcap` (9 associations, {2000;8}) into
`tests/pcaps/iccp/real/` (git-ignored, sha256-pinned; the captures are other people's data and are never committed). If that directory
exists, the run asserts on both: tshark 0 malformed, `iccp.lua` without Lua errors and every `Supported_Features` 12 bits, the
TASE2_Version above, and the Zeek analyzer with an empty weird.log and reporter.log. This is how the 9-bit string and the log limits
(Zeek `log_container_field_truncated`) were found.

**Oracle.** None for the TASE.2 layer, but now three readings: ours, igs-iec-tase2 (clean-room from 60870-6-503/802 with clause
references in its PROVENANCE.md) and telcon-tase2, plus two real captures for the wire shapes. **These peers are young and possibly
AI-assisted, so agreement is evidence, not proof;** where they disagree with each other (ww-otj.69) the standard decides, and it is not
in the repo. Bit meanings of `Supported_Features` (bit n = block n+1; ours sets blocks 1 and 2) are from igs's model and four real
`Supported_Features` values; the real ones set blocks 1, 2, 4, 5, 7 and others, never contradicting that numbering, but the numbering
itself is still not checked against the text of 503 8.2.9.

**Run.** `cargo test`; `make testpeers` (builds both peers from pinned commits: about 15 minutes), `tests/pcaps/iccp/fetch-real.sh`, then
`make test` (builds ferrofire and the example) and `make conformance PROTO=iccp`; `cargo +nightly fuzz run fd_iccp` in `fuzz/`.

**Gaps.**
- IEC 60870-6-503 and -802 are paywalled and not in the repo. Only the validity and current-source codes were corroborated by a vendor
  document (Ipesoft D2000); the three stacks and the captures now corroborate the version, the 12-bit features, and most of block 1,
  but not the QTimeTag order. Everything else is unverified ([field-device-iccp.md](field-device-iccp.md)).
- Both new peers are test daemons, not products: telcon's does not serve the special variables, igs's does not serve GetNameList.
- Conformance blocks 1 and 2 are reported; blocks 3 to 9 are not built. Operator request, buffering and critical reports are
  refused. No TLS (62351-3) or 62351-4.
- The peer is identified by remote IP; the ACSE AP-title is not checked, because the pinned crate does not expose it. Our client
  does send one (igs's table requires it).
- `iec61850-rs` 0.4.0 is patched in `third_party/` (Initiate nesting; see its PATCHES.md, ww-otj.68).

**Ideas.**
1. Buy the standard (IEC 60870-6-503 for the services and protocol, -802 for the object models) and check every layout line by
   line against `model.rs` and `transfer.rs`. This settles ww-otj.69 and the feature bits.
2. More real captures: the other public collections ([automayt/ICS-pcap](https://github.com/automayt/ICS-pcap),
   [tjcruz-dei/ICS_PCAPS](https://github.com/tjcruz-dei/ICS_PCAPS)) were not read in full; none has been checked for QTimeTag objects.
3. More open implementations as further readings: [FreeTase2](https://github.com/aklira/FreeTase2) (Python on libiec61850, GPL-3.0, blocks
   1, 2 and 4) and [pyiec61850-ng](https://github.com/f0rw4rd/pyiec61850-ng) (GPL-3.0, "highly experimental"), rusty-iccp, cB-Sigma's iccpsim
   (all untried). GPL: test peer only, as with libiec61850.
4. Vendor trial stacks: Triangle MicroWorks' IEC 60870-6 source library (blocks 1, 2 and 5), SISCO MMS-EASE and AX-S4 ICCP, Ipesoft D2000's
   ICCP driver, Elipse's. Evaluation terms were not checked. A trial client talking to our server is the closest thing to a product peer.
5. Interop events and utility partners: an EMS vendor or a utility can capture one Initiate and one InformationReport from a real link.

## ICCP parsers (Zeek and Wireshark)

**Checked.** `tools/zeek/iccp` (Spicy analyzer, `mms.log` and `iccp.log`) and `tools/wireshark/iccp.lua` (post-dissector over
tshark's MMS) are run over the same corpus, `tests/pcaps/iccp`: libiec61850 server and client captures (taken in a container by
`capture-iec61850.sh`; the pcaps are data), `tase2_assoc.pcap`, `tase2_report.pcap` (including a COTP-segmented report),
`malformed.pcap`, and `fuzz.pcap` (150 seeded mutants), generated by `make_pcaps.py`. Zeek: btest baselines (`tools/zeek/iccp/tests/Baseline`);
a message that fails to parse is dropped on its own and the next one on the connection still parses. Wireshark: field goldens in
`tests/pcaps/iccp/expected`, 0 malformed.

**Oracle.** tshark's built-in MMS decoder for the layers below TASE.2. For TASE.2 itself the synthetic `tase2_*.pcap` files were written
by `make_pcaps.py` from the same layouts the parsers implement; the libiec61850 captures are real MMS but contain no ICCP. The two real ICCP
captures (`fetch-real.sh`, see above) are the independent check: `make conformance PROTO=iccp` runs both parsers over them.

**Run.** `make zeek-iccp` (builds in the `ff-zeek` container; `ZEEK_ICCP_FLAGS=--update` rewrites baselines, read the diff);
`make wireshark` (host tshark; also runs the datagram test vector).

**Gaps.**
- Same as ICCP above: the parsers and the synthetic captures share the server's assumptions; a green run says they agree.
- Not decoded: alternate access, `typeDescription` of `getVariableAccessAttributes`, BER indefinite length, high tag numbers,
  more than the first presentation data value of a PPDU. Parse failures inside a message are silent (no event).
- Not tried in a running Malcolm; no live range run with a Zeek sensor on tcp/102.

**Ideas.** Any real ICCP capture (see above) is the test: run both parsers over it, and every unparsed or mislabelled field is a
bug in a layout. Compare the Wireshark script's output with a vendor tool's decode of the same capture.

## Quality gates

These apply to the whole of `src/`, from [ub-mode](https://github.com/unclebob/ub-mode); numbers and history are in
[quality.md](quality.md).

| Gate | What it checks | Run |
|---|---|---|
| `fmt-check`, `lint` | `cargo fmt --check`; `clippy --all-targets -D warnings` | `make fmt-check lint` |
| `crap` | [crapper](https://github.com/unclebob/crapper): CRAP (complexity and coverage) per function must be below 10 (`CRAP_MAX 9.99`); a complex function needs tests that cover it | `make crap` |
| `dry` | [dryer](https://github.com/unclebob/dryer): no duplicate-code candidate scoring 0.8 or more | `make dry` |
| `deny` | [cargo-deny](https://github.com/EmbarkStudios/cargo-deny): licence allowlist, RustSec advisories, yanked crates, wildcard dependencies, sources (needs network for the advisory database) | `make deny` |
| `mutate` | [mutator](https://github.com/unclebob/mutator): every covered mutation site reruns the suite; a surviving mutant fails (exit 3). Slow, so not in `check`. Last recorded run: 690 killed, 0 survived, 1 uncovered site (the daemon fork) | `make mutate` |
| `check` | fmt-check, lint, test, crap, dry, deny; CI runs it on every push, then `make fuzz-smoke` | `make check` |

What the gates do not say. Mutation testing shows the tests notice changes to the code that exists; it does not show the code
matches a standard. The recorded mutation numbers predate the field-device modules (`quality.md` covers the engine and transport
work); a mutate run including `src/fd` is not recorded there. Mutation workers run the suite in parallel, so tests that use
fixed ports or kernel object names can fail for the wrong reason and count as kills; the tests now lock and use free ports, and
the first baseline's 0 survivors is an upper bound.

## Burger range boards

[everything-burger-with-extra-sauce](https://github.com/adamsrnmsu/everything-burger) (burger) is a live smoke test for open
`sceptre-phenix-apps` and `sceptre-bennu` pull requests. A switchboard (`switchboards/<name>.yaml`) names the PRs to merge onto
upstream `main` and the items to switch on; burger installs the result into phēnix, stands up the waterway topology (provider VM
and two RTU VMs) with a SCORCH pipeline, and checks the outcome. It tests stock bennu, not ferrofire; it matters here because
ferrofire's references are stock bennu's behaviour and the boards show that the stock side it is compared with, and runs beside, behaves. The five boards below
carry five open bennu pull requests; each is one run (reports in `work/<board>/report.md`
in the burger repo, 2026-11-10):

| Board | bennu PR | What passed |
|---|---|---|
| `fdleak` | #79, close accepted Modbus client sockets | 1500 / 1500 connections, socket fds 5 -> 5, server still answering |
| `robust` | #81, harden GOOSE, Modbus, DNP3 and IEC 104 servers | malformed Modbus frames dropped, same pid, normal read still answered (4 cases); IEC 104 masters and DNP3 accept cases ran |
| `logctl` | #80, toggle file logging and level on a running device | 12 checks: rotation after 31 s at the 20 480 B cap, idempotent on/off, level and destination reported |
| `watcher` | #82, stop logging `pid = N` on every poll | watcher stderr 0 B/min on both RTUs |
| `arm64deb` | #78, native arm64 debs | package `Architecture: arm64`, no relabel; log scan reported a flood warning on rtu-1 and rtu-2 (0 crash-class, 0 error-class lines), so that board's verdict is WARN, not PASS |

Limits: each board is one run on one host; none exercises ferrofire's own field device, which is tested by the harnesses above.

## Known gaps in one list

1. ICCP/TASE.2 object layouts are from recall, checked against two young peers (igs, telcon) and two real captures but not the standard; the QTimeTag member order is disputed between the peers (ww-otj.69).
2. IEC 61850 server rests on a young single-author library; one independent peer stack.
3. No amd64 testpeers build exercised (the publish workflow exists, no tag has been pushed).
4. Fuzzing needs a nightly toolchain, and the CI run is 10 s per target.
5. No loss measured on a real network; netem loss is uniform.
6. Waterway physics golden comes from ferrofire's own earlier code; PYPOWER is the only power-flow reference.
7. Snapshot tests pin output format, not correctness.
8. Mutation results do not yet cover the field-device modules in the recorded numbers.
9. The drop-in pypower and stock conformance scripts need the Lima VM and are run by hand.
