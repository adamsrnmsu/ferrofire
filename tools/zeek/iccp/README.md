# ferrofire-iccp: ICCP / TASE.2 analyzer for Zeek (Spicy)

Parses ICCP (TASE.2, IEC 60870-6) on tcp/102: TPKT, COTP, OSI session, presentation, ACSE and MMS (ISO 9506), then
reads the TASE.2 objects out of the MMS PDUs. No public ICCP parser for Zeek or Wireshark existed when this was written
(the Wireshark MMS dissector shows ICCP as plain MMS, the Zeek MMS analyzers know nothing of TASE.2). This package is one
self-contained Spicy analyzer: no ISO-stack plugins to install first. MIT, like the rest of ferrofire.

Logs
- `mms.log`: one row per MMS PDU. `pdu` (initiate_req, confirmed_req, confirmed_resp, confirmed_err, unconfirmed,
  conclude_req ...), `service`, `invoke_id`, `scope` (vmd|domain|aa), `domain`, `target` (variable list name),
  `items`, `values` (type=value[quality]), `result`, `error`, `rtt` (response rows), `detail` (initiate parameters).
  Responses carry the request's names, so a read response shows what was read.
- `iccp.log`: one row per TASE.2 object operation, written once the connection has mentioned a TASE.2-only object
  (Bilateral_Table_ID, TASE2_Version, Supported_Features, Next_DSTransfer_Set, Transfer_Set_Name ...) or carried a
  transfer report. Plain IEC 61850 MMS therefore only fills `mms.log`. Columns: `op` (read, write, report,
  define_dataset, start_transfer_set), `scope`, `domain`, `name`, `class` (bilateral_table_id, tase2_version,
  supported_features, transfer_set, dataset, infomsg, data ...), `vtype` (real, realq, discrete, discreteq, state, stateq,
  bitstring, struct ...), `value`, `quality`, `result`, `transfer_set`, `ds_conditions`, `timetag`, and the connection's
  `bilateral_table_id`, `tase2_version`, `supported_features` once read, and `supported_blocks` (the set bits of the 12-bit
  Supported_Features as block names in bit order, bit n = block n+1: BasicServices, ExtendedConditions, BlockedTransfers,
  InformationMessages, DeviceControl, Programs, Events, AdditionalEvents, TimeSeries; bits past nine show as `Bit10` ...; the
  block names are recalled, see Assumptions). Wireshark: `iccp.supported_blocks`.
- `weird.log`: a message the analyzer drops is not silent: `iccp_parse_error` (BER/session failure inside one reassembled COTP
  message, the connection goes on), `iccp_tpkt_error` (TPKT/COTP desync, that direction ends), `iccp_unparsed_spdu` (a session
  data/connect SPDU with no MMS PDU or ACSE release/abort in it). `tests/suite/weirds.zeek` counts them over the fuzz and malformed
  corpora and checks that valid captures raise none.
- Coexistence on tcp/102 (ww-otj.52): the analyzer confirms a flow only when its first data TPDU is a session SPDU (CONNECT, ACCEPT,
  DATA); S7comm (starts 0x32) is rejected, so `conn.log` says `iccp` for ICCP and, with CISA ICSNPP s7comm loaded, `s7comm` for
  S7. ICSNPP's COTP logger also tags ICCP flows `cotp`. `make zeek-iccp-coexist` checks it in the testpeers image.
- `conn.log` gets service `iccp`.

Install
- Zeek with Spicy: `zkg install <path-or-git-url>` (builds with `spicyz`, runs `btest`), or by hand:
  `spicyz -o iccp.hlto analyzer/iccp.spicy analyzer/iccp.evt && zeek -r x.pcap iccp.hlto scripts`.
- Malcolm: see `docs/iccp-parsers.md`.
- It claims tcp/102 only. S7comm (also tcp/102; ICSNPP s7comm in Malcolm) attaches to the same port: untested together.

Tests: `make zeek-iccp` (repo root; needs docker, builds `docker/Dockerfile.zeek`: Zeek + Spicy + g++) runs the btest
suite in `tests/suite` against `tests/pcaps/iccp`, comparing `mms.log` and `iccp.log` to `tests/Baseline`.
`ZEEK_ICCP_FLAGS=--update make zeek-iccp` rewrites the baselines (read the diff). The corpus is libiec61850 MMS captures,
hand-built TASE.2 exchanges, malformed input and 150 seeded fuzz mutants; see `tests/pcaps/iccp/make_pcaps.py`.

Robustness: a COTP message that fails to parse (bad BER, truncated PDU, nesting deeper than 40, length over the data) is
dropped on its own, in a throw-away sink, and the next message on the connection still parses. TPKT or COTP framing
errors (bad version byte, impossible length) end that direction of the connection, because there is nothing to resync
on. Parse failures inside a message are silent: there is no event for them yet.

## Assumptions

The TASE.2 object model text (IEC 60870-6-503/802) is not freely available. Names and encodings were written from memory
and from vendor documentation, and are NOT checked against a licensed copy. Stage 2 (bead ww-otj.49) re-checks them
against ferrofire's own ICCP server (ww-otj.46). Where it matters:
- VCC-scope (`vmd`) variables: Bilateral_Table_ID (visible string), TASE2_Version (structure of two integers),
  Supported_Features (bit string). Domain-scope names are `domain/item`, the domain being the ICC name.
- Next_DSTransfer_Set returns a structure whose last member is the transfer set name. A write to that name is logged as
  `start_transfer_set`.
- A transfer report is an InformationReport. Against a named variable list the first three values are positional:
  Transfer_Set_Name (structure), Transfer_Set_Time_Stamp, DSConditions_Detected (bit string: Interval, Integrity,
  ObjectChange, OperatorRequest, OtherExternalEvent, bit 0 first). With an explicit variable list the names are on the wire.
- Values are typed by shape: float -> real, integer -> discrete, 8-bit string -> StateQ, structure of (real|integer,
  8-bit flags) -> RealQ/DiscreteQ, structure of (time, real|integer, flags) -> ...QTimeTag. The flags byte, MSB first:
  bits 0-1 state (StateQ) or spare, 2-3 validity (VALID, HELD, SUSPECT, NOTVALID), 4-5 current source (TELEMETERED,
  CALCULATED, ENTERED, ESTIMATED), 6 normal value, 7 time stamp quality. A Supported_Features bit string is shaped like a
  StateQ and is relabelled by name.
- `IM_*` / `Info*` names are classed `infomsg` (Information_Message objects have user-chosen names).
- Not decoded: alternate access, typeDescription of getVariableAccessAttributes, BER indefinite length, high tag numbers,
  more than the first presentation data value of a PPDU.
The name and class tables are `special_names`, `ds_condition_bits` and `report_header` at the top of `scripts/main.zeek`
(`&redef`able), and the shape rules are in `analyzer/iccp.spicy`. `tools/wireshark/iccp.lua` mirrors both; keep them in sync.
