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
| MQTT client + Sparkplug B | Unit tests of the engine and the payload codec; integration tests against a real Mosquitto child process; conformance with mosquitto_pub/sub, Eclipse Paho, an Eclipse Tahu host, tshark and Zeek; fuzz of the NCMD decoder | Mosquitto (broker), Paho and mosquitto clients (MQTT), Tahu (Sparkplug payload codec), tshark `mqtt`, Zeek `mqtt_*` logs. No stock oracle: bennu has no MQTT | Covered for the subset in [field-device-mqtt.md](field-device-mqtt.md); Sparkplug host behaviour is only as good as one Tahu run |
| SCPI instrument server (raw socket, HiSLIP, VXI-11) + `ferrofire scpi` client | Unit tests of the syntax, the status model, the routes, the HiSLIP state machine and the XDR/RPC service; a transcript golden (`tests/golden/scpi.transcript`); socket tests per transport (`scpi_net`, `scpi_hislip`, `scpi_vxi11`); the client binary against all three; conformance with PyVISA (socket, HiSLIP, VXI-11), lxi-tools (raw and VXI-11) and our client; fuzz of the line buffer and parser, the HiSLIP header and state machine, the RPC record reader and XDR decoder | PyVISA 1.16.2 + pyvisa-py 0.8.1 and lxi-tools 2.8-1 (independent clients, and independent HiSLIP and ONC RPC implementations), tshark (its HiSLIP, portmap and VXI-11 dissectors judge the framing) and Zeek as observers. No stock oracle: bennu has no SCPI, and the expected texts are our reading of SCPI-99 and IEEE 488.2 | Covered for the subset in [field-device-scpi.md](field-device-scpi.md); the standards are not independently executable, so the transcript is consistency with our reading, plus what two client stacks accept |
| Ground truth to Elasticsearch/OpenSearch (`ferrofire groundtruth`, the provider `[elastic]` export) | Unit tests of every pure piece; a fake-server test file; real Elasticsearch 8, OpenSearch 2 and a secured Elasticsearch 8 in containers; a field-by-field differential against the stock `groundtruth_monitor.py`; the SCORCH `providerdata` check | Stock pybennu (monitor, mapping), the servers themselves, the SCORCH component's own code | Covered for the documented subset; see [ground-truth-elastic.md](ground-truth-elastic.md); the stock side runs with two shims (below) |
| Whole system | Burger range boards, live range runs | Stock bennu, phēnix, OVS | Covers stock bennu fixes and ferrofire runs; see below |

