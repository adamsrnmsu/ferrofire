# ICCP / TASE.2 server (`<iccp-server>`)

Bead ww-otj.46. Not in stock bennu, so there is no oracle. Two young open TASE.2 stacks (igs-iec-tase2, telcon-tase2) and two real captures
now cross-check it (docs/testing.md, ww-otj.67); the proof was three-fold before them and its limits are stated here: (1) libiec61850's generic MMS client (`mms_utility`) browses and reads the server, which proves the MMS and
OSI layers against a third party; (2) tshark's `mms` dissector decodes every frame with 0 malformed and no expert warnings; (3) our
own thin ICCP client (`tests/common/iccp_client.rs`, command form `examples/iccp-client`) exercises the TASE.2 parts, which proves
the server agrees with **our reading** of IEC 60870-6-503, not with a third-party implementation. Feature `proto-iccp` (default on,
same dependency as IEC 61850: `iec61850-rs` 0.4.0, MIT OR Apache-2.0).

**UNVERIFIED against the standard text.** IEC 60870-6-503 and -802 are paywalled and not in this repo. The object layouts below are
from recall of public implementations and documentation; the validity (valid, held, suspect, notvalid) and current-source
(telemetered, calculated, entered, estimated) codes and their position in the quality octet were corroborated by a vendor document
(Ipesoft D2000), nothing else was. Everything marked "recall" is one place to fix in `model.rs` / `transfer.rs` if a real EMS
disagrees. First thing to do with a real peer: capture its Initiate and one InformationReport and diff against `conformance/iccp/`.

## What it serves

```xml
<iccp-server>
  <endpoint>tcp://0.0.0.0:102</endpoint>
  <event-logging>rtu-1-iccp.log</event-logging>
  <bilateral-table><id>BLT_RANGE_EMS1</id></bilateral-table>     <!-- value of Bilateral_Table_ID; default BLT_1 -->
  <tase2-version>2000.8</tase2-version>                           <!-- TASE2_Version, year.month; default 2000.8, 1996.8 for an old peer -->
  <data-state><tag>brkr</tag><name>BRKR_1</name><variant>QTimeTag</variant></data-state>
  <data-real><tag>load-power</tag><name>LOAD_MW</name><variant>QTimeTag</variant></data-real>
  <data-discrete><tag>tap</tag><name>TAP</name></data-discrete>
  <data-state><tag>cmd1</tag><name>CMD_1</name><variant>plain</variant></data-state>
  <peer>                              <!-- one row of the bilateral table per partner; at least one -->
    <name>EMS1</name>
    <domain>EMS1</domain>             <!-- the ICC scope the partner sees its objects in; default = name -->
    <address>10.0.0.5</address>       <!-- optional: only this remote IP; without it the row applies to any address -->
    <read>BRKR_1 LOAD_MW</read>       <!-- names, space or comma separated, or *; default * -->
    <write>CMD_1</write>              <!-- default: nothing -->
    <max-data-sets>16</max-data-sets> <max-transfer-sets>8</max-transfer-sets>
    <!-- optional, for clients that neither define a data set nor read Next_DSTransfer_Set (igstase2-client `start`): every
         association of this partner starts with these data sets and with DSTrans_1..N already allocated -->
    <preallocated-transfer-sets>2</preallocated-transfer-sets>
    <data-set><name>PRESET_DS</name><members>BRKR_1 LOAD_MW</members></data-set>
  </peer>
</iccp-server>
```

`name` defaults to the tag; names are letters, digits, `_` and `-`, at most 32. `variant` is `plain`, `q` (default) or `qtimetag`.
A tag of the wrong kind is a load error (state needs a binary tag, real and discrete an analog one); an unknown tag is a warning
and the object is skipped (`--strict`: an error). A `<read>` or `<write>` that names no object is a load error.

### Bilateral table: what it does and does not do

- A partner is identified by its **remote IP address** (the row with that `<address>`, else the first row without one). An address
  that matches no row gets its TCP connection closed before an association slot is taken. The ACSE AP-title / AE-qualifier the real
  standard keys on is **not available** from the OSI layer of the pinned crate (`Negotiated` does not expose it), so it is not
  checked. On the range this is the right key anyway: one EMS VM, one IP.
- Each row is its own **domain** (ICC scope). A partner sees only its domain: `GetNameList` of domains returns just that one, and any
  other domain name is object-non-existent. VCC (VMD) scope holds only `Bilateral_Table_ID`, `Supported_Features`, `TASE2_Version`.
- `<read>` / `<write>` are per object. An object in neither list is **invisible** (not listed, read gives object-non-existent). In
  the read list only: a write gives object-access-denied (3).
- The bilateral table is static configuration; nothing a client does changes it.

### Objects (recall)

| Config | MMS value, `plain` | `q` | `qtimetag` |
|---|---|---|---|
| `data-real` (analog tag) | float32 | {float32, flags} | {timestamp, float32, flags} |
| `data-discrete` (analog tag, rounded, clamped to 32 bit) | integer | {integer, flags} | {timestamp, integer, flags} |
| `data-state` (binary tag) | bit string(2): 1 off, 2 on | flags (bit string 8) | {timestamp, flags} |

