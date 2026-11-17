# `<scpi-client>`: the controller side of SCPI (design, ww-otj.88)

## Goal

Model the SCPI controller/follower pattern with two ferrofire field devices, or one ferrofire controller and real instruments:

- **Follower (instrument):** a field device with `<scpi-server>` (exists).
- **Controller:** a field device with `<scpi-client>`. It polls instruments on an interval, puts the answers in tags, and sends a set
  command when a tag that `<logic>` computes changes. The control law is the existing `<logic>` block; nothing new decides anything.

SCPI instruments never speak first, so the controller is where all the traffic starts. That is the traffic a range wants to show.

## Config

```xml
<field-device>
  <name>bench-ctl</name>
  <cycle-time>500</cycle-time>
  <comms>
    <scpi-client>
      <endpoint>TCPIP::10.1.1.50::hislip0::INSTR</endpoint>
      <interval>1000</interval>
      <query><tag>psu.volt</tag><command>MEAS:VOLT?</command></query>
      <query><tag>tank.level</tag></query>
      <set><tag>psu.out</tag><type>binary</type><command>OUTP {}</command></set>
    </scpi-client>
  </comms>
  <logic>
    psu.out = tank.level &lt; 80
  </logic>
</field-device>
```

| Key | Default | Meaning |
|---|---|---|
| `<endpoint>` | required | the instrument, in `ferrofire scpi`'s target syntax: `host[:port]` (raw socket, 5025), `TCPIP::host::port::SOCKET`, `TCPIP::host::hislip0::INSTR`, `TCPIP::host::INSTR` (VXI-11). A host with a `/` (`tcp://h:5025`) is refused; `ferrofire scpi` refuses it too |
| `<portmapper>` | 111 | VXI-11 only: the portmapper port (1111 for an unprivileged ferrofire instrument) |
| `<interval>` | 1000 (50-3600000) | milliseconds from the start of one poll to the start of the next. A poll that takes longer starts the next at once; polls never overlap |
| `<timeout>` | 5000 (100-600000) | milliseconds to connect and to wait for each response |
| `<check>` | true | after each command, read `SYST:ERR?` until it says 0 and log each error. Real controllers do this, and it is traffic worth seeing |
| `<query>` | none | `<tag>`, optional `<type>` (`analog`, the default, or `binary`) and `<command>`, which must be a query (a header ending in `?`; a config error otherwise). Each poll sends the command and writes the answer to the tag |
| `<set>` | none | `<tag>`, optional `<type>` and `<command>`, a template in which every `{}` is the value (a template without one is a config error). Sent when the tag's value differs from the last value sent |
| `<event-logging>` | `scpi-client.log` | the usual per-module CSV event log |

**Tags.** A `<query>` or `<set>` tag may already exist (from `<tags>`, with its initial value). Then its kind must match `<type>` if
one is given. Otherwise the client declares it as a device-local tag of `<type>` (`Tags::add_internal`), so a controller needs no
`<tags>` block. `<logic>` is parsed after `<comms>`, so it can use these tags. A tag may be in both a `<query>` and a `<set>`:
the client reads back what it sets.

**Default commands.** With no `<command>`, the client uses the paths a ferrofire `<scpi-server>` gives the same tag name
(`tag_mnemonic`): a query is `MEAS:SCAL:<M>?` for an analog tag and `MEAS:BIN:<M>?` for a binary one, and a set is `SOUR:<M> {}`. Two
ferrofire devices therefore pair up with nothing but tag names. A real instrument needs `<command>`.

## Behaviour

One task per `<scpi-client>`. It runs on a blocking thread (the SCPI wires in `src/scpi/transport.rs` are blocking `std` sockets),
reads the store and queues writes, and never touches the scan cycle. A dead instrument therefore never delays the device.

Each poll:

1. **Sets.** For every `<set>` whose tag's value (`Store::get`) differs from the last value sent on this connection, send the command
   with `{}` replaced. Analog values are NR2 or NR3 (Rust's shortest round-trip form, with `E` for an exponent); binary values are `1`
   or `0`. A NaN or infinite analog value is not sent; that is logged once per value. A set the instrument refuses counts as sent
   (its error is logged) and is not repeated until the value changes. After a reconnect every set is sent again,
   because the instrument may have reset.
2. **Queries.** Send each `<query>` and parse the first `,`- or `;`-separated field of the answer. An analog tag takes a number; the
   SCPI sentinels `9.91E+37` (not a number) and `+-9.9E+37` (infinity) become NaN and infinity (text such as `nan` or `inf` is not a
   number). A binary tag takes `1`, `0`, `ON` or `OFF`, in any case. Anything else is logged, and the tag keeps its value. The value goes to `Store::write`, the same path as a protocol write:
   a device-local tag changes at the end of the scan cycle, and an external tag goes to the provider.
3. **Errors.** With `<check>`, the client uses `Link::run(cmd, true)` on every command: a query goes out as `QUERY;*OPC?` and the
   error queue is drained after it, as `ferrofire scpi --check` does. Each error is a line in the event log. A rejected query leaves
   its tag alone.

A send or receive failure (no connection, a closed connection, a timeout, a HiSLIP FatalError, a VXI-11 error) ends the connection.
The client logs it once (and a failed connect once, not on every retry, until a connect works) and reconnects after 1 s, doubling to 30 s while the instrument stays away, and back to 1 s after a
connection that worked. This is the same rule `<mqtt-client>` uses. Stopping the device ends the task between commands.

## Code

- `src/fd/proto/scpi/client.rs`: the module. It implements `Module`, and the `REGISTRY` and `KNOWN` entries are
  `("scpi-client", ..., "proto-scpi")`.
- Pure functions, unit-tested without a socket: parse the config into a plan; decide which sets are due (last sent versus current);
  fill a template; parse an answer for a kind.
- `run`: `spawn_blocking` around a loop over `transport::open` and a `Link<Box<dyn Wire>>`. `Link` needs a public constructor from a
  `Box<dyn Wire>` (now private, in `connect_link`).
- Tests:
  - unit tests for the pure parts and the config errors;
  - an integration test that runs a ferrofire field device with `<scpi-server>` and one with `<scpi-client>` in-process, raw
    socket and HiSLIP. `<logic>` on the controller drives a set, the instrument's store shows it, and the controller's query tag
    tracks a value written on the instrument side;
  - a reconnect test: the instrument goes away and comes back, and every set is sent again.
- Docs: a `<scpi-client>` section in `docs/field-device-scpi.md`. CHANGELOG.md is generated by releasaurus from the commit
  subjects, so it is not edited by hand.

## Not in this change

- SRQ (instrument-initiated service requests); the server does not send them either.
- Several instruments in one element: use one `<scpi-client>` per instrument.
- Ramps, sequences or scripted traffic: `<logic>` and `delay:` cover timing. A scenario that needs more is a separate tool.
- Per-query intervals: one `<interval>` per element; use two elements for two rates.
