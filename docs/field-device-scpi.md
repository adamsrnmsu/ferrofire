# SCPI instrument (`<scpi-server>`) and the `ferrofire scpi` client

Feature `proto-scpi` (on by default, like `proto-mqtt`: the crap and mutate gates build the default features only). The field device
presents its point map as an SCPI-99 instrument over three transports that feed one parser and one device: the raw TCP socket of LXI
instruments (port 5025, ASCII, one message per line), HiSLIP (IVI-6.1, port 4880) and VXI-11 (ONC RPC: portmapper and core channel). No
dependency was added: SCPI is line-based text, HiSLIP and ONC RPC/XDR are small binary protocols, and all three are ours
(`src/scpi/syntax.rs`, `src/scpi/hislip.rs` and `src/scpi/rpc.rs` shared with the client; `src/fd/proto/scpi/`). Bennu has no SCPI, so
there is no stock behaviour to match; the config keys below are ferrofire's. PyVISA opens `TCPIP::host::hislip0::INSTR` and
`TCPIP::host::INSTR` as well as `::SOCKET`, and `lxi scpi` works without `--raw`.

## Config

```xml
<comms>
  <scpi-server>
    <endpoint>tcp://0.0.0.0:5025</endpoint>
    <manufacturer>Acme</manufacturer> <model>PA-100</model> <serial>0042</serial>
    <point><tag>level</tag></point>
    <point><tag>pump</tag><writable>true</writable></point>
    <point><tag>setpoint</tag><writable>true</writable><min-value>0</min-value><max-value>100</max-value><default-value>50</default-value></point>
  </scpi-server>
</comms>
```

| Key | Default | Meaning |
|---|---|---|
| `<endpoint>` | `tcp://0.0.0.0:5025` | the raw socket: `tcp://ip:port` |
| `<hislip-endpoint>` | `tcp://0.0.0.0:4880` | HiSLIP; `none` turns it off |
| `<vxi11-endpoint>` | `tcp://0.0.0.0:111` as root, else `tcp://0.0.0.0:1111` | the VXI-11 portmapper, on TCP and UDP; `none` turns VXI-11 off. 111 needs privileges, which a range image has (the device runs as root); unprivileged it is 1111, and a client must be told (`ferrofire scpi --portmapper 1111`; VISA and `lxi` always ask 111) |
| `<vxi11-core-endpoint>` | `tcp://0.0.0.0:4881` | VXI-11's core channel, where the portmapper sends clients (port 0 picks one) |
| `<status-scope>` | `instrument` | `instrument`: one error queue and one set of status registers shared by every session of every transport, as on a real instrument; `connection`: each connection, HiSLIP session and VXI-11 link has its own, starting clean |
| `<manufacturer>`, `<model>`, `<serial>` | `ferrofire`, the `<field-device>` `<name>`, `0` | the first three fields of `*IDN?`; no comma or control character. The fourth is the ferrofire version |
| `<max-connections>` | 16 (1-4096) | at the limit a new client is accepted and closed at once; the others are not affected. One limit for all transports: a HiSLIP session holds two connections, VXI-11 holds one per client and its links count against the same number |
| `<max-line>` | 1024 (64-65536) | longest line, without its terminator. A longer one is error -363 and is thrown away up to its newline |
| `<idle-timeout>` | 300 (0-86400) | seconds of silence after which a client is dropped; 0 = never. A silent client holds a place, not the server. A HiSLIP session is judged by its synchronous channel |
| `<point>` | none | a tag to serve: `<tag>` (must exist; an unknown one is warned about and skipped, an error under `--strict`), `<writable>` (false), `<min-value>` / `<max-value>` (limits for analog writes), and below `<default-value>`, `<read-path>`, `<write-path>` |
| `<event-logging>` | `scpi-server.log` | the usual per-module CSV event log (bind and accept failures) |

### Tags as commands

A tag is read with `MEASure:<KIND>:<TAG>?` where `<KIND>` is `SCALar` for an analog tag (the SCPI-99 `MEASure:SCALar` form) or `BINary` for a binary tag, and a writable tag is set with `SOURce:<TAG> <value>`
and read back with `SOURce:<TAG>?`. `<TAG>` is the tag name made into a mnemonic: letters and digits are upper-cased, anything else
becomes `_`, and a name that would not start with a letter gets a leading `T`. So `load-1_bus-1.mw` is `MEAS:SCAL:LOAD_1_BUS_1_MW?`, and
`1st` is `T1ST`. Two tags that end up with the same path are a config error, as is a path starting with `SYSTem`. A tag name that ends in
digits is its own mnemonic: `BUS1` is not `BUS` with suffix 1.

