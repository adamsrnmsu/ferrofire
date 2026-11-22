# External protocol modules (ww-7mw.3)

Date: 2026-11-21. Status: approved by the user 2026-11-21.

## Intent

Today every ferrofire protocol is a Rust module compiled into the binary (`src/fd/proto/mod.rs`: `Module` trait, `REGISTRY`, one
`proto-<name>` feature each). A new protocol means Rust, a registry line and a rebuild. This change adds a documented boundary so a
protocol can be a **separate program in any language**, started and stopped by the field device from its config, with no change to
ferrofire's core. The first program to use it is a new **IEC 60870-5-104 server and client**, written in Python on `c104`, which
ferrofire lacks today.

Why: the private comparison scores ferrofire 3 on extensibility. The rubric's 5 anchor is "a documented module or plugin boundary; a
new protocol is a separate process or crate in any language with no change to the core; existing modules show the pattern; clients
and servers both". The IEC 104 gap is also a listed ferrofire weakness.

## Goals

1. One message protocol between the device and a module, **ffmod/1**, written down in `docs/modules.md`.
2. Three transports for it (the user asked for all three):
   - **A. stdio**: ferrofire spawns the module and speaks ffmod/1 over its stdin and stdout.
   - **B. socket**: ferrofire listens on a socket and a module connects to it, started by ferrofire or by something else.
   - **C. bennu**: no ffmod/1. Instead ferrofire offers bennu's own channels at the device level: a command interface (stock
     `QUERY`/`READ`/`WRITE` wording) and change datagrams in datagram-v1 format, so an existing bennu or pybennu program can be a module.
3. `modules/iec104/`: an IEC 104 server and client in Python on `c104`, drop-in for stock bennu's `<iec60870-5-104-server>` and
   `<iec60870-5-104-client>` config elements (field-device-spec.md section 4.3, protocols plan rows 5 and 10).
4. Conformance and tests that hold the module to stock and to an independent decoder (see Testing).

## Non-goals

- Moving the existing Rust modules out of process. They stay compiled in; their latency is part of ferrofire's performance score.
- A module SDK or library in any language. The protocol is small enough to speak directly; the IEC 104 module shows how.
- BACnet. It can follow the same pattern later.
- Running modules on another host over a network with authentication. Transport B listens on loopback or a Unix socket by default.

## Anti-goals

- Any regression of the in-process modules: no new lock on the store's hot path, no change to their timing.
- A module that hangs or dies takes the device down. It must not: like a bind failure today, it logs and the rest keeps running.
- A GPL library linked into ferrofire. `c104` (GPL-3.0, wraps lib60870-C, the library stock bennu uses) stays in its own process.

## Constraints

- ferrofire is MIT and stays so. `modules/iec104/` is GPL-3.0 (user decision).
- Gates: `make fmt-check lint test crap dry deny` pass (CRAP < 10, dry < 0.8). Rust only; the Python module gets its own check.
- arm64 host, Docker Desktop. `ff-rust:22.04` has no Python; `ff-physics:22.04` has Python 3.10 and pip, and `c104` 2.2.1 has a
  manylinux aarch64 wheel.
- Config stays drop-in for stock: a stock `<iec60870-5-104-server>` element in a bennu config must load and serve with no edits.

## Approaches considered

All three were chosen, as transports of one design, rather than one picked:

| | A. stdio | B. socket | C. bennu channels |
|---|---|---|---|
| Lifecycle | ferrofire owns it | ferrofire or an outside supervisor | outside |
| Language needs | stdin/stdout | a socket | ZeroMQ (REP) + UDP multicast |
| Change stream | pushed | pushed | datagrams of the device's store changes |
| Writes | acked or rejected per write | same | stock command-interface replies |
| Where it can run | same host and container | same host; other containers with a shared socket or a TCP bind | anywhere the group and REP reach |

A is the default and the one the IEC 104 module uses. B is the same messages over a socket. C exists for bennu's own tools and
carries stock's limits (no per-write ack beyond the REP reply).

## Design

### Config

A new `<comms>` child, plus a manifest lookup so stock element names work:

```xml
<external-module>
  <name>iec104</name>                       <!-- free text, for logs -->
  <command>python3 /opt/ferrofire/modules/iec104/module.py</command>
  <transport>stdio</transport>             <!-- stdio (default) | tcp://127.0.0.1:PORT | unix:///path -->
  <config> ...the module's own XML, passed through verbatim... </config>
</external-module>
```

