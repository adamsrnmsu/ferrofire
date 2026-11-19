# Acceptance harness

Judges a field-device protocol the way a blue team sees it: capture the wire, then ask tshark and Zeek what they decoded.

    make testpeers                  # once: build the ferrofire-testpeers image (about 10 min; docker/testpeers/Dockerfile)
    make conformance PROTO=dnp3     # peers vs the stock oracle (ffc-stock-2026.11.03:22.04); exit 0 = PASS
    DUT=rust make conformance PROTO=dnp3   # the same peers and judgement against our binary (FF_BIN, default target-linux/debug/ferrofire)
    make conformance-selftest       # negative control: a damaged and an empty capture must be rejected

Output lands in `target/conformance/<PROTO>/`: the pcap, `peer.out`, `dut.out`, `zeek/*.log`.

## What it checks (every protocol)
1. The capture holds packets of the protocol (`PROTO_FILTER`); an empty capture fails.
2. `tshark -Y _ws.malformed` finds 0 frames, and `tshark -z expert,warn` has no Error or Warn rows. A bad checksum is only a Warn
   in Wireshark, so Warns count. `EXPERT_ALLOW` (regex, default `IIN Abnormality`) lists legitimate ones.
3. `check.sh`: field assertions (`want_field PCAP FILTER FIELD VALUE`) on what the peers sent and what came back.
4. `zeek.sh`: `zeek -r` with the CISA ICSNPP package, `want_zeek LOG COLUMN VALUE`. If the package is not in the image the step prints
   `SKIP`; `ZEEK_REQUIRED=1` turns that into a failure.

## Layout
    conformance/run.sh        host side: starts the DUT container (ff-conf-dut) and the peers (ff-conf-peers, same network namespace)
    conformance/inside.sh     peers side: tcpdump on lo, run peer.sh, then judge
    conformance/lib.sh        expert_clean, want_field, want_zeek
    conformance/<proto>/      proto.env (DUT config, start command, capture filter), peer.sh, check.sh, zeek.sh

Add a protocol: copy `conformance/dnp3/`, change the four files. Only `ff-conf-dut` and `ff-conf-peers` are created or removed (`CONF_NAME` changes the
prefix). A client DUT gets its stock server through `DUT_EXTRA` and `DUT_PRE` in `proto.env` (see `modbus-client/`).
Files go in and out with `docker cp` (no bind mounts: the Lima docker daemon cannot see every host path).

## Elasticsearch / OpenSearch
`make conformance PROTO=elastic` is not a protocol run: `run.sh` hands over to `conformance/elastic/run.sh`, which starts real Elasticsearch 8, OpenSearch 2 and a
secured Elasticsearch 8 and runs the provider export and `ferrofire groundtruth` against them, with the stock `groundtruth_monitor.py` as differential oracle and the
SCORCH `providerdata` check. See [../docs/ground-truth-elastic.md](../docs/ground-truth-elastic.md) and the "Ground truth" section of [../docs/testing.md](../docs/testing.md).

## Pointing it at another device
`DUT_IMAGE=<image> DUT_CMD='<command reading /tmp/cfg.xml>' conformance/run.sh dnp3` (the config is `CFG`, default the stock sample from
`BENNU_DATA`, default `~/git/sceptre-bennu/data/configs/ep`; for DNP3 it is `conformance/dnp3/server.xml`, because the stock parser exits silently on an XML comment before the root element). The binary must be in `DUT_IMAGE`. With `DUT` unset the oracle is
`ffc-stock-2026.11.03:22.04` (bennu v2026.11.03, `STOCK_IMAGE` overrides), so run on the daemon that has it and `ferrofire-testpeers`: the Makefile defaults
`TP_DOCKER_HOST` to the Docker Desktop socket (`differential.sh` and `run-stock.sh` default `DOCKER_HOST` to it too).

## Differential and golden replay (DNP3)
The acceptance harness above judges what a blue team sees. The differential proves the device answers like the stock one, byte for byte.

    make differential PROTO=dnp3 WHO=stock SET=diff   # the transcript of probe_diff.py against the stock image
    make diff-dnp3                                    # our binary vs the recorded stock transcripts, live (rebuilds target-linux/debug/ferrofire first)

