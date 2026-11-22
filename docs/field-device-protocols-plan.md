# Rust field device: protocol plan (parity with stock bennu, plus extras)

Bead ww-otj.2 (implementation of `field-device-spec.md`, the SPEC). Status: PLAN for Ryan's approval, nothing built, SPEC not edited.
Trigger (Ryan, 2026-11-10): "same protocol support as bennu, and any additional OT protocols that we can find and easily implement".
Citation keys as in the SPEC (`B:`, `MOD:`, `FD:`, `A:`, `R:`, `O:`), plus `GEN:` = `A:templates`, `GO:` = `B:src/gobennu`. crates.io data
fetched 2026-11-10. Everything about clients, serial, BACnet and the extras is **source-only (UNVERIFIED on the wire)**; the SPEC's
`O:` facts cover Modbus/DNP3/104 servers only. Oracle stays the deployed `bennu:arm64` image (SPEC Decision 9).

## 0. What "the same as bennu" is

The stock loader builds `base, modbus, iec60870-5, dnp3, bacnet` (`MOD:comms/CMakeLists.txt:1-5`) and registers exactly four protocols in
two roles: `modbus|dnp3|bacnet|iec60870-5-104` × `-server|-client` (`MOD:comms/base/CommsModuleCreator.cpp:23-30`). Modbus and DNP3 also take a
serial device as `<endpoint>`. The sceptre generator emits every one of them (`GEN:fd_server.mako:42-71`, `fd_client.mako:15-28,95-104`).
Not parity, with evidence: **IEC 61850** (not in the CMake list, not registered, links libraries absent from the tree: dead code), **SunSpec**
(`GO:`, Go, `BUILD_GOBENNU` default OFF `B:CMakeLists.txt:66` and `-D BUILD_GOBENNU=OFF` in `B:Dockerfile:119`, so not in the image, and
removed upstream), **IEC 101** (no `CS101_*` call under `MOD:iec60870-5`). The EXTRA survey's "61850 is parity" row is overruled by those files.

## 1. Tiers

### Tier 1: full stock parity (all generator-reachable; the SPEC covers rows 1, 3, 5 already)

