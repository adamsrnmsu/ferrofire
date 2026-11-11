# IEC 61850 server (`<iec61850-server>`)

Bead ww-otj.45. Not in stock bennu (`bennu-field-device` never loads it, PLAN section 0), so there is no oracle: the proof is libiec61850's
unmodified clients and GOOSE subscriber, tshark's `mms` and `goose` dissectors, and our own tests. Feature `proto-iec61850` (default on:
the dependency has no required crates beyond `rawether` and `roxmltree`; `--no-default-features --features ...` leaves it out).

## What it serves

An MMS server (TCP, default port 102) whose namespace is generated from the device's point map, and an optional GOOSE publisher.

```xml
<iec61850-server>
  <endpoint>tcp://0.0.0.0:102</endpoint>
  <event-logging>ied-1-iec61850.log</event-logging>
  <ied-name>simpleIO</ied-name>          <!-- default simpleIO -->
  <ld-inst>GenericIO</ld-inst>           <!-- default GenericIO; the MMS domain is ied-name + ld-inst: simpleIOGenericIO -->
  <binary-input><tag>brkr</tag></binary-input>
  <analog-input><tag>load-power</tag></analog-input>
  <binary-output><tag>breaker-cmd</tag><sbo>1</sbo></binary-output>
  <analog-output><tag>mw-setpoint</tag><ln>MMXU2</ln><name>SetMw</name></analog-output>
  <goose>
    <interface>eth1</interface>          <!-- required; no GOOSE element, no GOOSE -->
    <mac>01-0C-CD-01-00-01</mac>         <!-- default -->
    <appid>1000</appid>                  <!-- default -->
    <vlan-id>100</vlan-id>               <!-- optional; the frame is priority-tagged (VLAN 0, priority 4) without it -->
    <vlan-priority>4</vlan-priority>
    <min-time-ms>4</min-time-ms>         <!-- T1, default 4 -->
    <max-time-ms>1000</max-time-ms>      <!-- T0, default 1000 -->
    <control-block>gcbAnalogValues</control-block>
  </goose>
</iec61850-server>
```

The defaults are the names libiec61850's example server uses (`simpleIOGenericIO`, `GGIO1`, `LLN0$Events`, `EventsRCB01`, GoCB
`gcbAnalogValues`, appid 1000), so its stock clients and subscribers work without patching.

| Config element | Data object (default name) | Common data class | Writable by a client |
|---|---|---|---|
| `<binary-input>` | `GGIO1.Ind<n>` | SPS (`stVal`, `q`, `t`) | no |
| `<analog-input>` | `GGIO1.AnIn<n>` | MV (`mag.f`, `q`, `t`) | no |
| `<binary-output>` | `GGIO1.SPCSO<n>` | SPC (`stVal`, `Oper.ctlVal` boolean) | `Oper` (direct, or SBO with `<sbo>1</sbo>`) |
| `<analog-output>` | `GGIO1.APCSO<n>` | APC (`mxVal.f`, `Oper.ctlVal.f`) | `Oper` (direct, or SBO) |

`<ln>` (four capital letters and a number, `MMXU2`) and `<name>` (letters, digits, `_`, at most 32) override the node and the data
object per point; the number in a default name counts per node and role. Two points cannot share a node and a name. Every name is
validated at load because it is written into SCL text unescaped. The data set `Events` holds every point's value and quality; one
unbuffered report control block `EventsRCB01` (dchg, qchg, integrity, general interrogation) and the GoCB publish it.

Behaviour (all through the datastore, like every module):

- **Reads** come from the datastore: a 4 Hz refresh pushes each changed tag (and the time next to it) into the model; the commit
  produces reports and, if enabled, a new GOOSE state with the 4 ms to 1 s retransmission curve.
- **Controls**: an `Oper` becomes `Store::write`, which is queued and takes effect when the provider ACKs it (SPEC 3.2). Select, cancel
  and the control-model checks are the crate's (`ctlModel` 1 direct-normal, 2 SBO-normal). A wrong value type or an unmapped object is
  refused with an `AddCause`. Each operate is a line in the event log.
- **Limits**: at most 16 associations; one that stops reading is dropped when its 256-PDU queue fills; a malformed TPKT, COTP, session,
  presentation, ACSE or MMS PDU closes the association, never the device. A bind failure or an `accept()` failure (descriptors) is
  logged and survived. File services are off (`ObtainFile` is refused).

## GOOSE: CAP_NET_RAW and layer 2

GOOSE is not IP: it is an Ethernet frame (ethertype 0x88B8) sent to a multicast MAC. Consequences for a range:

