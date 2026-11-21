# `<modbus-client>` and `<dnp3-client>`: bennu drop-in and FEP (design, ww-7mw.1)

## Why

ferrofire serves Modbus/TCP and DNP3 but cannot poll another device, so it can neither replace a bennu fd-client on a
range nor act as a front-end processor (FEP) or gateway in front of RTUs or hardware. The comparison with bennu and ot-sim
scores it down for exactly this ("no Modbus client, no DNP3 master, so no FEP role"). The user wants both behaviours, with
a setting that switches between them.

## Goals, non-goals, anti-goals

- Goal: a sceptre-generated fd-client config (`fd_client.mako`) loads unchanged and behaves as stock bennu on the wire and
  on the command interface (`bennu` mode, the default).
- Goal: `fep` mode polls remote devices into ferrofire tags, so `<logic>` and ferrofire's own servers use and re-serve them,
  and local changes to output tags are written down to the remote device.
- Goal: one setting per client element switches the mode.
- Non-goal: serial endpoints (MBAP over serial, DNP3 serial). `net.rs` refuses serial today; that is F6, a follow-up bead.
- Non-goal: Modbus RTU framing with CRC, IEC 104 and BACnet clients, HELICS (ww-7mw.4), out-of-process modules (ww-7mw.3).
- Anti-goal: `bennu` mode differing from stock bennu on the wire or on the command interface, outside a listed divergence.
- Anti-goal: an echo loop in `fep` mode, where a polled value is written straight back to the device it came from.
- Anti-goal: a slow or dead remote device stalling the scan cycle or another connection.

## Approaches

