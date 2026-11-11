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
| ICCP / TASE.2 server | `mms_utility` browse and read; our thin ICCP client; tshark `mms`; fuzz | **None for TASE.2.** libiec61850 and tshark cover the MMS layer only | Weakest: object layouts are from recall |
| ICCP parsers (Zeek, Wireshark) | btest baselines and field goldens over `tests/pcaps/iccp` | tshark's own MMS dissector underneath; no independent ICCP capture | Tested on our own and synthetic captures |
| MQTT client + Sparkplug B | Unit tests of the engine and the payload codec; integration tests against a real Mosquitto child process; conformance with mosquitto_pub/sub, Eclipse Paho, an Eclipse Tahu host, tshark and Zeek; fuzz of the NCMD decoder | Mosquitto (broker), Paho and mosquitto clients (MQTT), Tahu (Sparkplug payload codec), tshark `mqtt`, Zeek `mqtt_*` logs. No stock oracle: bennu has no MQTT | Covered for the subset in [field-device-mqtt.md](field-device-mqtt.md); Sparkplug host behaviour is only as good as one Tahu run |
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
a hostile-bytes test, fuzz target `fd_iccp`. `conformance/iccp/`: libiec61850's `mms_utility` (domains, identity, directory, three
reads of named variables) and the thin client (browse, read, write, data set, transfer set, three reports) run against our
server; tshark must find 0 malformed frames and no expert warnings, Initiate, GetNameList, Read, Write,
DefineNamedVariableList and InformationReport present, ICCP names in the MMS fields.

**Oracle.** None for the TASE.2 layer. What is independent: `mms_utility` proves the OSI and MMS layers against a third party;
tshark's MMS dissector decodes every frame. What is not: the object layouts (Data_Real, State and Discrete with Q and QTimeTag,
`DSTransfer_Set`, report item order, `Supported_Features`), which the server, the thin client, the Zeek analyzer and the
Wireshark script all take from the same recall of the standard.

**Run.** `cargo test`; `make conformance PROTO=iccp` (needs `make testpeers` and a built ferrofire plus the example, `make test`);
`cargo +nightly fuzz run fd_iccp` in `fuzz/`.

**Gaps.**
- IEC 60870-6-503 and -802 are paywalled and not in the repo. The layouts are from recall of public implementations and
  documentation. Only the validity and current-source codes and their place in the quality octet were corroborated, by one
  vendor document (Ipesoft D2000). Everything else is unverified ([field-device-iccp.md](field-device-iccp.md)).
- Our client, server and parsers share the same assumptions, so they agree with each other whether or not the assumptions are
  right. A pass means "consistent with our reading".
- Conformance blocks 1 and 2 are reported; blocks 3 to 9 are not built. Operator request, buffering and critical reports are
  refused. No TLS (62351-3) or 62351-4.
- The peer is identified by remote IP; the ACSE AP-title is not checked, because the pinned crate does not expose it.

**Ideas.**
1. Buy the standard (IEC 60870-6-503 for the services and protocol, -802 for the object models) and check every layout line by
   line against `model.rs` and `transfer.rs`. This closes most of the gap on its own.
