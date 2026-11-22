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
Files go in and out with `docker cp` (no bind mounts: the docker daemon, Docker Desktop by default or a VM, may not see every host path).

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

    make differential PROTO=dnp3 WHO=stock SET=diff   # the transcript of probe_diff.py against the stock image (SET: diff big out grp cfg ovf nack nopv)
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
  for it (`Device::settle`). g2 and g32 reads never select them and g11 and g42 reads never select input events. Each of the four types has its own event buffer, 100 events unless `<event-buffer-size>` says otherwise (`probe_ovf.py`, below). A point left in
  class 0 raises none.
- g10 reads in v0 and v2 only (v0 answers as v2); g10v1 is IIN2.0 (NO_FUNC_CODE_SUPPORT); g10v3 and v4 are IIN2.1 (UNKNOWN_OBJECT), and so are g40v5 and
  above, with no objects. (The READ of g10v3 and g40v5 to v10 that answers IIN2.1 is what stock does, not a regression.)
- `stock-grp.golden` lists, for g3, g4, g11 to g13, g20 to g23, g41, g42, g50 to g52 and g80, v0 to v12, which are empty, NO_FUNC or unknown; `read.rs` has the tables.
  A NO_FUNC object (g13v1, g12v1, g50v1 ...) sets IIN2.0 and the other objects of the READ are still answered; an unknown object refuses the whole READ with IIN2.1 alone.
`probe_cfg.py` (`cfg`, `diff-cfg-server.xml`) pins the config keys of v2026.11.03: `class` on every point, `sgvar`, `egvar` and `deadband` on inputs, `event-buffer-size`, a
name opendnp3 does not know (the point is left out, silently), and an input and an output on one index. The probes behind it are in `docs/field-device-spec.md` 4.2.0.
`probe_ovf.py` (`ovf`) overflows the output event buffers (g11, g42) at the default 100. `probe_nack.py` (`nack`) and `probe_nopv.py` (`nopv`), on `diff-prov-server.xml` (an
output and an input on one tag), run a control against a provider that answers `ERR=` and against none: `tests/dnp3_golden.rs` replays them with the same provider.
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
    make differential PROTO=modbus WHO=stock SET=edges   # re-record one set from the stock image (also: diff, waterway, q4, divergent)
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

`make diff-modbus` runs `diff`, `waterway` and `edges`; `q4` and `divergent` are asserted in `tests/modbus_golden.rs` instead. `q4` is asserted twice: the clamp by default, and with `--compat-wrap` byte-equal to the stock transcript.
`--strict` (Q1 and Q2) is not recorded against stock, which has no such mode: `src/fd/proto/modbus/device.rs` unit-tests it. A strict server answers an unserved function (stock: the header-only frame, `80 04` for a short body) with exception 1, and a unit id other than its `<unit-id>` (stock ignores the key and serves every id) with silence.
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
| value outside the range: `(u32)(slope*v + intercept)`, low 16 bits kept on arm64 (1000.5 over `[0,1000]` reads 31, 1e10 reads 0xFFFF) | clamp to 0 / 65535, NaN reads 0 (SPEC Q4); `--compat-wrap` wraps like stock | a wrapped reading is noise; the rule is tested against stock's words. A negative value reads 0 in both: -1 reaches a register only through a range whose `<min-value>` is below it (ww-4d7) |
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

