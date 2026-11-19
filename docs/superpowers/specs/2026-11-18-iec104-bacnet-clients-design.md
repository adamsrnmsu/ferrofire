# `<iec60870-5-104-client>` and `<bacnet-client>`: bennu drop-in and FEP (design, ww-sh4)

Status: DESIGN for approval. Nothing built. Follows `2026-11-18-fd-clients-design.md` (ww-7mw.1), whose `<mode>` switch, command interface
and tag rules this reuses. Everything stock-side below is read from source and is **UNVERIFIED on the wire** until the capture bead
(see Testing) has run; where the source and the protocol plan disagree, it says so. The plan's rows are 10 and 11 (the task text
said 7-9; row 7 is the command interface, built, and 8-9 are the Modbus and DNP3 clients, built).

## Why

`fd_client.mako` emits four client elements: `<modbus-client>`, `<dnp3-client>`, `<bacnet-client>` and `<iec60870-5-104-client>`
(`sceptre-phenix-apps/.../templates/fd_client.mako:15-28,95-104`). ferrofire loads the first two. A range that has a BACnet or IEC 104
RTU, and an fd-client or FEP in front of it, cannot swap that client for ferrofire, and ferrofire cannot act as an FEP for those
protocols. Unlike Modbus and DNP3, there is **no IEC 104 or BACnet codec in the tree at all**: `proto-iec104` and `proto-bacnet` are
Cargo features with nothing behind them (`src/fd/proto/` has `modbus`, `dnp3`, `iccp`, `iec61850`, `mqtt`, `scpi`). So this is two
protocol stacks from zero, and the first question is order (last section).

## Goals, non-goals, anti-goals

- Goal: a `fd_client.mako` config with either element loads unchanged and behaves as stock bennu (`bennu` mode, default), on the wire
  and on the command interface.
- Goal: `fep` mode, the same switch as the Modbus and DNP3 clients: poll into tags, write local output-tag changes down, no echo.
- Non-goal: servers (plan rows 5-6, their own beads), serial, BACnet MS/TP, BBMD/foreign-device registration beyond what stock's
  `BACNET_*` environment does, 104 over TLS, segmentation, COV, redundancy groups.
- Anti-goal: `bennu` mode differing from stock on the wire or command interface outside a divergence listed in `conformance/README.md`.
- Anti-goal: copying GPL source. lib60870-C (GPL-3) and bacnet-stack (GPL-2 plus linking exception) are the stock implementations; they
  are read for behaviour and used as test peers inside the stock image only.

## What `fd_client.mako` emits

Per protocol present in `server_configs`, one client element holding one connection element per server:

| | `<iec60870-5-104-client>` | `<bacnet-client>` |
|---|---|---|
| element keys | none, then `<command-interface>tcp://<ep>:1330</command-interface>` | `<scan-rate>5</scan-rate>` first, then `<command-interface>...:1332` |
| connection | `<iec60870-5-104-connection>`: `<endpoint>tcp://ip:2404</endpoint>` | `<bacnet-connection>`: `<endpoint>udp://ip:47808</endpoint>`, `<instance>` = the server config's counter |
| points | the four point kinds (binary and analog, input and output) with `<address>`, `<tag>` (tag `var_<cfg>_I<n>` / `_O<n>`) | the same four |

Two generator facts to honour: `<scan-rate>` is **client-level and mandatory** for BACnet (a missing one aborts the client's setup in
stock), and the 104 client's command-interface port is **1330, the same as the DNP3 client's**, so one device with both clients
collides (stock too; ferrofire warns at load, an error under `--strict`). The `</modbus-tcp-client>` close tag the template emits for
Modbus is already tolerated by the loader; the 104 and BACnet close tags are correct.

## Stock bennu, per protocol (source: sceptre-bennu `src/bennu/devices/modules/comms/{iec60870-5,bacnet}/module`)