- The process needs **`CAP_NET_RAW`** to open the `AF_PACKET` socket. Docker: `--cap-add NET_RAW` (the default set already has it);
  bare metal: `setcap cap_net_raw+ep $(which ferrofire)` or run as root; Kubernetes: `securityContext.capabilities.add: [NET_RAW]`.
  Without it the publisher logs `cannot open <if> (needs CAP_NET_RAW)` to the event log and stderr and the MMS server runs on.
- Publisher and subscribers must share a **layer-2 segment** (same bridge or VLAN): no router forwards GOOSE. In phenix that is a
  switch/bridge the VMs are all attached to; `<interface>` is the NIC on that segment. On a bridge that filters multicast, join the group.
- Tagged frames: the crate always adds an 802.1Q priority tag; `<vlan-id>` sets the VLAN, otherwise it is VLAN 0 (priority-tagged).
- libiec61850 v1.6's subscriber does not deliver frames sent to `01-0C-CD-01-00-00` (observed on `lo`); the default is `...-00-01`.
- The GOOSE data set is the whole point map (value and quality of each point, 2 entries per point), `confRev` 1.
- **Subscriber: not built.** A field device has no consumer for the values, and the datastore's inputs come from the provider. The crate
  has a `Subscriber` (IEC 62351-6 replay rule, Edition 2 simulation bit) if a scenario needs one.

## The shared MMS layer (for ICCP)

`src/fd/mms.rs` is protocol-neutral and is what ww-otj.46 (ICCP/TASE.2) should reuse:

- `Handler` trait: `request(id, now, invoke_id, pdu) -> (answer, due)` for a confirmed-request PDU, `tick` for timers and unsolicited
  PDUs (reports, InformationReport), plus `opened`/`established`/`closed`.
- `Mms::new(handler, Config)` then `serve(listener, log, stop)`: tokio accept loop, one task per association over the sans-IO
  `Association::server` (TPKT, COTP class 0 with TSDU reassembly, session, presentation, ACSE, ISO 9506 framing), bounded outbound
  queues, the stop token, `mms.with(|h| ...)` to run code on the handler from another task (the datastore refresh).
- `step(handler, id, association, now)` is the same function the socket loop uses and the fuzz target feeds directly.

ICCP differs above this: Initiate carries TASE.2 parameters, the namespace is domain-scoped named variables (bilateral tables, data
sets, transfer sets) instead of an SCL model, so it implements `Handler` itself and decodes requests with
`iec61850_rs::proto::mms::Mms::parse`, which is public. `proto-iccp` already enables the dependency.

## Spike result (2026-11-10)

| | `iec61850-rs` 0.4.0 (hupe1980) | `iec61850-{mms,goose,server}` 0.4.0 (csp0924) | own ISO stack |
|---|---|---|---|
| Licence | MIT OR Apache-2.0 | MIT OR Apache-2.0 | MIT |
| Shape | one crate, ~50k lines, `forbid(unsafe_code)`, clippy denies panic/unwrap/indexing; deps optional (`roxmltree`, `rawether`) | 4+ crates, server 87k lines, optional tokio/rusqlite (C)/rustls | 6 layers plus MMS: about 3 days to the subset, 6+ to what a client expects |
| Server MMS | Initiate, GetNameList, GetVariableAccessAttributes, Read, Write, reports, controls, data sets, logs, files; model = SCL | same set; GetNameList follows run-time registries (needs `register_from_model`) | the subset only |
| Seams | `Association::server`, `Acsi`, `Ied` public and sans-IO; GOOSE frames via `Acsi::drain_frames` | async-first | n/a |
| Maintenance | 51 commits, 0.1.0 2026-09-05 to 0.4.0 2026-11-09, CI; claims libiec61850 and csp0924 interop tests | 535 commits, last push 2026-11-01 | ours |
| Risk | pre-1.0, every minor breaks (pin `=0.4.0`), single author, 338 downloads | the same | all of it is ours |

Picked by evidence: the in-process client round trips, libiec61850 clients operating it, and the pcap are in `tests/iec61850_*.rs` and
`conformance/iec61850/`. Not included upstream: TLS (62351-3), 62351-6 authentication, routable GOOSE (R-GOOSE).

## Proving it

    cargo test                                  # unit, socket (iec61850-rs client), robustness (mutated transcripts), accept()-failure
    make testpeers                              # once, and again after docker/testpeers/Dockerfile changed
    make conformance PROTO=iec61850             # libiec61850 clients + GOOSE subscriber on lo, tshark mms/goose, DUT=rust by default

Fuzz: `fuzz/fuzz_targets/fd_iec61850.rs` (`cargo +nightly fuzz run fd_iec61850` in `fuzz/`): arbitrary bytes through the OSI stack and
the ACSI server via `fd::mms::step`; the property is no panic.