Everything below is in the repo and runs from `make`. Make targets that need docker run in the `ff-rust:22.04` container or
in the `ferrofire-testpeers` image (`make testpeers`, about 10 minutes the first time, longer with the two Rust peers).

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
- **dnp3py 0.6.0** ([craigpnnl/dnp3py](https://github.com/craigpnnl/dnp3py), PNNL, MIT, pure Python, 2026), a master written independently
  of opendnp3 (ww-otj.55): `conformance/dnp3/dnp3py_master.py` does an integrity poll, a class poll, a direct operate (g41 idx 2), a
  select + operate (g41 idx 1) and a direct CROB (g12 idx 5), and asserts the point sets it decoded (binary inputs 2 and 5, analog inputs
  0 to 2). It exposes no control statuses, so `check.sh` reads them off the wire: four responses, all status 0 (SUCCESS). Result: it
  passes against the stock device and against ours, with tshark expert info clean. No disagreement found.
- Third-party captures (`tests/pcaps/public/fetch.sh`, ITI/ICS-Security-Tools, 188 DNP3 files): `check-parsers.sh` runs tshark and Zeek
  icsnpp-dnp3 over them. Baseline: 214 captures (DNP3 plus MMS and GOOSE), 4388 packets, 7 malformed (all in files named or built to be
  malformed: `direct_operate_crob_malform_but_good_crc`, `write_vto_empty_string`, `dataset_exchange`, two opendnp3 conformance dumps), 33
  expert rows; Zeek: 0 failures, 3 weird.log rows over 188 files. Parser parity only: it says nothing about our outstation.
- Findings recorded as tests (things the stock device does that the spec did not say): two-octet ranges whenever a type has a
  point above index 255; events before static data, in the order they happened; CON on every non-final fragment; NEED_TIME 30 s
  after a time write; a count qualifier on static data is NO_FUNC_CODE_SUPPORT; a request with any unknown object is refused whole.

**Oracle.** The stock bennu outstation (byte for byte), opendnp3 3.1.2, dnp3py 0.6.0 (a second, unrelated master), tshark, Zeek with CISA icsnpp-dnp3.

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
conformance test procedures; whether a free tool exists was not checked). Build and run the testpeers image on amd64. More
independent masters, found by the ww-otj.55 search and not yet run: [otfabric/dnp3-interop](https://github.com/otfabric/dnp3-interop)
(MIT; ghcr.io images of five stacks with a JSON-lines contract: opendnp3, DSC go-dnp3, dnp3py, cB-Sigma dnp3, nfm-dnp3; arm64
manifests unchecked), [cB-Sigma/dnp3](https://github.com/cB-Sigma/dnp3) (Apache-2.0, Go, Secure Authentication v5, file transfer),
[otfabric/go-dnp3](https://github.com/otfabric/go-dnp3) (MIT; possibly the same author family as cB-Sigma, so not two opinions) and
[dscsystems/go-dnp3](https://github.com/dscsystems/go-dnp3) (GPL-3.0, experimental). All are 2026 single-author projects, several
with a CLAUDE.md: they add diversity, not authority. Secure Authentication and file transfer are what they would exercise and the
stock goldens cannot.

## IEC 61850 (MMS and GOOSE)

**Checked.** `tests/iec61850_net.rs` (in-process and socket clients), `tests/iec61850_robust.rs` (mutated transcripts),
`tests/iec61850_accept.rs` (`accept()` failure), fuzz target `fd_iec61850` (bytes through the OSI stack and the ACSI server).
`conformance/iec61850/`: libiec61850's unmodified example clients (read, control, reporting) and GOOSE subscriber run against
our server on `lo`; `check.sh` asserts tshark's `mms` Initiate, GetNameList, Read, Write, InformationReport and `goose` fields
(appid, gocbRef, data set, entries) and what the peers printed. Values are not asserted, because the device's inputs come from
whatever publishes on the multicast group. 0 malformed frames and no expert warnings.

Two further clients, independent of libiec61850 and of `iec61850-rs` (ww-otj.55), run in the same capture:
- **iec61850bean 1.9.0** ([beanit/iec61850bean](https://github.com/beanit/iec61850bean), Apache-2.0, Java, own ASN.1 codec, Maven Central,
  sha256-pinned in the Dockerfile; last commit 2023-06, stable). `ConsoleClient` with `m` and `ga`: association, GetNameList and
  GetVariableAccessAttributes for the whole model, the `LLN0.Events` data set, and a read of all data. `check.sh` fails on a missing model,
  data set, quality fields, an incomplete `ga`, or any exception. Result: no disagreement.
- **otfabric/go-iec61850 v1.16.2** ([otfabric/go-iec61850](https://github.com/otfabric/go-iec61850), MIT, Go; `docker/testpeers/goiec/` is our
  ~100-line client over it): browse of logical devices, nodes and data objects, the data set (12 members), the GoCB read back
  (GoEna true, confRev 1), a typed read of `AnIn1.mag.f[MX]`, and a GOOSE subscriber on `lo` that decodes gocbRef, datSet, appid 1000,
  confRev and 12 entries. Result: no disagreement. Cosmetic: its browse lists the `LLN0` control blocks as data objects.

**Oracle.** libiec61850 v1.6.2 (GPL-3.0, built and run only as test peers, never linked), iec61850bean 1.9.0, go-iec61850 v1.16.2, tshark. No stock oracle: bennu never
loads an IEC 61850 server. Zeek has no MMS or GOOSE parser, so that step prints SKIP.

**Run.** `cargo test`; `make testpeers` (rebuild after the Dockerfile changed: images built before 2026-11-10 lack the control
client and GOOSE subscriber); `make conformance PROTO=iec61850`; `cargo +nightly fuzz run fd_iec61850` in `fuzz/`.

**Gaps.**
- The server is built on `iec61850-rs` 0.4.0, a pre-1.0 crate from a single author (first release 2026-09-05; every minor
  version breaks; pinned with `=0.4.0`). Its claims of libiec61850 interop are its own; ours is one stack, one version.
- Three clients now (libiec61850, bean, go-iec61850), but none of them is a server, and none was run against a differently-configured model
  (all use libiec61850's `simpleIOGenericIO` names). The Go stack's author cross-tests it against libiec61850 and bean, so its agreement
  is partly theirs. An agreement between `iec61850-rs` and ferrofire's use of it is not independent of the library.
- GOOSE: libiec61850's `goose_subscriber_example` receives nothing on `lo` (ww-otj.72). Cause found: its Linux receiver attaches the kernel
  filter `ether proto 0x88b8` (offset 12), which drops VLAN priority-tagged frames when the tag is inline, as on `lo` and virtual links; a
  NIC with VLAN offload strips the tag first. Our frames are tagged (VID 0, priority 4, as the standard recommends) and so are libiec61850's
  own publisher's: its publisher and subscriber examples do not hear each other on `lo` either. Not a ferrofire fault; the step is
  replaced by go-iec61850's subscriber, which decodes the tagged stream. The subscriber binary stays in the image. libiec61850's filter
  also does not deliver frames to `01-0C-CD-01-00-00` (observed on `lo`), so the default destination is `...-00-01`. No test on a physical L2 segment. No subscriber in ferrofire. No TLS (62351-3), 62351-6
  authentication or R-GOOSE.
- Values and control semantics (select-before-operate state machines) are exercised, not asserted against a reference.

**Ideas.** Differential fuzzing: feed the same mutated MMS PDUs to libiec61850's server and to ours and compare the response
class (reply, error, reject, close); a crash or a hang on either is a finding, and so is any disagreement worth reading.
Run more stacks against ours: [otfabric/iec61850-interop](https://github.com/otfabric/iec61850-interop) (MIT; pins libiec61850, bean,
dscsystems/go-iec61850, csp0924/iec61850-rust and cB-Sigma in ghcr.io images behind one JSON-lines adapter contract; arm64 manifests
unchecked), [csp0924/iec61850-rust](https://github.com/csp0924/iec61850-rust) (MIT or Apache-2.0, own COTP to ISO 9506 stack, a PICS, nine
fuzz targets; check it shares no code with `iec61850-rs`), [dscsystems/go-iec61850](https://github.com/dscsystems/go-iec61850) (GPL-3.0, a
terminal client `iedx`, 62351-3/-4) and a vendor client such as Triangle MicroWorks' test harness. Servers for the reverse direction
(our client against theirs): bean's `ConsoleServer`, `ied-server` of go-iec61850 and cB-Sigma's `iedsim`, all SCL-driven. Run an IED simulator's SCL through our SCL loader. Attend an interoperability event (IEC 61850 UCA International Users
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
| telcon `read --kind real_q_time_tag` / `state_q_time_tag` | telcon | telcon decodes the QTimeTag structures as {value, flags, time}; ours and igs's are {time, value, flags}; no real capture holds one | ww-otj.69 |
| telcon `transfer-set` against our server; our `arm` against telcon's server | telcon | telcon wants a 2-member `Transfer_Set_Name`; both real captures and ours have 3 (scope, domain, name) | ww-otj.70 |
| our GetNameList / variables against igs's server | igs | the server does not serve GetNameList (its docs) | peer limit, ww-c43 |
| our Identify, `Bilateral_Table_ID`, `TASE2_Version`, `Supported_Features`, data set members against telcon's server | telcon | the test daemon does not serve them (its own `blt-id` fails) | peer limit, ww-c43 |

Skipped with a reason: igs writing a `Data_State` (its CLI sends a boolean), telcon reading `TASE2_Version` / `Supported_Features` (no
command). igs `start` (interval and RBE) now passes: igs neither defines a data set nor reads `Next_DSTransfer_Set` and each CLI call is its own
association, so `conformance/iccp/server.xml` presets `PRESET_DS` and two transfer sets in the partner row (ww-otj.71). The Transfer_Set_Name
scope is `unsigned` on our side now (ww-otj.64; igstase2-server still refuses an unsigned scope in a *write*, DataAccessError 7, so our thin
client writes an integer; our server takes both). The 12- and 13-member DSTransferSet, Buffer_Time and data sets that list
`Transfer_Set_Name` / `DSConditions_Detected` with `variableListName` reports (ww-otj.65) are proved by two unit tests in
`src/fd/proto/iccp/mod.rs` that replay the real captures' client requests (mms.pcap frame 80, chaugan frames 81, 100 and 125, with the
captures' domain names) and assert the structure of the server's answers against the captures (frames 101 and 515: unsigned scope,
`variableListName`, one result per member). The captures themselves are not committed, so the requests are transcribed.
Conclude and release (ww-otj.73): ISO 9506 Conclude only agrees that the association may end; the ACSE release (RLRQ, ISO 8650) follows from the
concluding side and gets an RLRE, so the server no longer closes on the Conclude (the vendored `iec61850-rs` waits for the RLRQ, or the handshake
budget). igs also sends one more session SPDU (an abort with the transport-disconnect bit) after our RLRE, so `src/fd/mms.rs` closes with a FIN and
reads until the peer's FIN (`linger`, at most 1 s) instead of dropping the socket, which answered it with a reset. `proto.env` no longer allows
'Connection reset'; it still allows the Wireshark decode gaps ('zero-byte MMS PDU', 'OID not implemented' on the peers' RLRQ data SPDU).
`make conformance PROTO=iec61850` fails its GOOSE-subscriber step on the Lima daemon with the old binary and image too (ww-otj.72); the MMS steps pass.
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
- Conformance blocks 1 and 2 are reported; blocks 3 to 9 are not built. Operator request and critical reports are
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
- Wire: tshark finds 158 MQTT packets and 0 malformed, `expert,warn` empty; Zeek's built-in analyzer writes `mqtt_connect.log`
  (client ids, will topic), `mqtt_subscribe.log` and `mqtt_publish.log` rows for our topics.
- Mutation: `mutator --test-command 'cargo test --lib --test mqtt_net' src/fd/proto/mqtt` (not the whole `make mutate`): 101 sites, 101 killed, 0 survivors. The test command must not contain `&&`: the mutator does not run it through a shell, and the first run silently skipped the integration tests.
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

## SCPI instrument server and client

`<scpi-server>` and `ferrofire scpi` ([field-device-scpi.md](field-device-scpi.md)); `src/scpi/` (message syntax, the HiSLIP frame format and
the ONC RPC/XDR code, shared, and the client with its HiSLIP and VXI-11 wires in `transport.rs`) and `src/fd/proto/scpi/` (config and the raw
socket in `mod.rs`, the I/O-free instrument in `device.rs`, the HiSLIP and VXI-11 servers in `hislip.rs` and `vxi11.rs`).

**Strategy.** The syntax (`syntax.rs`) and the instrument (`device.rs`: line framing, status registers, error queue, routes) are pure and
need no socket. `tests/scpi_golden.rs` replays a transcript (`tests/golden/scpi.transcript`: request lines, exact responses, the write queue,
a new connection, a second session of the same instrument) with no sockets; `tests/scpi_net.rs` runs the raw socket on real sockets,
`tests/scpi_hislip.rs` and `tests/scpi_vxi11.rs` the other two (HiSLIP frames and ONC RPC records built by hand, so a wrong header is a
test and not a client's guess); `tests/scpi_client.rs` runs the real `ferrofire scpi` binary against all three; `make conformance
PROTO=scpi` runs the independent client stacks.

**Checked, and by what.**
- Syntax: units split at `;` outside quotes, headers (short and long forms in any case, suffix 1 only, rooted and relative paths, `*` commands
  leave the path alone), numbers (NR1 to NR3, signs, exponents, the three number errors), keywords, strings with doubled quotes: unit tests
  and the transcript.
- Status model: SESR class bits from the error number, clear on read and on `*CLS`; the status byte bits EAV, MAV (also the
  `*IDN?;*STB?` case), ESB, MSS and `*SRE` bit 6; the error queue's first-in first-out order, the 16-entry bound and -350 as the last entry:
  unit tests and the transcript, and through PyVISA in the conformance run (`*STB?` 36, `*ESR?` 16, `*STB?` 4, `SYST:ERR?` -222,
  `*STB?` 0).
- Writes: only a `<writable>` tag has a `SOURce` path; the type, finiteness and limits are `Point::check`, shared with MQTT; what passes is in
  the store's write queue and not in the store (unit test, golden, `scpi_net.rs`, and a stand-in provider receiving `WRITE=` in the conformance
  run); out-of-range is -222, a wrong type -104 or -224, `MIN`/`MAX`/`DEF` without a limit -221.
- Robustness: a line over `<max-line>` is -363 and discarded to its newline, with the buffer never holding more than the limit; random bytes
  and a 200 KB line leave the connection answering; the connection limit closes the extra client and serves the rest; an idle client is
  dropped and never holds up another; 300 connections of four kinds (in `scpi_net.rs`) and **2000** (in the conformance run, read from
  `/proc/<pid>/fd` of the device: 5 before, 5 after) leave no descriptor; a bind failure is logged and the rest of the device runs; stopping
  closes the clients. Fuzz target `fd_scpi` (60 s locally, 800,000 runs, no finding; 10 s per target in `make fuzz-smoke`): no panic, buffer and
  queue bounds, response bound.
- Status scope and `*RST`: `instrument` (default) shares one error queue and one set of registers between sessions of one transport (golden
  `@ session`), between transports (raw socket and HiSLIP, raw and VXI-11, in the socket tests; HiSLIP, VXI-11 and the raw socket
  through PyVISA in the conformance run) and `connection` keeps them apart; `*RST` queues the defaults of the writable tags and leaves
  SESE, SRE, SESR and the queue as they were (unit test and golden).
- HiSLIP: header cutting from one byte at a time and from garbage (a bad prologue and an oversized payload are FatalError 1); the
  handshake of both channels over sockets; data in several frames, responses cut to the client's maximum message size; **-410** exact
  (a message started with RMT-delivered clear over an unread response, `Interrupted` sent, the new message still run) and **-420**
  exact (a message without its END abandoned by another message or by the channel closing; a command in it dropped silently); device
  clear (async clear, sync complete, dropped messages in between, no error raised); the one lock (grant, timeout, release, a waiting
  request, a waiting `Data`, a lock dying with its session); status query with and without RMT-delivered; the fatal errors and the
  non-fatal one. Fuzz target `fd_hislip`.
- VXI-11: XDR opaque padding and bounds, record fragments, the portmapper (GETPORT for TCP and not UDP, DUMP, SET, rpcbind v3/v4 mismatch,
  UDP), every RPC error class, create_link (device name, lock, limit), write/read with every read reason (size, termination
  character, end, both at once), **-410** and **-420** exact from the write/read order, readstb with MAV, clear, the refused calls, links
  dying with their connection. Fuzz target `fd_vxi11`.
- Client: the resource strings (every form, case, board number, bad ones), the query rule, the `SYST:ERR?` loop and its limit, the timeout and the closed-connection messages, target parsing, scripts, the
  REPL loop (unit tests with in-memory streams); the exit statuses, files, stdin, `--json` and `--check` against the real server
  (`scpi_client.rs`); insta snapshots of the text and JSON formats (`tests/snapshots.rs`).
- Wire (conformance run): 15,804 packets on port 5025, at least 2000 handshakes, 0 malformed, `expert,warn` empty after allowing connection
  resets and the window closing during the 200 KB flood (flow control; `proto.env` says why); the requests and the responses are found as
  bytes in the capture (`*IDN?` and its answer, the -222 and -363 responses, the `;`-joined answer of a compound query); Zeek's `conn.log` has
  the connections (no analyzer exists).
- Mutation, scoped to the new code (the whole-tree `make mutate` relinks every test binary per mutant, which took over 3 minutes a
  mutant here): `mutator --max-workers 2 --no-coverage --test-command 'cargo test --lib --test scpi_golden --test scpi_net' src/fd/proto/scpi
  src/fd/proto/point.rs` and the same with `--test-command 'cargo test --lib --test scpi_client --test snapshots' src/scpi`: **252 sites, 252 killed,
  0 survived** (point.rs 16, scpi/mod.rs 31, device.rs 107, syntax.rs 68, client.rs 30). The first run left eight survivors in `device.rs`: three
  were untested initial values and a boundary (tests added), one a `(min, max)` arity that was always equal (now one number), two a guard
  that no input reaches (removed), one a filter that changed nothing (removed). Four in `client.rs` were a missing test for a failure that
  is not a timeout under `--check`, and for the socket options (tests added); a fifth (`let mut ok = true` in the REPL) was a false survivor,
  killed when run alone (a rewrite within the same mtime tick can leave cargo with the previous build: touch the sources after a mutation run).

**Oracles.** Independent: PyVISA with pyvisa-py (a client written by others, reading values and statuses), lxi-tools (a second client), tshark
and Zeek (the TCP). Not independent: the transcript and the unit tests are our reading of SCPI-99 and IEEE 488.2 (the standards are
not freely executable); `scpi_net.rs` and `scpi_client.rs` use our own client on both sides.

**Gaps.**
- No stock oracle and no real instrument: the expected texts follow the standards as we read them, and where they leave room (short
  forms of tag mnemonics, units, what `*RST` does, per-connection status) docs/field-device-scpi.md says what was chosen. A real
  instrument's behaviour on a rejected query (no response, or an empty line) may differ.
- On the raw socket -410 and -420 are still reached by an output cap and by a half-closed unterminated query; the genuine 488.2 conditions
  belong to a handshake TCP does not have. HiSLIP and VXI-11 have it and test the real cases (the RMT-delivered rule is our reading of
  IVI-6.1, checked against pyvisa-py, which sets the bit after every response it reads, not against a real instrument).
- No SRQ (HiSLIP `AsyncServiceRequest`, VXI-11 interrupt channel), no overlapped HiSLIP, no VXI-11 locks, no mDNS or portmapper
  `CALLIT` (field-device-scpi.md lists them; `lxi discover`'s VXI-11 broadcast works, checked in the conformance run). No real instrument, no NI-VISA or Keysight IO Libraries.
- No units, hexadecimal or block data, no `STATus`/`OPERation` subsystems (status byte bits 3 and 7 read 0), no `*PSC`, `*DDT` or `*RCL`
  (the save/recall commands) or other optional common commands (they are -113).
- Mutation runs covered only the new code, not the whole tree (`make mutate` exceeds the Docker VM on a loaded host).
- IPv6 is untested (a `[::1]` target is parsed, not connected to).

## Ground truth to Elasticsearch and OpenSearch

What it is: [ground-truth-elastic.md](ground-truth-elastic.md). Beads: ww-1df.19 (monitor), ww-1df.20 (provider export).

**Checked.**
- Pure pieces, as unit tests in `src/elastic/*.rs` and `src/groundtruth.rs`: UTC date arithmetic (leap day, the millisecond either side of midnight),
  index names, the config parse (keys, defaults, errors), the bulk body (NDJSON), reading a bulk response (which items to resend, which are refused),
  backoff (500 ms doubling to 30 s), the bounded queue (drop-oldest and its counter, a stuck consumer never blocks the producer, close and drain),
  the monitor's filter (substring, loose entries match everything), tag split, value-or-status, the alert/batch limiter, the provider's time fields
  (`simulator_time`, `time_drift`), the frequency source, the template's fields and types, and the HTTP/1.1 framing (URL, request bytes, `Content-Length`,
  chunked and close-delimited responses, size limits, an unclean TLS close).
- `tests/elastic.rs`, a fake Elasticsearch on a local port: one authenticated bulk request (basic and API key), the template put once before any
  document with three workers, retry with backoff on 503, a bulk response with a 201, a 429 and a 400 (only the 429 is resent; the 400 is counted
  as refused), a dead server with 20 000 items pushed in well under 2 s and the oldest dropped, and the binaries end to end: `ferrofire run --config` with
  the pybennu fields on every document, no export without `enabled = true`, `ferrofire groundtruth` with a filter and the stock document shape, the
  alert limit skipping only alert points, and bad invocations.
- `make conformance PROTO=elastic` (`conformance/elastic/`): real servers, brought up by `run.sh` on the Lima docker daemon, `ff-conf-*` names, no bind mounts.
  - Per server (Elasticsearch 8.19.3 with security off, OpenSearch 2, Elasticsearch 8.19.3 with TLS and basic auth, the same with an API key): the
    provider export (template installed, daily index mapping, fields, `measurement.frequency` on every document, `time_drift` in range, every tag, a
    binary tag typed bool) and `ferrofire groundtruth` on the same stream (fields, no trailer, UTC `@timestamp`, `bennu-<UTC day>`), both
    stopping cleanly on SIGTERM.
  - Differential against the **stock `groundtruth_monitor.py`**, unmodified, on Elasticsearch 8 (the stock script's `elasticsearch-py` 8 client refuses OpenSearch):
    the provider publishes the same snapshots to two endpoints, one for each monitor, into separate indices; 605 data documents compared field by field
    (value, type, device, field, timestamps within 0.5 s). The intended differences are asserted next to it: the `_ff` trailer indexed by stock only,
    the alert/batch limit (stock keeps `gt.before` only, ferrofire all five points of the message), UTC (stock under `TZ=NZT-13` writes `@timestamp` 13 h ahead,
    ferrofire does not).
  - TLS and auth are enforced: no CA file, a wrong password and no credentials each index nothing and log the failure.
  - The SCORCH `providerdata` component (`conformance/elastic/scorch_check.py`, in a Python 3.12 container with the phenix_apps source copied in):
    `ProviderData.start()` itself, with `elasticsearch.verify`, `min_documents`, `min_new_documents` and `expected_frequency`, against each of the four
    servers; negative controls (a wrong frequency, too few documents, too few new documents, a stopped provider, nothing in the last 5 s) each fail
    with the component's own message.

**Oracle.** The stock script and `elastic.py` for behaviour and field names; Elasticsearch and OpenSearch for what a bulk request means (their answers, not our reading);
the SCORCH component's own code for the queries.

**Run.** `make test` (units, fake server); `make conformance PROTO=elastic` (needs the Lima VM, the images `opensearchproject/opensearch:2` and
`docker.elastic.co/elasticsearch/elasticsearch:8.19.3`, `bennu:arm64`, `target-linux/debug/ferrofire` from `make test`, and `SCORCH_SRC`, default
`~/git/sceptre-phenix-apps/src/python/phenix_apps`; about 6 minutes). Artefacts in `target/conformance/elastic/`: the differential report and sample documents.
`KEEP=1` leaves the containers up between runs.

**Shims on the stock side, and why.** The `bennu:arm64` image has a `_Endpoint.so` for the wrong architecture (it cannot be loaded) and a pyzmq built without
draft support (no DISH socket). `conformance/elastic/stock_monitor.py` therefore stands in Python for the three-function Endpoint wrapper (libstdc++'s `std::hash`
for the group, the same hash ferrofire implements) and for the `Subscriber` base class (a DISH socket on the wire is a UDP socket and RADIO framing). It also adds the
`scheme` and an integer port to the node dict the stock script gives the `elasticsearch-py` 8 client. The stock handler, filter, rate limit, document building and
bulk push run as shipped. What this does not prove: that the real DISH socket and the real wrapper behave as the shims do.

**Gaps.**
- Real OpenSearch has no stock-script leg (the stock client cannot talk to it); the OpenSearch checks are ferrofire's own plus the SCORCH queries over HTTP.
  The SCORCH component's own `elasticsearch-py` client is used only against the unsecured Elasticsearch; for the other three servers the component's code runs on a client
  that makes the same two calls (`indices.stats`, `search`) over HTTP, because the component's client cannot authenticate and refuses OpenSearch.
- The only Elasticsearch/OpenSearch versions run are 8.19.3 and the `opensearchproject/opensearch:2` tag of 2026-11-11; Elasticsearch 7 and OpenSearch 1 are untested.
- No test restarts or pauses a real server mid-run: retry, partial failure and drop-oldest are tested against the fake server only.
- TLS is tested with a throwaway CA and name checks against `essec`; certificate chains with intermediates, client certificates and proxies are not.
- Nothing was measured at a rate that fills the queue on a real server (the queue is exercised against a dead one).
- Malcolm's own OpenSearch with its dashboards was not used.

**Ideas.** Pause and unpause the container mid-run and count the documents; run both monitors against Malcolm's OpenSearch image
already on the Lima VM; replay a recorded datagram capture through both monitors.

## Independent peers and public captures (ww-otj.55)

Several of our tests are circular (our client against our server; ICCP layouts recalled once and shared by the server, the test client and
both parsers). ww-otj.55 searched GitHub, Maven, PyPI, crates.io and the literature (November 2026) for peers and captures that are not our
code and not libiec61850, then ran the ones that built. Dates, licences and activity below come from the GitHub and crates.io APIs on
2026-11-10; behaviour claims are from READMEs unless the table says we ran it. Smoke report: `/var/tmp/peer-smoke.md` (not committed).

**In the testpeers image and the harness now** (all separate processes, pinned, never in the Cargo graph):

| Peer | Licence | Protocol | Check | Result |
|---|---|---|---|---|
| iec61850bean 1.9.0 | Apache-2.0 | IEC 61850 MMS client | `conformance/iec61850` (6 assertions) | agrees |
| otfabric/go-iec61850 1.16.2 via `ffgoiec` | MIT | MMS client, GOOSE subscriber | `conformance/iec61850` (5 assertions) | agrees; replaces the libiec61850 GOOSE step (ww-otj.72) |
| dnp3py 0.6.0 | MIT | DNP3 master | `conformance/dnp3` (8 + 2 assertions) | agrees, against stock and against ours |
| igs-iec-tase2, telcon-tase2 | Apache-2.0; MIT OR Apache-2.0 | ICCP client and server | `conformance/iccp` (ww-otj.67; section above) | found F1 to F3 and F8, fixed; see below |

**ICCP smoke findings** (filed and tracked in the ICCP section, not repeated here): F1 and F7 nesting level and service-error access code,
ww-otj.61 (fixed); F2 12-bit `Supported_Features`, ww-otj.62 (fixed); F3 `TASE2_Version` 2000.8, ww-otj.63 (fixed); F4 Transfer_Set_Name
scope as `unsigned`, ww-otj.64 (fixed); F5 and F6 12- and 13-member `DSTransferSet`, Buffer_Time, data sets with `Transfer_Set_Name` members and
`variableListName` reports, ww-otj.65 (fixed); F8 calling AP-title, ww-otj.66 (fixed). Related: ww-otj.71 (preset data sets, fixed); open: ww-otj.69 (QTimeTag order), .70, .73.

**Gaps this closed.** DNP3 had one master family (opendnp3); IEC 61850 had one client stack (libiec61850) under a server built on one young
library; both now have independent clients that agree. ICCP's recalled layouts now meet two independent stacks and two real captures (the
flag-octet layout decodes about 190 000 real quality octets sensibly; the other disagreements are listed above).

**Public captures.**

| Capture | Source and terms | Contents | Used |
|---|---|---|---|
| `mms.pcap.gz` | [Wireshark wiki](https://wiki.wireshark.org/MMS); no terms stated | 2005 ICCP between two utilities: association, 12-bit `Supported_Features`, TASE2_Version 1996.8, 12-member DSTransferSet writes, 115 reports | ICCP fetch script (ww-otj.67), never committed |
| `iccp_with_switchover_anon.pcap` | [chaugan/wireshark-iccp-dissector](https://github.com/chaugan/wireshark-iccp-dissector), repo GPL-2.0; origin unconfirmed | 7972 frames, 9 associations, TASE2_Version 2000.8, 13-member transfer sets, 1895 `variableListName` reports | same |
| ITI/ICS-Security-Tools `pcaps/dnp3`, `pcaps/IEC61850` | [ITI/ICS-Security-Tools](https://github.com/ITI/ICS-Security-Tools), repo CC-BY-4.0; per-capture terms unstated | 188 DNP3 files (authentication, file transfer, restart, unsolicited, CROB, malformed), 21 single-service MMS PDUs (cancel, kill, takeControl, getDomainAttributes...), GOOSE, one MMS+GOOSE session | `tests/pcaps/public/fetch.sh` (pinned commit, sha256 per file) and `check-parsers.sh`; not committed |
| DNP3 Intrusion Detection Dataset | [Zenodo 7348493](https://zenodo.org/records/7348493), CC BY 4.0 | nine attack scenarios; benign traffic is opendnp3, so not an oracle | not used |
| Wireshark `test/captures` `dnp_coverage`, `iec104_coverage` | [wireshark/wireshark](https://github.com/wireshark/wireshark/tree/master/test/captures), GPL-2.0 repo, per-file terms unstated | 5 and 14 KB of dissector coverage | not used |
| [automayt/ICS-pcap](https://github.com/automayt/ICS-pcap) | no licence; git-lfs | S7, ENIP, FINS, GE-SRTP, BACnet, Modbus, IEC 104 and more | not used; the only source of FINS and GE-SRTP traffic |
| Datasets with DOIs (verified at Crossref / DataCite 2026-11-10; contents not opened) | [10.5281/zenodo.19704482](https://doi.org/10.5281/zenodo.19704482) IEC104-IDS-2025 (CC BY 4.0, real RTU and breaker); [10.21227/zb9s-yg93](https://doi.org/10.21227/zb9s-yg93) IEC 104 command abuse, raw pcaps; [10.21227/xxq7-sk20](https://doi.org/10.21227/xxq7-sk20) GOOSE and SV, raw pcaps; [10.5281/zenodo.20696376](https://doi.org/10.5281/zenodo.20696376) MMS supervisory control (normalized records, not raw pcap); [10.5281/zenodo.19969459](https://doi.org/10.5281/zenodo.19969459) GOOSEIDRS (CSV); [10.5281/zenodo.8375656](https://doi.org/10.5281/zenodo.8375656) Modbus+MMS MITM (xlsx tables only) | | not used |

Only the ITI `dnp3` and `IEC61850` folders are fetched; its other folders (S7, ENIP, Modbus, C37.118, BACnet...) are not. No public ICCP capture other than the
two above turned up. The Brno traffic generator paper and the other literature below state no data release.

**Remaining options, by protocol.** None of these has been run.

| Protocol | Candidate | Licence, language, activity | Independent of what we use? | Would check |
|---|---|---|---|---|
| ICCP | [cB-Sigma/iec61850-sim](https://github.com/cB-Sigma/iec61850-sim) `iccpsim` | Apache-2.0, Go, 2026-10, v0.1.0 | yes, own OSI stack | a third independent TASE.2 server; blocks 1 to 5 and 7 |
| ICCP | [gazzasaur/rusty-iccp](https://github.com/gazzasaur/rusty-iccp) | MIT, Rust, 2026-11, 0.9.3 | yes; initiator library only (igs recorded it does not serve) | association path, AP-title handling |
| ICCP | [riclolsen/json-scada](https://github.com/riclolsen/json-scada) ICCP drivers | GPL-3.0 drivers, closed Go library, prebuilt arm64 and amd64 binaries; needs MongoDB; very active | plausibly (native Go) but unverifiable | the best field-proven client: bilateral tables, DSTS, `*QTimeTagExtended`; licence of the binary unclear |
| ICCP | [boosali/iccp-sim](https://github.com/boosali/iccp-sim) | no licence, vendors telcon crates | no (telcon) | convenience only |
| ICCP | FreeTase2, fcovatti/iccp, fledge-south/north-tase2 | GPL-3.0 / GPL-3.0 / Apache-2.0 with commercial MZ libtase2 | **no**: all wrap libiec61850 or libtase2 | nothing independent (FreeTase2 is a Python wrapper over libiec61850, dormant since 2020) |
| ICCP | Triangle MicroWorks IEC 60870-6, MZ libtase2, SISCO, Ipesoft D2000 | commercial | yes | no free evaluation terms found |
| IEC 104 | [j60870](https://github.com/gythialy/j60870) (OpenMUC lineage) | GPL-3.0 (commercial too), Java, 2026-11 | yes (separate from lib60870) | APCI state machine, ASDU codec, interrogation, SBO; **ferrofire has no 104 server to test** |
| IEC 104 | [mz-automation/lib60870](https://github.com/mz-automation/lib60870) | GPL-3.0, C, 2026-08 | the reference stack; c104 and the Rust `lib60870` crate wrap it | same; primary oracle once a server exists |
| IEC 104 | [otfabric/go-iec104](https://github.com/otfabric/go-iec104) + `iec104ctl`; wendy512/iec104; cB-Sigma/iec60870 | MIT; Apache-2.0; Apache-2.0, Go | yes | scripted clients; go-iec104 CI cross-tests j60870 and lib60870 |
| DNP3 | dnp3-interop images, cB-Sigma/dnp3, go-dnp3 | see the DNP3 section | yes | Secure Authentication v5, file transfer |
| BACnet/IP | [bacnet-stack](https://github.com/bacnet-stack/bacnet-stack) apps (`bacwi`, `bacrp`, `bacrpm`, `bacwp`) | GPL-2.0 with GCC exception, C, v1.5.10 2026-10 | yes | Who-Is, ReadProperty(Multiple), WriteProperty; Tier 2, no BACnet server module exists yet |
| S7comm | [python-snap7](https://github.com/gijzelaerr/python-snap7) 3.x | MIT, Python, 2026-11-08 | yes | COTP, setup, ReadVar/WriteVar; Tier 2 |
| OPC UA | open62541 (MPL-2.0), OPC Foundation UA-.NETStandard, asyncua (LGPL-3.0), Eclipse Milo (EPL-2.0) | C, C#, Python, Java; all active | mutually independent | hello/ack handshake, secure channel, browse, subscriptions; Tier 2 |
| EtherNet/IP | [OpENer](https://github.com/EIPStackGroup/OpENer), pycomm3, cpppo | BSD-style, MIT, GPL-3/commercial | yes | ListIdentity, RegisterSession, CIP; Tier 2 |
| C37.118 | [pyPMU](https://github.com/iicsys/pypmu), openPDC | BSD-3, MIT | yes | CFG-2, data frames; ITI has four C37.118 pcaps |
| MQTT / Sparkplug | Mosquitto, Paho, Tahu | already in use (MQTT section) | yes | done |
| Modbus | libmodbus tests; mbpoll is libmodbus | LGPL-2.1, C | libmodbus: **not** independent of mbpoll | the server is being merged separately; its peers are in the Modbus section |
| MMS framing, GOOSE | [MatrixEditor/icspacket](https://github.com/MatrixEditor/icspacket) (GPL-3.0, Python, ASN.1 for COTP/ACSE/MMS); goosestalker, pygoose (Scapy) | 2026-08; 2021; 2025 | yes | raw PDU crafting for the TASE.2 and 61850 servers; malformed-PDU fuzz |

Excluded as wrappers of libiec61850: pyiec61850-ng, the PyPI `iec61850` package (a façade over a Rust workspace we did not trace),
OpenPLC61850, libiec61850 forks.

**Literature** (each DOI resolves at Crossref; the papers state what they say, we have not obtained the full texts of the paywalled ones):
- Ilgner, Cika, Stusek, *SCADA-Based Message Generator for Multi-Vendor Smart Grids: Distributed Integration and Verification of TASE.2*,
  Sensors 2021, [10.3390/s21206793](https://doi.org/10.3390/s21206793) (CC BY 4.0; extends the ICUMT 2020 paper
  [10.1109/icumt51630.2020.9222445](https://doi.org/10.1109/icumt51630.2020.9222445)). Message-flow figure of the join procedure and the
  DSTS negotiation order, to compare with `transfer.rs`. No byte layouts, no released code or captures.
- *Secure ICCP Final Report*, US DOE / OSTI 2018, [10.2172/1436849](https://doi.org/10.2172/1436849). The PDF was not retrievable; the
  secure-association profile (TLS, 62351) is the part to read if we add 62351-3.
- *Examination of ELCOM-90, TASE.1 and ICCP/TASE.2*, IEEE Trans. Power Delivery,
  [10.1109/61.584329](https://doi.org/10.1109/61.584329), and the Springer chapter *Telecontrol Standard IEC 60870-6 TASE.2 Globally
  Adopted*, [10.1007/978-3-7091-6421-1_5](https://doi.org/10.1007/978-3-7091-6421-1_5): conformance blocks and profile overviews (paywalled; not
  confirmed to hold object tables).
- GOOSE Secure dataset paper, Energies 2024, [10.3390/en17236098](https://doi.org/10.3390/en17236098).
None gives the byte layouts of `Bilateral_Table`, `Transfer_Set`, DS conditions or `Supported_Features`. Those still need the IEC 60870-6-503
and -802 text (ICCP section, idea 1).

**Still open.** ICCP layouts are not checked against the standard. No IEC 60870-5-104 server exists to test j60870 or lib60870 (file a bead when
one is planned). Nothing was run on amd64. The "remaining options" above are untried; every one is a young single-author project except the
bacnet-stack, open62541, OpENer, Mosquitto, python-snap7 and lib60870 group, which are the ones to prefer when the Tier 2 modules land.

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
2. IEC 61850 server rests on a young single-author library; three independent client stacks (libiec61850, bean, go-iec61850) agree with it, but no independent server has been used against our client side, and none has run a model other than libiec61850's example.
3. No amd64 testpeers build exercised (the new peers are multi-arch: bean is Java, go-iec61850 pure Go, dnp3py pure Python) (the publish workflow exists, no tag has been pushed).
4. Fuzzing needs a nightly toolchain, and the CI run is 10 s per target.
5. No loss measured on a real network; netem loss is uniform.
6. Waterway physics golden comes from ferrofire's own earlier code; PYPOWER is the only power-flow reference.
7. Snapshot tests pin output format, not correctness.
8. Mutation results do not yet cover the field-device modules in the recorded numbers.
9. MQTT is checked against Mosquitto, Paho and Tahu only; see the MQTT section for what a commercial Sparkplug host might still dislike.
10. The drop-in pypower and stock conformance scripts need the Lima VM and are run by hand.
11. SCPI is checked against two clients (PyVISA, lxi-tools), tshark's HiSLIP and VXI-11 dissectors and our reading of SCPI-99, IEEE 488.2 and IVI-6.1; no real instrument, no SRQ (see its section).
