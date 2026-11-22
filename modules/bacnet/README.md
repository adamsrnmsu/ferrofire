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

The Task 0b spike (`spike/server.py`, `spike/NOTES.md`) with the device feed in place of its stand-in datastore:

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
- One I-Am at start. Who-Is gets a global-broadcast I-Am. RP, RPM, WP, WPM, ReadRange and DCC are answered as stock answers them.
  Anything else is rejected with `unrecognized-service`.

Against `conformance/stock/probe_bacnet.py` with the stock sample element, driven through `hello`:
- The transcript equals `conformance/stock/bacnet-server.golden` except for the same 28 diff lines (of 499) as the spike, listed
  below.
- `nmap --script bacnet-info` equals `bacnet-server.nmap.golden`.
- Stock's own `bacnet-client` (ffc-stock-2026.11.03) polls this server and gets answers.

Differences from stock:

| Area | Stock | Here |
|---|---|---|
| AV 0, BV 0, MSI 0 detail properties (property sweep) | bacnet-stack demo values | bacpypes3 defaults, some missing (the other decoy types are not swept) |
| ReadRange of a trend-log log-buffer | 66-76 synthetic records | `services/property-is-not-a-list` |
| AI present-value write with out-of-service set | accepted and enqueued (`ai.c`) | `write-access-denied`, as with out-of-service clear (not probed) |
| Properties and services no probe touched (COV, Who-Has, BO priority 6) | per bacnet-stack | bacpypes3 behaviour, untested against stock |
| Config errors | logged when the device loads | `error` after `hello` (no `udp://`, a missing `endpoint`/`instance`/`address`/`tag`, a non-number) |

0.1.0 defects the server works around are listed in `spike/NOTES.md`: DCC, Abort without the server flag, the device wildcard,
RPM `all`, ReadRange, WPM errors, vendor properties, bit-string lengths. To those this module adds the `notificationClass`
exception that OctetStringValue objects logged at start.

## Client (protocols plan row 11)

- `ready` lists inputs as `in` and outputs as `out`. The device serves the element's `<command-interface>` (port 1332) itself and
  runs the client echo rules. It sends the module `values` for output tags when they are due.
- Binding:
  - Stock writes the endpoint into its own `address_cache` and binds from it: "Found address in cache", and no Who-Is on the wire
    (captured from ffc-stock, `bacnet.c:398-420`, `BacnetBindToDevice`).
  - So this module sends no Who-Is either. It takes the endpoint as the peer's address and writes no `address_cache` file.
  - An endpoint without `udp://`, or one bacpypes3 cannot resolve, is an `error`. Stock exits.
- Every `scan-rate` seconds, per connection:
  - ReadProperty present-value of each point, one at a time: binaries then analogs, by instance, as stock's maps order them. An
    input and an output on one instance share an entry, as in stock.
  - Each value read is a `write`, changed or not. A failed read is logged and skipped.
- A `values` change of an output tag becomes a WriteProperty of present-value without priority, ENUMERATED for a binary and REAL
  for an analog. The peer's answer becomes `{"t":"sent","tag","value","ok"}`: `ok` is a SimpleAck within 3 s. Changes of other
  tags are ignored.
- The local device instance is the number of connections, as stock's last `Device_Set_Object_Instance_Number` leaves it.
