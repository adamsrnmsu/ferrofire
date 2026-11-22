# Stock IEC 104: what bennu-field-device does (ww-vck, Task 0)

The facts later tasks code to. One line per fact, each with its evidence: a line of a golden, or a `file:line` in sceptre-bennu.

- **Stock** is the oracle image `ffc-stock-2026.11.03:22.04`, built from sceptre-bennu tag `v2026.11.03` (01d35c4) by
  `bench/stock_image.sh`. Source lines are those of `git -C ~/git/sceptre-bennu show v2026.11.03:<path>`. The working tree
  there is an older commit (c9c2cf6, about 20 commits behind): its `Server.cpp` has no select-before-operate and different
  command replies, so do not read it for this.
  - `MOD/` is `src/bennu/devices/modules/comms/iec60870-5/module/`;
  - `LIB/` is `src/bennu/devices/modules/comms/iec60870-5/protocol/src/iec60870/` (lib60870-C, identical in tree and tag);
  - `CI/` is `src/bennu/devices/modules/comms/base/CommandInterface.cpp`.
- **Goldens**, each from two runs that gave the same bytes (Task 0 report):
  - `G` = `conformance/stock/iec104-server.golden`: the sample server (single-point mode), every probe section;
  - `GD` = `conformance/stock/iec104-server-double.golden`: the same in double-point mode, DUT `TZ=XST5`;
  - `GB` = `conformance/stock/iec104-server-big.golden`: 130 more binary and 70 more analog inputs;
  - `GC` = `conformance/stock/client-iec104.golden`: the stock client against the stock server (double-point mode).
- Masking in the goldens: N(S) and N(R) are never shown, CP56Time2a in the server's frames is `__`, the probe's own S-frames are not
  printed, periodic frames are a distinct set per window, times are whole seconds. Nothing else in stock was nondeterministic in two
  runs (one TCP `D-SACK` expert note on lo, seen in one dev run, is allow-listed in `proto.env`).
- How to run: `conformance/iec104/proto.env`, `conformance/iec104-client/proto.env`.

## Contradictions with the design and spec 4.3 (read first)

- **All three commands get ACT_CON then ACT_TERM.** 45, 46 and 50 at a configured output: the ASDU echoed with COT 7, then again
  with COT 10. Spec 4.3's "45 and 50 only an echo (COT 7)" describes the pre-tag source. `G:39`
  `G:45` `G:51`; `MOD/Server.cpp:518-523`, `:608-613`,
  `:691-696`.
- **The select bit is refused, not ignored (quirk Q7 no longer holds).** With S/E = 1, each of 45, 46 and 50 gets a negative ACT_CON
  (COT `47` = 7 with P/N) and no write. `G:78` and the two steps after it;
  `MOD/Server.cpp:488-494`, `:574-579`, `:661-666`.
- **A command to an input, to the wrong kind of output, or with a bad double state is refused.** Negative ACT_CON (`47`), no write:
  45 at a binary input, 45 at an analog output, 50 at a binary output, 50 at an analog input, 46 state 0 or 3.
  `G:66` through `G:76`;
  `MOD/Server.cpp:832-845` (isValidOutputPoint), `:571-572`, `:592-600`.
- **Select-before-operate exists at the tag.** Config keys `sbo-enabled` (default false) and `sbo-timeout-secs` (default 30) on the
  server, `sbo-enabled` on each client connection: `MOD/DataHandler.cpp:59-60`, `:117`, `MOD/Server.hpp:43`. The start log line
  reads `Initialized IEC60870-5-104 server: <endpoint> (select-before-operate disabled)` (`MOD/Server.cpp:103-104`). SBO-on behaviour
  is not recorded here: the design has no SBO.
- The client's binary write is `C_DC_NA_1` with S/E 0 (direct execute), not select: `GC:23`;
  `MOD/ClientConnection.cpp:132-141`.

## Server: commands 45, 46, 50

- Direct execute at a configured output: ACT_CON (COT 7) then ACT_TERM (COT 10), each the received ASDU with only the COT byte
  changed (type, VSQ, originator, CA, IOA, value and qualifier as sent). `G:37` and the five steps after it.
- The originator address is echoed: the client's commands (OA 3) come back with OA 3. `GC:32`
- The common address is not checked: a 45 to CA 2 is executed and its ACT_CON/ACT_TERM carry CA 2. `G:92`
  and the last provider line, `G:186` of `G:180-186` (the seventh write, `generator-1_bus-1.active:false`).
- What reaches the provider (`<output>` names, in order): 45 ON → `true`, 45 OFF → `false`, 46 OFF(1) → `false`, 46 ON(2) → `true`,
  50 → the float as `%f` (`77.500000`, `-2.500000`). `G:180-185`; `MOD/Server.cpp:752-830`.
