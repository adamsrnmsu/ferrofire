# Acceptance harness

Judges a field-device protocol the way a blue team sees it: capture the wire, then ask tshark and Zeek what they decoded.

    make testpeers                  # once: build the ferrofire-testpeers image (about 10 min; docker/testpeers/Dockerfile)
    make conformance PROTO=dnp3     # peers vs the stock oracle (bennu:arm64); exit 0 = PASS
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

Add a protocol: copy `conformance/dnp3/`, change the four files. Only `ff-conf-dut` and `ff-conf-peers` are created or removed.
Files go in and out with `docker cp` (no bind mounts: the Lima docker daemon cannot see every host path).

## Pointing it at another device
`DUT_IMAGE=<image> DUT_CMD='<command reading /tmp/cfg.xml>' conformance/run.sh dnp3` (the config is `CFG`, default the stock sample from
`BENNU_DATA`, default `~/git/sceptre-bennu/data/configs/ep`; for DNP3 it is `conformance/dnp3/server.xml`, because the stock parser exits silently on an XML comment before the root element). The binary must be in `DUT_IMAGE`. With `DUT` unset the oracle is
`bennu:arm64` (SPEC section 8), so run on the daemon that has it: the Makefile defaults `TP_DOCKER_HOST` to the Lima socket.

## Differential and golden replay (DNP3)
The acceptance harness above judges what a blue team sees. The differential proves the device answers like the stock one, byte for byte.

    make differential PROTO=dnp3 WHO=stock SET=diff   # the transcript of probe_diff.py against bennu:arm64
    make diff-dnp3                                    # our binary vs the recorded stock transcripts, live (build with make test first)

`conformance/dnp3/`: `probe_diff.py` (about 230 requests: link layer, reads by group and variation, events and their order, IIN, every
control function, routing, CloseNew), `probe_big.py` (130 binary + 130 analog + 450 analog-output points: event overflow,
multi-fragment responses, two-octet ranges), `dnp3probe.py` (helpers, and a stand-in provider REP socket so that commands reach the
datastore as they do with a live provider), `diff-server.xml` / `diff-big-server.xml` (configs), and what the stock device did:
`stock-*.golden` (transcript), `stock-*.trace` (the same run as machine-readable `send` / `sleep` / `publish` / `recv` steps) and
`stock-*.logs` (its event log). The transcripts mask two things a wall clock decides: the provider time inside time-bearing objects
and IIN1.4 NEED_TIME.

`tests/dnp3_golden.rs` replays the traces against our state machine with a virtual clock (no docker, runs in `cargo test` and
under the mutator): every reply must be the same link frames and application bytes, and the event log the same lines. **Runs of
`differential.sh` must not overlap**: every device joins the multicast group 239.0.0.1:40000 on the same bridge, so one probe's
publishes land on the other device (this contaminated the first goldens).

To re-record the stock side: `TRACE=conformance/dnp3/stock-diff.trace LOGS=conformance/dnp3/stock-diff.logs conformance/differential.sh dnp3 stock diff > conformance/dnp3/stock-diff.golden` (and `big`).

## Modbus/TCP

Judged three ways, all against the same server: byte for byte against stock, by independent clients, and by what a blue team sees.

    make diff-modbus                    # our binary vs the recorded stock transcripts, live in containers (build with make test first)
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

## IEC 61850 (no oracle)
Bennu never loads an IEC 61850 server, so `conformance/iec61850/` has only the acceptance half, and `proto.env` sets `DUT_DEFAULT=rust`:

    make conformance PROTO=iec61850     # libiec61850's unmodified example clients + GOOSE subscriber against our server, on lo

`server.xml` uses libiec61850's default names, so nothing is patched. `check.sh` asserts tshark's `mms` Initiate, GetNameList, Read,
Write, InformationReport and `goose` fields (appid, gocbRef, data set, entries) and what the peers printed. Values are not asserted: the
DUT's inputs come from whatever publishes on the multicast group. No Zeek parser exists for MMS or GOOSE (`zeek.sh` prints SKIP). The
DUT container gets `--cap-add NET_RAW` (`DUT_CAPS` in `proto.env`) because GOOSE needs `CAP_NET_RAW`; see docs/field-device-iec61850.md.
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
| tshark 4.4, tcpdump, nmap 7.95 (`iec61850-mms`, `iec-identify`, `s7-info`, `bacnet-info`, `enip-info`, `modbus-discover`) | Debian | | analysis |
| Zeek 9.0.1 + icsnpp-dnp3, modbus, bacnet, s7comm, enip, opcua-binary, synchrophasor | CISA | | `/etc/ff-testpeers.zeek-packages` lists what installed |

Not in the image: icsnpp-profinet (no such package), a Zeek IEC 61850/ICCP parser (none exists; those protocols are judged by tshark `mms`/`goose`
and the nmap scripts). nmap has no `dnp3-info` script. Licence texts: `/usr/share/doc/ferrofire-testpeers/`.

## Publishing
`.github/workflows/testpeers.yml` builds linux/amd64 + arm64 and pushes `ghcr.io/<owner>/ferrofire-testpeers` on a `v*` tag (or by hand).
It does nothing until a tag exists.

## ICCP / TASE.2 (no oracle, no open peer)
`conformance/iccp/`: libiec61850's `mms_utility` (generic MMS client) browses and reads the server, and our thin ICCP client
(`examples/iccp-client`, copied into the peers container through `PEER_BIN` in `proto.env`) does the TASE.2 part: write, data set,
transfer set, InformationReport. `make conformance PROTO=iccp` (`DUT_DEFAULT=rust`). The judgement is tshark's `mms` dissector
(0 malformed, 0 expert warnings, ICCP names in `mms.vmd_specific`/`mms.domainId`/`mms.itemId`); see docs/field-device-iccp.md for what that does
and does not prove.

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