`conformance/dnp3/`: `probe_diff.py` (about 230 requests: link layer, reads by group and variation, events and their order, IIN, every
control function, routing, CloseNew), `probe_big.py` (130 binary + 130 analog + 450 analog-output points: event overflow,
multi-fragment responses, two-octet ranges), `dnp3probe.py` (helpers, and a stand-in provider REP socket so that commands reach the
datastore as they do with a live provider), `diff-server.xml` / `diff-big-server.xml` (configs), and what the stock device did:
`stock-*.golden` (transcript), `stock-*.trace` (the same run as machine-readable `send` / `sleep` / `publish` / `recv` steps) and
`stock-*.logs` (its event log).
`probe_out.py` (`out`) and `probe_grp.py` (`grp`), both on `diff-server.xml`, pin what stock v2026.11.03 does with output points and with the groups that have no points:
- Once ASSIGN_CLASS gives g10 or g40 a class, a change of an output point is an event with IIN1.1 to 1.3, as for an input, but in
  its own groups: g11 for a binary output (v1 flags, v2 flags and a zero time; default v1) and g42 for an analog output (v1 to v8 as g32; default v1; the time
  is zero in the raw bytes of `stock-out.trace`; the golden masks provider times, so that was read from the trace, and an output tag is not in the provider's stream). A control reaches the output status and raises its event at once (a READ 20 ms later shows both), but after its own response, whose IIN has no class bit
  for it (`Device::settle`). g2 and g32 reads never select them and g11 and g42 reads never select input events. Each of the four types has its own 100-event buffer (opendnp3 `AllTypes(100)`, as for inputs; not probed for outputs). A point left in
  class 0 raises none.
- g10 reads in v0 and v2 only (v0 answers as v2); g10v1 is IIN2.0 (NO_FUNC_CODE_SUPPORT); g10v3 and v4 are IIN2.1 (UNKNOWN_OBJECT), and so are g40v5 and
  above, with no objects. (The READ of g10v3 and g40v5 to v10 that answers IIN2.1 is what stock does, not a regression.)
- `stock-grp.golden` lists, for g3, g4, g11 to g13, g20 to g23, g41, g42, g50 to g52 and g80, v0 to v12, which are empty, NO_FUNC or unknown; `read.rs` has the tables.
  A NO_FUNC object (g13v1, g12v1, g50v1 ...) sets IIN2.0 and the other objects of the READ are still answered; an unknown object refuses the whole READ with IIN2.1 alone.
 The transcripts mask two things a wall clock decides: the provider time inside time-bearing objects
and IIN1.4 NEED_TIME.

`tests/dnp3_golden.rs` replays the traces against our state machine with a virtual clock (no docker, runs in `cargo test` and
under the mutator): every reply must be the same link frames and application bytes, and the event log the same lines. **Runs of
`differential.sh` must not overlap**: every device joins the multicast group 239.0.0.1:40000 on the same bridge, so one probe's
publishes land on the other device (this contaminated the first goldens).

To re-record the stock side: `TRACE=conformance/dnp3/stock-diff.trace LOGS=conformance/dnp3/stock-diff.logs conformance/differential.sh dnp3 stock diff > conformance/dnp3/stock-diff.golden` (and `big`).

## Modbus/TCP

Judged three ways, all against the same server: byte for byte against stock, by independent clients, and by what a blue team sees.

    make diff-modbus                    # our binary vs the recorded stock transcripts, live in containers (rebuilds target-linux/debug/ferrofire first)
    make differential PROTO=modbus WHO=stock SET=edges   # re-record one set from bennu:arm64 (also: diff, waterway, q4, divergent)
    make conformance PROTO=modbus       # pymodbus, mbpoll and nmap modbus-discover vs the stock oracle; DUT=rust for ours
    cargo test --test modbus_golden --test modbus_net --test modbus_clients --test modbus_emfile

**Differential sets** (`conformance/modbus/`, helpers in `modbusprobe.py`; every request is one `#T` trace record, replayed with no sockets by
`tests/modbus_golden.rs`, which also compares the provider payloads and the event log, and whether the connection was closed):

| Set | Config | What it covers | Stock side |
|---|---|---|---|
| `diff` | bennu's own `modbus-server.xml`, provider down | the 24 rows of SPEC 4.1, every function code 0-255, a body of 0-3 bytes for twelve of them, extra bytes, quantity limits and check order, FC15/16 byte counts, unit/transaction ids, two requests in one segment | `stock-diff.{golden,trace,logs}` |
| `waterway` | the waterway `rtu-1` as `fd_server.mako` renders it (`gen_waterway.py`), provider down | the real register map, published values like the simulation's, writes that do not land | `stock-waterway.*` |
| `edges` | logic-free, all four tables, 12 scaling ranges (`gen_edges.py`), a stand-in provider (ZMTP REP that ACKs and reports each `WRITE=`) | in-range scaling to the last bit, bit packing, the top of the address space, FC5 values, FC15/16 spill-over, what the provider is sent (`%f`, `nan`) | `stock-edges.*` |
| `q4` | the `edges` config, no provider | 23 values outside every range (SPEC Q4); not byte-equal by design, see below | `stock-q4.*` |
| `divergent` | the `diff` config | frames that we refuse and stock does not (below); a record, not a golden | `stock-divergent.golden` |

`make diff-modbus` runs `diff`, `waterway` and `edges`; `q4` and `divergent` are asserted in `tests/modbus_golden.rs` instead.
Runs of `differential.sh` must not overlap, for the reason given under DNP3. Stock applies a write the provider never acknowledged about
15 s after the REQ client gives up, which changes what a read returns: the provider-down probes read before they write and finish within 15 s.

**What stock does (recorded, and in places not what SPEC 4.1 said):**
- Any function reads address and quantity first: a body under 4 bytes is exception code 4 under function byte 0x80 (`80 04`), whatever the function. With 4
  bytes or more an unserved function gets the header-only frame (SPEC Q2); FC43 `0e 01 00` is `80 04` for the same reason, not because FC43 is special.
- Limits: quantity 0 or over 2000 (FC1/2) or 125 (FC3/4) is exception 3, checked before addresses. A read whose `addr + qty` ends past 65535 is exception 2,
  for coils too (unmapped coil: 3). FC15/16 have no quantity limit beyond the byte count (which must be `ceil(qty/8)` / `2 * qty`: else 3); a frame is at most 254 long anyway.
- FC5: ON is any value whose high byte is 0xFF (0xFF00, 0xFFFF, 0xFF80 ...), everything else is OFF. FC5 and FC6 echo the whole request PDU, extra bytes included.
- Writes validate and queue address by address: those before an unmapped one are already queued when exception 2 goes back. Scaling is
  `slope = 65535 / (max - min)` in doubles with one fused multiply-add; `<max-value>`/`<min-value>` count only together, else `[-1000, 2000]`. A range of no width reads 0
  and writes `nan`.
- The event log: one `info` line per read (after the checks) naming address and count, one `Data successfully written.` per coil/register written, an `error` line
  naming the first unmapped address; nothing for a refused quantity or byte count (`stock-*.logs`).

**Where we differ on purpose** (each row asserted in `tests/modbus_golden.rs` or `tests/modbus_net.rs`):
| Stock | Ours | Why |
|---|---|---|
| MBAP length over 254: `80 04` up to ~1018 bytes, dies on a 3 KB frame (Q9) | close the connection | a PDU is at most 253 bytes; never a buffer overflow |
| length under 2: `80 04` and garbage from the leftover bytes | close | not Modbus |
| protocol id other than 0 is echoed and served | close | not Modbus/TCP |
| a frame cut short and the client half-closes: answered `80 04` from the previous request's buffer (bennu #81) | no answer, close | a frame is answered only when whole |
| FC15/16 whose data is shorter than the byte count says: writes what its buffer holds | exception 3 | never act on bytes the client did not send |
| value outside the range: `(u32)(slope*v + intercept)`, low 16 bits kept on arm64 (1000.5 over `[0,1000]` reads 31, 1e10 reads 0xFFFF) | clamp to 0 / 65535, NaN reads 0 (SPEC Q4) | a wrapped reading is noise; the rule is tested against stock's words |
| FC5/FC6 at address 65535: exception 2 *after* queueing the write and garbage writes to the wrapped addresses 0.. | exception 2, nothing queued | stock writes out of bounds |
| with the provider down, a write (and a logic-driven output) the provider never acknowledged is applied to the datastore about 15 s later (a coil or a discrete input sharing its name then flips; seen with `diff-server.xml`, no write sent) | never applied: with no provider the read-back never changes (SPEC 3.2) | the abandoned-write apply is a side effect of stock's retry timeout; with a provider that answers the two agree |
| a second write to a tag during the first one's round trip with the provider can be lost (the ACK erases it) | compare-and-erase: the newer write survives | a race, not a behaviour |

**Independent clients.** `tests/modbus_clients.rs`: tokio-modbus 0.17 (dev-dependency). `make conformance PROTO=modbus`: pymodbus and libmodbus' `mbpoll` read and write
over port 502, nmap's `modbus-discover` runs, tshark must find 0 malformed frames and 0 expert warnings in the capture (`mbtcp`), and Zeek's icsnpp-modbus must log the
function codes. **Hostile input and descriptors**: `tests/modbus_net.rs` (2000 connect/close cycles with answered, cut-short, refused and silent connections:
`/proc/self/fd` must not grow; 300 concurrent clients; bad frames get no answer and the next client is served), `tests/modbus_emfile.rs` (descriptor limit lowered: the
accept loop reports once, backs off and serves again when descriptors are free), `fuzz/fuzz_targets/fd_modbus.rs`. These are the bennu #79/#81 shapes (everything-burger's
`modbus-churn` and `modbus-badframes`).