- A command at an IOA with no point: negative ACT_CON (`47`), nothing else. `G:65`
- The IOA of 45, 46 and 50 is cut to its low 16 bits (`uint16_t addr`), so IOA 65536 executes at address 0. Source only:
  `MOD/Server.cpp:483`, `:566`, `:656`.
- COT 8 (deactivation) with no selection held: negative DEACTIVATION_CON (COT 9, byte `49`), for 45, 46 and 50.
  `G:84`; `MOD/Server.cpp:452-466`.
- Any other COT (3 here): negative COT 45 "unknown COT" (byte `6d`), one frame. `G:90`;
  `MOD/Server.cpp:467-473`.
- After a command the interrogation and the periodic report show the output's datastore value. With a provider that publishes the
  same point names (as `bacnet/feed.py` does here), that is the published value, not the command's: the GI after the six commands
  equals the one before. `G:55`

## Server: interrogation and other type ids

- GI QOI 20: ACT_CON, the binaries ASDU (`M_SP_NA_1`, or `M_DP_NA_1` in double mode), the analogs ASDU (`M_ME_NC_1`, QDS 0), all
  COT 20, all points including outputs ordered by address, then ACT_TERM. `G:13`; `MOD/Server.cpp:298-364`.
- Any other QOI (21 here): one negative ACT_CON (`47`), no data. `G:22`; `MOD/Server.cpp:358-361`.
- GI with COT 8 and QOI 20 is answered as a full interrogation (ACT_CON COT 7 ... ACT_TERM): the handler ignores the COT.
  `G:23`; `LIB/cs104/cs104_slave.c:1815`, `MOD/Server.cpp:302`.
- GI with any other COT (3): two frames, negative COT 45 (`6d`) then negative COT 44 (`6c`). `G:28`;
  `LIB/cs104/cs104_slave.c:1833` (responseCOTUnknown) then `:2031-2036` (not handled: unknown type).
- GI to CA 2: ACT_CON and ACT_TERM carry CA 2, the data ASDUs CA 1. `G:31`
- 101 counter interrogation, 102 read (COT 5), 103 clock sync, 105 reset process, 58 `C_SC_TA_1`, and a monitor type sent by the
  master (1): the ASDU echoed with negative COT 44 "unknown type id" (byte `6c`). `G:96` and the steps
  after it; no handler is set (`MOD/Server.cpp:54-68`), so `LIB/cs104/cs104_slave.c:2023-2036` answers.
- 104 `C_TS_NA_1` and 107 `C_TS_TA_1` with COT 6: echoed with COT 7 (the library answers). `G:103`
  `G:107`; `LIB/cs104/cs104_slave.c:1934-1949`, `:2005-2021`.
- 107 with any other COT: negative COT 45 (`6d`). `G:109`
- 101, 102, 103, 105, 106 with a COT the library does not accept: negative COT 45 (`6d`), then negative COT 44 (`6c`), two
  frames. The library accepts 101 at COT 6 or 8, 102 at 5, 103 and 105 at 6, 106 at 6 or 3. The accepted COTs get only the 44,
  because no handler is set. Source only, not probed: `LIB/cs104/cs104_slave.c:1841`, `:1859`, `:1867`, `:1884`, `:1892`, `:1930`,
  `:1955`, `:1974`, `:1982`, `:2001` (responseCOTUnknown), then `:2031-2036` (unknown type).
- 104 with any other COT: negative COT 45, like 107 (the same code). Source only: `LIB/cs104/cs104_slave.c:1938-1940`.
- An ASDU under 6 bytes (the header) closes the connection, with no answer. Source only: `LIB/cs101/cs101_asdu.c:157-160`
  (createFromBuffer gives NULL), `LIB/cs104/cs104_slave.c:2270-2272` (handleMessage false), `:2683-2684` (isRunning false).
- A GI (COT 6 or 8) with a payload under one object (4 bytes) closes the connection, with no answer. At
  another COT it gets negative 45, then negative 44, as a full GI does. Stock reads object 0 from the payload whatever the VSQ
  count says (`LIB/cs101/cs101_asdu.c:420`). Source only:
  `LIB/cs104/cs104_slave.c:1827-1828` (handleASDU false), `:2264-2267`, `:2683-2684`.

## Server: APCI (STARTDT, STOPDT, TESTFR, I-frames before STARTDT, k, w, t1, t2, t3)