`<read-path>` and `<write-path>` replace the default paths with `MEASure:VOLTage:BUS1`-style text (mnemonics of letters, digits and `_`,
joined with `:`; the upper-case part of a mnemonic is its short form, a mnemonic with no lower-case letter has only the one form).
`<write-path>` is an error on a tag that is not `<writable>`.

| Command | Meaning |
|---|---|
| `MEAS:SCAL:LEVEL?` | the live value: a number (NR1 / NR2, never an exponent), a binary tag as `1` or `0`. A value that is not a number is `9.91E+37`, infinity `+-9.9E+37` (the SCPI sentinels) |
| `SOUR:SETPOINT 12.5` | set a writable tag: an analog takes a number, `MIN`, `MAX` or `DEF`; a binary takes `0`, `1`, `ON` or `OFF` |
| `SOUR:SETPOINT?` | the value the store holds, which is the old one until the provider has acknowledged the write |
| `SOUR:SETPOINT? MIN` / `MAX` / `DEF` | the configured limit or `<default-value>` |

A set command goes through the same checks as a write by any other protocol (`proto::point::Point::check`, shared with MQTT): the tag is
writable, the value has the tag's type, an analog is finite and within `<min-value>` / `<max-value>`. What passes is queued in the
datastore's write queue and takes effect when the provider acknowledges it, so a set followed by a read can still show the old value.

## The language

- **Messages.** A line (`\n`, `\r\n` or a bare `\r`) is a program message. On HiSLIP and VXI-11 a message ends at its END (`DataEND`, the write flag END) and a line terminator inside it still ends a line. It holds program message units separated by `;` (not inside a quoted
  string). Responses to the queries of one message are joined with `;` in order and sent as one line ending in `\n`. A message with no
  query gets no response, and neither does a query that failed.
- **Headers.** Case-insensitive; `:` separates mnemonics; a mnemonic has a short and a long form (`MEASure`: `MEAS` or `MEASURE`, nothing
  in between) and may carry a numeric suffix, which must be 1 (`MEAS1` is fine, `MEAS2` is -114). A leading `:` starts at the root; a
  header without one is relative to the previous unit's header, so `SYST:ERR?;VERS?` is `SYST:VERS?`. Common commands (`*...`) leave the
  path alone. `?` marks a query; a header and its parameters are separated by white space.
- **Parameters.** Separated by `,`. Numbers `[sign] digits [. digits] [E [sign] digits]`; character data (`ON`, `MAX`); strings in `"` or
  `'` (a doubled quote is a quote). Hexadecimal, octal, binary and block data are not accepted (-104). Units after a number are not
  accepted (`5V` is -131).
- **Fixed commands.** `*IDN?`, `*RST` (below), `*CLS`, `*ESE n`, `*ESE?`, `*ESR?`, `*OPC`, `*OPC?`,
  `*SRE n`, `*SRE?`, `*STB?`, `*TST?` (0), `*WAI` (no-op, nothing is overlapped); `SYSTem:ERRor[:NEXT]?`, `SYSTem:ERRor:COUNt?`, `SYSTem:VERSion?` (`1999.0`).
  `*ESE` and `*SRE` take 0 to 255; a fraction is rounded. A common command has no short form and no suffix.

### Status

State follows `<status-scope>`. With the default, `instrument`, there is **one** status system: an error caused by any session (raw
socket, HiSLIP, VXI-11) is in the one error queue, `*ESR?` and `*STB?` read the same registers, and whoever reads `SYST:ERR?` takes the entry
for everybody; `*ESE` and `*SRE` are the instrument's, not a session's. That is what a real instrument does, and what the second client
of a shared bench sees. Only the line buffer and the message being collected are per connection. The status system starts clean when the
device starts (the power-on bit of the event status register is never set). With `connection` each connection (a HiSLIP session and a
VXI-11 link count as one) has its own and starts clean.

- **Standard Event Status Register** (`*ESR?`, cleared by the read and by `*CLS`): OPC 1 (`*OPC`), QYE 4 (a -4xx error), DDE 8 (-3xx),
  EXE 16 (-2xx), CME 32 (-1xx). The bit is set by the error, also when the queue is full and the entry is lost.
- **Status byte** (`*STB?`): bit 2 (4) the error queue is not empty; bit 4 (16) MAV, a response of the same message is waiting
  (`*IDN?;*STB?` reads 16 more than `*STB?` alone); bit 5 (32) ESB, `ESR & ESE` is not 0; bit 6 (64) MSS, `STB & SRE` is not 0. Bits 3 and 7
  (questionable, operation) have no register behind them and read 0. `*SRE` cannot set bit 6.