Drop-in: a module directory holds a `module.toml` manifest naming the stock `<comms>` tags it serves
(`serves = ["iec60870-5-104-server", "iec60870-5-104-client"]`) and its command. At load, a `<comms>` child that no compiled module
claims is looked up in the manifests under the modules directory (`--modules-dir`, default `/opt/ferrofire/modules`, also
`FERROFIRE_MODULES`). A hit is started as if it were an `<external-module>` whose `<config>` is that element. The `KNOWN` table's
"needs the `proto-iec104` feature" error then only appears when no manifest serves the tag.

### ffmod/1 messages

JSON, one object per line, UTF-8, `\n`-terminated. Field `t` is the type. Values are JSON `true`/`false` for binary points and numbers
for analog. Unknown fields are ignored (forward compatible); an unknown `t` is answered with `error` and otherwise ignored.

Device to module:
- `{"t":"hello","v":1,"device":"rtu-1","opts":{"strict":false},"config":"<iec60870-5-104-server>...</...>","points":[{"tag":"x","kind":"analog","value":2.5},...]}`:
  first message; `points` is every tag of the device with its kind and current value; `opts` carries the device flags a module may
  honour (`strict` today).
- `{"t":"values","changes":[["x",2.6],...]}`: tags whose value changed since the last `values`. No timestamps: a module that needs
  time (IEC 104 CP56Time2a) uses wall time, as stock does. Produced by a task per module that takes one `Store::snapshot()` per device
  scan cycle (`cycle_ms`, plumbed to the module the way `scan::run` gets it) and diffs it against the last one: the same cost as the
  104 rpoll and BACnet refreshers, and nothing added to the scan loop itself. (Builder-check answer; my call.)
- `{"t":"ack","id":7}` / `{"t":"reject","id":7,"err":"unknown tag"}`: the answer to a module `write`: `Store::write` queued it, or
  refused it (`UnknownTag`, `WrongKind`). Informational: a module answers its own peer at once, as stock does (stock echoes ACT_CON
  whatever happens next), and never waits on `ack`. It logs a `reject`. (My call.)
- `{"t":"stop"}`: shut down. ferrofire then closes the stream; see Lifecycle.

Module to device:
- `{"t":"ready","role":"server","uses":[{"tag":"x","kind":"analog","dir":"in"},...]}`: after `hello`; the tags the module maps, with
  `dir` `in` (the module's peer supplies it: a client's polled input, a server's command) or `out` (the device supplies it), and `role`
  `server` or `client`. ferrofire checks the tags as `Tags::mapped` does for a compiled module: wrong kind or unknown tag is a
  warning, or an error under `--strict` (then the module is stopped and the device logs why). For `role: client`, the device runs the
  client rules of `proto/client.rs` (`input_polled` for `in` tags, `Out` with the Echo rule for `out` tags: the device decides what
  goes down in `values`), so the module does not reimplement them. (Builder-check answer; my call.)
- `{"t":"write","id":7,"tag":"x","value":true}`: a value from the module's peer (a server's command, a client's polled input). The
  same path as a compiled module's write: `Store::write` for a server, the `proto/client.rs` path for a client.
- `{"t":"log","level":"info","msg":"..."}`: optional; stderr lines are logged too.
- `{"t":"error","msg":"..."}`: the module cannot run (bind failure, bad config). ferrofire logs it and does not restart it.

A server module reads values from `hello` and `values` and answers its peers from its own copy. A client module polls its peer and
sends `write`s.

### Transports

- **A. stdio**: `<command>` is split like a shell word list (no shell). stdout carries ffmod/1 only; stderr lines are logged with the
  module's name. Environment: `FERROFIRE_MODULE=<name>`.
- **B. socket**: ferrofire binds `tcp://127.0.0.1:PORT` or `unix:///path` (loopback by default; another address needs `--strict`
  off and is logged as a warning). One module connection at a time; a new connection replaces the old one and gets a fresh `hello`.
  If `<command>` is present ferrofire also spawns it, with `FERROFIRE_MODULE_ADDR` set; without it, the module is started elsewhere.
- **C. bennu**: `<external-module><transport>bennu</transport><command-interface>tcp://127.0.0.1:1340</command-interface>
  <publish-group>udp://239.0.0.1:40001</publish-group></external-module>`. ferrofire serves a device-level command interface with the
  wording of `src/fd/cmdif.rs` (QUERY lists every device tag; READ any tag; WRITE goes through `Store::write`) and multicasts every
  scan cycle's store changes (from the same per-cycle diff as `values`) as datagram-v1 bodies framed with `bennu::frame` on the
  group, as a provider's publish looks, so a pybennu subscriber reads the device's state the way it reads a provider. Direction:
  device to module only; the module's writes come in through the REP `WRITE`. QUERY lists every device tag (the device has no
  per-module mapping on this transport). No `hello`, no ack beyond the REP reply. `<command>` optional.