## Field-device clients

`<modbus-client>` and `<dnp3-client>` in `bennu` mode ([docs/field-device-clients.md](../docs/field-device-clients.md)), judged against the stock fd-client itself:
the client is the DUT, the **stock** server runs beside it in the same container (under `stock/servectl.py`, so the peers can stop and start it), and
`stock/probe_cmdif.py` drives the client's command interface the way `bennu-probe` does, with tcpdump on the protocol port.

    DUT=rust make conformance PROTO=modbus-client    # our client vs the stock modbus-server; check.sh wants the stock client's transcript
    DUT=rust make conformance PROTO=dnp3-client      # our master vs the stock outstation
    conformance/stock/run-stock.sh modbus-client     # re-record stock/client-modbus.golden and .pcap from the stock client (also: dnp3-client)

The goldens were recorded from `ffc-stock-2026.11.03:22.04` (bennu v2026.11.03); `STOCK_IMAGE` (default that image) names the stock image,
`CONF_NAME` the container prefix, `TP_DOCKER_HOST` the daemon that has the image and `ferrofire-testpeers`.

**The transcript** (`stock/client-<proto>.golden`): per command `request<TAB>reply`, then what the capture saw until the next command, `>` from the client
and `<` from the server: payload bytes, and `SYN`, `FIN`, `RST`. For Modbus that is every byte. For DNP3 a clock decides where a periodic scan falls, so the
compared transcript shows each frame as link control, addresses and the application fragment with CRCs, the transport octet and the sequence number taken out, and
leaves out the all-classes scans and refused connection attempts; `client-dnp3-raw.golden` has every frame as sent, and `check.sh` notes whether ours equals that
too (it has, frame for frame). `tests/modbus_client.rs` and `tests/dnp3_master.rs` replay the commands in one process and check the exact startup and command bytes.

