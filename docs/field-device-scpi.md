# SCPI instrument (`<scpi-server>`) and the `ferrofire scpi` client

Feature `proto-scpi` (on by default, like `proto-mqtt`: the crap and mutate gates build the default features only). The field device
presents its point map as an SCPI-99 instrument on a raw TCP socket, the socket protocol of LXI instruments (port 5025, ASCII, one
message per line). No dependency was added: SCPI is line-based text and the parser is ours (`src/scpi/syntax.rs`, shared with the client;
`src/fd/proto/scpi/`). Bennu has no SCPI, so there is no stock behaviour to match; the config keys below are ferrofire's. HiSLIP (4880) and
VXI-11 are not implemented (`lxi scpi` needs `--raw`, PyVISA needs a `::SOCKET` resource).

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
| `<endpoint>` | `tcp://0.0.0.0:5025` | `tcp://ip:port` |
| `<manufacturer>`, `<model>`, `<serial>` | `ferrofire`, the `<field-device>` `<name>`, `0` | the first three fields of `*IDN?`; no comma or control character. The fourth is the ferrofire version |
| `<max-connections>` | 16 (1-4096) | at the limit a new client is accepted and closed at once; the others are not affected |
| `<max-line>` | 1024 (64-65536) | longest line, without its terminator. A longer one is error -363 and is thrown away up to its newline |
| `<idle-timeout>` | 300 (0-86400) | seconds of silence after which a client is dropped; 0 = never. A silent client holds a place, not the server |
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

- **Messages.** A line (`\n`, or `\r\n`) is a program message. It holds program message units separated by `;` (not inside a quoted
  string). Responses to the queries of one message are joined with `;` in order and sent as one line ending in `\n`. A message with no
  query gets no response, and neither does a query that failed.
- **Headers.** Case-insensitive; `:` separates mnemonics; a mnemonic has a short and a long form (`MEASure`: `MEAS` or `MEASURE`, nothing
  in between) and may carry a numeric suffix, which must be 1 (`MEAS1` is fine, `MEAS2` is -114). A leading `:` starts at the root; a
  header without one is relative to the previous unit's header, so `SYST:ERR?;VERS?` is `SYST:VERS?`. Common commands (`*...`) leave the
  path alone. `?` marks a query; a header and its parameters are separated by white space.
- **Parameters.** Separated by `,`. Numbers `[sign] digits [. digits] [E [sign] digits]`; character data (`ON`, `MAX`); strings in `"` or
  `'` (a doubled quote is a quote). Hexadecimal, octal, binary and block data are not accepted (-104). Units after a number are not
  accepted (`5V` is -131).
- **Fixed commands.** `*IDN?`, `*RST` (no-op: the point map is not a state to reset), `*CLS`, `*ESE n`, `*ESE?`, `*ESR?`, `*OPC`, `*OPC?`,
  `*SRE n`, `*SRE?`, `*STB?`, `*TST?` (0), `*WAI` (no-op); `SYSTem:ERRor[:NEXT]?`, `SYSTem:ERRor:COUNt?`, `SYSTem:VERSion?` (`1999.0`).
  `*ESE` and `*SRE` take 0 to 255; a fraction is rounded. A common command has no short form and no suffix.

### Status

State is **per connection**: a client sees its own error queue and registers, and a new connection starts clean (the power-on bit of the
event status register is never set, because a connection is not a power-on).

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
| -121, -123, -131 | Invalid character in number, Exponent too large, Invalid suffix | a malformed number, `1E999`, `5V` |
| -200 | Execution error | the store refused a queued write, a tag is gone |
| -221 | Settings conflict | `MIN` / `MAX` / `DEF` of a tag that has no such limit or default |
| -222 | Data out of range | outside `<min-value>` / `<max-value>`, `*ESE 256` |
| -224 | Illegal parameter value | a binary given a number other than 0 and 1, an unknown keyword |
| -350 | Queue overflow | the error queue is full |
| -363 | Input buffer overrun | a line longer than `<max-line>`; discarded up to its newline |
| -410 | Query INTERRUPTED | more than 256 KiB of responses to one read from the socket: the ones not yet sent are discarded |
| -420 | Query UNTERMINATED | a query in a last line that had no newline before the client half-closed (the response is sent anyway) |

-410 and -420 are the nearest a raw socket comes to the 488.2 conditions of the same name, which belong to a handshake TCP does not have.

## `ferrofire scpi`

```
ferrofire scpi HOST[:PORT] [COMMAND...] [--file SCRIPT|-] [--timeout SECONDS] [--check] [--json]
```

| | |
|---|---|
| `ferrofire scpi lab '*IDN?' 'MEAS:SCAL:LEVEL?'` | one message per argument; the response of each query is printed |
| `--file script.scpi` / `--file -` | commands from a file or stdin, one per line; blank lines and lines starting `#` are skipped; added after the arguments |
| no commands, stdin a terminal | a prompt (`scpi> ` on stderr); `quit`, `exit` or end of input leaves. No commands and stdin not a terminal sends nothing |
| `--timeout 5` | seconds to connect and to wait for each response |
| `--check` | after each command read `SYST:ERR?` until it says 0; each error is a line on stderr (`error: <command>: -222,Data out of range;...`) and the exit status is 1 once the run is over. Any non-zero code counts, positive ones too |
| `--json` | one object per command: `{"command":"...","response":"..."|null,"errors":[{"code":-222,"message":"..."}]}` |

A message is a query when some header in it (outside quotes) ends in `?`, and then one response line is read: a message such as
`*IDN?;*CLS` is answered by the instrument with one line, so the client reads one. (Looking only at the last header would leave a line unread after `*IDN?;*CLS`.) A query the instrument rejects gets no response at all; under `--check` the client waits out the
timeout, reads the error queue and reports what it holds (only a timeout with an empty queue is a failure), so scripts that expect errors
want a short `--timeout`. Exit status: 0 fine; 1 the instrument reported errors; 2 the run itself failed (no connection, no response in time,
a bad argument). Port 5025 and a plain socket are all it assumes: it works against real instruments that offer the socket service.
