# c104 2.2.1 spike: verdict NO-GO

Task 0 gate of the external-modules plan. Question: can a Python module on `c104==2.2.1` match what a SCADA master sees from
stock `bennu-field-device` (spec "Fidelity bar", field-device-spec.md 4.3)? Answer: **no**, on one point that is inside the bar.

Run: `./run.sh stock|c104 single|double` (containers `ff-sdd-t0-*`, removed on exit; `PROBE=<file>` swaps the probe; `KEEP=1`
keeps the container). `server.py` is the c104 server with the stock sample map (`ep/iec60870-5-104-server.xml`, `<subtype>` added
as in conformance/stock/iec104-server.xml): binary-output 0,1; analog-output 2,3; binary-input 100,101; analog-input 102,103; CA 1.
`probe_ext.py` is a wider master probe (acks with N(R), decodes COT; periodic COT 1 frames are summarised). Transcripts in `out/`.
`control_only.py` is the isolated check for the control-point-only case.

## The blocker: outputs cannot be both in the GI and commandable at one IOA

Stock has an output at IOA 0 that (a) appears in the GI as `M_SP_NA_1`/`M_ME_NC_1` (COT 20, "all points incl. outputs") and
(b) accepts `C_SC_NA_1` at IOA 0. c104 holds one point per IOA per station and routes an incoming command by IOA only:

- `add_point` returns `None` when the IOA is taken, whatever the type. A control point added first then makes the monitor point at
  the same IOA fail (GI loses it; the first attempt crashed `server.py` on `None.type`). IOA 0 alone lets two points in, by a bug
  (`station.get_point(0)` is always `None`), and it does not help below.
- Monitor point first (GI correct, byte-identical to stock): a command to that IOA is answered by the monitor point and refused:
  `UNEXPECTED C_SC_NA_1 UNKNOWN_IOA` (IOA 0), `MISMATCHED_TYPE_ID` (IOA 1, 3), wire `2d 01 47 ...` then `2d 01 2f`/`2c`
  (negative ACT_CON, then COT 44/47) instead of stock `2d 01 07` + `2d 01 0a`. The `on_receive` callback never runs, so the
  device would never see a write. 45, 46 and 50 all fail this way.
- Control point only at that IOA (`control_only.py`): the command works (callback runs, echo `2d 01 07 ...`), but the GI then omits
  the point entirely (`01 81 14 00 01 00 64 00 00 00`: only the monitor point at 100). A c104 GI is built from monitoring
  types only; there is no GI hook (`Server` has no `on_interrogation`), and a second station at CA 1 is refused.
- Remapping (monitor shadow at another IOA) breaks "IOA from `address`". Fronting c104 with a frame-rewriting proxy is writing
  an IEC 104 server, which is what the module was to avoid.

So at least one of {GI content incl. outputs, command handling, IOA from `address`} cannot be matched. That is the stop condition
the design names. No Task 1 work started.

## What matched (monitor points only, `out/*-single.txt`, `out/*-probe104.txt`)

- STARTDT con, CA 1, IOA 3 bytes.
- GI QOI 20: ACT_CON (COT 7), `M_SP_NA_1` vsq 4 COT 20 (IOA 0,1,100,101), `M_ME_NC_1` vsq 4 COT 20 (2,3,102,103, QDS 0), ACT_TERM
  (COT 10): the same frames as stock, same order, byte for byte (single mode).
- Other QOI (21): one negative ACT_CON `64 01 47`, no data, no ACT_TERM, as stock.
- Periodic COT 1 every `rpoll-rate` (own timer + `c104.Batch(PERIODIC)`; `report_ms` not used): one `M_SP`+ one `M_ME` ASDU per tick.
- Double mode GI: `M_DP_NA_1` (type 3) `03 04 14 ... 01/02` same bytes as stock.
- 107 test: ACT_CON echo as stock.

## Remaining differences (besides the blocker)

| Area | Stock | c104 | Fixable? |
|---|---|---|---|
| Command reply | ACT_CON 7 + ACT_TERM 10 for 45, 46, 50 | ACT_CON only seen (control-only run, 1 s) | probably: transmit ACT_TERM from the callback; untested |
| Command to unknown IOA | one `47` neg ACT_CON | `47` then `2f` | no hook seen |
| Types 101, 102, 103 | `6c` (COT 44 unknown type), no more | 101: ACT_CON+ACT_TERM; 102: ACT_CON + data COT 5; 103: ACT_CON | clock-sync has `on_clock_sync`; 101/102 built in |
| Double mode periodic | binaries `M_DP_TB_1` (31) COT 3, per point; analog COT 1 | `M_DP_NA_1` (3) COT 1 batched | yes, per-point `M_DP_TB_1` points + transmit(SPONTANEOUS) |
| GI/periodic grouping | one ASDU per kind, vsq 4, no SQ | same for non-contiguous IOAs; c104 sets SQ (vsq 0x82) on contiguous IOAs (e.g. 102,103 when outputs are absent) | partly; wire-only |
| Periodic sender | last-opened connection only | all connections (`transmit_batch`) | per-connection transmit untested |
| APCI timers, k/w, queues | lib60870 defaults, queues 1000 | `protocol_parameters` exposes k/w/t0-t3; queue size not | timers yes, queues no |

## Verdict

**NO-GO.** The plan should come back to the user. Options for them: (1) accept outputs readable at a shifted IOA (breaks "IOA from
`address`"); (2) accept GI without outputs, commands working (breaks "all points incl. outputs"); (3) write the 104 codec in the
module (about the size of a hand-rolled Modbus server, no GPL dependency, full control of the quirks); (4) the Rust `iec104` crate the
spec's table lists as a candidate for in-core use.