- **Error queue.** Sixteen entries, first in first out. When it is full the sixteenth becomes `-350,"Queue overflow"` and later errors are
  dropped (the oldest are kept, SCPI-99). `SYST:ERR?` pops one: `-113,"Undefined header;FOO?"` (the text may be followed by `;` and detail, with
  quotes doubled), or `0,"No error"`. `*CLS` empties it.

| Code | Text | Raised for |
|---|---|---|
| -100 | Command error | (in the table; the specific -1xx below are used instead) |
| -102 | Syntax error | not a header, an empty unit, a bad separator, an unterminated string, a byte that is not text |
| -104 | Data type error | a string or hexadecimal where a number is wanted, `ON` for an analog |
| -108 | Parameter not allowed | too many parameters, any parameter on a read |
| -109 | Missing parameter | a set or `*ESE`/`*SRE` without its value |
| -113 | Undefined header | no such path, a command sent as a query or the reverse, a tag that is not writable set |
| -114 | Header suffix out of range | a numeric suffix other than 1 |
| -121, -123, -131 | Invalid character in number, Exponent too large, Invalid suffix | a malformed number, or one with more digits than a double holds (400 digits, no exponent); `1E999` (only a number with an exponent is -123); `5V` |
| -200 | Execution error | the store refused a queued write, a tag is gone |
| -221 | Settings conflict | `MIN` / `MAX` / `DEF` of a tag that has no such limit or default |
| -222 | Data out of range | outside `<min-value>` / `<max-value>`, `*ESE 256` |
| -224 | Illegal parameter value | a binary given a number other than 0 and 1, an unknown keyword |
| -350 | Queue overflow | the error queue is full |
| -363 | Input buffer overrun | a line longer than `<max-line>`; discarded up to its newline |
| -410 | Query INTERRUPTED | raw socket: more than 256 KiB of responses to one read: the ones not yet sent are discarded. HiSLIP, VXI-11: a new message while the previous response was not read (exact, below) |
| -420 | Query UNTERMINATED | raw socket: a query in a last line that had no newline before the client half-closed (the response is sent anyway). HiSLIP: a query in a message that never got its END; VXI-11: a read while a message is half written, or with nothing to say (exact, below) |

On the raw socket -410 and -420 are the nearest it comes to the 488.2 conditions of the same name, which belong to a handshake TCP does
not have; HiSLIP and VXI-11 have it, and there they are exact (IEEE 488.2 6.3.2.2 and 6.3.2.3).

### `*RST`

IEEE 488.2 10.32 has `*RST` return the device to its power-on configuration and leave the status system (error queue, SESR, SESE, SRE) and the
communication state alone; ferrofire does exactly that and nothing more. The device **settings** of this instrument are its writable
tags: `*RST` sets every writable tag that has a `<default-value>` to it, through the same checks and write queue as `SOUR:TAG <value>` (so
the value reaches the provider like any other write, and `SOUR:TAG?` shows the old one until the provider acknowledges). A writable tag
with no `<default-value>` is left as it is, and so are the measurements, which are not settings. It does **not** clear the error queue or
ESR/ESE/SRE (that is `*CLS`, and `*ESE 0;*SRE 0`), and it does not touch the input buffer or the other sessions.

## HiSLIP

IVI-6.1 version 1.0, `TCPIP::host::hislip0::INSTR` (sub-address `hislip0`; another is a FatalError). Two connections make a session:
`Initialize` (synchronous channel) gets `InitializeResponse` with the session id, `AsyncInitialize` on a second connection joins it.

| Message | Behaviour |
|---|---|
| `Data`, `DataEND` | one SCPI program message in any number of frames of one MessageID; it runs at the `DataEND`. The response carries the query's MessageID and is cut into frames no longer than the client's maximum message size (272 bytes until `AsyncMaximumMessageSize`), the last one a `DataEND`. Larger than 1 MiB in one frame is a FatalError |
| `AsyncMaximumMessageSize` | the client's size is kept (never below 272), the server's (1 MiB) returned |
| `AsyncStatusQuery` | the status byte in the control code; MAV (16) while a response has not been read |
| `AsyncLock`, `AsyncLockInfo` | one exclusive lock for the whole instrument: a request waits up to its timeout (ms) for the holder; every lock request is granted as exclusive, a shared-lock string is not distinguished. While another session holds it, a session's `Data` waits (raw socket and VXI-11 clients are not subject to the HiSLIP lock). A lock dies with its session |
| `AsyncDeviceClear`, `DeviceClearComplete` | drops the message being collected and the unread response with no error, then `DeviceClearAcknowledge`; messages in between are dropped |
| `AsyncRemoteLocalControl` | accepted, does nothing |
| `Trigger` | accepted, does nothing |
| anything else | an `Error` (unrecognized message type) and the session goes on; a header that is not `HS`, a message before both channels exist, a second `Initialize`, an unknown session: `FatalError` and the connection closes |

