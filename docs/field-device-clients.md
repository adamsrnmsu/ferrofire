# Field-device clients: `<modbus-client>`, `<dnp3-client>` and `<iec60870-5-104-client>`

Features `proto-modbus`, `proto-dnp3` and `proto-iec104` (all default). A field device with one of these elements polls or commands other devices.
One setting per element, `<mode>`, picks what it is:

- **`bennu`** (the default): a drop-in for the stock bennu fd-client. A config that sceptre generates (`fd_client.mako`) loads unchanged
  and the device behaves as stock on the wire and on the command interface, quirks included
  ([what stock does, and where we differ](../conformance/README.md#field-device-clients)).
- **`fep`**: a front-end processor or gateway. The client polls the remote devices into ferrofire tags, so `<logic>` and the device's own
  servers use and re-serve them, and a local change to an output tag is written down to the remote device.

The only dependency is `tokio-serial` (feature `proto-serial`, default on; no udev), for serial endpoints. The Modbus client reuses the server's MBAP codec, the DNP3 master the outstation's link, transport and
application layers, the 104 client the server's APCI and ASDU codec (`src/fd/proto/modbus/client.rs`, `src/fd/proto/dnp3/master.rs`,
`src/fd/proto/iec104/client.rs` and `client_shell.rs`, shared parts in `src/fd/proto/client.rs`), and the command interface is a `zeromq` REP socket (`src/fd/cmdif.rs`).

**Serial endpoints.** An `<endpoint>` without `tcp://` is a device path, opened by `net::connect` (`net::open_serial`, `src/fd/net.rs`) at 9600 8N1,
no flow control, raw. Modbus: MBAP framing on the port, no slave byte, no CRC, as stock (`SerialChannel.cpp`); stock stores 9600 8N1 and never applies it, so
its port runs at the OS defaults, ours at 9600 8N1; the request timeout is 1000 ms as on TCP (stock `SerialClient.hpp`). DNP3: opendnp3's `SerialSettings`
defaults, client address 1, link, transport and application layers as on TCP; its `asyncOpenDelay` (500 ms) is declared in `SerialSettings.h` and never read by
the library, so there is no delay after open. A port that cannot be opened is retried as a refused connection is. `tests/serial_clients.rs` puts a pty pair between our
client and our TCP server, and `tests/serial_servers.rs` between our TCP client and our server on the pty. Against stock, `conformance/stock/ptyrelay.py` runs
both clients and both servers on a pty in the stock image (`SERIAL=`, conformance/README.md "Serial endpoints"): ours equals stock there too.

**UDP endpoints (DNP3 master, ww-8l7).** A `<dnp3-connection>` endpoint `udp://ip:port` is a UDP socket on an ephemeral port, connected to the outstation
(`net::connect`, `Conn::Udp`): one write is one datagram (a request's link frames go out together), one read one whole datagram (the master reads into a 64 KiB
buffer, so nothing is cut). Connecting always succeeds. A datagram to a port where nothing listens comes back as ICMP port unreachable, which Linux reports on the
next send, so the master's next request or keep-alive fails and the connection is made anew with the usual backoff. `tests/dnp3_udp.rs` runs our master
against our outstation over UDP. The Modbus client refuses `udp://`.

## Config

The bennu schema, unchanged:

```xml
<comms>
  <modbus-client>
    <modbus-connection>
      <endpoint>tcp://10.0.0.12:502</endpoint>
      <unit-id>1</unit-id>
      <input-register><address>30000</address><max-value>1000</max-value><min-value>0</min-value><tag>level</tag></input-register>
      <discrete-input><address>10000</address><tag>alarm</tag></discrete-input>
      <coil><address>0</address><tag>pump</tag></coil>
      <holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>setpoint</tag></holding-register>
    </modbus-connection>
    <command-interface>tcp://0.0.0.0:1331</command-interface>
  </modbus-client>
  <dnp3-client>
    <address>1</address>
    <scan-rate>5</scan-rate>
    <dnp3-connection>
      <endpoint>tcp://10.0.0.11:20000</endpoint>
      <address>10</address>
      <binary-input><address>2</address><tag>brkr</tag></binary-input>
      <binary-output><address>5</address><tag>toggle</tag></binary-output>
      <analog-input><address>0</address><tag>power</tag></analog-input>
      <analog-output><address>1</address><tag>mw-setpoint</tag><sbo>1</sbo></analog-output>
      <class-scan-rates><all>5</all><class1>1</class1></class-scan-rates>
    </dnp3-connection>
    <command-interface>tcp://0.0.0.0:1330</command-interface>
  </dnp3-client>
</comms>
```

| Key | Where | Default | Meaning |
|---|---|---|---|
| `<endpoint>` | a connection | required | `tcp://ip:port` of the remote device |
| `<unit-id>` | `<modbus-connection>` | 0 | Modbus unit id |
| `<max-value>`, `<min-value>` | a register | -1000 to 2000 unless both are given | the range a register is scaled over |
| `<address>` | `<dnp3-client>`; `<dnp3-connection>` | required | the master's link address; the outstation's |
| `<scan-rate>` | `<dnp3-client>` | required | seconds between all-classes scans (class 1, 2, 3, 0); 0 is off |
| `<class-scan-rates>` | `<dnp3-connection>` | `all` = scan-rate, the rest off | `<all>`, `<class0>` ... `<class3>`: seconds, 0 is off |
| `<sbo>` | a DNP3 output | 0 | commands for this tag are SELECT then OPERATE instead of DIRECT_OPERATE |
| `<command-interface>` | the client | none | where `bennu-probe` and the SCEPTRE apps reach the client (ZMTP REP) |

and three ferrofire-only keys:

| Key | Where | Default | Meaning |
|---|---|---|---|
| `<mode>` | `<modbus-client>`, `<dnp3-client>`, `<iec60870-5-104-client>` | `bennu` | `bennu` or `fep`; anything else is a config error naming it |
| `<scan-rate>` | `<modbus-connection>`, `<iec60870-5-104-connection>` | 1 | `fep` only: seconds between polls (104: between GIs; decimals allowed). In `bennu` mode a WARN, an error under `--strict`: stock has none |
| `<status-tag>` | `<modbus-connection>`, `<dnp3-connection>`, `<iec60870-5-104-connection>` | none | `fep` only: a binary tag, true while that connection has a session and its last poll was answered (104: a STARTDT session whose last GI got its ACT_TERM) |
| `<type>` | a Modbus register | `analog` | `fep` only: `binary` makes the tag binary: non-zero reads true, true writes 1 |
| `<event-logging>` | `<modbus-client>` | none | a file for one line per request (stock's client keeps no log, so there is no default file) |

**Tags.** Every point's `<tag>` is declared as a device-local tag of the point's kind, starting at 0 or false, so a client config needs no
`<tags>` block and `<logic>` (read after `<comms>`) can use the points. A server on the same device can map them when the client comes
first in `<comms>`. A tag that exists already (another connection's, a `<tags>` entry) keeps its first declaration; the later mapping is
left out with a WARN (an error under `--strict`). In `fep` mode `<min-value>` equal to `<max-value>` is a config error.

## `bennu` mode

- **Modbus.** Nothing is polled. One connection per `<modbus-connection>`, opened at start and reopened only by a request that finds it
  closed. A command-interface `READ` is one live FC 1/2/3/4 of that register; a `WRITE` is FC5 (for `true`/`false`) or FC6 (for a
  number, scaled by the address's range as stock scales).
- **DNP3.** At start the class 0 integrity poll; whenever a response says the outstation restarted, a clear-restart write and another
  class 0 poll; then the scans at their rates. `READ` answers from the master's cache; `WRITE` updates the cache at once and sends a CROB
  (`true`/`false`) or an AnalogOutputFloat32 (a number).
- **IEC 104.** At connect STARTDT and one GI, back to back; the connection is made once and never again. `WRITE` updates the cache at
  once and sends a `C_DC_NA_1` (`true`/`false`) or a `C_SE_NC_1` (a number), direct execute, ACKed whatever the server answers
  ([conformance/README.md](../conformance/README.md#iec-60870-5-104)).
- **Tags** hold what stock's client holds (the last value read or written; for DNP3 the cache), from the end of the next scan cycle.

## `fep` mode

- **Modbus** polls each connection every `<scan-rate>`, inputs and outputs. Contiguous addresses of a table are one request, up to 125
  registers or 2,000 bits. A merged read the device refuses with exception 2 is read address by address from the next poll on. A register
  is unscaled through its own range; a write is scaled and clamped to it.
- **DNP3** scans as in `bennu` mode and, after each scan, reads the output status (groups 10 and 40, the analog outputs as float32,
  g40v3). An input value goes into the input's tag and an output status into the output's, and **only the status read feeds the
  outputs**: a class scan of stock v2026.11.03 carries them too (as g10v2 and an integer g40v1), and a second poll in one scan would use
  up the grace after a write. An outstation that never answers the status read is not asked again on that connection (a warning, once).
  An outstation that reports its outputs as inputs (ferrofire's own before ww-270, stock's 2026-11-07 image) is understood too: an input
  value at an index that only an output has goes to that output. (In `bennu` mode the cache is stock's: one address space per kind,
  where an input and an output cannot share an address.)
- **IEC 104** sends a GI (station, QOI 20) every `<scan-rate>`, one at a time, after a write-down at connect. The GI's data and every
  periodic or spontaneous ASDU feed the tags, single and double points (with or without time) and floats alike: Q12 is a `bennu` mode
  quirk only. An input and an output of one kind share one address space (as the server's): the later point at an address wins.
  Writes go down as direct execute: a binary as `C_DC_NA_1` when the last type-1 or type-3 ASDU at COT 20 (interrogated) was double
  points (`M_DP_NA_1`), else `C_SC_NA_1` (also before the first GI); a number as `C_SE_NC_1`. A command counts as answered when its
  ACT_CON comes, or any reply to it with the negative bit set (COT 44-47 too), one per point at a time; a refused command is not
  repeated. A refused GI (negative) ends it with the status false.
- **Writes down.** Once per `<cycle-time>` each connection looks at its output tags (coils, holding registers, DNP3 and 104 outputs) and sends
  the ones that changed locally: a `<logic>` assignment, a write to one of the device's servers, a command-interface `WRITE`.
  Modbus changes found in the same pass go down as one FC15 (coils) or FC16 (holding registers) per run of contiguous addresses, merged
  as the reads are (up to 1,968 coils or 123 registers); a lone change is FC5 or FC6. A run the device refuses with exception 1 or 2 is
  written address by address from the next cycle on. The echo of any write (function, address, quantity or value) must match the request,
  or the connection is dropped and the change is sent again; `bennu` mode checks the function only, as stock does.
- **The echo rule.** A value is sent only when it differs both from the last value sent and from the last value polled in for that point,
  so what a poll brought is never written back. Values are compared as the wire carries them (a register after scaling, a DNP3 analog
  output as a float32): a setpoint a register or a float32 cannot hold exactly is sent once, and the tag keeps the value that was
  asked for. What the device reports back in another form is a different value, and a form that cannot hold the setpoint shows it
  changed: an outstation that reports an analog output's status only as an integer (the older stock and ferrofire, as inputs, g30v1)
  shows 42 for a setpoint of 42.5, the tag takes the 42, and logic that keeps assigning 42.5 sends it again after each scan. DNP3
  outputs read as g10/g40 are not affected (the read asks for a float32); for the others use whole numbers, or do not hold such a
  setpoint from logic. A local change wins over a poll that arrives at the same time; the poll result for that
  point is ignored until the write is answered. The value a tag starts with (0 or false) counts as sent, so a FEP does not open by
  zeroing every output.
- **A device that is away.** A request has 1000 ms (104: a command's ACT_CON, a GI's ACT_CON; a GI under way gets 1000 ms from each of its frames to the next, so a large
  station may take longer than that in all; one that is late drops the connection). A failed poll leaves the tags at their last values and turns `<status-tag>` false; the next attempts come 1, 2, 4 ... s
  apart (Modbus up to 30 s, DNP3 up to 60 s as in `bennu` mode, 104 up to 60 s). Local changes made meanwhile are not
  queued one by one: when the device returns, the newest value of each changed tag is sent, before anything is read. A write the device
  answers but refuses (an exception, a DNP3 status other than success, a negative ACT_CON) is not repeated; a later poll puts the device's value back. The first poll after a write is not believed when it still shows
  another value (a device may answer a write before its read-back changes).
- **What is never sent.** NaN (a WARN). A number past a register's range is clamped to the range's end.
- **The command interface** answers `READ` from the tag and takes a `WRITE` to an output of the tag's kind (`true`/`false` for a binary
  tag, a number for an analog one); it goes down like any other local change.
- Each connection has its own task: a slow or dead device stalls neither the scan cycle nor another connection.

## A gateway

A FEP that polls a Modbus RTU and re-serves its points over DNP3, with a status point and one line of logic:

```xml
<SCEPTRE>
  <field-device>
    <name>fep-1</name>
    <cycle-time>100</cycle-time>
    <comms>
      <modbus-client>
        <mode>fep</mode>
        <modbus-connection>
          <endpoint>tcp://10.0.0.12:502</endpoint>
          <unit-id>1</unit-id>
          <scan-rate>0.5</scan-rate>
          <status-tag>rtu-2-up</status-tag>
          <input-register><address>30000</address><max-value>1000</max-value><min-value>0</min-value><tag>level</tag></input-register>
          <coil><address>0</address><tag>pump</tag></coil>
        </modbus-connection>
      </modbus-client>
      <dnp3-server>
        <endpoint>tcp://0.0.0.0:20000</endpoint>
        <address>10</address>
        <analog-input><address>0</address><tag>level</tag></analog-input>
        <binary-input><address>1</address><tag>rtu-2-up</tag></binary-input>
        <binary-output><address>0</address><tag>pump</tag></binary-output>
      </dnp3-server>
    </comms>
    <logic><![CDATA[
      pump = pump && (level < 900)
    ]]></logic>
  </field-device>
</SCEPTRE>
```

A DNP3 master that operates binary output 0 sets `pump`; within a cycle or two the FEP sends FC5 to the RTU. The RTU's level arrives
every half second and is served as analog input 0. When the level passes 900 the logic clears `pump`, and that goes down too.

## Tests

`tests/modbus_client.rs`, `tests/dnp3_master.rs` and `tests/iec104_master.rs` (the stock transcripts replayed in one process, the framing
rules a transcript cannot show), `tests/iec104_fep.rs` (the 104 FEP against our server; by hand against the stock server image),
`tests/fd_fep.rs` (whole devices: gateway, write-down without echo, a peer that is down or silent, a write while it is down, NaN and
range ends, exception 2), `tests/cmdif_net.rs`, unit tests beside the code, fuzz targets `fd_cmdif`, `fd_modbus_client` and `fd_dnp3_master`, and
against stock bennu: `make conformance PROTO=modbus-client`, `PROTO=dnp3-client` and `PROTO=iec104-client` ([conformance/README.md](../conformance/README.md#field-device-clients)).