**What stock does** (recorded; rows 7 to 9 of [the protocol plan](../docs/field-device-protocols-plan.md) carry the detail):
- Command interface: the op is `QUERY`/`READ`/`WRITE` in upper or lower case only; a `WRITE` is a binary write for `true`/`false` and otherwise whatever `stod`
  makes of the text (` 7`, `7x`, `0x10`, `nan`), and the reply echoes the text.
- Modbus: one connection kept open, one reconnect attempt by the request that finds it closed, transaction ids from 3 counting every request, no polling;
  `value*slope + intercept` through 32 bits without a clamp; one range per address for the whole connection, the last declared.
- DNP3: class 0 poll, clear-restart, class 0 poll, then the all-classes scan every `scan-rate`; no enable- or disable-unsolicited request; `READ` from a cache that
  only binary and analog inputs update, by address, into whichever tag has it; `WRITE` updates the cache first and always answers ACK; a command given while the
  outstation is away is dropped; reconnects at once, then 1, 2, 4 ... s apart.

**Where we differ on purpose:**
| Stock | Ours | Why |
|---|---|---|
| a failed READ or WRITE answers `ERR=Failed reading tag 't': ` followed by the bytes of a destroyed string (different every time; masked `<text>` in the goldens) | the text stock meant: `readRegisterByTag(): Failure`, `writeCoil(): Failed writing coil` | never send memory |
| a request without `=`, a `WRITE` without `:value`, a value `stod` refuses or that overflows a double (`WRITE=gen-power:TRUE`): the device dies (`terminate called after throwing an instance of 'std::invalid_argument'`) | `ERR=Malformed request (must be QUERY=\|READ=tag\|WRITE=tag:value)` or `ERR=Failed writing tag 't': value 'v' is not true, false or a number`; nothing is written | a typo must not take the FEP down |
| Modbus: no response timeout; a server that accepts and stays silent blocks the client and its command interface for good (no reply in 100 s) | 1000 ms per request, then the `ERR=Failed ...` reply and a new connection for the next request | a dead peer must not wedge the device |
| a request with bytes that are not UTF-8 is echoed back byte for byte in an error reply | such bytes come back as U+FFFD | the reply is built as text |
| a tag mapped twice: the last mapping wins | the first wins, with a WARN (an error under `--strict`) | one rule for every module that declares tags |
| a command-interface bind failure exits the device | logged; the client runs without it | the rest of the device keeps running, as for every module |
| DNP3: a connection lost is reopened at once, also when it never carried a response (a tight loop through a relay in front of a dead outstation) | at once only after a session that was answered; otherwise the 1, 2, 4 ... 60 s pauses | no connection storm |
| DNP3: after a clear-restart that leaves IIN1.7 set, opendnp3 stops clearing but polls class 0 again on every response that carries the bit | the class 0 poll is not re-demanded either | no poll loop against an outstation that never clears the bit |
| DNP3: commands wait in opendnp3's master queue, which has no size, only a 10 s start timeout (the callback result goes to stdout); WRITE is answered at once | the queue holds 1000 commands per connection (`COMMAND_QUEUE`); one more is dropped and WRITE is answered the same; the warning is logged once per overflow episode (the first drop), and a second line with the count when the queue drains | a WRITE flood while an outstation is silent cannot grow memory; the reply is stock's |
| DNP3 server: stock v2026.11.03 takes `class`, `sgvar`, `egvar`, `deadband`, `event-buffer-size` in the config (default class 0; a control reaches the output status at once, and the sweep skips an output while its write is pending) | `class`, `sgvar`, `egvar`, `deadband`, `event-buffer-size` are refused or ignored; the rest is replicated, ASSIGN_CLASS and output events included | the generator never emits them |
| an out-of-range Modbus write is converted from double to `uint16_t`, which C++ leaves undefined: the arm64 build wraps it (`2000` over 0..1000 leaves as `fffe`), another build may not | the arm64 bytes, on every architecture | the capture is from arm64 |