Both clients share `CommsClient` and the command interface already reproduced in `src/fd/cmdif.rs` (QUERY/READ/WRITE, `ACK=`, `ERR=`,
stock's crash cases). READ answers from a **local cache per tag** (initially 0/false), updated by incoming data and optimistically by
writes. Only the cache fill and the write path are protocol specific.

### IEC 60870-5-104 (lib60870-C 2.3.1, `iec60870-5/module/ClientConnection.cpp`, `DataHandler.cpp`)

- Config: `endpoint` (only `tcp://`; anything else prints `Unknown endpoint protocol` and **`exit(-1)`**); the four point kinds, `address`
  read as uint16, `tag`. Connections are started while parsing, so an earlier one is already running if a later one is malformed.
- Session: `CS104_Connection_create(ip, port)`, **originator address 3**, common address (CA) **1**. On connect: `STARTDT act`, then
  immediately a general interrogation `C_IC_NA_1` (type 100, COT 6 activation, QOI 20 station), **once**. No polling after that: stock
  relies on the server's reverse-poll (`rpoll-rate`) to keep the cache fresh. **A failed connect prints an error and the client carries
  on, never retrying**: the cache stays 0 and writes go nowhere, yet the command interface still answers `ACK=`.
- Decode: only `M_ME_NC_1` (13, float) into analog points and `M_DP_NA_1` (3, double point) into binary points, matched by IOA (low 16
  bits) in the binary or analog map, regardless of CA, COT or the point's kind. Double point OFF(1) gives false, ON(2) true, 0 and 3 log
  "indeterminate" and give false. Everything else is dropped silently, so: against a `single` server (`M_SP_NA_1`, type 1) binaries
  **never update**; against a `double` server the GI reply (type 3) updates them **once** and the periodic reverse-poll (`M_DP_TB_1`,
  type 31) never does. Analogs update on the GI reply and every periodic `M_ME_NC_1`. (Plan Q12 says the first half; the second half
  follows from SPEC 4.3 and this decode and is new.)
- Maps: the two address-to-tag maps are keyed by IOA only, so a binary-input and a binary-output on one IOA share a slot and the later
  one wins; both tags still get their own cache.
- Write binary (any binary tag, input or output, the kind is not checked): `C_DC_NA_1` (46), COT 6, CA 1, IOA, `DOUBLE_POINT_OFF` (1) /
  `ON` (2), **select = true**, qualifier 0. The select bit means "select, do not execute"; stock's own server ignores it (SPEC Q7) so the
  pair works, but a compliant RTU would not act. Local cache then set to the written value. Write analog: `C_SE_NC_1` (50), COT 6, the
  value as float32, **select = true**, qualifier 0, cache set. Both return success whatever the send did.
- Console lines (`RECVD ASDU type`, `IOA: .. value: ..`) are stdout, not wire or command interface; not replicated.
- APCI timers and windows (t0-t3, k, w) and exactly what the library sends around STARTDT (does the GI go out before `STARTDT con`?
  when does it send S-frames and TESTFR?) are library behaviour, **not in sceptre source**: they come from the capture.

### BACnet/IP (bacnet-stack, `bacnet/module/ClientConnection.cpp`, `.../protocol/bacnet/basic/object/bacnet.c`)

- Config: `<scan-rate>` (uint32 seconds, 0 is a busy loop), per connection `endpoint` (`udp://ip:port` only, else **`exit(-1)`**) and
  `instance` (the server's device id), the four point kinds with `address` = object instance. Local device instance is 1, 2, ... per
  connection, in order.
- Start: writes the endpoint into a file `address_cache` in the cwd (keyed by the remote instance), initialises the stack from
  `BACNET_IFACE`, `BACNET_IP_PORT` (default 47808, so the client **binds 47808** itself), `BACNET_BBMD_*`, then binds the remote device.
  The plan says a Who-Is goes out; the source says the freshly written cache entry is found first, so **a Who-Is may never be sent**
  ("Found address in cache" is the other branch). The capture decides. A bind failure is **`exit(-1)`**; the file is deleted on exit.
- Poll: one thread per connection, serial, one request in flight: for each binary point then each analog point (address order),
  `ReadProperty` `Present_Value` (property 85), waiting up to 100 ms for the ack, then sleeps `scan-rate`. Errors print and go on.
- **Object type quirk**: a point reads and writes `BINARY_INPUT`/`ANALOG_INPUT` only when its stock register type is `eValueReadOnly`.
  Analog inputs are, binary inputs are `eStatusReadOnly`, so **every binary point, input or output, addresses a `BINARY_OUTPUT`
  object**. (Source reading; a Wireshark trace settles it.)
- Decode: the ack's Present_Value goes to the cache of the connection **last bound** (`Target_Device_Object_Instance` is a process
  global overwritten by each `BacnetBindToDevice`), so with two connections the first one's acks land in the second's cache. Binary reads
  take `value.type.Boolean` out of an ENUMERATED union, which works on little-endian for 0 and 1. Analog reads take REAL.
- Write: `WriteProperty` Present_Value, **no priority**, binary as ENUMERATED 0/1, analog as REAL passed through text
  (`std::to_string(double)`, so six decimals). On failure the stock error text comes back as `ERR=Failed writing tag ...`. On success
  the cache is set; for binary it is set to the **lookup status (always true), not the written value**, so a write of false still reads
  back true until the next poll. A stock bug; replicated in `bennu` mode (the command interface shows it), fixed in `fep`.
- Process-global C state (one local device instance, one UDP socket, one target) means multiple connections work only partly in stock.
  Which of that to replicate is decided from a two-server capture, not from this reading.

## Approaches

**Codecs.**

| | IEC 104 | BACnet |
|---|---|---|
| A. own | APCI (6 bytes; U, S, I frames), ASDU header, six ASDU types (1, 3, 13, 31, 45/46/50, 100), t1/t2/t3, k/w. Small and fully under our control, which stock quirks (originator 3, select bit, decode filter) need | BVLC, NPDU, APDU (confirmed request, simple ack, complex ack, error, reject, abort), tag/value encoding, Who-Is/I-Am, RP, WP, invoke ids, address handling. The client subset is about ten encoders |
| B. crate | `iec104` 0.6.0 (MIT, 2026-10-24, 3.0k downloads, "API still evolving"); `fegrid-iec60870-cs104` 0.1.0 (44 downloads, 2026-10-03) | `bacnet-client` 0.15.0 and `bacnet-server` 0.15.0 (MIT, 2026-10-29, 12.1k and 7.4k); `bacnet-rs` 0.3.2 (MIT OR Apache-2.0, 91k, "not production-ready" per the plan) |
| excluded | `lib60870`/`lib60870-sys` (GPL-3), `iec60870-5` (paid) | `bacnet-stack` port (GPL-2 plus exception), `embedded-bacnet` (client only, not a peer) |

(crates.io figures re-fetched 2026-11-18; the licence column is the registry's, unread beyond that. Plan section 3 has the rest.)

Recommendation, **for the user to choose**: **IEC 104 own (A)**, the DNP3 choice again. The client needs few types, the stock behaviour is
exactly the parts a crate would hide (originator, select bit, only two decoded types), and the codec is shared with the row 5 server.
**BACnet: one 0.5-day spike, shared with row 6, decides for both roles**, with a client-specific pass bar: the crate must produce
ReadProperty and WriteProperty requests byte-equal to the stock capture (invoke id, max-APDU and segmentation bits, no priority, ENUMERATED
and REAL encodings) and let us bind an `endpoint` address without Who-Is. I expect that to fail on the bytes and fall to own for the
client encoders, but the spike is cheaper than guessing. Either way the BACnet unknowns are in the capture, not the crate.

**Where the mode switch lives.** Settled by ww-7mw.1: a `<mode>` child of each client element, `bennu` default, any other value an error.
Not reopened.

## Config

The bennu schema unchanged, plus the existing ferrofire-only keys (`src/fd/proto/client.rs`, documented in
`docs/field-device-clients.md`):

| Key | Where | Meaning here |
|---|---|---|
| `<mode>` | both client elements | `bennu` or `fep` |
| `<status-tag>` | both connection elements | `fep` only: binary tag, true while the session is up and the last poll succeeded |
| `<scan-rate>` | **104** connection | `fep` only (new, seconds, default 1): how often a general interrogation is re-sent. In `bennu` mode ignored with a WARN, as stock has none |
| `<scan-rate>` | **BACnet** client | stock key (integer seconds, required); in `fep` it is the poll period and decimals are accepted |

The 104 `<scan-rate>` is connection-level like Modbus's, so `client::fep_keys` serves it as is. The BACnet one is client-level in stock, so
its parse is the one new shape: read at the element, handed to each connection. Point tags are device-local (`client::point_tag`), as
in the Modbus and DNP3 clients.

## Behaviour

**`bennu` mode** copies the section above, quirks included. Per connection one task; nothing blocks the scan cycle or another
connection. What follows from stock and is therefore *not* better than stock: 104 never reconnects after a failed connect; BACnet
polls serially with a 100 ms ack wait; neither client retries a write.

**`fep` mode:**

- 104: connect with `proto::backoff`; STARTDT; GI at connect and every `<scan-rate>`. The GI returns every point, outputs included, so
  each output gets a last polled value for the echo rule for free. The decode accepts all of `M_SP_NA_1`, `M_DP_NA_1`, `M_ME_NC_1`,
  `M_DP_TB_1` (the stock filter, Q12, is a `bennu`-only quirk), and periodic ASDUs update tags as they arrive. Writes down send `C_DC_NA_1`
  and `C_SE_NC_1` with **select = false** (execute), since a select-only command does nothing on a real RTU.
- BACnet: bind by Who-Is (directed at the `instance`) so a device that moved is found again after a failure, not the cache file; no file
  is written. Poll with ReadProperty of every point (binary points by their real object type: input points read `BINARY_INPUT`), and the
  outputs too. ReadPropertyMultiple batching is a later optimisation, not part of this work. Writes down are WriteProperty without a
  priority (the standard default, 16). Reconnects use `proto::backoff`.
- Both: polled values enter tags through the store's queued writes; a local output-tag change goes down once per device cycle under the
  existing `Echo`/`Out` rule (sent only if different from the last value sent and the last value polled); a failed poll keeps the last
  tags and clears `<status-tag>`; the command interface in `fep` is `TagPoints`.

## Testing (the `conformance/modbus-client` pattern)

- **Capture first** (one bead per protocol, 0.5-1 day, before any code): stock fd-client in `ffc-stock-2026.11.03:22.04` against a stock
  server, through `conformance/run.sh` as `modbus-client` is. 104: stock `iec60870-5-104-server` under `stock/servectl.py` (so peer.sh can
  stop and start it), once with `subtype` single and once double, so the decode filter is observed, not inferred. BACnet: stock
  `bacnet-server` beside it, two servers for the multi-connection question. Outputs: `stock/client-iec104.{xml,golden,pcap}`,
  `stock/client-bacnet.*`, recorded by `run-stock.sh iec104-client|bacnet-client` (two more cases in its `modbus-client|dnp3-client` branch).
  This also answers, and corrects the plan's rows 10-11 where they differ: Who-Is or not, which BACnet object types are hit, what the
  104 client sends around STARTDT, the APCI timers, what a dead server looks like.
- **Per protocol dir** `conformance/iec104-client`, `conformance/bacnet-client`: `proto.env`, `peer.sh`, `check.sh` as in
  `modbus-client`. `peer.sh` drives `stock/probe_cmdif.py` against the DUT while the server is up, stopped, and restarted; `check.sh` wants
  the command-interface transcript and the wire (decoded by tshark's `iec60870_104` and `bacnet` dissectors, filters confirmed in the
  capture bead) equal to the stock golden, with sequence numbers, invoke ids and clock fields masked. `DUT=stock` records, `DUT=rust`
  checks. Same cross-pair as rows 10-11 say: our client against the stock server, and (once rows 5-6 exist) the stock client against ours.
- **Config:** a `fd_client.mako` config for each protocol loads; `<mode>` errors, the 104 `<scan-rate>` WARN, the 1330 clash warning
  and BACnet's missing `<scan-rate>` are tested.
- **FEP:** ferrofire FEP polling a ferrofire RTU and re-serving through its own server: needs rows 5-6, or the stock server as the peer
  until they land. Write through the FEP's server reaches the RTU with no echo; RTU stopped, `<status-tag>` false and backoff;
  restarted, recovers.
- **Robustness:** fuzz the response parsers in the existing layout; a response from an unreachable or hostile server never panics.
- **No regression:** `make conformance` and the benches as in ww-7mw.1.

## Sizing

Plan rows 10 and 11 say 1.5-2 days each and assume the server codec exists. The estimates, by order:

| Piece | Servers first | Clients first |
|---|---|---|
| stock capture, 104 and BACnet | 1 total (the server capture bead, rows 5-6, already has to do it for the servers; client capture adds half a day each) | 1.5-2 total |
| 104 codec | in row 5 (2-3 d) | +1.5 here |
| BACnet codec and crate spike | in row 6 (4-5 d incl. spike) | +3 here (client subset) |
| 104 client | 1.5-2 | 1.5-2 |
| BACnet client | 1.5-2 | 1.5-2 |
| extra: `<scan-rate>` for 104, mako-port warning, fuzz, docs | 0.5 | 0.5 |
| **Total for the two clients** | **~5-6.5 after the servers** | **~10-12, and most of the codec is then redone for the servers** |

Totals over servers plus clients come out about equal (the codec has to be written once either way), so order is about where the risk
lands, not days saved.

## Should the servers come first? Yes

Recommend rows 5-6 first, then this bead as one follow-on:

- The shared codec is written once, in the row that has a stock golden and a stock peer to byte-diff it against, then reused. Clients
  first would build it for the client subset (no server-side reverse-poll, no object model, no 1 Hz refresh) and then extend it.
- The client conformance and the FEP test need a ferrofire server on the far end for the `rust-vs-rust` and "stock client reads ours"
  legs; without servers they have only the stock peer.
- The BACnet crate spike (row 6) answers the client question too, so doing the client first either runs the spike twice or skips it.
- Against: the clients are cheaper (5-6.5 d against 6-8 for the two servers), and they alone let ferrofire replace a stock fd-client on a range today.
  If that is wanted sooner than the servers, do the 104 client first (small, own codec designed to be shared, no spike) and leave BACnet.
  The cost is a codec written with the server in mind that nobody has checked against a server.

## Left to the builder

- Module layout: `src/fd/proto/iec104/{codec,client}.rs` and `src/fd/proto/bacnet/{codec,client}.rs` (the `modbus/` shape), with the
  bennu-mode cache split from the `fep` session as `modbus/client.rs` does it.
- BACnet binding and invoke-id details, beyond the capture; 104 timer values, from the capture.
- Which of stock's multi-connection BACnet behaviours to replicate, after the two-server capture; each divergence lands in
  `conformance/README.md`.
- Log wording and event-log rows (follow the server modules).

## Open questions for the user

1. Order: servers first (recommended), or the 104 client first?
2. IEC 104 codec: own (recommended) or `iec104` 0.6.0?
3. BACnet: the shared spike decides (recommended), or go own now?
4. `fep` write-down uses the select bit cleared (execute) for 104 commands. Agreed, or keep stock bytes in both modes?