Flags are one octet, first bit first: state (2 bits: 0 between, 1 off, 2 on, 3 invalid), validity (2: 0 valid, 1 held, 2 suspect, 3
notvalid), current source (2: 0 telemetered), normal value (1), timestamp quality (1). The device always sends source telemetered,
normal, timestamp valid; validity is valid, or notvalid if the tag has no value. The timestamp is integer unix seconds, from the
provider's last update of the point (the clock for tags with none). Not built: the `*Extended` types (COV counter), `Data_Real` with
`TimeStampExtended`, Data_Structure.

### Services

| Service | Behaviour |
|---|---|
| Initiate / Conclude, Identify, Status, GetCapabilityList | the crate's association; Identify `ferrofire iccp-server <version>`; status operational; empty capability list |
| GetNameList | domains (own only); VMD variables; domain variables (visible objects, `Bilateral_Table_ID`, `Next_DSTransfer_Set`, allocated transfer sets); data sets. Sorted, 100 per page with `moreFollows` |
| GetVariableAccessAttributes | the type of every object, special variable and the DSTransfer_Set |
| Read | named variables (objects, specials) and a named data set (one result per member, denied members fail individually) |
| Write | objects the table lets the partner write: becomes `Store::write` (queued, effective when the provider ACKs, like every protocol); wrong type is type-inconsistent (7), state `invalid`/`between` is object-value-invalid (11); quality and time a client sends are ignored. DSTransfer_Sets. Writing a whole data set by name is refused (9) |
| DefineNamedVariableList / DeleteNamedVariableList / GetNamedVariableListAttributes | data sets in the partner's domain, members must be readable objects of that domain, or the report's own `Transfer_Set_Name`, `Transfer_Set_Time_Stamp`, `DSConditions_Detected` (real clients list them first, domain-scoped), max 256 members and `max-data-sets` per association; **they live and die with the association** (a real peer redefines them at each association; persistence is not built) unless the partner row presets them (`<data-set>`, ww-otj.71). A data set a running transfer set reports cannot be deleted (matched, not deleted) |
| anything else (file services, journals, ...) | MMS reject (unrecognised service) |

### Transfer sets and reports

Client reads `Next_DSTransfer_Set` (domain scope): the answer is the name `{scope 1, domain, name}` (scope **unsigned**, as both real
captures and their vendors send it; an integer scope is accepted on input too, ww-otj.64) of an allocated, not yet configured
set (`DSTrans_1`, `DSTrans_2`, ... up to `max-transfer-sets`, then temporarily-unavailable). It writes that variable with the
DSTransfer_Set structure (recall): Data_Set_Name, Start_Time, Interval, TLE, Buffer_Time, Integrity_Check, DSConditions_Requested
(5 bits: interval timeout, integrity timeout, object change, operator request, other external event), Block_Data, Critical, RBE,
All_Changes_Reported, Status, Event_Code_Requested: 13 members (2000 edition); the 1996 edition has no All_Changes_Reported (12 members, as in the 2005
capture, where the flag before Event_Code is true in all four writes: Status; that reading is from the captures, not the standard). Both layouts are accepted whatever `<tase2-version>` says; a read-back uses the layout of the
configured version (ww-otj.65). `Status = true` arms it (the current values become the change baseline, the
timers start); `false` stops it. It can be read back.

Reports are one unconfirmed InformationReport per due condition set, checked every 100 ms. A data set whose members include the report's
own (`Transfer_Set_Name`, ...) is reported whole **by its name** (`variableListName`, domain-scoped), one result per member in the data
set's order, with the report members filled in: the layout of both real captures. An RBE report of the changed members only is a
`listOfVariable` of the report members the data set lists (domain-scoped, as mms.pcap frame 86) and then the changed members; a data
set that lists none gets the plain `listOfVariable` =
`Transfer_Set_Name` (VMD scope, the `{scope, domain, name}` structure), `Transfer_Set_Time_Stamp` (integer unix seconds),
`DSConditions_Detected` (bit string 5), then each reported member by its domain-scoped name. The names are plain MMS text, so
Wireshark shows them (see "Why no Lua dissector"). The item order is recall.

| Covered | Not covered (refused with object-value-invalid where a client could ask) |
|---|---|
| DSConditions interval timeout, integrity timeout (the whole data set), object change (value change of any member, held back for Buffer_Time seconds after the first change so that changes gather into one report), RBE (change-only reports) | operator request and other external event conditions; Critical (refused; so no Transfer_Report acknowledgement); Block_Data (refused); Start_Time, TLE, All_Changes_Reported, Event_Code (accepted, stored, no effect) |

### Conformance blocks, stated plainly

