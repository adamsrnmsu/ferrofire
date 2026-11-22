# bacpypes3 0.1.0 spike: verdict GO

Task 0b gate of the external-modules plan. Question: can a Python module on `bacpypes3==0.1.0` (MIT, pure Python) give a BACnet client what stock
`bennu-field-device` gives (spec amendment 2026-11-21, protocols plan rows 6 and 11)? Answer: **yes**, for everything a client of the configured
objects and of the device sees, at the price of an override layer over a library with a good many 0.1.0 defects (listed below).

Run: `./run.sh` (containers and network `ff-sdd-t0b-spike-*`, removed on exit; `DCC_WAIT=0` skips the 70 s DCC-duration wait). `server.py` is the
spike server, built from `conformance/stock/bacnet-server.xml` (instance 1234, BI 0-1, BO 2-3, AI 0-1, AO 2-3, `address` = object instance).
`wirediff.sh STOCK.pcap SPIKE.pcap` compares what each server sent, frame by frame. Output in `out/` (`spike.txt` transcript, `diff.txt` against the
golden, `nmap.txt`, `bacnet-spike.pcap.gz`, `server.log`).

## The golden (new, `conformance/stock/`)

`probe_bacnet.py` (bacpypes3 client, 499 lines of transcript) against stock with the sample `<bacnet-server>` config; `run-stock.sh bacnet`
re-records `bacnet-server.golden`, `.pcap.gz` (tcpdump started before the device, so its start-up I-Am is in it) and `bacnet-server.nmap.golden`
(`nmap --script bacnet-info`). An early version of the probe gave the same transcript on repeated stock runs; the sweep leaves out the device's local-date and local-time, which move. It covers: Who-Is (global, ranged, miss, unicast), device properties,
the object-list (whole, by index, out of range), RP of 17 properties on each configured object, RPM (per object, all eight at once, `all`, mixed
errors), unknown objects/properties, WriteProperty (priorities, relinquish, read-only, unknown, wrong type, out of range), writes to the other object
types, WPM, ReadRange, vendor properties, a sweep of every property id 0..260 on nine objects, unsupported services, DCC (password, disable, disable
initiation, duration, Who-Is while disabled), unconfigured outputs (AO/BO 500: empty arrays). Stock facts it records, none of them in the plan before:

- The device is the bacnet-stack demo device: name `SimpleServer`, vendor 260, max APDU 1476, no segmentation, protocol revision 19.
- Object list is 4074 objects (device, network-port 1, AI/AO/BI/BO 0..999, and 14 more types, see the golden). Any instance below 1000 of the four
  types exists and reads 0; 1000 and up is `object/unknown-object`. Names are `ANALOG INPUT n` etc., whatever the XML tag.
- The whole object-list or an RPM `all` of the device cannot be sent: `ABORT segmentation-not-supported`.
- AO present value is a uint8: negative is `property/value-out-of-range`, the rest is truncated and wraps mod 256 (300.7 -> 44, 70000 -> 112), 255
  reads back as relinquished. BO/AO refresh from the datastore sits at array slot 1 (BO) and slot 16 (AO): a BO write is accepted and changes the
  array, never the present value; an AO written at priority 16 is back to the datastore value within a second, one at 8 stays.
- Priority 0 or 17 on a write is an **Abort, reason other**. A WriteProperty to AO 500, AV 0, BI 0 is ACKed (BI/AV/BV/MSI/... store the value or drop it).
- DCC needs the password `filister`; `disable` silences everything but DCC (a Who-Is too), `disable-initiation` silences nothing a probe sees, a duration is ignored
  (still silent after 70 s). Unsupported services (`ReinitializeDevice`, `SubscribeCOV`) are `reject unrecognized-service`.