| # | Protocol / role | Parity means (config keys, behaviour) | Proven by (SPEC §8 method) | Days |
|---|---|---|---|---|
| 1 | Modbus/TCP server | SPEC §4.1 unchanged (FC 1-6/15/16, scaling, Q1-Q4, Q9 fix, event log). **Built (ww-otj.57, 2026-11-10)**: byte-equal to stock on five recorded sets, divergences listed in `conformance/README.md` | 24-row golden `run-stock.sh modbus`, fuzz | 1.5 (F1) |
| 2 | Modbus serial server + client | `<endpoint>` without `tcp://` = device path. **MBAP framing on the port, no CRC, no slave byte** (`MOD:modbus/module/SerialChannel.cpp:100-122`: reads the MBAP length at bytes 4-5 and hands `len+6` to the same stack). 9600 8N1 is stored but never applied (`:17-29`), so the port runs at OS defaults; no baud/parity keys exist. Client timeout 1000 ms (`B:src/bennu/distributed/SerialClient.hpp:18`). The generator also emits an ignored `<ip>` (`fd_server.mako:56-60`) | pty pair (`socat pty,raw pty,raw`): stock client ↔ our server and our client ↔ stock server, bytes equal | 0.5 (F6) |
| 3 | DNP3 outstation (TCP) | SPEC §4.2 unchanged | golden `run-stock.sh dnp3` + `R:images/bennu-smoke.sh` (stock client) | 5-6 (F3) |
| 4 | DNP3 serial server + client | opendnp3 `SerialSettings` defaults: 9600 8N1, no flow, 500 ms open delay (`B:.../dnp3/protocol/.../channel/SerialSettings.h:37-44`); link address = last IP octet for `dnp3-serial` (`fd_server.mako:47-51`), client address 1; no baud keys. Link/transport/app layers identical to TCP | pty pair, both directions | 1 (F6) |
| 5 | IEC 104 server | **Built (ww-vck, 2026-11-22)**, `src/fd/proto/iec104/` (APCI, ASDU codec and slave without I/O, a socket loop per connection). SPEC §4.3, rewritten from the measured stock record `conformance/iec104/STOCK.md`: GI at COT 6 or 8, 45/46/50 ACT_CON then ACT_TERM, select bit refused, single redundancy group (a STARTDT deactivates the others; the report goes to the last-opened connection until any closes), k 12 / w 8 / t1 15 / t2 10 / t3 20, at most 5 masters. `sbo-enabled` true refused (ww-3a4). Divergences in `conformance/README.md` (IEC 60870-5-104) | `conformance/iec104` (`SET=single\|double\|big`): transcripts equal to `stock/iec104-server[-SET].golden`, N(S)/N(R) checked in tshark; the stock 104 client against ours differs from `stock/client-iec104.golden` by the store's ACK hold (a commanded value in the report for 2 s), open | 2-3 (F4) |
| 6 | BACnet/IP server | **Built (ww-7mw.3, 2026-11-22) as an external module**, `modules/bacnet/` on bacpypes3 ([modules.md](modules.md)); a stock element loads unchanged. Golden `stock/bacnet-server-feed.golden` (stock fed by a provider) and `stock/bacnet-server.golden` (none); ours equals the fed one but for the divergences listed in `conformance/README.md` (BACnet/IP); nmap `bacnet-info` equal; the stock `bacnet-client` gets the stock transcript from ours. `<bacnet-server>{endpoint udp://ip:port (prefix mandatory, ip/port parsed then **discarded**), instance (device id), event-logging (default bacnet-server.log), binary-input/binary-output/analog-input/analog-output{address = object instance, tag}}` (`MOD:bacnet/module/DataHandler.cpp:54-104`, `Server.cpp:22-30`). Services: Who-Is/I-Am (+ one I-Am at start), ReadProperty, RPM, WriteProperty, WPM, ReadRange, DCC, reject the rest (`.../bacnet/basic/object/bacnet.c:384-440`). 1 Hz refresh from the datastore: BI/AI present value, BO priority 0, AO priority 16; network writes enqueue the tag (`Server.cpp:67-105`). Transport from `BACNET_IFACE`, `BACNET_IP_PORT` (47808), `BACNET_BBMD_*` (`.../datalink/dlenv.c:404-500`). Object list: fixed 1000 objects per type (UNVERIFIED) | byte-diff against the two goldens (`probe_bacnet.py`, `run-stock.sh bacnet`: Who-Is, RP/RPM on device + each object, WP, unknown object, DCC); `nmap --script bacnet-info` output equal; stock `bacnet-client` reads/writes ours | 4-5 incl. spike (F4b) |
| 7 | Command interface (every client) | **Built (ww-7mw.1, 2026-11-18)** as `src/fd/cmdif.rs`. ZMTP REP at `<command-interface>`; the request is read as a C string, the reply ends in a NUL. As captured from stock (ww-7mw.1, `conformance/stock/client-modbus.golden`): the op is `QUERY`/`query`, `READ`/`read`, `WRITE`/`write` and nothing else, so **not** case-insensitive (`Query=` → `ERR=Unknown command type (must be QUERY\|READ\|WRITE)`); the tag is the text between the first and second `=` (`READ=load-power=x` reads `load-power`). `QUERY=` → `ACK=t1,t2,` in name order. `READ=t` → `ACK=t:v`, `v` a float32 printed `%f` (`ACK=load-power:250.492096`) or `true`/`false`, by the point's type; an unmapped tag → `ERR=Client does not have a mapping for tag 't'`. `WRITE=t:v` is dispatched by the text of `v`, not by the point: `true`/`false` is a binary write, anything else goes through `stod` (so ` 7`, `7x`, `0x10`, `nan`, `inf`, `5:6` are numbers); the reply `ACK=Wrote tag t -- v` echoes `v` as sent. A failed read or write → `ERR=Failed reading tag 't': ` / `ERR=Failed writing tag 't': ` followed by bytes of a destroyed string (masked `<text>` in the golden). Stock **dies** on a request without `=`, a `WRITE` without `:v` and a `v` that `stod` refuses (`WRITE=gen-power:TRUE` → `terminate called ... stod`). Source: `MOD:base/CommandInterface.cpp:35-137`. Ports in generated configs: 1330 dnp3/104, 1331 modbus, 1332 bacnet | `conformance/modbus-client`, `conformance/dnp3-client`: the transcript of `stock/probe_cmdif.py` equal to the stock client's | 1 (F5) |
| 8 | Modbus client | **Built (ww-7mw.1, 2026-11-18)**, with a `fep` mode beside the stock one: [field-device-clients.md](field-device-clients.md); divergences in `conformance/README.md`. `<modbus-client><modbus-connection>*{endpoint, unit-id (default 0), coil\|discrete-input\|holding-register\|input-register{address,tag,max-value,min-value}}`. **No polling, no scan-rate** (`ClientScanBlock` is dead). As captured (`conformance/stock/client-modbus.golden`): one TCP connection per `<modbus-connection>`, opened at start and kept; a request finding it closed makes one connect attempt and fails at once when that is refused (`> SYN` `< RST`), and there is no response timeout (a server that accepts and stays silent blocks the client, and its command interface, for good: no reply in 100 s). The transaction id starts at 3 and counts every request, sent or not. READ = live FC 1/2/3/4 of one register (`> 000600000006010475300001`), unscaled to a float32 `(raw - intercept) / slope`; WRITE of `true`/`false` = FC5 at the point's address whatever its table, WRITE of a number = FC6 with `value*slope + intercept` converted to an unsigned 32 bit integer and cut to 16 bits, so not clamped (`WRITE=load-mw-setpoint:2000` over 0..1000 → `fffe`, `-5` → `0000`, `nan` → `0000`); a point with no range (a coil) writes 0. An exception or a lost connection is the `ERR=Failed ...` reply. Source: `MOD:modbus/module/ClientConnection.cpp:72-136,212,243`, `DataHandler.cpp:109-208`. Generator closes the element as `</modbus-tcp-client>` (`fd_client.mako:97-99`): the loader must tolerate it | `conformance/modbus-client`: our client vs the stock `modbus-server`, transcript (replies and Modbus bytes) equal to stock-vs-stock; generated fd-client config loads | 1 (F5) |
| 9 | DNP3 master | **Built (ww-7mw.1, 2026-11-18)**, with a `fep` mode beside the stock one: [field-device-clients.md](field-device-clients.md). `<dnp3-client>{address, scan-rate (s), command-interface, dnp3-connection*{endpoint, address, binary/analog-input{address,tag}, binary/analog-output{address,tag,sbo}, class-scan-rates{all,class0..3}}}` (`DataHandler.cpp:145-262`). As captured (`conformance/stock/client-dnp3.golden`, every frame in `client-dnp3-raw.golden`): unconfirmed user data (link control `c4`); on the first connection READ class 0 (`c0c0013c0106`), then, the response carrying IIN1.7, WRITE g80v1 index 7 = 0 (`02500100070700`, clear restart) and READ class 0 again; **no** enable- or disable-unsolicited request is ever sent (the unsolicited mask is class 0, which names no event class). The `all` scan is READ class 1, 2, 3, 0 (`013c02063c03063c04063c0106`), first right after startup and then every `all` seconds after the previous one (`all` defaults to `scan-rate`; a per-class rate of 0 is off). A lost connection is retried at once, then after 1, 2, 4 ... s; the application sequence restarts at 0 on the new connection and the transport sequence does not, and startup is not repeated: the overdue scan goes first, and only a response with IIN1.7 brings the clear-restart and the class 0 read back. The cache maps binary addresses and analog addresses to tags, inputs and outputs in one map each; only groups 1/2 and 30/32 update it (output status is ignored). READ answers from the cache and never fails. WRITE follows the text of the value (row 7): `true`/`false` is a CROB LATCH_ON (3) / LATCH_OFF (4), count 1, 100 ms on and off (`050c0128010005000301640000006400000000`), a number is g41v3 (`05290328010002000000e04000`); a tag with `sbo` sends SELECT then OPERATE, the others DIRECT_OPERATE, also for an input (`WRITE=brkr:false` operates CROB 2); the operate is not sent when the select is refused or echoes a NaN. The cache is updated before the request leaves, the reply is always `ACK=Wrote tag ...`, and a command given while the connection is down is dropped. Source: `ClientConnection.cpp:78-84,146-245`, `ClientSoeHandler.hpp:31-42` | `conformance/dnp3-client`: our master vs the stock outstation, transcript (replies and frames, sequence numbers and periodic scans masked) equal to the stock master's | 3-4 (F5) |
| 10 | IEC 104 client | **Built (ww-vck, 2026-11-22)**, bennu and fep modes: `src/fd/proto/iec104/client.rs` (the master and the fep `Poller`, without I/O) and a socket loop per connection in `iec104/client_shell.rs`. `<iec60870-5-104-connection>*{endpoint (tcp:// only, else a load error: stock exits), sbo-enabled (true refused, ww-3a4), binary/analog input/output{address,tag}}`, no scan-rate. Measured stock (`conformance/iec104/STOCK.md`, "Client"): it connects once and never again, STARTDT and **one** GI (originator 3, CA 1) back to back, then relies on the server's rpoll; decodes only `M_ME_NC_1` and `M_DP_NA_1` (Q12: `M_SP_NA_1` and the report's `M_DP_TB_1` ignored; `--strict` decodes both); a WRITE is `C_DC_NA_1` or `C_SE_NC_1` by the value text, **direct execute** (S/E 0), to the tag's address, ACKed at once whatever the server answers. `<mode>fep`: a GI every `<scan-rate>`, `<status-tag>`, writes down through the echo rule, reconnect with backoff ([field-device-clients.md](field-device-clients.md#fep-mode)) | `conformance/iec104-client` with `DUT=rust`: transcript equal to `stock/client-iec104.golden` (no `rust-client.diff`); `tests/iec104_master.rs`, `tests/iec104_fep.rs` | 1.5-2 (F5) |
| 11 | BACnet client | **Built (ww-7mw.3) as an external module**: bennu's sample loads unchanged (the device declares a manifest-served client's point tags at load, as for compiled clients). Against the stock server its scan and request bytes equal the stock client's; the listed divergences are the device's write semantics (`WRITE=` answered at once, sent with the next cycle; a write of the value held sends nothing; the poll right after a write is skipped; no `WRITE=` to an input tag), `conformance/bacnet-client/rust-client.diff`, open in ww-4s8. `<bacnet-client>{scan-rate (s), bacnet-connection*{endpoint udp://, instance, points}, command-interface}`; no Who-Is: the client binds from the `address_cache` file in cwd (observed stock wire); ReadProperty Present_Value per point every scan-rate; WriteProperty PV without priority, ENUMERATED for binary, REAL for analog (`MOD:bacnet/module/ClientConnection.cpp:55-180`) | our client vs stock `bacnet-server`; `bennu-probe` equal | 1.5-2 (F5) |