Block numbers are recall too (1 basic services, 2 extended condition monitoring, 3 blocked transfers, 4 information messages, 5
device control, 6 programs, 7 events, 8 additional events, 9 time series). `Supported_Features` reports **blocks 1 and 2** (the first two
bits of a **12-bit** string, IEC 60870-6-503 8.2.9: bit n is block n+1; ferrofire sent 9 bits before ww-otj.62, which igs-iec-tase2 rejects).
`TASE2_Version` is `{2000, 8}` by default (field peers; igs requires it, 503 5.3.3.3 ends the association on a mismatch) and
`<tase2-version>1996.8</tase2-version>` selects the edition of the 2005 capture (and the 12-member DSTransfer_Set read-back). The Initiate response never negotiates a nesting level
above the client's proposal (ww-otj.61), and object-non-existent as a service error is `access` 2. What that means here:

- **Block 1**: association, domain/VMD browse, Read and Write of Data_Real/State/Discrete (+Q, +QTimeTag), data sets, transfer sets with
  interval and integrity timers. Complete except what is in the table above.
- **Block 2**: object-change and RBE reporting only. Not the operator-request/external-event conditions or critical
  reports with acknowledgement.
- **Not built, ever, unless asked**: blocks 3 to 9. In particular no Block 5 device control (select-before-operate `Command` /
  `Device` objects with check-back names): a partner that must operate a breaker writes a `data-state` object that is in its
  `<write>` list, which is a plain data value write, not a TASE.2 device operation.
- Not built: TLS (IEC 62351-3), IEC 62351-4 authentication, redundant/secondary associations, VCC-scope data objects.

## Limits and failure behaviour

16 associations at most; one that never finishes its handshake within 10 s, or stops reading until its 256-PDU queue fills, is dropped;
a malformed TPKT, COTP, session, presentation, ACSE or MMS PDU closes that association, never the device; a bind or `accept()`
failure is logged and survived. Each write, each data set and each transfer-set change is a line in the event log. A report larger
than the partner's negotiated PDU is silently not sent (members are capped at 256, about 12 KB).

## Why no Lua dissector

Plain MMS in tshark already prints every ICCP name (`Transfer_Set_Time_Stamp`, `DSConditions_Detected`, `EMS1`/`LOAD_MW`,
`DSTrans_1`) and the structure shapes, so an analyst can follow a conversation without help; only floats show as raw hex
(`floating-point: 0800000000`) and the flag octets as bit strings. A dissector would also encode our own recall of the layouts as if it
were authoritative, which is the wrong thing to ship while they are unverified. Revisit once a real peer capture confirms the layouts.
The acceptance harness asserts on the plain MMS fields (`mms.vmd_specific`, `mms.domainId`, `mms.itemId`).

## Using it from an EMS / SCADA VM on the range

1. In the field device's config, map the tags the EMS should see, and add one `<peer>` per EMS with that EMS's IP in `<address>`
   (leave it out only if the source address is not stable). List in `<write>` exactly the tags the EMS may command.
2. Point the EMS's ICCP client (or a gateway such as an OSI/TASE.2 stack on the VM) at `tcp://<rtu-ip>:102`; its remote
   address is the RTU's, port 102 (TSEL/SSEL/PSEL defaults as libiec61850 and the crate use: 0001 each). Its Bilateral Table
   should name domain = the `<domain>` of its row and `Bilateral_Table_ID` = `<bilateral-table><id>`; the AP-title is not checked (igstase2-server's tables are keyed by it, so our thin client sends one:
`ICCP_CALLING_AP_TITLE=1.3.9999.1 iccp-client ...`).
3. The EMS associates, reads `Bilateral_Table_ID`, browses its domain, defines a data set of the objects it wants, reads
   `Next_DSTransfer_Set`, writes the DSTransfer_Set with `Status` true and its interval / conditions, and then receives
   InformationReports. With Wireshark on the span port, filter `mms`; the reports are `unconfirmed-PDU informationReport` and name
   the transfer set and conditions. A red-team operator doing the same with `mms_utility` or any MMS client sees the browse and reads.
4. Check by hand, no EMS needed: `iccp-client HOST:102 DOMAIN LOAD_MW,BRKR_1 CMD_1` (build with `cargo build --example iccp-client`).

## Proving it

    cargo test                                  # unit, socket tests with the thin client (tests/iccp_net.rs), hostile-bytes test
    make conformance PROTO=iccp                 # needs make testpeers (or TESTPEERS=<image>) and a built ferrofire + example (make test)
    cargo +nightly fuzz run fd_iccp             # in fuzz/: bytes into the OSI stack, or one MMS PDU into the handler; no panic

`make conformance PROTO=iccp` runs `mms_utility` (domains, identity, directory, three reads) and the thin client (browse, read,
write, data set, transfer set, three reports) against `conformance/iccp/server.xml`, then asserts: tshark 0 malformed, 0 expert
warnings, Initiate/GetNameList/Read/Write/DefineNamedVariableList/InformationReport present, ICCP names in the MMS fields, what both
peers printed. The thin client is copied into the peers container (`PEER_BIN` in `proto.env`). No Zeek parser exists for MMS.