**Serial endpoints** (plan F6, bead ww-y5q): an `<endpoint>` that is a device path is opened at 9600 8N1, no flow control, raw. Modbus puts MBAP on the port (no slave byte, no CRC);
DNP3 uses the same link frames as on TCP. Recorded from the source, not from the stock image, because neither `ffc-stock-2026.11.03:22.04` nor `ferrofire-testpeers`
has `socat` and our servers have no serial path yet (ww-z5w): opendnp3's `asyncOpenDelay` (500 ms) is never read, so there is no delay after open; stock Modbus never
applies its stored 9600 8N1 (our port is 9600 8N1 raw, so a peer at the OS default of 9600 is unaffected). `tests/serial_clients.rs` runs both clients over a pty pair
into our TCP server (the first DNP3 frame equals the TCP one, every Modbus request is 12 octets for one register); a conformance run of the stock server behind a
pty (a `python3` `pty.openpty` relay would do, the image has python3) is the open item in ww-z5w.

`fep` mode has no stock counterpart; its rules are in [docs/field-device-clients.md](../docs/field-device-clients.md) and its tests in `tests/fd_fep.rs`.

## IEC 61850 (no oracle)
Bennu never loads an IEC 61850 server, so `conformance/iec61850/` has only the acceptance half, and `proto.env` sets `DUT_DEFAULT=rust`:

    make conformance PROTO=iec61850     # libiec61850's unmodified example clients + GOOSE subscriber against our server, on lo

