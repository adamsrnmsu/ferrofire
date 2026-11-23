# BACnet/IP module

An external ffmod/1 module (`docs/superpowers/specs/2026-11-21-external-modules-design.md`) for the stock `<bacnet-server>` and
`<bacnet-client>` elements, on [bacpypes3](https://github.com/JoelBender/BACpypes3) 0.1.0 (MIT). MIT, like ferrofire (`LICENSE`).
A stock element loads as written: `module.json` tells the device that this module serves both tags, and the device starts
`python3 module.py` in this directory, with the element as `hello.config`.

    make modbac-image    # ff-modbac:22.04: ff-physics:22.04 + bacpypes3, the module at /opt/ferrofire/modules/bacnet
    make modbac-test     # test_module.py: the module driven by a scripted device, a bacpypes3 peer on loopback

The version is pinned (`requirements.txt`): the server overrides 0.1.0 internals (below), so another version needs the golden
re-run first.

## Transport

As stock's bacnet-stack (`dlenv.c`, `bip-init.c`): the address and netmask of `BACNET_IFACE` (default `eth0`) at `BACNET_IP_PORT`
(default 47808); `BACNET_BBMD_ADDRESS` (`BACNET_BBMD_PORT` 47808, `BACNET_BBMD_TIMETOLIVE` 60000) registers as a foreign device.
The `<endpoint>` ip and port of a server are parsed and discarded, as stock does; it must contain `udp://`.

## Server (protocols plan row 6)

The Task 0b spike's server (`spike/NOTES.md`) with the device feed in place of its stand-in datastore:

- `ready` lists binary/analog inputs as `out` (the device supplies them) and outputs as `in`.
- `hello` and `values` fill the module's copy of the device. A `null` (a non-finite analog) keeps the previous value.
- Every second, as `Server::update` does, BI/AI present values come from that copy. So do BO priority slot 1 (stock's priority "0")
  and AO slot 16, with stock's uint8 AO: truncated, mod 256, 255 relinquishes, a negative value leaves the slot alone.
- A network write becomes a `write`, as `Server::writeBinary`/`writeAnalog` enqueue it:
  - The tag comes from stock's shared maps: BI and BO on one instance share a map entry, AI and AO the other.
  - A BO write goes out when it is accepted.
  - An AO write goes out with the value as written (7.5, not the stored 7), even when stock then answers value-out-of-range.
  - A BI present-value write is ACKed and goes out too (`bi.c`).
  - A relinquish (null) does not go out.
  - Nothing goes out for an AO or BO written while Out_Of_Service is TRUE (135-2016bl: decoupled from the output; stock sends it).
- One I-Am at start. Who-Is gets a global-broadcast I-Am. RP, RPM, WP, WPM, ReadRange and DCC are answered as stock answers them.
  Anything else is rejected with `unrecognized-service`.

Held to stock by `conformance/bacnet/` (`DUT=rust make conformance PROTO=bacnet`; `conformance/README.md`, BACnet/IP): the real
module through ferrofire, fed by a provider so that nonzero values are compared:
- The `conformance/stock/probe_bacnet.py` transcript equals stock's fed one (`conformance/stock/bacnet-server-feed.golden`) except
  for the rows below and two device-side ones (the provider's write hold, the order of a `WRITE=` batch), exactly as
  `conformance/bacnet/rust-feed.diff` lists them.
- `nmap --script bacnet-info` equals `bacnet-server.nmap.golden`.
- Stock's own `bacnet-client` (ffc-stock-2026.11.03) polls and writes this server and gets the same transcript as from the stock
  server (`SERVER=rust make conformance PROTO=bacnet-client`).

Differences from stock:

| Area | Stock | Here |
|---|---|---|
| Current_Command_Priority on AO, BO, MSO, BV; AV COV_Increment; integer-value 1's name | absent / served / `ANALOG VALUE 1` (a duplicate name) | served / absent / `INTEGER VALUE 1`: the spec decides (135-2012as, 135-2012aw, Object_Name unique); see `conformance/README.md` |
| command, integer-value, life-safety-point, load-control, lighting-output, channel, octetstring-value property sets | demo-device sets | name, type, status flags only (open) |
| ReadRange of an absent property or object; of a Log_Buffer for a requester that accepts under 1476 bytes | `services/property-is-not-a-list`; a 1476-byte reply whatever the request says | `property/unknown-property`, `object/unknown-object`; a reply cut to the requester's maximum with more-items: the spec decides (15.8); see `conformance/README.md` |
| Out_Of_Service, input Present_Value while out of service, the feed, priority 6, priority 0 or 17 | see the rows in `conformance/README.md` (spec over stock: decoupling, BI and MSI Present_Value denied while in service, parameter-out-of-range) | |
| COV subscription, Who-Has, ConfirmedCOVNotification | Reject unrecognized-service | the same |
| Config errors | logged when the device loads | `error` after `hello` (no `udp://`, a missing `endpoint`/`instance`/`address`/`tag`, a non-number) |
| The UDP port is held by another process at start (probe: a second socket on the port, `modules/bacnet/test_module.py`) | `bip_init` fails and the device exits 1 | `error` after 5 s without a bind and no `ready` (bacpypes3 0.1.0 retries the bind every second and never raises, so the module waits on its transport-ready event); the rest of the device keeps running, and `--strict` catches the error |
| Who-Is to 255.255.255.255 (limited broadcast; `peer.sh`, `extra.txt`) | answered with an I-Am | answered: a socket is also bound to 255.255.255.255 (bacpypes3 binds the interface address and the directed broadcast, which Linux does not deliver a limited broadcast to) |

0.1.0 defects the server works around are listed in `spike/NOTES.md`: DCC, Abort without the server flag, the device wildcard,
RPM `all`, ReadRange, WPM errors, vendor properties, bit-string lengths. To those this module adds the `notificationClass`
exception that OctetStringValue objects logged at start.

## Client (protocols plan row 11)

The device declares the client's point tags at load (`docs/modules.md`, Manifests), so bennu's sample `<bacnet-client>` runs
unchanged. Against the stock server the scan and the requests are the stock client's byte for byte
(`DUT=rust make conformance PROTO=bacnet-client`), and so is the command interface's `WRITE=`: a `put` over ffmod/1, answered
with the peer's outcome (ww-4s8).

- `ready` lists inputs as `in` and outputs as `out`. The device serves the element's `<command-interface>` (port 1332) itself and
  runs the client echo rules. It sends the module `values` for output tags when they are due.
- `put` (the command interface's `WRITE=`, any tag) sends the WriteProperty at once and answers `done` when bacpypes3's own
  transaction ends (APDU timeout 3 s, 3 retries: about 12 s with the server away), never earlier. The request is not wrapped in a
  timeout, because cancelling it leaves bacpypes3 retransmitting: a write reported failed does not land later. A tag's object is
  the one stock's type rule names, input tags included (`stock_type`): a binary tag is a binary-output 0 (read and write), an
  analog-input tag is an analog-input 0, which the server refuses.
- Binding:
  - Stock writes the endpoint into its own `address_cache` and binds from it: "Found address in cache", and no Who-Is on the wire
    (captured from ffc-stock, `bacnet.c:398-420`, `BacnetBindToDevice`).
  - So this module sends no Who-Is either. It takes the endpoint as the peer's address and writes no `address_cache` file.
  - An endpoint without `udp://`, or one bacpypes3 cannot resolve, is an `error`. Stock exits.
- Every `scan-rate` seconds, per connection:
  - The object type is stock's (`stock_type` in module.py, `ClientConnection.cpp`): every binary point is read and written as a
    binary-output, an analog point as an analog-input only while its tag was last declared as one (the sample's BI 0 and 1 are
    read as BO 0 and 1, its AI 1 as AO 1). Requests carry stock's `00 05` header (no segmentation, max APDU 1476).
  - ReadProperty present-value of each point, one at a time: binaries then analogs, by instance, as stock's maps order them. An
    input and an output on one instance share an entry, as in stock.
  - Each value read is a `write`, changed or not. A failed read is logged and skipped.
- A `values` change of an output tag becomes a WriteProperty of present-value without priority, ENUMERATED for a binary and REAL
  for an analog. The peer's answer becomes `{"t":"sent","tag","value","ok"}`: `ok` is a SimpleAck within 3 s. Changes of other
  tags are ignored.
- The local device instance is the number of connections, as stock's last `Device_Set_Object_Instance_Number` leaves it.