- STARTDT act `68 04 07 00 00 00` → con `68 04 0b 00 00 00`. `G:9`
- TESTFR act → con `68 04 83 00 00 00`, also before STARTDT and while stopped. `G:2` `G:119`;
  `LIB/cs104/cs104_slave.c:2282-2286`.
- An I-frame before STARTDT: the server closes the TCP connection, no answer. `G:4`;
  `LIB/cs104/cs104_slave.c:2256`, `:2275-2278`.
- An S-frame before STARTDT: the server closes. `G:6`; `LIB/cs104/cs104_slave.c:2329-2334`.
- STOPDT act: an S-frame, then STOPDT con `68 04 23 00 00 00`; no periodic frames while stopped; STARTDT again brings them back; an
  I-frame while stopped closes the connection. `G:114` onward; `LIB/cs104/cs104_slave.c:2301-2318`.
- Every I-frame the server sends acks the master's I-frames through its N(R) (`LIB/cs104/cs104_slave.c:1655-1674`); a separate
  S-frame comes only when it has nothing to send or its window is full.
- k = 12: with no ack from the master the server sends 12 I-frames and then nothing. `G:166`;
  `LIB/cs104/cs104_slave.c:66`. ASDUs it cannot send wait in a queue (1000 deep, `MOD/Server.cpp:45`) and go when acked.
- w = 8: with its window full, the server answers the master's 8th unacked I-frame with an S-frame, the 7th with nothing.
  `G:168`; `LIB/cs104/cs104_slave.c:67`, `:2686-2692`.
- t1 = 15 s: the server closes 15 s after the oldest I-frame it sent that is not acked. `G:170`;
  `LIB/cs104/cs104_slave.c:69`, `:2566`.
- t2 = 10 s: an I-frame from the master that the server cannot ack by an I-frame gets an S-frame 10 s later.
  `G:173`; `LIB/cs104/cs104_slave.c:70`, `:2531-2546`.
- t3 = 20 s: after 20 s with nothing received, TESTFR act `68 04 43 00 00 00`; unanswered, the connection is closed t1 (15 s)
  later, at 35 s. `G:177` `G:178`; `LIB/cs104/cs104_slave.c:71`, `:2503-2526`.
- t0 = 10 s (connect timeout): standard value, not observable on a server (`LIB/cs104/cs104_slave.c:68`).
- At most 5 client connections (`CONFIG_CS104_MAX_CLIENT_CONNECTIONS`, `.../protocol/config/lib60870_config.h:66`): source only,
  not probed.

## Server: two connections and the periodic report

- Mode `CS104_MODE_SINGLE_REDUNDANCY_GROUP` (`MOD/Server.cpp:52`): a STARTDT on one connection deactivates every other
  (`LIB/cs104/cs104_slave.c:1410-1435`). `G:137`
- A deactivated connection that sends an I-frame is closed. `G:138`
- The periodic report goes to the connection opened last, and only while it is started: a second connection that is open but not
  started silences the report for both. `G:134`; `MOD/Server.cpp:713-725` (open sets `mConnection`).