The BACnet client and server are under [BACnet/IP](#bacnetip).

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
| DNP3: commands wait in opendnp3's master queue, which has no size, only a 10 s start timeout (the callback result goes to stdout); WRITE is answered at once | the same 10 s start timeout (`START_TIMEOUT`, logged), and the queue holds 1000 commands per connection (`COMMAND_QUEUE`); one more is dropped and WRITE is answered the same; the warning is logged once per overflow episode (the first drop), and a second line with the count when the queue drains | a WRITE flood while an outstation is silent cannot grow memory; the reply is stock's |
| DNP3 server: a `<deadband>` that is not a number (`abc`, empty, `inf`) makes stock log `ERROR: There was a problem parsing dnp3 server setup: conversion of data to type "d" failed` to stdout and carry on with no DNP3 outstation at all (nothing listens on the port, no Size lines) | the config is refused with an error naming `<deadband>` | the generator never emits it |
| an out-of-range Modbus write is converted from double to `uint16_t`, which C++ leaves undefined: the arm64 build wraps it (`2000` over 0..1000 leaves as `fffe`), another build may not | the arm64 bytes, on every architecture | the capture is from arm64 |

**GOOSE subscriber** (`<iec61850-rtu>`, ww-n29): `DUT=rust make conformance PROTO=iec61850-rtu`. go-iec61850 (`ffgoiec publish`) publishes
`PUB/LD0 gcbTrip` from `iec61850-rtu/substation.scd` on lo, Tr.general false then true, false, true. The DUT subscribes twice: from the
same SCD by `RTU1`'s `ExtRef` (`intAddr` mirror) and from `sub.cid`'s SEL block as bennu does. `provider.py` stands in for the provider
and prints the writes they cause: the mirror follows both ways, each TRIP writes `false` once. bennu never builds its module, so no oracle.

**UDP endpoints** (DNP3, ww-8l7): `UDP=PORT conformance/differential.sh dnp3 rust SET` turns the DUT's `tcp://127.0.0.1:PORT` into `udp://127.0.0.1:PORT`
and the probe (`dnp3/dnp3probe.py`) sends the same requests as datagrams, a new socket standing for a new connection. Stock has no UDP endpoint, so `make
diff-udp` compares ours with the stock TCP goldens of all eight sets. One step differs by design and is skipped on UDP: probe_diff's second master, refused
by stock's one-master slot (CloseNew) and answered on UDP; the golden's `second connection: closed` becomes `second connection: skipped on udp`.
`make conformance PROTO=dnp3-udp` (DUT ours) runs opendnp3 3.1.2's own UDP channel (`FF_DNP3_UDP=1 ff-dnp3-master`, the same polls and commands
as `dnp3/peer.sh`) against our outstation on `udp://127.0.0.1:20000` and judges the capture with tshark and Zeek's DNP3 UDP analyzer.

**Serial endpoints** (plan F6, beads ww-y5q, ww-z5w, ww-8k4): an `<endpoint>` that is a device path is opened at 9600 8N1, no flow control, raw. Modbus puts
MBAP on the port (no slave byte, no CRC); DNP3 uses the same link frames as on TCP. Neither image has `socat`, so `stock/ptyrelay.py` (python3 only) stands in:
in the DUT container it makes a pty, links its slave to `/tmp/ttyFF`, and joins its master side to the TCP port the TCP run uses. `SERIAL=listen:PORT` (a server
DUT: one TCP client at a time is joined to the port) or `SERIAL=connect:PORT` (a client DUT: what it writes goes to the TCP server on PORT, dropped while that
refuses) turns that `<endpoint>` into `/tmp/ttyFF`, so the peers, captures and transcripts are the TCP run's. The relay passes on what the device wrote after
20 ms of silence on the port, as a line reader frames it: a line carries bytes, not writes.

A pty has no speed or parity, so the relay models the line (ww-y51). The port starts as a real ttyS does before a program sets it,
9600 8N1, canonical, with echo. The relay reads what the device then sets (termios2, so a speed set as `BOTHER` shows as its number),
and the transcript ends with it: `# serial line: 9600 8N1 flow=none raw`. Bytes take the time the line needs at `SERIAL_LINE`
(default `9600-8N1`: 10 bits a byte, about 1.04 ms). When the device's speed, data bits, parity or stop bits differ from the line's,
every byte arrives corrupted, both ways. That stands in for a mismatched UART's framing and parity errors; which garbage a real one
makes is not modelled.

    make diff-serial                                                    # both servers, every differential set, ours on a serial port vs stock
    make diff-serial-mismatch                                           # negative control: at 19200-8N1 and 9600-8E1 neither stock nor ours may talk
    SERIAL=connect:5502 DUT=rust make conformance PROTO=modbus-client   # our client on a serial port vs stock/client-modbus-serial.golden
    SERIAL=connect:20000 DUT=rust make conformance PROTO=dnp3-client    # our master; stock's serial transcript is its TCP one, so client-dnp3.golden
    SERIAL=connect:5502 conformance/stock/run-stock.sh modbus-client    # re-record the stock client's serial golden