`server.xml` uses libiec61850's default names, so nothing is patched. `check.sh` asserts tshark's `mms` Initiate, GetNameList, Read,
Write, InformationReport and `goose` fields (appid, gocbRef, data set, entries) and what the peers printed. Values are not asserted: the
DUT's inputs come from whatever publishes on the multicast group. No Zeek parser exists for MMS or GOOSE (`zeek.sh` prints SKIP). The
DUT container gets `--cap-add NET_RAW` (`DUT_CAPS` in `proto.env`) because GOOSE needs `CAP_NET_RAW`; see docs/field-device-iec61850.md.
The libiec61850 GOOSE subscriber is not run: its kernel filter drops VLAN-tagged frames on `lo` (ww-otj.72); go-iec61850 subscribes
instead. iec61850bean and go-iec61850 (independent of libiec61850) are asserted in `check.sh` as well.
The image built before 2026-11-10 lacks the control client and the GOOSE subscriber (their binaries are named
`client_example_control` and `goose_subscriber_example`; the Dockerfile asked for other names): run `make testpeers` again, or point
`TESTPEERS=` at an image that has them.

## Test peers (docker/testpeers)
| Peer | Source | Licence | Use |
|---|---|---|---|
| `mbpoll` (libmodbus), `python3-pymodbus` | Debian | GPL-3 (`mbpoll`: test peer only, never linked) / BSD-3 | independent Modbus/TCP clients |
| `outstation-demo`, `master-demo` | opendnp3 3.1.2 examples | Apache-2.0 | stock DNP3 peers (interactive master) |
| `ff-dnp3-master HOST PORT LOCAL REMOTE [wait_s] [crob IDX CODE\|select-crob IDX CODE\|ao IDX VALUE]...` | ours, on opendnp3 | MIT / Apache-2.0 | scriptable master: integrity poll, commands, poll |
| `server_example_*`, `iec61850_client_example1`, `client_example_control`, `iec61850_client_example_reporting`, `goose_publisher_example`, `goose_subscriber_example`, `goose_observer` | libiec61850 v1.6.2 | **GPL-3.0, test peers only, never linked** | IEC 61850 server/client/GOOSE |
| `mms_utility -h HOST -d`, `-r VAR -a DOMAIN` | libiec61850 | GPL-3.0 | generic MMS client for ICCP named-variable reads |
| `java -cp '/opt/iec61850bean/*' com.beanit.iec61850bean.app.ConsoleClient -h H -p 102` (stdin: `m`, `ga`, `q`) | iec61850bean 1.9.0, Maven Central, sha256-pinned | Apache-2.0 | independent IEC 61850 MMS client (Java, own ASN.1 codec) |
| `ffgoiec ADDR IFACE [REF...]` | ours (`docker/testpeers/goiec/`) over otfabric/go-iec61850 v1.16.2 | MIT | independent MMS client and GOOSE subscriber (Go) |
| `/opt/dnp3py/bin/python` with `dnp3` | dnp3py 0.6.0 (PNNL), pip in a venv | MIT | independent DNP3 master (pure Python) |
| `igstase2-client`, `igstase2-server` | aklira/igs-iec-tase2 + igs-iso9506-mms, pinned commits | Apache-2.0 | independent TASE.2 client and server (ICCP conformance) |
| `tase2-client`, `tase2-server` (daemons, JSON on a Unix socket) | janfietz/telcon-tase2 `tase2-test-tools`, pinned commit | MIT OR Apache-2.0 | independent TASE.2 client and server (ICCP conformance); `jq` is in the image for them |
| tshark 4.4, tcpdump, nmap 7.95 (`iec61850-mms`, `iec-identify`, `s7-info`, `bacnet-info`, `enip-info`, `modbus-discover`) | Debian | | analysis |
| Zeek 9.0.1 + icsnpp-dnp3, modbus, bacnet, s7comm, enip, opcua-binary, synchrophasor | CISA | | `/etc/ff-testpeers.zeek-packages` lists what installed |

Not in the image: icsnpp-profinet (no such package), a Zeek IEC 61850/ICCP parser (none exists; those protocols are judged by tshark `mms`/`goose`
and the nmap scripts). nmap has no `dnp3-info` script. Licence texts: `/usr/share/doc/ferrofire-testpeers/`.

Third-party captures for parser checks (not part of `make conformance`): `tests/pcaps/public/fetch.sh` then `check-parsers.sh`
(ITI/ICS-Security-Tools DNP3, MMS, GOOSE; pinned commit, sha256 per file; see docs/testing.md).

## Publishing
`.github/workflows/testpeers.yml` builds linux/amd64 + arm64 and pushes `ghcr.io/<owner>/ferrofire-testpeers` on a `v*` tag (or by hand).
It does nothing until a tag exists.

