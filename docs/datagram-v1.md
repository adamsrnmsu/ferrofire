# ferrofire datagram v1

How a ferrofire provider publishes state to RTUs (field devices): one UDP datagram per chunk, usually multicast. ferrofire owns this
wire. The bytes are **wire-compatible with libzmq `udp_engine.cpp` 4.3.x RADIO/DISH, verified against stock bennu** (`Subscriber.cpp`
joins a DISH on the group below), so stock field devices and `bennu-probe` keep working, and nothing in ferrofire depends on libzmq's
DRAFT RADIO/DISH API. The request path (`QUERY`/`READ`/`WRITE`) is plain libzmq REQ/REP, a stable API, and is not part of this page.

## Datagram

```
+---------------+-----------------+------------------------------------------+
| u8 group_len  | group (ASCII)   | body (ASCII, ends with one NUL)          |
+---------------+-----------------+------------------------------------------+
  1 byte          group_len bytes   rest of the UDP payload
```

- `group_len` is at most 15 (libzmq's DRAFT `ZMQ_GROUP_MAX_LENGTH`).
- The body is `tag:value,` points back to back, then the trailer (below), then `\0`. Values are decimal (`5`, `0.25`, `-3e-7`) or
  `true`/`false` for binaries. Tag names never contain `:` or `,`.
- A datagram never exceeds **1400 bytes of body**, so UDP, IP and group headers fit a 1500 B Ethernet frame without fragmentation.

## Group (the part that is easy to get wrong)

`group = lower-hex(H(endpoint))` with its **last hex digit dropped**, where `H` is libstdc++'s `std::hash<std::string>` (`_Hash_bytes`,
64 bit) and the hex has no leading zeros. The digit is dropped because bennu's `endpoint.hash()` fits the 15 byte group limit that way.
`endpoint` is the publish endpoint string with the interface part removed: `udp://10.1.1.2;239.0.0.1:40000` and
`udp://*;239.0.0.1:40000` both hash `udp://239.0.0.1:40000`. The publisher and every subscriber of one stream must therefore be given
the same endpoint string (a stock field device's `<input><endpoint>`).

`_Hash_bytes(p, len, seed = 0xc70f6907)`, all arithmetic modulo 2^64, `mul = 0xc6a4a7935bd1e995`, `shift_mix(v) = v ^ (v >> 47)`:

```
hash = seed ^ (len * mul)
for each full 8-byte word w (little endian):  hash = (hash ^ (shift_mix(w * mul) * mul)) * mul
if len % 8 != 0: w = the remaining bytes as a little endian integer; hash = (hash ^ w) * mul
return shift_mix(shift_mix(hash) * mul)
```

Test vector (checked by `tests/bennu.rs` and by `tools/wireshark/test.sh`, which hashes in Python):

| | |
|---|---|
| endpoint | `udp://239.0.0.1:40000` |
| `H(endpoint)` | `0x54386972605942ba` |
| group | `54386972605942b` (15 chars) |
| datagram, seq 7, cycle 3, t 1234567890 | `0f` + `"54386972605942b"` + `"sim.t:0.1,lock.height:5,lowervalve.open:0,_ff.seq:7,_ff.cyc:3,_ff.t:1234567890,"` + `00` |

Hex: `0f35343338363937323630353934326273696d2e743a302e312c6c6f636b2e6865696768743a352c6c6f77657276616c76652e6f70656e3a302c5f66662e7365713a372c5f66662e6379633a332c5f66662e743a313233343536373839302c00`

## Chunking

One publish cycle is the whole tag list in model order, split into datagrams of whole points (a point is never cut). The tag part of a
chunk is at most `1400 - 96` bytes; the 96 reserved bytes hold the trailer, whose worst case (three 20 digit values) is 85. The first
datagram of a cycle starts with `sim.t:` (the model clock), which the benchmark client uses to find cycle boundaries.

## Sequence trailer

Every datagram ends, before the NUL, with three pseudo-tags (appended, so a stock reader sees ordinary points it has no use for;
bennu's `InputModule` ignores names it did not subscribe to):

| tag | meaning |
|---|---|
| `_ff.seq` | count of datagrams this endpoint has sent since the process started; +1 per datagram, across cycles |
| `_ff.cyc` | publish cycle number of this endpoint; equal in every datagram of one cycle, +1 per cycle |
| `_ff.t` | send time, unix ns |

Counters are per endpoint (per group), so each per-RTU stream has its own `seq`. A gap in `seq` is exactly the number of lost datagrams.

## Endpoints

`udp://[iface;]host:port`, `host` an IPv4 address (multicast or unicast). Default `udp://*;239.0.0.1:40000`; `iface` is the local address
that owns the outgoing interface (`IP_MULTICAST_IF`), `*` or absent lets the kernel pick. TTL 1, loopback on. One endpoint carries all
tags by default; `publish_slices:` in the model or `--publish-tags ENDPOINT=a,b,c|@config.xml` add endpoints that carry only the
named tags (a tag may be in several). Two endpoints with the same group would share a stream, so duplicates are refused at startup.

## Refresh and loss model

Every cycle carries the **full state**, repeated at the publish period (default 100 ms). There is no retransmission, no ack and no
per-subscriber state: a datagram that is dropped is healed by the next cycle, within one period, and a subscriber that starts late
has everything after at most one period. This is the same model as OPC UA Part 14 (PubSub, UADP) and IEC 61850 GOOSE (periodic
repeat of the full dataset): the receiver is stateless apart from "last value", and a lost update costs at most one period of staleness,
not an error. The cost is bandwidth, bounded and constant: tags x period.

## Failure behaviour

Sender (provider):
- The send socket is non-blocking. A refused `send_to` (ENOBUFS, network unreachable, interface down, ...) costs that datagram: it is
  counted (`Publisher::dropped`), logged at most once per 10 s, and the engine tick goes on. `seq` still advances, so receivers see the gap.
- Datagram bytes are produced and sent from the engine thread right after each tick (`publish_all`): there is no queue to back up.

Receiver (`ferrofire::datagram::Receiver`; the rules for the bench client and the Rust field device). A datagram is ignored, never fatal, when it is:

| case | rule |
|---|---|
| shorter than its `group_len`, empty, or not UTF-8 | garbage, dropped |
| another group | not ours, dropped |
| body without the final NUL, or whose last point lacks its comma | truncated, dropped whole (a cut number would parse as a wrong value) |
| tag not subscribed | the point is skipped |
| point with no `:`, empty name or a value that is not a finite number / `true` / `false` | the point is skipped, the others of the datagram count |
| `_ff.seq` not above the newest seen **and** `_ff.t` not newer | duplicate or out of order, dropped |
| `_ff.seq` lower but `_ff.t` newer | the provider restarted (counter back at 0): accepted, newest reset |
| no `_ff.*` tags (stock provider) | nothing to order by, accepted |

The receiver keeps its last values, so after any of these it simply shows the previous state until the next good cycle.

What stock bennu does (`Client.cpp`, `Subscriber.cpp`): the **request** client uses a 5 s timeout and 3 tries (lazy pirate: close and
recreate the socket after the last), so while the provider is down an RTU's writes stall up to ~15 s; reads from the multicast stream
simply go stale. Unknown point names in an update are ignored. A malformed `ERR=` is logged, not fatal. The Rust field device should
not copy the 5 s: use a 1 s reply timeout, 2 tries, then recreate the socket (linger 0), with 100 ms to 1 s backoff, and mark the
RTU's inputs stale after 3 missed publish periods. See `docs/field-device-spec.md` section 14.

Provider request side (REP): every request gets exactly one reply on every path (unknown tag, malformed text, bad value, empty,
oversized over 64 KiB, non-UTF-8, several frames, a panic inside a handler: `ERR=<reason naming the tag or problem>`), the receive
loop wakes at least every 100 ms, and the shared state's locks ignore poisoning, so one bad request cannot wedge a client or the engine.

## Tooling

- `tools/wireshark/ferrofire_datagram.lua`: Wireshark dissector (group, every point, `_ff.seq/cyc/t`, truncation warnings). It claims
  UDP port 40000; use Decode As for another port. `tools/wireshark/test.sh` checks it with tshark against the test vector above.
- `ferrofire::datagram::Receiver`: reference receiver implementing the table above.