### Lifecycle

- Start after the store is built, like the compiled modules (`start_module`). Spawn failure or `error` logs and the device keeps running.
- Exit or broken stream: restart with `proto::backoff` (1 s doubling to 30 s), a fresh `hello` each time. `error` means no restart.
- A module that has not sent `ready` within 10 s of `hello` is stopped and restarted on the backoff.
- Stop: send `stop`, close stdin, SIGTERM after 2 s, SIGKILL after 5 s. The device's own stop never waits longer than that.
- Writes from a module that is restarting are lost, as a peer's would be on a dropped TCP session; the module's peer sees the
  restart as a reconnect.

### IEC 104 module (`modules/iec104/`)

- `module.toml`, `module.py` (one file if it stays readable), `README.md`, a `requirements.txt` pinning `c104==2.2.1`.
- Server: section 4.3 of field-device-spec.md, observed stock behaviour: CA 1, IOA from `address`, single or double points by
  `subtype`, `rpoll-rate` periodic sends with COT 1, GI with QOI 20 only, commands 45/46/50 written to the device. Quirks stock has
  (periodic send to the last connection only) are kept by default and listed in the README.
- Client command interface: a stock `<iec60870-5-104-client>` has a `<command-interface>` (port 1330). The device serves it, with
  `cmdif::serve` and a `Points` adapter over the store limited to the module's `uses`, as `TagPoints` does for fep mode; the module
  needs no ZeroMQ. (Builder-check answer; my call.)
- Client: protocols plan row 10: one GI at connect, then the server's periodic sends; decodes `M_ME_NC_1` and `M_DP_NA_1`
  (Q12: `M_SP_NA_1` too under `--strict`, passed in `hello`), writes `C_DC_NA_1` and `C_SE_NC_1`.
- Fidelity bar (my call): equal to stock on what a SCADA master sees: the points, type ids, COTs, values and command outcomes of a GI,
  the periodic sends and each command, and the quirks in section 4.3 that change those. Frame grouping (how many points per ASDU,
  where a split falls), APCI timers and queue sizes may differ if c104 does not expose them; each difference is listed in the
  module README and in conformance/README.md, as the other protocols' divergences are. Step one is a spike of c104 against
  `probe_104.py`: if c104 cannot give stock's GI content, command handling or single/double modes, the work stops there and comes back
  to the user before any other code.
- Config errors (my call): the module checks its element after `hello` and sends `error` for what stock refuses at load (missing
  `rpoll-rate` or `subtype`, a non-tcp endpoint). Without `--strict` the device logs it and runs on, as for a compiled module's bind
  failure; with `--strict`, an `error` or refused tags before `ready` make the device exit non-zero, as a load error would. Valid stock
  configs load unchanged; only the timing of a refusal differs, and the docs say so.
- Run in `ff-physics:22.04` or any image with Python 3 and the wheel; `docker/Dockerfile` does not change for the core.

## Testing

- **Rust, no Python** (in `ff-rust`): a fake module is a tiny Rust test binary or a `sh` script that echoes scripted ffmod/1 lines.
  Unit tests cover message parsing, `ready` checks (strict and not), the change diff, ack/reject, restart backoff, the ready timeout,
  stop escalation, transport B reconnect, transport C replies (golden with `cmdif`'s wording) and the manifest lookup.
- **Conformance** (`conformance/iec104/`, like the other protocol dirs), run with `ff-physics:22.04` and the stock image:
  - Differential: stock bennu's 104 client and `conformance/stock/probe_104.py` against stock's 104 server and against ferrofire with
    the module, same config; transcripts equal except documented differences.
  - Client: ferrofire's 104 client (the module) against stock's 104 server, and stock's client against ours.
  - Independent check: c104 and stock both use lib60870-C, so agreement with stock is not independence. The capture is also decoded
    with Wireshark's `iec60870_104` dissector (tshark) and checked against the expected ASDUs.
- **Bench**: a run in the existing control-latency or plumbing bench is optional; the cell to move is extensibility, not performance.

## What is left to the builder

- Crate choices for JSON (serde_json is likely already a dependency) and process handling (tokio::process).
- Exact module file layout, the README worked example, and how `docs/modules.md` is organised.
- Whether transport C's datagrams are sent from the scan loop or a separate task, as long as the hot path gains no lock.

## Decided by the user

- `modules/iec104/` is GPL-3.0 (its own `LICENSE` file in that directory); the rest of ferrofire stays MIT. `NOTICE` says so.