## ICCP / TASE.2 (no oracle; two independent peers)
`conformance/iccp/`, `DUT_DEFAULT=rust`: `make conformance PROTO=iccp`. One capture (tcp 102, 10102, 10103) of four legs, judged by tshark
(0 malformed, expert info clean), `tools/wireshark/iccp.lua` and the Zeek Spicy analyzer (`prepare.sh` builds it in the testpeers image):

| Leg | Client | Server |
|---|---|---|
| 1 | libiec61850 `mms_utility`; our thin client (`examples/iccp-client`, `PEER_BIN`) | ferrofire on 102 |
| 2 | `igstase2-client` (igs-iec-tase2, Apache-2.0) | ferrofire on 102 |
| 3 | telcon-tase2 `tase2-client` daemon (MIT OR Apache-2.0) | ferrofire on 102 |
| 4 | our thin client, `ICCP_STEPS=1` (one `step NAME: ok|FAIL` line per check) | `igstase2-server` on 10102 (`igs-server.toml`), telcon `tase2-server` on 10103 |

Both peers are built from pinned commits in `docker/testpeers/Dockerfile` and run as separate processes; ferrofire's Cargo graph does not
contain them. `check.sh` marks each expectation `ok`, `xfail BEAD-OR-REASON` (it must still fail; if it starts to pass the run fails until
the mark is removed) or `skip` (printed, with the reason): see docs/testing.md for the list. `proto.env` adds `TSHARK_EXTRA` (`-d
tcp.port==10102,tpkt ...`, applied by `lib.sh`'s `tshark` function) and `PEER_EXTRA` (repo files copied into the peers container: the Lua
dissector and the Zeek package sources). `ICCP_STEPS`, `ICCP_CALLING_AP_TITLE` are read by the example.

**Real captures.** `tests/pcaps/iccp/fetch-real.sh` downloads the Wireshark wiki `mms.pcap` and chaugan's
`iccp_with_switchover_anon.pcap` (pinned commit, sha256 verified) into `tests/pcaps/iccp/real/`, which is git-ignored; `run.sh` copies the
directory to `/real` when it exists, and `check.sh` / `zeek.sh` then assert on each: 0 malformed, Lua without errors, 12-bit
`Supported_Features`, the `TASE2_Version` of that capture, an empty Zeek weird.log and reporter.log. Without the directory the step prints SKIP.

To run: `make testpeers` (use `TESTPEERS=<image>` for another tag),
optionally `tests/pcaps/iccp/fetch-real.sh`, then `make conformance PROTO=iccp`. Output in `target/conformance/iccp/`: `iccp.pcap`,
`peer.out` (every operation as `### label`, output, `rc=`), `dut.out`, `zeek/`, `zeek-real-*/`, `lua.err`.
The peers are young and possibly AI-assisted: agreement is evidence, not proof. See docs/field-device-iccp.md for what the server serves.

## MQTT and Sparkplug B

    make conformance PROTO=mqtt         # DUT=rust is the default: bennu has no MQTT, so there is no stock side