Exact IEEE 488.2 conditions: **-410 Query INTERRUPTED**: a new message starts (its first frame) while the response of the previous one was
not read, which HiSLIP tells by the RMT-delivered bit (bit 0 of the control code of `Data`, `DataEND`, `Trigger` and `AsyncStatusQuery`) being clear.
The server sends `Interrupted` (naming the MessageID of the lost response), queues -410 and runs the new message. **-420 Query
UNTERMINATED**: a message that has `Data` frames but no `DataEND` is abandoned, because the client started another MessageID or went away; a
query in it is -420 and is not run (a command in it is dropped silently). PyVISA sets RMT-delivered after every response it has read, so a
well-behaved client never sees -410; a client that never sets it gets -410 for every message after a response.

Not implemented: overlapped mode (the server always answers synchronized, which a client must accept), `AsyncServiceRequest` (SRQ: poll the
status byte), `AsyncInterrupted` (only the synchronous `Interrupted` is sent), TLS and authentication (HiSLIP 2.0), vendor messages.

## VXI-11

ONC RPC (RFC 5531) over TCP with record marking, XDR (RFC 4506). The **portmapper** (program 100000 version 2, on TCP and UDP at
`<vxi11-endpoint>`) answers `NULL`, `GETPORT` (the VXI-11 core program 0x0607AF version 1 and the abort program 0x0607B0, TCP, give
`<vxi11-core-endpoint>`'s port; anything else 0) and `DUMP`; `SET` and `UNSET` answer false; `CALLIT` is not offered. rpcbind versions 3 and 4,
which libtirpc clients try first, are answered `PROG_MISMATCH` (2, 2) so that they fall back to version 2. The **core channel** (program
0x0607AF version 1, also answered on the portmapper's port):

| Call | Behaviour |
|---|---|
| `create_link` (10) | the device name must be `inst0` (any case), else error 3; no lock (`lockDevice` true is error 8); at `<max-connections>` links error 9. Returns the link id, the core port as the abort port, and 1 MiB as `maxRecvSize` |
| `device_write` (11) | feeds the data to the parser; with the END flag it ends the message and runs its last line. A write that starts a message while the response to the last is unread **INTERRUPTS** it: the response is dropped and -410 queued (IEEE 488.2) |
| `device_read` (12) | up to `requestSize` bytes of the response, to the termination character when the flag asks (the low byte of `termChar`). The reason: END (4) with the last byte of the response, CHR (2) for the character, REQCNT (1) when the size was reached. Reading while a message is half written (no END) drops it and queues -420 (UNTERMINATED; the query is not run); so does reading with nothing to say (488.2 6.3.2.2: addressed to talk, no query pending). Either way the reply is error 15 (io_timeout), at once (the instrument does not make a client wait out `io_timeout`) |
| `device_readstb` (13) | the status byte, with MAV (16) while a response is waiting |
| `device_clear` (15) | drops the input being collected and the unread response, no error |
| `device_trigger` (14), `device_remote` (16), `device_local` (17) | error 0, nothing happens |
| `device_lock` (18), `device_enable_srq` (20), `device_docmd` (22), `create_intr_chan` (25), `destroy_intr_chan` (26) | error 8 (operation not supported) |
| `device_unlock` (19) | error 12 (no lock held by this link) |
| `destroy_link` (23) | error 0; an unknown link is 4 for every call |
| abort program `device_abort` (1) | on the core port; clears the link |

A link belongs to the connection that created it and dies with it. A call that does not decode is `GARBAGE_ARGS`, an unknown procedure
`PROC_UNAVAIL`, an unknown program `PROG_UNAVAIL`, an RPC version other than 2 is denied, a record over 2 MiB closes the connection.
Not implemented: the interrupt channel (SRQ), locks, DOCMD, a separate abort port, the UDP core channel, the portmapper's `CALLIT`.

## `ferrofire scpi`

```
ferrofire scpi HOST[:PORT]|RESOURCE [COMMAND...] [--file SCRIPT|-] [--timeout SECONDS] [--check] [--json] [--portmapper PORT]
```

| Target | Protocol |
|---|---|
| `lab`, `lab:5025`, `[::1]:5025` | the raw socket, port 5025 by default |
| `TCPIP::lab::5025::SOCKET` | the raw socket |
| `TCPIP::lab::hislip0::INSTR`, `TCPIP::lab::hislip0,4881::INSTR` | HiSLIP, port 4880 unless named after a comma |
| `TCPIP::lab::INSTR`, `TCPIP::lab::inst0::INSTR` | VXI-11: the portmapper on port 111 (`--portmapper PORT` for another; a VISA resource cannot say), then the core channel it names, device `inst0` or the name given |

The VISA board number (`TCPIP0::`) and the case of `INSTR` / `SOCKET` do not matter.

| | |
|---|---|
| `ferrofire scpi lab '*IDN?' 'MEAS:SCAL:LEVEL?'` | one message per argument; the response of each query is printed |
| `--file script.scpi` / `--file -` | commands from a file or stdin, one per line; blank lines and lines starting `#` are skipped; added after the arguments |
| no commands, stdin a terminal | a prompt (`scpi> ` on stderr); `quit`, `exit` or end of input leaves. No commands and stdin not a terminal sends nothing |
| `--timeout 5` | seconds to connect and to wait for each response |
| `--check` | after each command read `SYST:ERR?` until it says 0; each error is a line on stderr (`error: <command>: -222,Data out of range;...`) and the exit status is 1 once the run is over. Any non-zero code counts, positive ones too |
| `--json` | one object per command: `{"command":"...","response":"..."|null,"errors":[{"code":-222,"message":"..."}]}` |

A message is a query when some header in it (outside quotes) ends in `?`, and then one response line is read: a message such as
`*IDN?;*CLS` is answered by the instrument with one line, so the client reads one. (Looking only at the last header would leave a line
unread after `*IDN?;*CLS`.) A query the instrument rejects gets no response at all. Under `--check` the client therefore sends a query
as `QUERY;*OPC?`: a rejected query still gets the `1` of `*OPC?` as its whole answer, so the rejection is seen at once, its error read
from the queue, with no wait for `--timeout`; an accepted query answers `RESPONSE;1` and the client prints `RESPONSE`. (Standard 488.2,
so any instrument that joins responses with `;` takes it. One that does not is taken at its word, and a lone `1` for a query of a real
instrument that answers `1` is read as no response: use the command without `--check` or read `*OPC?` yourself.) A query that gets no answer at all within `--timeout` is
still a failure unless the error queue says why. Exit status: 0 fine; 1 the instrument reported errors; 2 the run itself failed (no
connection, no response in time, a bad argument, a HiSLIP FatalError, a VXI-11 error). It assumes nothing of the instrument beyond the
protocol: it works against real instruments that offer the socket service, HiSLIP or VXI-11.

## Deviations from a real instrument that remain

- **No SRQ.** HiSLIP's `AsyncServiceRequest` and VXI-11's interrupt channel are not implemented; the status byte is polled (`*STB?`,
  `AsyncStatusQuery`, `device_readstb`). `*SRE` and MSS exist, nothing is sent when MSS rises.
- **HiSLIP**: no overlapped mode, no `AsyncInterrupted`, no shared locks (every lock is exclusive and the lock is not seen by the other transports), no TLS or
  authentication, no vendor messages.
- **VXI-11**: no locks (`device_lock` is error 8), no interrupt channel, no `DOCMD`, the abort program shares the core port, the core
  channel is TCP only, the portmapper has no `CALLIT` and there is no rpcbind. `lxi discover` works (it broadcasts a GETPORT to UDP 111,
  which the portmapper answers, and asks `*IDN?` over the core channel; conformance checks it).
  `device_read` with nothing to read is error 15 at once, where a real instrument waits for `io_timeout`.
- **Raw socket**: -410 and -420 are approximations (above); its responses are not flow-controlled per message.
- **`*RST`** resets the tags that have a `<default-value>`; it cannot reset what the provider owns.
- **Status** is one system per `<status-scope>`, not per transport; there is no questionable or operation register, and the power-on
  bit (PON) of the event status register is never set.
- **Numbers**: hexadecimal, octal, binary, block data and units are not accepted (-104, -131); a mantissa longer than a double holds is -121,
  where IEEE 488.2 allows up to 255 digits and names the condition -124 (Too many digits).
- **mDNS / LXI discovery** (`_scpi-raw._tcp`, `_hislip._tcp`, `_vxi-11._tcp`, the LXI identification XML) is not offered; the VXI-11 broadcast that `lxi discover` uses is.