Shared across rows: the SPEC §8 differential (same config, same probe, two containers `ff-fd-stock`/`ff-fd-rust`, normalised transcripts
byte-equal) plus the **cross-pair** for clients (stock server ↔ our client through `bennu-probe`; our server ↔ stock client is already in F3-F4b).

### Tier 2: easy additions (≤ 3 days, maintained permissive crate or small spec), ranked by range value

| # | Protocol | Blue / red visibility | Implementation | Days |
|---|---|---|---|---|
| 1 | **S7comm** server (ISO-on-TCP 102) | Zeek ICSNPP s7comm, Arkime, nmap `s7-info`, Metasploit/ISF/Caldera; most-attacked PLC protocol | `snap7-server` 0.1.7 (MIT, young: spike) or ~600 lines own (COTP, setup-comm, ReadVar/WriteVar on DB/M/I/Q mapped to tags, SZL identity with configurable strings: Conpot's defaults are a known honeypot tell) | 3 |
| 2 | **MQTT** publisher/subscriber, Sparkplug B optional | Zeek built-in, Suricata, Arkime | `rumqttc` (Apache-2.0): tags as retained topics, writes by subscription; Sparkplug via `prost` + own schema (`sparkplug-rs` is EPL). **Built (ww-otj.56, 2026-11-11)** as `<mqtt-client>`, feature `proto-mqtt` (**default-on**, unlike the other extras: the crap/mutate gates only see default features; `deny.toml` ignores six rumqttc advisories, see there): [field-device-mqtt.md](field-device-mqtt.md); proven by `make conformance PROTO=mqtt` (Mosquitto, Paho, Tahu host, tshark, Zeek), `tests/mqtt_net.rs`, fuzz `fd_mqtt`; no stock oracle | 2 (+1-2) |
| 2b | **SCPI** instrument server (raw socket 5025) and the `ferrofire scpi` client | LXI instrument tooling (PyVISA, lxi-tools); tshark has no SCPI dissector, so the capture is judged as TCP text plus the transcript | no crate: SCPI is line-based ASCII, the parser is ours. **Built (ww-otj.77, .78, 2026-11-11)** as `<scpi-server>`, feature `proto-scpi` (**default-on**, like MQTT): each point-map tag is a `MEASure` / `SOURce` path, IEEE 488.2 status model and SCPI-99 error queue: [field-device-scpi.md](field-device-scpi.md); proven by `make conformance PROTO=scpi` (PyVISA + pyvisa-py, lxi-tools, `ferrofire scpi --check`, 2000-connection churn), `tests/scpi_golden.rs`, `tests/scpi_net.rs`, `tests/scpi_client.rs`, fuzz `fd_scpi`; no stock oracle. HiSLIP and VXI-11 not built | 1-2 |
| 3 | **OPC UA** server (opc.tcp 4840) | ICSNPP opcua-binary; the range already runs an OPC gateway and has `opc_template.mako` | `async-opcua-server` 0.19 (**MPL-2.0**, see §3); flat address space from the point map, None + Basic256Sha256 self-signed. Tier 2 because it fits both thresholds; the cost is a NOTICE entry and a default-off feature until the static build is verified | 3-4 |
| 4 | Modbus extras: FC43/0x0E device id, FC17, FC08, real RTU (CRC) on serial and RTU-over-TCP | nmap `modbus-discover`, Metasploit banner grab; RTU for non-bennu peers | own, on the F1 codec; `<framing>rtu</framing>` opt-in (default stays MBAP for parity, row T1.2) | 1 |
| 5 | Omron FINS server (UDP/TCP 9600) | ICSNPP omron-fins, nmap `omron-info` | own (~400 lines), `omron-fins` 0.6.0 as test client | 2 |
| 6 | IEEE C37.118 PMU (4712) | ICSNPP + Arkime synchrophasor | own (CFG-2/DATA frames, CRC-CCITT; `pypmu` BSD-3 as reference). ferrofire's power flow already yields bus V/angle, so the phasors are real | 2-3 |
| 7 | GE SRTP (18245) | ICSNPP ge-srtp | own (~300 lines) | 2 |
| 8 | DNP3 over UDP (20000) | Zeek DNP3 (UDP parsing UNVERIFIED) | reuse F3 with a UDP socket. **Built (ww-8l7, 2026-11-21)**, outstation and master, `udp://ip:port`: [field-device-spec.md](field-device-spec.md) §4, [field-device-clients.md](field-device-clients.md); proven by `tests/dnp3_udp.rs` and `make diff-udp` (the stock DNP3 probes as datagrams, transcripts equal to the stock TCP goldens) | 0.5 |
| 9 | IEC 60870-5-101 (serial) | none (Zeek sees no serial) | `fegrid-iec60870-cs101` 0.1.0 (MIT/Apache) after F4 | 2 |

Wave 1 = rows 1-4 (9-12 days); rows 5-9 on request. Any Tier 2 protocol is reachable from a scenario only after a 3-line branch in
`fd_server.mako` (or its Go port, ww-otj.6); until then it is hand-written XML.

### Tier 3: worth it, but harder (reason given)

| Protocol | Why worth it | Why harder | Days |
|---|---|---|---|
| EtherNet/IP + CIP (44818) | Rockwell realism; ICSNPP enip, Arkime, Suricata enip, nmap `enip-info`, pycomm3/Caldera | no server crate: own encapsulation (ListIdentity/ListServices/RegisterSession/SendRRData), Identity object, Logix symbolic tag read/write; `rust-ethernet-ip-protocol` 1.2.1 (MIT) for codecs | 4-5 |
| IEC 61850 MMS server + GOOSE publisher (**built**, ww-otj.45) and GOOSE subscriber `<iec61850-rtu>` (**built**, ww-n29: bennu's unbuilt module, SEL CID and standard SCL `ExtRef`; [field-device-iec61850.md](field-device-iec61850.md)) | Caldera `iec61850` (MMS) red value; substation scenarios | crates are ~10 weeks old (`iec61850-rs` 0.4.0 hupe1980 2026-11-09; csp0924 `iec61850-{mms,goose,server}` 0.4.0), blocking I/O, no TLS; GOOSE needs `CAP_NET_RAW` and an L2 segment; Malcolm has no parser | 4-6 |
| PROFINET DCP identify + DCE/RPC EPM | nmap `multicast-profinet-discovery`, Zeek profinet | raw L2 + `CAP_NET_RAW`; full RT IO-device (`pnio`) is 10+ days and needs a PLC master in the range | 3 |
| SunSpec (Modbus profile) | solar scenarios | not in the stock image, nothing emits `<sunspec-tcp-server>`, needs a `string` tag kind the C++ datastore never had (`FD:FieldDevice.cpp:84-93`) and FC3/FC16-only semantics (`GO:.../sunspec.go:40-50`) | 2 |
| HART-IP (5094) | nmap `hartip-info`, ICSNPP hart-ip | spec access UNVERIFIED (HCF membership) | 2-3 |

### Not doing (reason)

| Item | Reason |
|---|---|
| Step Function `dnp3` 1.8.0 / `rodbus` 1.6.0 | non-commercial, non-production agreement, no redistribution (SPEC §5.2; text re-fetched 2026-11-10, unchanged) |
| `lib60870(-sys)` GPL-3, roboplc `iec60870-5` (Bohemia, paid), `s7commplus` (LGPL-3), `sparkplug-rs` + Eclipse `.proto` (EPL-2.0), `cip` 0.1.0 ("non-standard"), `darra-profinet-slave` (proprietary), `rust-snap7` (C core, licence unverified), `bacnet-stack` port (GPL-2 + exception) | licence |
| S7comm-plus, Codesys | proprietary or 8-10+ days each, no blue parser, at most a banner stub later |
| ICCP/TASE.2 | was listed here; Ryan overrode it (2026-11-10) and it is built on the shared MMS layer: docs/field-device-iccp.md |
| EtherCAT subdevice, KNX/IP, Niagara Fox, PCWorx/ProConOS, MELSEC/SLMP, ROC Plus, BSAP, Genisys, C12.22 | no phenix template, niche audience or real-time raw Ethernet; 2-7 days each on request only |
| IEC 61850 as a *parity* item | stock never loads it (§0) |

## 2. Architecture: protocols as modules on the core datastore

The SPEC §2.2 already says "tag name selects a compiled-in module" and "one `Datastore` behind a `Mutex`". This makes it concrete, and no more:

```rust
// src/fd/proto/mod.rs: the only contract a protocol module sees
pub trait Module: Send + 'static {
    /// Parse this module's own <comms> child (e.g. <modbus-server>); errors name the key. Client modules register
    /// their point tags as internal tags here (stock keeps them in the client's own map; same observable behaviour).
    fn parse(node: roxmltree::Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> where Self: Sized;
    /// Serve until `stop`. A bind failure logs and returns; the rest of the device keeps running (stock). Never panics on input.
    fn run(self, store: Store, stop: CancellationToken) -> impl Future<Output = ()> + Send;
}
pub static REGISTRY: &[(&str, Start)] = &[            // Start = fn(Node, &mut Tags, &Opts) -> Result<Boxed run>
    #[cfg(feature = "proto-modbus")]  ("modbus-server", start::<modbus::Server>),
    #[cfg(feature = "proto-modbus")]  ("modbus-client", start::<modbus::Client>),
    #[cfg(feature = "proto-dnp3")]    ("dnp3-server",   start::<dnp3::Outstation>),
    // one line per tag; the loader walks <comms> children and looks each tag up here
];
```

- **Store API** (all a module needs): `kind(tag) -> Binary|Analog` (fixed at load); `get_bool/get_f64(tag)`; `write(tag, value)` into the
  updated-tag queue only (applied after the provider ACK or the scan, SPEC §3.2); `drain_writes(owner)` for the consumer side (the output
  module in an fd-server, the owning `<X-connection>` in an fd-client, which then forwards to the remote server and updates its cache
  optimistically as stock does); `snapshot()` for the 1 Hz refreshers (DNP3, BACnet) and the 104 `rpoll`. Modules poll; no change
  notification until OPC UA subscriptions need one.
- **Shared helpers**, written once in F0/F1 and reused by every row: `EventLog` (SPEC §3.5 CSV), `transport::open(endpoint)` (`tcp://` listener/
  connector or `tokio-serial` path, feature `proto-serial`), `command_interface::serve(endpoint, store, tags)` (ZMTP REP, same code as the REQ
  client of ww-otj.34), `Range` scaling, a fuzz-target template for frame parsers.
- **Enable by feature and by config.** Cargo features `proto-modbus`, `proto-dnp3`, `proto-iec104`, `proto-bacnet`, `proto-serial` are default
  (the parity set); extras (`proto-s7`, `proto-mqtt`, `proto-opcua`, `proto-fins`, ...) default-off, `all-protocols` for the fat build. A module
  runs only if its tag is in `<comms>`; a tag whose feature is off is a load error naming the feature; `ferrofire field-device --list-protocols`
  prints the compiled registry. No tag ever needs a string kind (SunSpec is out), so the datastore stays `bool | f64`.
- **Loader requirements this surfaces:** (a) one `str::replace("</modbus-tcp-client>", "</modbus-client>")` before `roxmltree`, because stock
  (rapidxml, UNVERIFIED) tolerates the generator's mismatch and `roxmltree` does not; (b) `<comms>` may hold several servers, one per protocol
  (`fd_server.mako` loops over `protocols`), already SPEC §3.1; (c) BACnet binds the `<endpoint>` by default and honours the `BACNET_*`
  env vars when set (stock binds env only; see decision 11).
- **A new protocol in a day:** (1) `src/fd/proto/<name>.rs` with `parse` + `run` (~300-600 lines on the Store API, frames bounds-checked,
  connection closed on violation, `--strict` honoured, unknown tag = WARN + skip, no `SHELL_0` in any string, SPEC §6); (2) a `REGISTRY` line and a
  `proto-<name>` feature (cargo-deny, ww-otj.35, must pass); (3) `conformance/<name>/`: sample XML, `probe_<name>.py`, `.golden` from the oracle
  (stock bennu for Tier 1; the protocol's reference client + the nmap script + the Zeek ICSNPP log for extras, via the acceptance harness below);
  (4) a fuzz target from the template, a README row, and a `fd_server.mako` branch if scenarios should emit it.

## 3. Crates, licences, and the three decisions

| Need | Use (ver / date / downloads) | Licence | Excluded alternatives |
|---|---|---|---|
| Modbus server | hand-rolled MBAP (SPEC D3); `tokio-modbus` 0.17.0 as test client | MIT / MIT-Apache | `rodbus` (Step Function) |
| Modbus client | `tokio-modbus` 0.17.0 client (2.0M) or the own codec | MIT OR Apache-2.0 | |
| Serial | `tokio-serial` 5.5.0 / 2026-06 / 8.2M | MIT | |
| **DNP3 outstation + master** | **own**, shared link/transport/app layers; opendnp3 3.1.1 (Apache-2.0, vendored in bennu) as reference only | MIT (own) | `dnp3` 1.8.0 (Step Function); `rusty-dnp3-*` 0.0.1 (17 dl), `industrial-protocols-dnp3` 2.0.0 (unvetted) |
| IEC 104 server + client | `iec104` 0.6.0 / 2026-10 / 2.4k (spike, SPEC D4); fallback `fegrid-iec60870-*` 0.1.0 | MIT; MIT/Apache | `lib60870(-sys)` GPL-3; roboplc `iec60870-5` paid |
| **BACnet server + client** | spike `bacnet-server`/`bacnet-client` 0.15.0 (rusty-bacnet, 2026-10-29, 7k/11k dl) first, `bacnet-rs` 0.3.2 (2026-09, 91k dl, "not production-ready") second; own (7-9 days) if neither passes the golden | MIT; MIT OR Apache-2.0 | `bacnet-stack` (GPL-2+exception): not ported; `embedded-bacnet` client-only |
| ZeroMQ REP/REQ | `zeromq` 0.7.0 (ww-otj.34) or hand-rolled ZMTP NULL (SPEC §5.3) | MIT | `zmq` (libzmq, breaks static) |
| XML | `roxmltree` + the one-line pre-normalise | MIT OR Apache-2.0 | |
| S7comm | `snap7-server` 0.1.7 / 2026-05 / 1k (spike); `tokio-s7` 0.1.7 test client | MIT; Apache-2.0 | `rust-snap7`, `snap7-rs` (C), `s7commplus` (LGPL) |
| OPC UA | `async-opcua-server` 0.19.0 / 2026-07 / 152k | **MPL-2.0** (file-level copyleft: fine unmodified, NOTICE entry, publish any patch to its files, explicit cargo-deny exception) | `open62541` (C), `opcua` 0.12 (unmaintained) |
| MQTT | `rumqttc` 0.25.1 / 9.9M; `prost` for Sparkplug | Apache-2.0 | `sparkplug-rs` (EPL-2.0) |
| FINS, C37.118, GE SRTP, ENIP | own; test clients `omron-fins` 0.6.0 (MIT), `pypmu` (BSD-3), `rust-ethernet-ip` 1.2.1 (MIT) | MIT (own) | `cip` 0.1.0 ("non-standard") |
| IEC 61850 | **`iec61850-rs` 0.4.0, pinned** (spike done 2026-11-10, ww-otj.45, docs/field-device-iec61850.md); csp0924 `iec61850-*` and an own stack not chosen | MIT OR Apache-2.0 | `libiec61850` (GPL, test peer only) |

Policy (lands with ww-otj.35 `deny.toml`): allow MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode; MPL-2.0 only by named exception; deny GPL/LGPL/AGPL/EPL
and anything crates.io calls "non-standard".

**Decisions.** (1) **DNP3**: own outstation (SPEC D2) **and own master** on the same layers; the master is the 3-4 extra days in F5. The Step Function
crate stays out of the default build; a private default-off `dnp3-stepfunc` feature only if Ryan's counsel reads range use as non-commercial,
non-production and the binary is never redistributed. FFI to opendnp3 (SPEC D2 option C) remains the fallback if F3 + the master exceed 10 days.
(2) **BACnet**: parity, crate chosen by the 0.5-day spike against the stock golden, own as fallback. (3) **IEC 61850**: not parity, Tier 3, deferred until
a substation scenario or a Caldera MMS exercise; re-check crate maturity then.

## 4. Phases (merged with SPEC §9; F0-F4 unchanged, F5/F6 resized and corrected, F4b and F7+ new)

| # | Phase | Deliverable | Depends on | Acceptance | Days |
|---|---|---|---|---|---|
| F0 | Core + module registry | SPEC F0 plus `Module`/`REGISTRY`/features, loader pre-normalise, `EventLog`, `transport::open` | ww-otj.35 (cargo-deny); ww-otj.34 (zeromq) is related, shares the ZMTP code, not a blocker | SPEC §8.1; logic corpus; `--list-protocols`; generated fd-client XML loads | 2.5-3 |
| F1 | Modbus/TCP server | SPEC F1. **Done** (`src/fd/proto/modbus`, `conformance/modbus`, `tests/modbus_*.rs`, fuzz `fd_modbus`) | F0 | 24-row golden, fuzz, waterway RTU config | 1.5 |
| F2 | Daemon, watcher, firmware | SPEC F2 | F1 | SPEC §6 tests; **waterway RTUs on Rust** (milestone 1) | 2 |
| F3 | DNP3 outstation | SPEC F3 (link/transport/app shared with the master) | F0 | golden + `bennu-smoke.sh` | 5-6 |
| F4 | IEC 104 server | SPEC F4 | F0 | golden + stock client | 2-3 |
| F4b | BACnet server (new) | stock golden capture (0.5) → crate spike (0.5) → server: 4 object types, Who-Is/I-Am, RP/RPM/WP/WPM/ReadRange/DCC, 1 Hz refresh, endpoint + env transport | F0, golden bead | golden byte-diff, `nmap bacnet-info` equal, stock `bacnet-client` round trip | 4-5 |
| F5 | Command interface + 4 clients (was 3-4 d) | ZMTP REP (1); Modbus client (1); DNP3 master (3-4); 104 client (1.5-2); BACnet client (1.5-2) | F1, F3, F4, F4b | `bennu-probe` against ours vs stock servers, transcripts equal; stock fd-client configs load; smoke test reversed | 8-10 |
| F6 | Serial + packaging (corrected) | MBAP-over-serial (0.5), DNP3 serial with opendnp3 defaults (1), musl static aarch64 + x86_64, overlay swap, docs (1) | F1, F3, F2 | pty pair both directions; `file`/`ldd` static; `rb-` range experiment. **Milestone 2: full stock parity** | 2.5 |
| F7 | Tier 2 wave 1 | S7comm (3), MQTT (2, +1-2), OPC UA (3-4), Modbus extras (1); each its own bead, no mutual dependency | F1, acceptance harness | reference client + nmap script + Zeek ICSNPP log per protocol, pcap in `conformance/<name>/` | 9-12 |
| F8 | Tier 2 wave 2 (on request) | FINS, C37.118, GE SRTP, DNP3/UDP, IEC 101 | F3/F4 as applicable | same harness | 8-10 |
| F9 | Tier 3 (one decision each) | ENIP/CIP, 61850, PROFINET DCP, SunSpec, HART-IP | | same harness | per row |

Running totals: SPEC 15-17 days (F0-F4, F6) → **full parity F0-F6 ≈ 28-33 days** (+12-16 for BACnet, the four clients and the serial corrections) →
+9-12 for wave 1. Acceptance harness (one bead, before F7): `conformance/blue/`: tcpdump the probe run, `zeek -r` with the ICSNPP packages in a
container, assert the expected log files and rows; `nmap --script` output golden. It also proves Tier 1 the way the blue team sees it.

## 5. Decisions for Ryan (recommendation first)

1. **Parity scope** = the stock image: 4 protocols × server + client, Modbus/DNP3 serial, command interface. SunSpec and 61850 out (§0). Alt: add SunSpec (2 d + a string tag kind).
2. **Order**: parity first (F0-F6) before any Tier 2, because the generator emits every Tier 1 protocol and none of Tier 2. Alt: slot S7comm (3 d) after F2 for an early blue/red win.
3. **DNP3**: own outstation + own master; Step Function only as a private default-off feature on counsel's say-so; opendnp3 FFI fallback past 10 days.
4. **BACnet crate**: decided by the spike (`bacnet-server` first, `bacnet-rs` second, own fallback). Alt: own from the start (7-9 d, highest fidelity, most code).
5. **Modbus serial** = MBAP over the port, as stock; real RTU only as the Tier 2 `<framing>rtu</framing>` opt-in. Alt: RTU only (would not interoperate with a stock `modbus-serial` peer).
6. **Tier 2 green light**: wave 1 = S7comm, MQTT, OPC UA, Modbus extras (9-12 d). Alt: S7comm only.
7. **OPC UA under MPL-2.0**: accept (unmodified dependency, NOTICE, deny.toml exception), feature default-off until the static musl build is verified. Alt: skip; keep the OPC gateway.
8. **EtherNet/IP**: build after wave 1 (4-5 d). Alt: skip.
9. **IEC 61850**: defer (Tier 3). Alt: GOOSE publisher only (2-3 d) for a red-team L2 exercise.
10. **Generator XML**: tolerate `</modbus-tcp-client>` in the loader **and** fix it upstream in sceptre-phenix-apps (with the Tier 2 template branches). Alt: upstream only (today's templates would not load).
11. **BACnet transport**: bind `<endpoint>` by default, env vars override when set. Alt: env-only like stock (the `<endpoint>` ip stays decorative).
12. **SPEC errata** (follow-up bead, not applied here): §1/§11.10 BACnet is parity; §4/F6 "Modbus RTU" → MBAP over serial; §4.4 "poll every scan-rate/5" holds for DNP3/BACnet only, Modbus reads live, 104 does one GI; §4.4/F5 budget 3-4 → 8-10 and the REP side of ZMTP; §3.1 loader tolerance; F6 DNP3 serial defaults and link-address rule; §5 BACnet crates; new Q12. Also `README.md:34`, which says the field device does "DNP3, IEC 61850 and ICCP first": neither the SPEC nor this plan says so.
13. **Bead hygiene**: the ww-otj.2 note still says the device talks NNG; ww-otj.13 dropped NNG and SPEC §3.3/§14 (datagram v1 + ZMTP REQ) is current. Update the note.