What the stock image does on a serial port (recorded 2026-11-20, `ffc-stock-2026.11.03:22.04`):
- DNP3 outstation: every differential set (diff, big, out, grp, cfg, ovf, nack, nopv) gives the TCP golden byte for byte. DNP3 master: the TCP transcript.
- Modbus server: the TCP goldens, except that it answers **one frame per read**: a second request in the same write waits in its buffer for the next bytes,
  and from then on every reply comes one request late. The two cases that put two frames in one write (`pipelined FC4+FC3` in `diff`, `two writes ... in one
  segment` in `edges`) are skipped when `SERIAL` is set; `stock-serial-diff.golden` (recorded) and `stock-serial-edges.golden` (`stock-edges.golden` without that
  case; stock's run differs from it only in how often the NaN write is re-sent, ww-tqx) are the serial goldens, `waterway` needs none.
- Modbus server on the paced line (bytes in 32-octet pieces, as a UART delivers them): a request that arrives in more than one
  read (the 254-octet `FC4 + 245 extra bytes` in `diff`, the long FC3/FC16 rows in `edges`) is answered, and then stock answers
  nothing more (ww-bb5.12). Up to that row stock equals the serial goldens; ours equals them through to the end.
- Modbus client: a request to a server that is away waits out its 1000 ms timeout (`~1s` in `client-modbus-serial.golden`) instead of a refused connection.

Ours equals all of these. Where we differ on a serial port: we answer both frames of a write at once, as stock does on TCP (a master that pipelines gets
its replies in order, not one late). Our DNP3 master has opendnp3's 10 s start timeout too (`START_TIMEOUT`): a session on a serial port stays up while the
outstation is away, so a queued command could otherwise be sent long after it was given. Port settings: stock's DNP3 sets 9600 8N1, raw, no flow control, and so do both of ours. Stock's Modbus sets raw and no flow
control but never the speed, so it runs at whatever the port has (38400 on a bare pty, the relay's 9600 here, the host's default on
a real port). Ours sets 9600 8N1, a difference we keep on purpose. The model checks the settings and the timing a line imposes; real
UART hardware, cabling and RS-485 direction control are not covered.

`fep` mode has no stock counterpart; its rules are in [docs/field-device-clients.md](../docs/field-device-clients.md) and its tests in `tests/fd_fep.rs`.

## BACnet/IP

`<bacnet-server>` and `<bacnet-client>` are served by an external module, `modules/bacnet/` (bacpypes3 0.1.0, ffmod/1:
[docs/modules.md](../docs/modules.md)), so `DUT=rust` runs our binary in `ff-modbac:22.04` (`RUST_DUT_IMAGE`), which has the module at
`/opt/ferrofire/modules/bacnet`. `make modbac-image` builds it; rebuild it after any change under `modules/bacnet/`.

    make conformance PROTO=bacnet                     # the stock server: probe_bacnet.py, nmap bacnet-info, tshark, Zeek icsnpp-bacnet
    DUT=rust make conformance PROTO=bacnet            # ours, and its transcript against stock's (the differential)
    make conformance PROTO=bacnet-client              # the stock client against the stock server: the golden itself
    SERVER=rust make conformance PROTO=bacnet-client  # the stock client against our server
    DUT=rust make conformance PROTO=bacnet-client     # our client against the stock server
    conformance/stock/run-stock.sh bacnet-feed        # re-record stock/bacnet-server-feed.golden (also: bacnet-client, and bacnet for
                                                      # the provider-less stock/bacnet-server.golden of Task 0b)

**Server** (`conformance/bacnet/`, protocols plan row 6). The device answers Who-Is by broadcast, so the probe needs an address of its
own: `OWN_NET=1` puts the DUT on a private network `ff-conf-net` that the peers join (capture on `eth0`). Beside the DUT runs
`feed.py`, its provider: it publishes `FEED` (BI 0 and 1 active, AI 0 250.5, AI 1 300.7) to the `<input>` group twice a second and
ACKs and logs the writes that reach the `<output>` REP. `peer.sh` waits until AI 0 reads 250.5, runs `stock/probe_bacnet.py` (bacpypes3,
pip-installed by `prepare.sh`; 502 lines: Who-Is, the device and every configured object by RP and RPM, a sweep of property ids 0 to
260 on nine objects, WP and WPM with their read-backs, errors, ReadRange, DCC with its 70 s duration), `nmap --script bacnet-info`,
and appends the provider's log (`## provider`). `check.sh`: I-Am device 1234 vendor 260, the fed present values of each object (AO as
stock's uint8: 250.5 reads 250, 300.7 reads 44), WP AO 2 42.5 then a read of 42, `object/unknown-object`; nmap equal to
`stock/bacnet-server.nmap.golden`; the transcript equal to `stock/bacnet-server-feed.golden` (stock, three runs alike) or, for ours,
differing from it by exactly `bacnet/rust-feed.diff` (a divergence that goes away fails too). Zeek: `bacnet`, `bacnet_discovery`,
`bacnet_property`, `bacnet_device_control` logs. Stock with a feed confirms what the module had only modelled: BI/AI present values,
BO priority slot 1 and AO slot 16 follow the datastore at 1 Hz, and an output tag reads the value published for its provider point.

Where ours differs from stock (`bacnet/rust-feed.diff`, the same in three runs):

| Area | Stock | Ours | Whose |
|---|---|---|---|
| AV 0, BV 0, MSI 0 detail properties (sweep) | bacnet-stack demo values | bacpypes3 defaults, some missing | module, open (known since Task 0b) |
| ReadRange of a trend-log log-buffer | 66-76 synthetic records | `services/property-is-not-a-list` | module, open (known since Task 0b) |
| A value written and ACKed by the provider, read within about 2 to 4 s (6 lines: AO 3 2.5 s after its write reads 9 where stock reads 44; AO 2 and 3 priority slot 16; AO 2 after its relinquish; AI 0 at the DCC step) | the provider's next datagram (0.5 s here) wins at once | the ACKed value is held 2 s against the stream (`ACK_HOLD`, ww-1df.32), then the stream wins | device, by design: a real provider applies the write at its next tick and then publishes it, so both converge; `feed.py` never applies a write |
| AI present-value write with out-of-service set | accepted and enqueued (`ai.c`) | `write-access-denied`, as with out-of-service clear (not probed) | module, not probed |
| Properties and services no probe touched (COV, Who-Has, BO priority 6) | per bacnet-stack | bacpypes3 behaviour, untested against stock | module, untested |
| Config errors | logged when the device loads | `error` after `hello` (no `udp://`, a missing `endpoint`/`instance`/`address`/`tag`, a non-number) | module, by design |
| The order of the points in one `WRITE=` batch to the provider | `generator-1_bus-1.active, load-1_bus-1.active, bus-1.gen_mw, load-1_bus-1.mw` | by tag name (`bus-1.gen_mw` first) | device; the same pairs, and a provider reads them as a set |

The no-provider golden of Task 0b (`stock/bacnet-server.golden`) stays as its record; the fed run supersedes it (Task 7 found ours
different from it by the first two rows only).

**Client** (`conformance/bacnet-client/`, row 11). bennu's sample `<bacnet-client>` (`stock/client-bacnet.xml`: device 1234 at
127.0.0.1:47808, scan-rate 5, command interface 1330) with a BACnet server beside it in the same network namespace, under
`stock/servectl.py`: `SERVER=stock` (bennu's sample server in the stock image) or `SERVER=rust` (ours in `ff-modbac`; `run.sh`'s
`SIDE_IMAGE` container). Both stacks use `BACNET_IFACE=lo`, the client port 47809 (`DUT_ENV`) and the server 47808. `peer.sh` drives
the command interface (`stock/probe_cmdif.py`): reads, a RADIO datagram for the server's inputs, a write of each point type, errors,
the server stopped and started. `probe_cmdif.py --report 47809 bacnet` prints the replies and the client's frames as BVLC/NPDU/APDU
bytes with the invoke id masked; the scans (RP present-value) are left out where they fall and listed once at the end, each request
with each distinct answer; a reply of 3 s or more is `~3s+` (stock's WRITE waits out its retries). The golden is the stock client
against the stock server (`stock/client-bacnet.golden`, two runs alike).

- **The stock client against our server** (`SERVER=rust`): the transcript equals the golden (PASS): same answers to every scan
  request, WriteProperty and error, through a stop and restart.
- **Our client** (`DUT=rust`, the sample unchanged): PASS, the transcript differs from the golden by exactly
  `bacnet-client/rust-client.diff` (the same in two runs). The device declares the client's point tags at load (as compiled
  clients do; `docs/modules.md`), and the module reads the object types stock's type rule names (every binary point as a
  binary-output, AI 1 as AO 1, `ClientConnection.cpp`) with stock's `00 05` request header, so every scan request and answer is the
  stock client's. What the diff lists is the device's write semantics, open in ww-4s8:

| Area | Stock client | Ours | Whose |
|---|---|---|---|
| `WRITE=` on the command interface | sends the WriteProperty inside the request and answers with its outcome (`ACK=Wrote tag ...`, or `ERR=...` after about 5 to 10 s with the server away) | answers `ACK=Wrote tag ...` at once; the WriteProperty goes out with the next scan cycle (`peer.sh` waits 3 s after each write so both fall under their command), and one not answered within 3 s is reported failed and sent again when due. With the server away: `ACK`, and the write lands after the restart (bacpypes3's own retry, then the device's) | device (client echo rule over ffmod/1) |
| `WRITE=` of the value the tag already holds (`load-breaker-toggle:true` after a poll read true) | a WriteProperty all the same | nothing on the wire (the echo rule sends changes only) | device |
| `WRITE=` to an input tag (`brkr`, `load-power`) | sent as a WriteProperty to the object its type rule names (BO 0 is ACKed and then reads active in the scans; AI 0 is `write-access-denied`, `ERR`) | `ERR=Failed writing tag ...` with nothing on the wire | device |

The echo rule also skips the first poll after a write it sent (a device may answer and show the old value a moment longer), so a
read right after a write can show the written value where stock's next poll already shows the server's; `peer.sh` reads after two
scans, so the transcript does not show it.

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
    make conformance PROTO=scpi-client  # DUT=rust; the DUT runs in the testpeers image (RUST_DUT_IMAGE), not the stock image

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