`conformance/mqtt/server.xml` has two `<mqtt-client>` elements on the same tags (plain topics and a Sparkplug B edge node). The broker
is Mosquitto, started by `peer.sh` inside the peers container on 127.0.0.1:1883 (the lab default of the range's broker image: anonymous,
plaintext); the device retries until it is up. `peer.sh` then runs, in order: `mosquitto_sub` (retained values), `mosquitto_pub` (a QoS 1
command), `paho_client.py` (Eclipse Paho for Python: reads the topics, writes an in-range and an out-of-range command), the Tahu host
(`docker/testpeers/tahu/TahuHost.java`: sends a Rebirth NCMD, decodes the NBIRTH and NDATA with Eclipse Tahu 1.0.21, writes a metric by NCMD)
and finally `kill -9`s the device to see the wills. `provider.py` is a stand-in provider REP socket that logs every `WRITE=`, which is how
"the command reached the write queue" is observed. `check.sh` asserts the tshark `mqtt` fields and the peers' results, `zeek.sh` the
`mqtt_connect` / `mqtt_subscribe` / `mqtt_publish` logs (Zeek's built-in analyzer, no package). `proto.env` sets `PEER_FLAGS` so that the
peers container shares the device's process namespace (needed for the `kill`). Tahu and Paho are EPL-2.0 test peers; ferrofire links none of it.

## SCPI (raw socket 5025)

    make conformance PROTO=scpi         # DUT=rust is the default: bennu has no SCPI, so there is no stock side
    make testpeers                      # first, once: the image needs pyvisa and lxi-tools (docker/testpeers/Dockerfile)

`conformance/scpi/server.xml` serves five tags (`level`, `flow`, `brkr`, `breaker-cmd`, `setpoint`) on 127.0.0.1:5025; `provider.py` is
a stand-in provider that logs every `WRITE=`, and one datagram loop gives the tags their values. `peer.sh` then runs, in order:
`pyvisa_client.py` (PyVISA 1.16.2 with pyvisa-py 0.8.1, a `TCPIP::127.0.0.1::5025::SOCKET` resource: `*IDN?`, reads of several tags, a
compound query, a write that must reach the provider, an out-of-range write that must give -222 in `SYST:ERR?` and the right `*STB?` /
`*ESR?` before and after), `lxi scpi --raw` (lxi-tools 2.8-1: `*IDN?` and a read), `ferrofire scpi --check` (our own client, the same
binary as the device, via `PEER_BIN`: a script that must exit 0, a bad write that must exit 1 with the -222 text), a churn of 2000
connections (completed, half-sent, aborted with a RST, half-closed) with the device's descriptor count read from `/proc/<pid>/fd` before and
after, and an overlong line and random bytes on one connection that must still answer afterwards. `proto.env` shares the device's process
namespace for the descriptor count.

There is no SCPI dissector in tshark and no Zeek analyzer, so `check.sh` asserts what a capture of plain text can show: at least 2000
handshakes to 5025, payload both ways, the requests and the responses as bytes found in the packets (`*IDN?` and its answer, the -222
and -363 responses, the `;`-joined answer of a compound query), 0 malformed frames and no expert errors (`expert_clean`), and Zeek's
`conn.log` rows for port 5025. PyVISA and lxi-tools are MIT / BSD-3-Clause test peers; ferrofire links none of it.

## SCPI controller (`<scpi-client>` against an instrument we did not write)

    make testpeers                      # first, once: the image needs scpi-parser-tcp, scpi-parser-vxi11 and rpcbind (docker/testpeers/Dockerfile)
    make conformance PROTO=scpi-client  # DUT=rust; the DUT runs in the testpeers image (RUST_DUT_IMAGE), not bennu:arm64

The DUT is a ferrofire controller (`conformance/scpi-client/controller.xml`) with two `<scpi-client>`s, one per transport, polling the same
independent instrument: the `examples/test-tcp` and `examples/test-vxi11` instruments of [scpi-parser](https://github.com/j123b567/scpi-parser) (Jan Breuer,
BSD-2-Clause, C, the parser library instrument firmwares embed; pinned by commit, built in the image, never linked). The raw-socket one is on
127.0.0.1:5025; the VXI-11 one (`TCPIP::127.0.0.1::INSTR`) registers with `rpcbind` (started by `peer.sh`) on port 111 and answers on the
ephemeral core-channel port the portmapper hands out. scpi-parser's VXI-11 example is incomplete upstream (its rpcgen output was never
committed), so the Dockerfile generates it from the VXI-11 IDL in [liblxi](https://github.com/lxi-tools/liblxi) (BSD-3-Clause, pinned) plus the
spec's interrupt-channel lines; only that IDL is not scpi-parser's, the instrument is. The controller's own `<scpi-server>` on 5030
is how the harness drives it: `peer.sh` starts the instruments after the controller (which has been retrying), sets `want` to 36 and then 4
over 5030, `<logic>` (`ese_set = want`, `vese_set = want`) makes the controller send `*ESE <v>` to each, the controller reads `*ESE?` back into
`ese` and `vese`, and the harness must read the same value from the controller for each; `MEAS:VOLT:DC?` must arrive as the number 0 on both. `check.sh`
finds in the capture the sets (`*ESE 36.0`, `*ESE 4.0`), the queries as `QUERY;*OPC?` with their joined answers (`36;1`, `4;1`), the error-queue
reads answered `0,"No error"` and no error answer at all; on VXI-11 the same sets, queries and answers as tshark-decoded `device_write` and
`device_read` data on the core channel, and `create_link` answered without error; 0 malformed frames and no expert errors, and Zeek's `conn.log`
rows for ports 5025 and 111. (The VXI-11 tags are `vese`/`vvolt` because a trailing digit is an SCPI numeric suffix: `ese2` would collide with `ese`.)