2. Public captures. Searched 2026-11-10: no ICCP or TASE.2 capture was found. Wireshark's wiki has an MMS capture,
   [`mms.pcap.gz`](https://wiki.wireshark.org/SampleCaptures) (plain MMS; no licence stated on the page; not checked whether it
   holds TASE.2 names). The community collections [automayt/ICS-pcap](https://github.com/automayt/ICS-pcap) (no licence in the
   repo, last push 2020) and [tjcruz-dei/ICS_PCAPS](https://github.com/tjcruz-dei/ICS_PCAPS) (no licence) were not found to
   list ICCP; the file lists were not read in full. A 2013 Wireshark Q&A thread
   ([ask.wireshark.org/questions/19908](https://ask.wireshark.org/questions/19908/dissector-for-iccptase2)) reports TASE.2 showing
   as malformed MMS, which means someone had a capture; asking there or on the Wireshark and Zeek lists for one is cheap.
3. Open implementations, as a second reading of the standard (not an oracle, but independent of our assumptions):
   [FreeTase2](https://github.com/aklira/FreeTase2) (Python, built on libiec61850, GPL-3.0, last push April 2020, has a `tase2`
   package and tests; its README lists blocks 1, 2 and 4), and
   [pyiec61850-ng](https://github.com/f0rw4rd/pyiec61850-ng) (GPL-3.0; its README calls its TASE.2 support "highly
   experimental"). Both come from the same libiec61850 base and from someone else's recall. Run FreeTase2's client against our
   server and compare its object layouts with ours; where they differ, the standard decides. GPL: test peer only, as with
   libiec61850.
4. Vendor trial stacks: Triangle MicroWorks' IEC 60870-6 (TASE.2/ICCP) source library
   ([product pages](https://files.trianglemicroworks.com/products/source-code-libraries/iec-60870-6); states blocks 1, 2 and 5,
   data sets, transfer sets, SBO control); SISCO MMS-EASE and AX-S4 ICCP; other commercial ICCP gateways (for example
   Ipesoft D2000's ICCP driver, Elipse's ICCP driver). Evaluation licences and terms were not checked; ask each vendor. A trial
   client talking to our server is the closest thing to an independent peer.
5. OpenMUC (Java; includes an open MMS client and server library, licence to be checked per module) as a generic MMS peer
   with a TASE.2 layer written on top; weaker than a real ICCP stack.
6. Interop events and utility partners: an EMS vendor or a utility with an ICCP link can capture one Initiate and one
   InformationReport from a real link; diff against `conformance/iccp/`. That diff is the first thing to do with any real peer.

## ICCP parsers (Zeek and Wireshark)

**Checked.** `tools/zeek/iccp` (Spicy analyzer, `mms.log` and `iccp.log`) and `tools/wireshark/iccp.lua` (post-dissector over
tshark's MMS) are run over the same corpus, `tests/pcaps/iccp`: libiec61850 server and client captures (taken in a container by
`capture-iec61850.sh`; the pcaps are data), `tase2_assoc.pcap`, `tase2_report.pcap` (including a COTP-segmented report),
`malformed.pcap`, and `fuzz.pcap` (150 seeded mutants), generated by `make_pcaps.py`. Zeek: btest baselines (`tools/zeek/iccp/tests/Baseline`);
a message that fails to parse is dropped on its own and the next one on the connection still parses. Wireshark: field goldens in
`tests/pcaps/iccp/expected`, 0 malformed.

**Oracle.** tshark's built-in MMS decoder for the layers below TASE.2. For TASE.2 itself, none: the `tase2_*.pcap` files were
written by `make_pcaps.py` from the same layouts the parsers implement. The libiec61850 captures are real MMS but contain no ICCP.

**Run.** `make zeek-iccp` (builds in the `ff-zeek` container; `ZEEK_ICCP_FLAGS=--update` rewrites baselines, read the diff);
`make wireshark` (host tshark; also runs the datagram test vector).

**Gaps.**
- Same as ICCP above: the parsers and the synthetic captures share the server's assumptions; a green run says they agree.
- Not decoded: alternate access, `typeDescription` of `getVariableAccessAttributes`, BER indefinite length, high tag numbers,
  more than the first presentation data value of a PPDU. Parse failures inside a message are silent (no event).
- Not tried in a running Malcolm; no live range run with a Zeek sensor on tcp/102.

**Ideas.** Any real ICCP capture (see above) is the test: run both parsers over it, and every unparsed or mislabelled field is a
bug in a layout. Compare the Wireshark script's output with a vendor tool's decode of the same capture.

## MQTT client and Sparkplug B

`<mqtt-client>` ([field-device-mqtt.md](field-device-mqtt.md)); engine and codec in `src/fd/proto/mqtt/`.

**Strategy.** The module is split so that most of it needs no socket: `engine.rs` (what to publish when, command validation),
`sparkplug.rs` (payload encode and decode) and the config parser are unit tested; `tests/mqtt_net.rs` runs the real client against
a real Mosquitto started as a child process (`mosquitto` and `openssl` are in the `ff-rust` image (an image built before ww-otj.56 must be rebuilt: `docker build -t ff-rust:22.04 docker`, because `make image` only builds when the image is missing), so it runs under `make test` and
under the mutator); `make conformance PROTO=mqtt` runs two `<mqtt-client>` elements (plain and Sparkplug) on one device against
Mosquitto inside the `ferrofire-testpeers` image with the peers below.

**Checked, and by what.**
- Publish on change and every refresh, retained values for a late subscriber, `status` online / offline: unit tests, `mqtt_net.rs`,
  `mosquitto_sub` and Paho in the conformance run.
- Commands: `.../set` and NCMD writes reach the store's write queue only if the point is writable, the type fits, the number is finite
  and within the limits; an unknown tag is logged and skipped (unit, `mqtt_net.rs`, fuzz `fd_mqtt`, and in the conformance run Paho and
  `mosquitto_pub` commands reach a stand-in provider while an out-of-range one does not).
- Will and goodbye: a clean stop publishes `offline` / NDEATH (`mqtt_net.rs`); a dropped connection makes the broker send the will
  (`mqtt_net.rs` aborts the task; the conformance run `kill -9`s the device and reads both wills).
- Bounded queue: against a broker that accepts the connection and then acknowledges nothing, the queue fills, the rest of the tick is dropped and the
  log hears of it once (`mqtt_net.rs`); a `<queue>` too small for one birth is refused at load.
- Reconnect: the client starts before the broker, and the broker is killed and restarted under it (`mqtt_net.rs`); the backoff
  arithmetic is unit tested. The scan cycle is not involved: the module only reads the datastore.
- TLS 1.2/1.3 with rustls, a client certificate and a password: `mqtt_net.rs` with a throwaway CA made by `openssl`; a wrong password
  never gets a session.
- Sparkplug B: NBIRTH / NDATA / NDEATH shape, `bdSeq` agreement between will and birth, `seq` wrapping, aliases, Rebirth NCMD, NCMD
  writes by name and alias: unit tests and `mqtt_net.rs`; in the conformance run an **Eclipse Tahu 1.0.21** payload decoder (our small
  `TahuHost.java`, jars from Maven Central, EPL-2.0, test peer only) decodes our NBIRTH and NDATA, its Rebirth NCMD produces a new
  NBIRTH and its NCMD write reaches the provider.
- Wire: tshark finds 152 MQTT packets and 0 malformed, `expert,warn` empty; Zeek's built-in analyzer writes `mqtt_connect.log`
  (client ids, will topic), `mqtt_subscribe.log` and `mqtt_publish.log` rows for our topics.
- Fuzz: `fd_mqtt` feeds arbitrary bytes as an NCMD and as a plain command through `Engine::message` and the decoder; invariants: no panic,
  a read-only point is never queued, a queued analog is within its limits.

**Oracles.** Independent: Mosquitto (broker semantics, retained messages, wills), Paho and the mosquitto clients (a client not written by
us reading and commanding), Tahu (the Sparkplug payload codec), tshark and Zeek (the MQTT framing). Not independent: the engine tests
and `mqtt_net.rs` use `rumqttc` on both sides.

**Gaps.**
- No stock oracle and no second broker: Mosquitto only (no EMQX / HiveMQ / VerneMQ).
- Tahu is used as a codec and a thin host; no Ignition or other commercial Sparkplug host has been tried, and the primary-host `STATE`
  topic, DBIRTH / DDATA and Sparkplug metric types other than Int64 / Double / Boolean on output are not implemented.
- MQTT 3.1.1 only (no MQTT 5 properties); QoS 2 is passed through to the library and not specifically tested.
- `ssl://` needs `<tls-ca>`: the system trust store is not used. TLS is tested against Mosquitto only, with RSA certificates.
- A tag name containing `/` cannot be commanded in the plain scheme.
- Retained messages on a command topic are ignored, so a command cannot be made persistent through the broker either.
- Network faults between device and broker (loss, delay) are not injected; only a refused connection, a restart and a kill.

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

1. ICCP/TASE.2 object layouts from recall; no independent ICCP peer or capture; client, server and parsers share assumptions.
2. IEC 61850 server rests on a young single-author library; one independent peer stack.
3. No amd64 testpeers build exercised (the publish workflow exists, no tag has been pushed).
4. Fuzzing needs a nightly toolchain, and the CI run is 10 s per target.
5. No loss measured on a real network; netem loss is uniform.
6. Waterway physics golden comes from ferrofire's own earlier code; PYPOWER is the only power-flow reference.
7. Snapshot tests pin output format, not correctness.
8. Mutation results do not yet cover the field-device modules in the recorded numbers.
9. MQTT is checked against Mosquitto, Paho and Tahu only; see the MQTT section for what a commercial Sparkplug host might still dislike.
10. The drop-in pypower and stock conformance scripts need the Lima VM and are run by hand.