- If the last-opened connection is deactivated (another one's STARTDT), nobody gets the report.
  `G:152`
- Any connection's close stops the report for all until a connection opens: after D closes, E (open, started, answering a GI) gets
  none. `G:153` to `G:163`; `MOD/Server.cpp:726-733` (clears `mConnected` whichever closes).
- A newly opened connection, once started, gets the report again. `G:143`
- The GI is answered on the connection that asked, report or not. `G:157`
- Single mode report, every `rpoll-rate` s: one `M_SP_NA_1` and one `M_ME_NC_1` ASDU, COT 1, same layout as the GI's.
  `G:18`; `MOD/Server.cpp:155-224`.

## Server: double-point mode and its time base

- GI: binaries as `M_DP_NA_1` (type 3), false → 1 (OFF), true → 2 (ON); analogs as in single mode. `GD:13`;
  `MOD/Server.cpp:110-116`, `:369-435`.
- Report: analogs one `M_ME_NC_1` ASDU with COT 1; each binary point (outputs too) its own `M_DP_TB_1` (type 31) ASDU, VSQ 1,
  COT 3 (spontaneous), with a CP56Time2a. `GD:19`; `MOD/Server.cpp:226-274`, `:138-149`.
- The CP56Time2a is UTC (the DUT ran with `TZ=XST5`, five hours off, and the time decoded within 5 s of UTC), day-of-week 0,
  IV 0, SU 0. `GD:18`; `LIB/apl/cpXXtime2a.c:495` (gmtime_r), `:508-509`.
- Commands in double mode answer as in single mode. `GD:40`

## Server: maximum ASDU payload (measured)

- `MAX_ASDU_PAYLOAD_SIZE` is 240 (`MOD/Server.hpp:33`), checked before each object is added (`MOD/Server.cpp:313`, `:338`).
  `M_SP_NA_1` (4 bytes an object) splits at 60 objects (VSQ `3c`), `M_ME_NC_1` (8 bytes) at 30 (VSQ `1e`); the rest go in the last
  ASDU. The same in the report. Points stay in address order across the split (100, 101 before 1000). `GB:15`
  `GB:18` `GB:22`
- A kind with no points still gets its (empty) ASDU: source only (`MOD/Server.cpp:329`, `:354`), no probe config has none.

## Client (`<iec60870-5-104-client>`)

- It connects once, when the device starts, and never again: with no server at start it logs `Error -- Could not start
  IEC60870-5-104-CLIENT -- RTU Connection: tcp://...` and `Exiting.` but keeps running, never connected; after the server goes away
  and comes back there is no SYN. Reconnect delay and attempts: none. `GC:72` and the end of `GC` (no
  SYN after it); `MOD/ClientConnection.cpp:44-56`. Also seen in a dev run with no server at start: one SYN, refused, none in 60 s.
- At connect: STARTDT act and the GI (`C_IC_NA_1`, COT 6, originator 3, CA 1, QOI 20) back to back, before STARTDT con.
  `GC:5` `GC:6`; `MOD/ClientConnection.cpp:36`, `:46-49`.
- It sends no other GI, ever (`MOD/ClientConnection.cpp:48`, "server has reverse polling loop").
- Data in: `M_ME_NC_1` with any COT updates analogs (GI and report); `M_DP_NA_1` updates binaries (OFF → false, ON → true, else false
  and a log line). The report's spontaneous `M_DP_TB_1` is ignored, so binaries change only at the GI or by the client's own writes:
  `GC:57` (written true, the server's report says OFF); `MOD/ClientConnection.cpp:377-433`. `M_SP_NA_1` is ignored too (Q12), source
  only: no run had a single-point server.
- Writes: `true`/`false` → `C_DC_NA_1` ON(2)/OFF(1), anything else → `C_SE_NC_1` float, both S/E 0, qualifier 0, COT 6, CA 1, to the
  tag's IOA whatever its kind: `WRITE=O_bus-1.gen_mw:true` sends `C_DC_NA_1` to IOA 2, `WRITE=O_generator-1_bus-1.active:7` sends
  `C_SE_NC_1` to IOA 0, writes to input tags are sent too. `GC:42`;
  `CI/:111-120`, `MOD/ClientConnection.cpp:132-141`, `:186-195`.
- The reply is `ACK=Wrote tag <tag> -- <value as typed>` as soon as the command is sent, whatever the server answers (a negative
  ACT_CON is only logged), and the local value is set at once; analogs are then overwritten by the next report.
  `GC:40` after writing 42.5; `MOD/ClientConnection.cpp:139-141`, `:362-369`.
- Reads: `ACK=<tag>:<%f>` for analogs, `ACK=<tag>:true|false` for binaries; QUERY lists the tags sorted. `GC:12`
- With the server away (killed: its side sends FIN then RST, the client sends nothing): reads answer the last values, writes answer
  `ACK=Wrote tag ...` and set the local value, nothing goes on the wire; after the server is back there is still no SYN, and the
  values stay. No `ERR` comes from the server being away. `GC:63` to the end of `GC`; `MOD/ClientConnection.cpp:132-141`.
- When the server goes, the client acknowledges the I-frames it has not yet acknowledged (an S-frame) and then closes: against the
  killed server that S-frame draws its RST, and the client's close sends no FIN (`GC:64-65`, the S-frame left out);
  `LIB/cs104/cs104_connection.c:897-900`, then `Socket_destroy`.
- cmdif `ERR` texts: an unmapped tag in READ or WRITE: `ERR=Client does not have a mapping for tag 'nope'` `GC:60`
  `GC:55`; an op other than QUERY/READ/WRITE in either case (`Query=`, `FOO=x`): `ERR=Unknown command type (must be
  QUERY|READ|WRITE)` `GC:61`; `CI/:56`, `:107`, `:134`. Every reply ends in a NUL.
- A request with no `=` (`nope`) crashes the stock client (`Segmentation fault` in its output, every reply after it times out):
  `CI/:27-29` indexes `split[1]` of a one-element split. Seen in a dev run; left out of `GC`. A stock crash we do not replicate.
- APCI parameters: lib60870 defaults k 12, w 8, t0 10 s, t1 15 s, t2 10 s, t3 20 s (`LIB/cs104/cs104_connection.c:42-47`), the
  standard values. Not observed for the client: its S-frames are left out of `GC`.