- Codecs. A: own, reusing the Modbus server's MBAP codec and the DNP3 outstation's link, transport and application layers
  (the plan's choice, `docs/field-device-protocols-plan.md` section 3: MIT, quirks under our control). B: `tokio-modbus`
  for the client; Step Function's `dnp3` crate is restrictively licensed, so the master would be ours either way.
  **Chosen by the user: A.**
- Where the switch lives. 1: a `<mode>` child of each client element. 2: a device-wide `--client-mode` flag. 3: implicit,
  `fep` when a Modbus connection has a `<scan-rate>`. **Chosen: 1** (my call, proposed and not objected to): configs
  sceptre generates today default to `bennu`, and the phēnix `Ferrofire` simulator type (ww-7mw.2) can set it per device.

## Scope

Three rows of the protocol plan, built to its specs, plus the mode:

- Row 7, the command interface (ZMTP REP at `<command-interface>`), which both clients need. Exact replies as row 7 says:
  `QUERY=` gives `ACK=t1,t2,`; `READ=t` gives `ACK=t:v` (`%f`, 6 decimals; `true`/`false`); `WRITE=t:v` gives
  `ACK=Wrote tag t -- v`; anything else gives `ERR=Unknown command type (must be QUERY|READ|WRITE)`. Replies are
  NUL-terminated and the op is case-insensitive. The ZMTP REP side reuses `zeromq` (already a dependency).
- Row 8, the Modbus client, and row 9, the DNP3 master. Their config schema is exactly the one in those rows, including the
  loader tolerating `</modbus-tcp-client>`.

## Config

The bennu schema, unchanged, plus these ferrofire-only keys:

| Key | Where | Default | Meaning |
|---|---|---|---|
| `<mode>` | `<modbus-client>`, `<dnp3-client>` | `bennu` | `bennu` or `fep`; any other value is a config error naming it |
| `<scan-rate>` | `<modbus-connection>` | 1 (s) | `fep` only: seconds between polls. In `bennu` mode it is ignored with a WARN (an error under `--strict`), as stock has none |
| `<status-tag>` | `<modbus-connection>`, `<dnp3-connection>` | none | `fep` only: a binary device-local tag, true while that connection's session is up |

Point tags are registered as device-local tags (plan line 85), so `<logic>` and the servers on the same device see them.

## Behaviour

**`bennu` mode** copies stock, quirks included, per rows 8 and 9:

- Modbus: no polling. A command-interface `READ` is one live FC 1/2/3/4 of that register. A `WRITE` is FC5 or FC6, scaled
  by the point's range. Unit id defaults to 0.
- DNP3: startup integrity poll (Class 0), the unsolicited mask set to Class 0, and unsolicited not disabled. Scans run at
  `scan-rate` or per class, with 0 meaning off. The SOE handles Binary and Analog only. CROB is SelectAndOperate when
  `sbo`, else DirectOperate. Analog outputs are always AnalogOutputFloat32. The local cache updates optimistically.

**`fep` mode:**

- Modbus polls each connection every `scan-rate`. Within each table it merges contiguous addresses into one request, up to
  125 registers or 2,000 bits. Polled values go into their tags through the store's queued writes, so they take effect at
  the end of a scan cycle like any protocol write. A command-interface `READ` answers from the tag.
- DNP3 scans as in `bennu` mode, and its results go into the tags.
- Writes down: when an output point's tag (coil, holding register, DNP3 binary or analog output) changes locally, the
  client sends it, the same way `<scpi-client>` sends a `<set>`. **Echo rule:** a value is sent only when it differs both
  from the last value sent and from the last value polled in for that point.
- A failed poll leaves the tags at their last value and sets `<status-tag>` false. Reconnects use `proto::backoff`.

**Both modes:** one task per connection. A client never blocks the scan cycle or another connection.

## Details (my calls, settled after the builder check)

- **Stock is the truth.** Client behaviour in the plan is source-only and unverified on the wire. Capturing the stock goldens
  is part of this work: stock fd-client containers built from `ffc-stock-2026.11.03:22.04` against stock and ferrofire
  servers, through `conformance/stock`. Where the wire differs from the plan or the source, match the wire and correct the
  plan's row. `bennu`-mode errors, timeouts, exceptions, connection handling (persistent or per command) and the
  command-interface replies for them follow stock as captured. Source: `ClientConnection.cpp` and `DataHandler.cpp` in
  sceptre-bennu, as the rows cite.
- **Tags.** Point tags are device-local in both modes, with kinds from the point type. Their initial value is 0 or false,
  the store's zero. In `bennu` mode the tag holds what stock's client map holds (the last value read or written, or
  DNP3's optimistic cache), so the command interface is unchanged. A stock fd-client has no `<logic>`, so seeing those tags
  there changes nothing stock can observe. The same tag in two connections, or in a client and an `<output>`, is a WARN
  with the first winning (an error under `--strict`).
- **`fep` also polls outputs.** Modbus reads coils and holding registers as well as inputs. DNP3 adds output-status reads
  (groups 10 and 40) to its scans. This is ferrofire-only behaviour, so stock does not constrain it. It gives every output
  a last polled value for the echo rule, and gives the read-back a real FEP has.
- **Writes down in `fep`.** Each connection compares its output tags with the last value sent and the last value polled,
  once per device scan cycle (`<cycle-time>`), not once per `scan-rate`, so a write leaves within one cycle. When a poll
  and a local write race, the local write wins: it is sent, and a poll result for that point is ignored until the write is
  acknowledged or fails.
- **Modbus scaling in `fep`.** A register is unscaled through `min-value`/`max-value`, the inverse of the server's
  scaling, and a write is scaled as in `bennu` mode. A binary tag on a register reads non-zero as true and writes 1 or 0.
  `min-value` equal to `max-value` is a config error. `unit-id` defaults to 0 in both modes. On exception 02 (illegal
  address), a merged read splits once into per-point reads, and the split is remembered for that connection. Any other
  failure fails the whole read.
- **DNP3 in `fep`.** It is the same master as `bennu` mode (unsolicited enabled, Class 0 mask) plus the output-status reads.
- **`<status-tag>`** is true while the TCP session is up and the last poll succeeded. It is false before the first connect
  and after a failed poll.
- **The command interface in `fep`** is exposed. A `READ` answers from the tag. A `WRITE` queues the tag, which then goes
  down through the normal write-down path and the echo rule, and replies `ACK=Wrote tag t -- v` as stock does.
- **`<mode>`:** an unknown value is an error in every mode. `<mode>` and `<status-tag>` are documented in a new
  `docs/field-device-clients.md`, alongside the other protocol pages.

## Testing

- **Cross-pairs against stock bennu**, as rows 8 and 9 say:
  - our Modbus client against a stock `modbus-server`, and our DNP3 master against a stock outstation (the smoke test
    reversed);
  - `bennu-probe` transcripts must equal the stock-against-stock ones;
  - DNP3 link and application bytes must match a stock-master golden.
  Stock bennu (opendnp3) is the independent implementation.
- **Config:** a config generated by `fd_client.mako` loads. `<mode>` errors and the `<scan-rate>` WARN are tested.
- **FEP:** a ferrofire FEP polls a ferrofire RTU and re-serves the values through its own `<modbus-server>`. A write to the
  FEP's server reaches the RTU, and no echo follows it. When the RTU is stopped, `<status-tag>` goes false and polling
  backs off; when it is restarted, the session recovers.
- **Robustness:** fuzz targets for the client-side response parsers, in the existing fuzz layout.
- **No regression:** the fdmicro and control benches show the same numbers within noise, and `make conformance` passes.

## Left to the builder

- Module layout (for example `proto/modbus/client.rs`, `proto/dnp3/master.rs`) and where the command interface lives.
- How the merged Modbus reads are split, beyond the protocol limits.
- The log wording, and the event-log rows for client traffic (follow the server modules).
- Any divergence from stock that turns out to be needed, listed in `conformance/README.md` like the server's.