- RPM `all` on a configured object lists a fixed set (the golden's RPM-ALL blocks); AI also carries vendor properties 9997 (real 90.51), 9998
  (unsigned 90), 9999 (signed -200).
- Who-Is is answered with a global-broadcast I-Am (BVLC 0x0b, DNET ffff), also for a unicast Who-Is. Protocol-services-supported is a 44-bit string
  and object-types-supported 60 bits, not the 51/66 a revision-19 library sends.
- ReadRange: `services/property-is-not-a-list` for anything but a list; `device-address-binding` is an empty list; the eight trend-logs have synthetic
  log-buffers (66-76 records).

## What matched

The transcript of the spike equals the golden except for the differences below (28 diff lines of 499; `out/diff.txt`). On the wire, of 6724
server-sent BACnet frames the spike's differ only in the sweep of non-configured types and the trend-log ReadRange. Specifically equal, byte for byte
where a frame was captured: I-Am (fields and the broadcast form), RP/RPM values, error classes and codes, aborts and rejects, WP/WPM effects incl.
the AO wrap, the priority arrays after each write, the 4074-entry object list in stock's order, DCC, and `nmap --script bacnet-info`.

**Not compared with stock: any nonzero value.** Stock ran with no provider, so every value in the golden is 0/inactive and the 1 Hz refresh
from the datastore was never observed on stock. On the spike alone, with a seeded datastore, BI/AI/BO/AO read the seeded values (AO 33.7 reads
33.0, by the uint8 rule above); that is our model of stock's refresh (BO slot 1, AO slot 16, from the plan row), not a measurement.

## Remaining differences

| Area | Stock | Spike | Fixable? |
|---|---|---|---|
| AV 0, BV 0, MSI 0 detail properties (sweep) | AV: cov-increment, object-name, 117; BV: object-name, priority-array (16 nulls), relinquish-default; MSI: name `MULTISTATE INPUT 0`, number-of-states 254, state-text (254 empty), event-state | bacpypes3 defaults (e.g. MSI name `MULTI-STATE-INPUT 0`, 3 states), several missing | yes, data-driven from the golden; no library obstacle. The other 11 decoy types were not swept at all |
| ReadRange of a trend-log log-buffer | 66-76 synthetic records (date, time, real, flags), "more items" flag | `property-is-not-a-list` | yes, but the records and the 8 logs' other properties are bacnet-stack demo data to reproduce; 0.1.0 has no ReadRange, so the whole service is ours |
| Properties no probe read | per stock | bacpypes3 values (e.g. a BO/AO `current-command-priority` that stock lacks) | unknown extent; add to the sweep |
| Services not probed | WPM tested (3 cases), COV, Who-Has, I-Have, ReadRange partly | Who-Has/I-Have/COV switched off to reject | untested against stock |
| Timing | Who-Is/RP latency | not measured | |

## bacpypes3 0.1.0 defects worked around in `server.py` (each cost a probe run to find)

1. DCC does nothing: the mixin reads `self.smap` (absent) and compares an `Enumerated` to `"enable"` (never equal); the state machine's disabled branch
   reads `pdu.apduType` off an undecoded PDU, so a disable can never be undone. Own handler plus a filter in front of the state machine.
2. An Abort raised by the application leaves the server flag off (`0x70` where stock sends `0x71`); the client treats it as its own and times out.
3. The `device,4194303` wildcard never matches (ObjectIdentifier vs tuple): `nmap bacnet-info` got `unknown-object`.
4. RPM `all`/`required` iterate a Python `set` (order varies per process, properties stock lacks): explicit stock lists.
5. ReadRange is `raise NotImplementedError()` (clients see `device/operational-problem`). WPM answers a missing object with a plain Error and no
   `firstFailedWriteAttempt`; stock names the object and property.
6. Vendor properties, Who-Is reply addressing and bit-string length are fixed in the library; patched at the response/`read_property_to_any` level.
7. Local objects refuse unknown attributes (no side tags on an object); four `OctetStringValue` objects log a `notificationClass` exception in
   `_post_init` (harmless); the stock vendor id 260 is not in the vendor registry, so it is set after `from_args`.

## Verdict

**GO.** Nothing a client of the configured objects or the device sees is out of reach: the spike gives the stock transcript and capture, including
stock's bugs (AO uint8, BO read-only, Abort other, DCC quirks). Costs to carry into Task 1: (a) `server.py` is an override layer (about 350 lines of Python)
resting on 0.1.0 internals, so the version must stay pinned; (b) the decoy object types need a data-driven pass (replay table from the golden) and
the trend-log ReadRange, if "equal to stock" is to cover scanners that read them; (c) the datastore feed (provider in, writes out) is a stand-in
here; (d) the client side (protocols plan row 11): the probe is already a bacpypes3 client doing row 11's operations against stock (Who-Is, ReadProperty,
WriteProperty without priority, REAL and ENUMERATED values), so the library side is shown; what is not done is a comparison with the stock
client's own bytes and its scan/address-cache behaviour. The Rust crates in the plan
(`bacnet-rs`, rusty-bacnet) were not compared; this spike only shows the Python route is viable, not that it is cheaper than them.
