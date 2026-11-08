# Rust field device (drop-in for `bennu-field-deviced`) — design spec

Bead ww-otj.1 (migration roadmap step 2, epic ww-otj). Status: SPEC, nothing built. Answers the roadmap line in
`provider-spec.md` §10: "Rust drop-in: same config XML, same ZeroMQ subscription to the provider, Modbus/DNP3/IEC 104
servers on tokio", for a field device that is memory-safe, has no vendored x86 libraries and builds on arm64 without pain.

Citation keys: `B:` = `~/git/sceptre-bennu` (Ryan's working tree at `c9c2cf6` **with uncommitted edits**, same caveat as
`provider-spec.md`); `FD:` = `B:src/bennu/devices/field-device/`; `MOD:` = `B:src/bennu/devices/modules/`;
`DMN:` = `B:src/bennu/executables/daemon/`; `A:` = `~/git/sceptre-phenix-apps/src/python/phenix_apps/apps/sceptre`;
`T:` = `~/git/sceptre-phenix-topologies`; `R:` = `~/git/phenix-waterway-range`; `P:` = `R:docs/provider-spec.md`;
`O:` = **observed** first-hand on 2026-11-08 by running the stock binaries of image `bennu:arm64` (built 2026-11-07,
Lima VM `phenix`, containers `ff-fd-*`, all removed afterwards); every `O:` item is reproducible with
`conformance/stock/run-stock.sh` (§8). `W:` = web source, cited by URL; crates.io data fetched 2026-11-08.
Anything not checked first-hand is marked **UNVERIFIED**.

**Clean-room note.** bennu/pybennu are GPL-3.0-or-later and `lib60870-C` (vendored under bennu) is GPL-3.0. This document
records behaviour, wire bytes, config keys and CLI flags (interface facts) with citations so the implementer can work
from the spec and the captured transcripts (`conformance/stock/`) and **does not need to open bennu source**. Nothing here is
a translation. `lib60870-C` internals were not read for this spec (its timers/defaults are marked UNVERIFIED where relevant).

---

## 1. Goals / non-goals

Goals
- One Rust process, `bennu-field-deviced` compatible on the command line, that reads the **unchanged** sceptre-generated
  `config.xml` (`A:templates/fd_server.mako`), subscribes to the provider exactly like the stock device (RADIO/DISH UDP
  multicast in, ZeroMQ REQ out), runs the scan cycle with the `<logic>` block, and serves Modbus/TCP, DNP3 and IEC 60870-5-104
  so stock SCADA, OPC gateway, FUXA, historian, `bennu-probe`, Zeek/Malcolm and attacker tooling see the same device.
- Same **observable fingerprint** where it matters to a training range: register scaling, exception codes, IIN bits,
  ASDU types/COTs, reply cadence, the firmware-hash/brash toggle (§6). Divergences are listed (§3.6) and each is a flag or a decision.
- Hostile-input safe: every parser bounds-checked, no panics on network input, fuzz-tested (§8.4). The stock Modbus listener
  dies to one 3 KB frame (§4.1, `O:`), which is the main safety motive for the rewrite.
- Ship in the same repo and binary family as the provider (§2), so one `ferrofire` image overlay replaces both.
- First user-visible milestone is **the waterway range**: its two RTUs are Modbus-only fd-servers with `logic:` (§7), so
  phases F0-F2 (≈ 5 days) already put Rust RTUs on the range.

Non-goals (v1)
- BACnet, SunSpec (that is gobennu, Go), IEC 61850/GOOSE (the module exists in the tree but is **not** registered in
  `MOD:comms/base/CommsModuleCreator.cpp:23-30`, so stock never loads it), OpenPLC, Alicanto.
- Replacing `bennu-brash` (roadmap step 3) or the Python/C++ providers. Brash stays as is; we only keep its hooks working (§6).
- Protocol security features (DNP3 SAv5, IEC 62351, TLS): not in stock, not added.
- Bug-for-bug replication of memory-unsafe behaviour (§3.6).

---

## 2. Inventory of the stock field device and architecture of the replacement

### 2.1 What runs on an RTU VM today

| Process | Role | Evidence |
|---|---|---|
| `bennu-field-deviced` | daemon wrapper: root check, pidfile, `daemon(1,0)`, log redirection, brash toggle, then loads the XML and runs forever | `DMN:bennu-field-deviced/main.cpp:152,95,242`; started by `A:templates/sceptre_start.mako:48` |
| `bennu-field-device` | same loader, foreground, `--file`, `--log-level` (the image build lacks `--log-level`, `O:`) | `B:src/bennu/executables/bennu-field-device/main.cpp`; used by `R:images/bennu-smoke.sh` |
| `bennu-watcherd` | firmware watcher: SHA-256 of `/home/sceptre/bennu-field-deviced.firmware` against three allowed hashes, copies it over `/usr/bin/bennu-field-deviced`, restarts the device | `DMN:bennu-watcherd/main.cpp:326-404`; `A:templates/sceptre_start.mako:37-45` |
| `bennu-brash` | login shell of user `sceptre` (setuid root), calls `bennu-field-deviced --c start|stop|restart`; **out of scope** | `B:src/bennu/executables/bennu-brash/main.cpp`, `R:images/scripts/bennu-arm64.sh` |
| `socat` on :1337 | the "shell" the daemon starts when the firmware marker is flipped (§6) | `MOD:shell/BrashServer.cpp:62` |

The stock loader is a plugin architecture: the XML root's child tags select shared libraries to `dlopen`
(`B:src/bennu/parsers/Parser.cpp:84-96`, tags registered at `MOD:comms/base/CommsModuleCreator.cpp:23-30`). The Rust version
has no plugins: the same tag names select compiled-in modules.

### 2.2 Replacement layout

```
 config.xml ─► roxmltree ─► Config (validated) ──┐
                                                  ▼
 UDP multicast  ──► DISH-compatible receiver ─► Datastore ◄── scan task (logic, outputs)  ◄── cycle-time sleep
 (RADIO framing)    (InputModule)               points + tags      │            │
                                                 ▲   ▲             │            └─► REQ client ─► provider :5555
                              protocol servers ──┘   └─ updated-tag queues (writes from Modbus/DNP3/104)   (OutputModule)
                              (tokio tasks: Modbus, DNP3, IEC 104, event logs)
```

- **One binary, multi-call**, like `P:` §2: `ferrofire field-device ...` plus `argv[0]` personalities
  `bennu-field-deviced`, `bennu-field-device`, `bennu-watcherd`. Cargo features `provider` and `fielddev` (both default) so a
  field-device-only image gets a smaller file. The hash trick in §6 requires the marker string to occur **once in the whole file**.
- **Static musl is allowed for this binary** (Decision 5): the glibc-only rule in `P:` §2.2 exists because the Simulink adapter
  shares SysV shm/POSIX semaphores with the stock provider; the field device has no such contract. UNVERIFIED: that
  `aarch64-unknown-linux-musl` + a hand-written `fork/setsid` daemonizer behaves identically (the stock uses libc `daemon(1,0)`).
- **Threads**: one scan task (stock: one `std::thread`, `FD:FieldDevice.cpp:132-157`), a tokio multi-thread runtime for the
  UDP receiver, REQ client and protocol servers. Shared state is one `Datastore` behind a `Mutex` (stock: per-store shared
  mutexes, `FD:DataStore.hpp`). The runtime starts **after** the daemonizing fork.
- The engine crate (`ferrofire` lib, `src/expr.rs`, `src/model.rs`) is **not** reused for logic: the `<logic>` dialect differs (§3.4).

### 2.3 CLI contract (what scripts and brash call)

From `A:templates/sceptre_start.mako:45,48` and brash (`B:src/bennu/executables/bennu-brash/main.cpp:153-156`):

```
bennu-field-deviced --c start|stop|restart [--env default] [--file /etc/sceptre/config.xml]
bennu-field-deviced start --env x --file f        # positional command also accepted
bennu-field-device  --file f [--log-level error|warn|info|debug|0-3]
bennu-watcherd restart --f /etc/sceptre/config.xml --b /home/sceptre/bennu-field-deviced.firmware --w /etc/sceptre/watcher.ini
```

- The stock parser is Boost.Program_options, which accepts **unambiguous prefixes** (`--c`, `--f`, `--b`, `--w` are used in
  the shipped scripts). The Rust CLI must accept the same prefixes (hand-rolled argv loop or clap `infer_long_args`).
- `bennu-field-deviced` (all `FD:`-adjacent facts from `DMN:bennu-field-deviced/main.cpp`):
  - must run as root (`:152`), else prints "Must be root..." and exits 1;
  - `--env` default `default`; `instance = env + "-bennu-field-deviced"`; pidfile `/var/run/<instance>.pid` (`:38,60,106`);
  - `start`: refuses when the pidfile holds a **nonzero pid, with no liveness check** (so a crash leaves it "running" until
    `stop`/`restart`) — replicated, because the watcher uses the same test; `daemon(1,0)` (cwd kept, stdio closed), writes
    its pid, `umask(0)`; then stdout/stderr are re-opened `"w"` (truncating) at `/etc/sceptre/log/<instance>-stdout.log` and
    `-stderr.log` (`:242-246`);
  - `stop`: SIGTERM, sleep 1 s, SIGKILL, write `0` to the pidfile (`:60-86`); `restart` = stop (if "running") + start;
  - the brash toggle runs **before** the command is interpreted (§6); syslog mask `LOG_UPTO(LOG_ERR)`;
  - after `Parser::load` the process idles in a 10 ms loop until SIGINT/SIGTERM.
- Config file lookup (`B:src/bennu/parsers/Parser.cpp:21-25`): the name is tried against `.`, `..`, `/etc/sceptre/`, `/usr/share`,
  `/usr/local/share` (`O:` an absolute `/tmp/m.xml` was logged as resolved to `..//tmp/m.xml`). Replicate the search list; resolve absolute paths first.
- Failure to load logs `ERROR: bennu-field-device failed when loading the field device configuration file!` and exits 1.

---

## 3. Core semantics (all protocol modules sit on this)

### 3.1 Config XML (root `<SCEPTRE>`, one `<field-device>`)

| Element | Meaning | Default / rule | Cite |
|---|---|---|---|
| `name` | device name | required | `FD:FieldDeviceCreator.cpp:15` |
| `cycle-time` | scan period, ms | 1000 | `FD:FieldDevice.cpp:26` |
| `tags/external-tag{name,io,type}` | alias `name` for the point whose `<id>` is `io`; `type` = `binary`\|`analog` decides which tag list it joins | alias to an unknown `io` id prints `ERROR: Cannot map an alias...` and the tag is dropped | `FD:FieldDevice.cpp:57-78` |
| `tags/internal-tag{name,(status\|value)}` | device-local tag; `status` = bool, `value` = double | other child = tag ignored | `FD:FieldDevice.cpp:80-93` |
| `input{endpoint, binary{id,name}*, analog{id,name}*}` (0..n) | one DISH subscription per element; each `<id>` names a **point** `name` | `endpoint` e.g. `udp://239.0.0.1:40000` | `MOD:io/IOModule.cpp:15-45`, `A:fd_server.mako` |
| `output{endpoint, binary*, analog*}` (0..n) | one REQ client per element to the provider REP; same id/name scheme | `endpoint` e.g. `tcp://10.0.0.5:5555` | same |
| `logic` | text block of `lhs = expr[,delay:N]` lines | optional | `MOD:logic/LogicModule.cpp` |
| `comms/<server>` | at most **one** server per protocol (the handlers `return` after the first element) | see §4 | `MOD:comms/modbus/module/DataHandler.cpp:29` |

Order of processing (matters for `hasTag` checks): logic text, inputs, outputs, tags, comms (`FD:FieldDevice.cpp:26-110`).
A protocol mapping whose tag is unknown is **silently skipped** (`addCoil` etc. return false and the caller ignores it,
`MOD:comms/modbus/module/DataHandler.cpp:58-62`). Rust: same skip, but one `WARN` line naming the tag, and `--strict` makes it fatal.

Points vs tags: a **point** is a named slot in the external store (`load-1_bus-1.mw`), created by `<binary>`/`<analog>` under
`<input>` or `<output>`; a **tag** (`var_I1`) is an alias onto a point (or an internal slot). Points are keyed by **name only**,
so an input id and an output id that share the same point name share one value (`O:` holding register `40000` ← tag `var_O3` ← point
`load-1_bus-1.mw` returned the value published for input `I1`, same point). Replicate.

Config generation (`A:templates/fd_server.mako`): tag names are `var_<host>_I<addr>` / `var_<host>_O<addr>`, point ids
`<host>_I<addr>` / `<host>_O<addr>`, point names `<device>.<field>` (`waterway`: `lowergate.open`). Input registers
(`analog-input`, `binary-input`, `input-register`, `discrete-input`) become `<input>` points; everything else `<output>`
(`fd_server.mako:23-28,78-84,111-170`). The `<logic>` block is built by textual substitution of `<device>.<field>` names to `var_*` tags
(`fd_server.mako:179-190`). Endpoints: `modbus` → `tcp://<ip>:502`, `dnp3` → `tcp://<ip>:20000` + link address 10, `iec60870-5-104` →
`tcp://<ip>:2404` + `rpoll-rate` 2 + `subtype` (`fd_server.mako:44-71`). The generator **never** emits DNP3 `sgvar/egvar/class/deadband`
or `sbo`, so v1 supports exactly the defaults in §4.2 and rejects the rest with a clear error.

### 3.2 Datastore

- External store: `point → variant<int,double,bool>` + a per-point timestamp (seconds since epoch, set only by
  provider updates; `0` until the first update). Internal store: `tag → variant`. Typed reads: `getDataByTag<T>` returns the
  external point value if the tag is an alias, else the internal value (`FD:DataManager.hpp:62-84`).
- **Type mismatch**: stock `std::get<T>` on the wrong alternative throws; a published `true` for a double point overwrites the variant
  with a bool, after which a protocol read of that point throws inside an io thread (`FD:DataStore.hpp`, `MOD:io/InputModule.cpp:95-105`,
  `P:` §2.1 and `R:CONTRACT.md:82` record the same hazard). **Rust: never replicate.** Types are fixed at load from the `<binary>`/`<analog>` element; a
  mismatching published value is dropped and counted; a protocol mapping of the wrong type (a coil on an analog tag) is a load error.
- **Updated-tag queues** (`FD:DataManager.hpp:134-251`): a write from a protocol (Modbus FC 5/6/15/16, DNP3 operate, 104 command) or a logic
  result goes into `updatedBinary/updatedAnalog[tag] = value`. They do **not** touch the store. Each scan cycle:
  1. external tags in the queues are sent to the provider by the output module; **only after an ACK** the store is set (`setDataByTag`)
     and the queue entry is erased **iff it still holds the same value** (compare-and-erase, `OutputModule.cpp:92-107`); on no reply the
     queue is left as is and retried next cycle (one `E: OutputModule: provider not responding` per outage);
  2. internal tags in the queues are applied to the internal store and erased (`updateInternalData`, `DataManager.hpp:239`).
  Consequence (`O:`): a Modbus write followed by a read returns the **old** value until the provider round trip completes or the next cycle ends; with
  the provider down the read-back never changes (`FC6 40000=32768` then `FC3 40000` → still `4020`).
- Provider failure handling to keep: batch all pending outputs in one `WRITE=`; on `ERR=` retry per point and drop rejected points; ≥ 5 s REQ timeout × 3, then
  recreate the socket ("lazy pirate", `P:` §2.1, `B:src/bennu/distributed/Client.cpp:11-12,54-127`; `O:` log `Client send: no response from server, retrying...`).

### 3.3 Provider link (wire formats are `P:` §2.1, not repeated)

- **Input (DISH side)**: the stock device binds a DISH socket to the `<input>` endpoint and joins group = `hex(std::hash(endpoint))` minus the last digit
  (`B:src/bennu/distributed/Subscriber.cpp:12,19`). The Rust receiver is a plain UDP socket joined to the multicast group parsing
  `[u8 group_len][group][body\0]` datagrams, group value from the `P:` verified vectors (`udp://239.0.0.1:40000` → `54386972605942b`).
  **`O:` confirmed from the receiving side**: a hand-built datagram `0f "54386972605942b" "load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,..." 00`
  sent with a Python UDP socket to `239.0.0.1:40000` was accepted by the stock device and served through Modbus (input register `30000` = `0x4020`).
  Hash for other endpoints: `P:` §2.1's libstdc++ MurmurHash2-64 port (shared code with the Rust provider).
- Body parsing (`MOD:io/InputModule.cpp:23-118`): split on `,`, then on `:`; empty parts skipped; fewer than 2 parts = malformed; the value
  `true`/`false` is a bool, everything else goes through `std::stod` (accepts a numeric **prefix**, `inf`, `nan`, hex floats, leading blanks; a failure
  prints `E: InputModule::subscriptionHandler -- value=...`); unknown point names are counted and ignored. Rust uses a C-`strtod`-prefix
  parser (Decision 6), not `str::parse::<f64>`.
- **Output (REQ side)**: payload `WRITE=point:value[,point:value...]` (`true`/`false`, analog `%f` 6 decimals) + NUL;
  reply `ACK=...`/`ERR=...`. The batch contains only points owned by that `<output>` element plus points no output element owns (`OutputModule.hpp` `sends()`).
- ZeroMQ REQ in Rust: see §5.3 and Decision 5. A REQ socket frames each message as an empty delimiter frame then the body (ZMTP 3.x NULL mechanism, no security).

### 3.4 Logic engine (`<logic>`)

The stock engine textually substitutes tag values into each line and evaluates the right-hand side with a vendored MIT library
(`cparse`, `MOD:logic/cparse/`, licence file `LICENSE.mit`, which is fine to read/port with attribution but we do not need to).
Semantics below were established from source **and verified with a 14-line corpus run on the stock binary** (`conformance/stock/logic_corpus.xml`, `O:`).

Scan cycle (`FD:FieldDevice.cpp:132-157`, `MOD:logic/LogicModule.cpp:100-250`): every `cycle-time` ms, **after** the work (period = cycle + work):
1. `scanInputs`: for every non-blank logic line, replace each known tag name on the **right-hand side** by its current value
   (`True`/`False` for binary tags, `std::to_string(double)` = six decimals for analog tags), longest tag names first;
2. `scanLogic`: evaluate the lines in order; a line changes a tag only if the result differs (`!=` on doubles, exact) from the current value **and** the tag is
   not already queued; the change goes to the updated-tag queue (§3.2). Lines never see results of earlier lines in the same cycle (read-before-write;
   the tree's own unit test `B:test/test_unit_logic.cpp:44-60` asserts this: `out = foo + foobar` then `flag = out > 3` sees the old `out`);
3. `processOutputs`: send queued external tags to the provider, then apply queued internal tags.

Line syntax: `lhs = rhs[,delay:N]`. Split on the **first** `=` (so `==`, `<=` are fine on the right). The right-hand side is split on **every** `,`;
exactly one trailing `delay:<int>` piece is allowed, ≥ 3 pieces = `ERROR: Check logic...too many pieces`. **Consequence (`O:`): any comma inside a function call
breaks the line** (`a8 = sqrt(16) + pow(2,10)` logged `WARN: Delay logic error` + a parse error and was skipped). The LHS must be a known tag; otherwise
the line is evaluated but nothing happens (neither the binary nor the analog branch matches). Results are `asBool()` for binary LHS, `asDouble()` for analog.

Dialect (all numbers are doubles; `O:` marks results observed on the stock build):

| Construct | Stock behaviour | Rust v1 |
|---|---|---|
| `+ - * /` | double math; `1/0` → `inf` (`O:`) | same |
| `%` | operands truncated to int64 (`7 % 3` = 1 `O:`); zero divisor = C++ UB (crash on most targets) | int64 remainder; zero divisor → line error, tag unchanged |
| `**` | `pow`; precedence 3, **left-associative**: `2 ** 3 ** 2` = 64 `O:`; unary minus has the same level: `-2 ** 2` = 4 `O:` | same table |
| `<<`, `>>` | int64 shifts | same (shift ≥ 64 → line error) |
| `< <= > >= == !=` | `==` on numbers compares as doubles and treats booleans as 0/1 (`0.0 == False` → 1 `O:`) | same |
| `&& \|\|` | **both operands truncated to int64 first**: `0.5 && 1` = 0, `0.4 \|\| 0` = 0 (`O:`) | same (documented quirk, `--strict` uses `!= 0`) |
| `!` | `asBool` of the operand: `!0.5` = 0 (`O:`) | same |
| functions | `sin cos tan abs sqrt pow float real int str eval type extend sum print list map` | `sin cos tan abs sqrt pow int float`; multi-arg calls allowed (superset, since stock cannot run them) |
| literals | `True False None`, `# // /* */` comments, strings, lists, maps | `True False`; comments; the rest is a load-time error naming the line |
| tag spelling | textual substitution; a tag is only replaced if the next char is not alphanumeric and it is not at column 0 (substring hazards) | tokenizer with longest-match against the tag list at identifier boundaries (deviation for pathological names only) |

`delay:N` state machine (`LogicModule.cpp:160-215`, `O:` `Delaying tag: d for 600ms` then `[ DELAYED 400ms]`, `200ms`, then applied):
a tag whose result differs and `delay > 0` and which is not yet delayed gets a timer of `N × cycle-time` ms and is **not** written; while a timer runs the line
is **skipped** (timer decremented by one cycle per scan) so flips inside the window are invisible; when the timer reaches ≤ 0 the line is evaluated again and the result is written
if it still differs (timer removed), or the timer is removed if it no longer differs.

Waterway logic strings (`R:topologies/waterway-arm64/waterway-scenario.yaml`, rtu-1/rtu-2) use only `1*(...)`, `&&`, `==`, `>=`, `<=`, `+`, `-`, `;` as statement
separator (the generator turns `;` into newlines) — all inside the table above. The stock image has a **counter bug** that the tree does not: `c = c + 1` stopped
advancing at 2 on the shipped `bennu:arm64` (`O:`), while `B:test/test_unit_logic.cpp:44-54` asserts it advances every cycle. Rust follows the tree's test (Decision 9).

### 3.5 Event logs

Each protocol server opens `<event-logging>` (relative to the cwd, truncated at start, default `<proto>-server.log`) and writes CSV lines
`seq,YYYY-Mon-DD-HH:MM:SS,level,<module-name>,<event>,<message>` (`B:src/bennu/utility/DirectLoggable.cpp`, Boost `second_clock::local_time`, e.g. `2026-Nov-08-14:22:05`).
Module names: `modbus-server`, `dnp3-server`, `iec60870-5-104-server`. Event names/messages are listed per protocol in §4 (source-derived; the
file contents were not captured: UNVERIFIED on a live run). Replicate: ELK/dashboards in some exercises tail these files (`fdlist` in `A:field_devices.py:150-158` only reports ports).

### 3.6 Quirk policy (Decision 7): "compat by default, never unsafe"

| # | Stock behaviour | Default | Flag |
|---|---|---|---|
| Q1 | Modbus unit id ignored on requests, echoed in the reply (`O:` uid 0, 7, 255 all served) | replicate | `--strict` rejects other than configured |
| Q2 | Modbus unsupported function (FC 23, 8) answered with a **7-byte header-only** frame `0001 0000 0001 01` (`O:`); FC 43 → `80 04` | replicate | `--strict` → proper exception 0x01 |
| Q3 | Modbus read of an unmapped *coil* → exception 0x03; unmapped discrete/holding/input → 0x02 (`MOD:comms/modbus/module/Server.cpp:202-243` vs `:420-`; `O:`) | replicate | — |
| Q4 | analog registers scaled to 16-bit over `[min,max]`, conversion `double → uint16` truncating; out-of-range input is C++ UB (arch-dependent) | truncate in range, **clamp** out of range | `--compat-wrap` UNVERIFIED target |
| Q5 | DNP3 binary/analog *outputs* are published as **inputs** (g1v1 / g30v1, Class 0, never events) because the output entry reuses the input point map without variations (§4.2, `O:`) | replicate | — |
| Q6 | DNP3 `AnalogOutputInt16/Int32/Double64` commands return SUCCESS and write nothing (`O:` status 0) | replicate | `--strict` → NOT_SUPPORTED |
| Q7 | IEC 104 select/execute bit ignored, commands handled as direct operate (`O:`) | replicate | — |
| Q8 | pidfile start guard without liveness test (§2.3) | replicate | `--strict` checks `/proc/<pid>` |
| Q9 | Modbus MBAP `length` read into a 1024-byte buffer: **frames > ~1018 bytes corrupt memory**; `O:` a 1100-byte frame was answered, a 3000-byte frame killed the listener (reply reset, later connects refused, log `...accepting an incoming connection: Already open`) | **not replicated**: length > 254 → close the connection | — |
| Q10 | type-mismatched published value / mapping crashes the device (§3.2) | **not replicated** | — |
| Q11 | sample `ep/iec60870-5-104-server.xml` has no `<subtype>`; the loader throws `No such node (subtype)` and **never starts the server** (`O:`; `MOD:comms/iec60870-5/module/DataHandler.cpp:56`) | `subtype` required as stock; missing = clear error naming the key | `--lenient` defaults to `single` |

---

## 4. Protocol servers

All three bind the address given in `<endpoint>` (`tcp://<ip>:<port>`, no wildcard assumption; a bind failure logs and leaves the server absent while the
rest of the device keeps running, as stock). Serial endpoints (a path without `tcp://`) exist for Modbus and DNP3 (`MOD:comms/modbus/module/Server.cpp:46-55`,
`.../dnp3/module/Server.cpp:38-52`): phase F6, via `tokio-serial`.

### 4.1 Modbus/TCP server (`<modbus-server>`)

Config: `endpoint`, `event-logging`, `coil{address,tag}`, `discrete-input{..}`, `holding-register{address,tag,max-value,min-value}`,
`input-register{..}`. Range defaults `max 2000 / min -1000` only if **both** keys are not given (`DataHandler.cpp:70`); the generator always emits both
for registers (`fd_server.mako:87-89`, `R:docs/plan.md:121` documents `[0,1000]`). `unit-id`, `ip` and `instance` appear in other templates and are ignored by the server.

Behaviour:
- MBAP framing, one task per connection, any number of connections, FC 1, 2, 3, 4, 5, 6, 15, 16 only (`Server.cpp:116-126`).
- Analog scaling: `slope = 65535 / (max - min)`, `intercept = -slope × min`; read `reg = slope × v + intercept`, write `v = (raw - intercept) / slope`
  (`Server.cpp:170-198`, `Server.hpp:83`). `O:` input value `250.5`, range `[0,1000]` → `0x4020` (16416; 250.5 × 65.535 = 16416.3).
- Reads fail the **whole** request if any address in the span is unmapped. Writes validate and enqueue address by address, so a multi-write whose later address is unmapped has
  already queued the earlier ones before the exception goes back (`Server.cpp:244-270`); replicate (UNVERIFIED on the wire, F1 probe).
- A write only enqueues (§3.2); the response echoes the request (`O:`).

Observed transcript (`conformance/stock/probe_modbus.py`, `O:`; MBAP header `0001 0000 <len> <uid>` omitted for brevity, request → PDU reply):

| Request | Reply PDU | Note |
|---|---|---|
| FC4 `30000` ×1 | `04 02 4020` | scaled 16-bit |
| FC4 `30000` ×2 / `29999` ×1 | `84 02` | unmapped in span |
| FC2 `10000` ×2 | `02 01 02` | bit0=false, bit1=true (LSB first) |
| FC2 `10000` ×3 | `82 02` | |
| FC1 `0` ×2 | `01 01 02` | |
| FC1 `5` | `81 03` | unmapped coil → 0x03 (Q3) |
| FC3 `40000` / `40001` | `03 02 4020` / `83 02` | |
| FC5 `0`=ON / `9` / `0`=1234 | echo / `85 02` / echo | no value validation |
| FC6 `40000`=32768, FC16 `40000`×1, FC15 `0`×2 | echo, `10 9c40 0001`, `0f 0000 0002` | |
| unit id 7 / 0 / 255 | served, id echoed | Q1 |
| FC23, FC8 | header-only frame | Q2 |
| FC43 | `80 04` | |
| FC4 ×0, ×126 | `84 03` | quantity checks: 0 and >125 → 0x03 |
| FC3 after FC6 (provider down) | unchanged | §3.2 |

Event log events: `read coils|discrete inputs|holding registers|input registers`, `write coils|holding registers`, levels `info`/`error`, messages as in `Server.cpp`.

### 4.2 DNP3 outstation (`<dnp3-server>`)

Config: `endpoint`, `address` (outstation link address, required), `event-logging`, `binary-input{address,tag,[sgvar,egvar,class]}`, `binary-output{address,tag,[sbo]}`,
`analog-input{address,tag,[sgvar,egvar,class,deadband]}`, `analog-output{address,tag,[sbo]}` (`MOD:comms/dnp3/module/DataHandler.cpp:41-110`). The stock stack is opendnp3 3.1.1
(Apache-2.0; `B:.../dnp3/protocol/CMakeLists.txt:10-13`).

Stock behaviour (source + `O:` transcript from `conformance/stock/probe_dnp3.py`, master 1 → outstation 10):
- Channel: TCP server, `ServerAcceptMode::CloseNew` (a second master closes the *new* connection), event buffer 100 per type (`Server.cpp:64,102`). No unsolicited responses observed.
- Database: **binary inputs and binary outputs share one map**, likewise analog (`Server.cpp:197-210,263-274`). Inputs default to `Group1Var2` static / `Group2Var2` events / Class 1 (binary) and `Group30Var6` / `Group32Var6` /
  Class 1 (analog) (`Server.cpp:296` and the binary equivalent above it). **Outputs** are inserted without any variation or class (`addBinaryOutput`/`addAnalogOutput`, `Server.cpp:263-274`), so the library defaults
  apply: static `Group1Var1` / `Group30Var1` (32-bit integer) and no events (Class 0 behaviour). They appear in integrity polls as
  inputs and never produce events (Q5). `O:` integrity poll response (hex, link CRCs stripped):

  ```
  e0 81 90 00                       app ctl FIR|FIN|CON, RESPONSE, IIN1=0x90 (DEVICE_RESTART|NEED_TIME) IIN2=0
  02 02 28 0100 0200 01 000000000000   g2v2 events: idx 2, flags 0x01, time 0     (brkr, class 1)
  20 06 28 0100 0000 01 0000000000000000 g32v6: idx 0, flags 0x01, double 0.0   (load-power, class 1)
  01 02 00 02 02 01                 g1v2 static idx 2 flags 0x01
  01 01 00 05 05 00                 g1v1 packed idx 5 (a binary-OUTPUT, Q5)
  1e 06 00 00 00 01 <8 bytes>       g30v6 static idx 0 (double)
  1e 01 00 01 01 01 00000000        g30v1 static idx 1 (an analog-OUTPUT as 32-bit int, Q5)
  ```
  The second poll repeats the same events (no CONFIRM was sent): events stay buffered until confirmed. IIN was `0x90` in every response observed (no time sync, no restart clear).
- A 1 Hz thread rewrites **every** point from the datastore (`Server.cpp:136-164`): binaries with flags `Binary(bool)` (online), analogs with flags `0x01` and a timestamp argument that is seconds-since-epoch passed
  where `DNPTime` expects milliseconds (UNVERIFIED, only visible for time-bearing variations). New values therefore reach masters ≤ 1 s + cycle after the provider update; events fire when
  the value or flags change (first update clears the RESTART flag, hence the initial events above).
- Commands (`ServerCommandHandler.cpp`, `O:`):

  | Request | Result |
  |---|---|
  | CROB g12v1 direct operate / select, index mapped | status 0, echo of the object; `LATCH_ON` → true, any other op code → false, writes the tag |
  | CROB index unmapped | status 12 (OUT_OF_RANGE) |
  | `AnalogOutputFloat32` g41v3 on an `sbo` point, direct operate | status 2 (NO_SELECT) |
  | same, select then operate | status 0 / 0, value written to the tag |
  | `AnalogOutputInt16/Int32/Double64` | status 0, nothing written (Q6) |
  | `sbo` flag | select always succeeds for a mapped index; direct operate on an `sbo` point → NO_SELECT (`ServerCommandHandler.cpp:41-45`, source only) |

  A write only enqueues the tag (§3.2); the master sees it in the database after the provider echo + the next 1 Hz refresh.
- Link layer: unconfirmed user data `ctrl 0x44` from the outstation; responses CRC-framed by the library.

### 4.3 IEC 60870-5-104 slave (`<iec60870-5-104-server>`)

Config: `endpoint` (tcp only; otherwise logs `Unknown endpoint protocol` and no server), `rpoll-rate` seconds (**required**), `subtype` (**required**; contains `double` → double-point mode, else single),
`event-logging`, `binary-input|output{address,tag}`, `analog-input|output{address,tag}` (`MOD:comms/iec60870-5/module/DataHandler.cpp:51-100`). Library: `lib60870-C` (GPL-3), mode
`CS104_MODE_SINGLE_REDUNDANCY_GROUP`, queues 1000/1000 (`Server.cpp:38`). One server per process (static `gServer`); the periodic sender talks to the **last opened** connection only.

Observed wire behaviour (`conformance/stock/probe_104.py`, `O:`; hex, APDU start `68 <len>`; I-frame control fields little-endian N(S)<<1, N(R)<<1):
- `STARTDT act` `68 04 07 00 00 00` → `STARTDT con` `68 04 0b 00 00 00`.
- Common address is fixed at **1**, originator 0, IOA 3 bytes LE (the config address is a uint16).
- General interrogation `C_IC_NA_1` (type 100) with QOI 20 (the only QOI handled, others get a negative ACT_CON, `Server.cpp:294`, double-point variant `:365`):
  `68 0e 00 00 02 00 64 01 07 00 01 00 000000 14` (ACT_CON, COT 7) → data → ACT_TERM (COT 10).
  Data, COT 20, **all** points of the map including outputs, ordered by address: type 1 `M_SP_NA_1` vsq 4 → `68 1a ... 01 04 14 00 01 00 | 000000 00 | 010000 00 | 640000 00 | 650000 00` (IOA + SIQ),
  type 13 `M_ME_NC_1` vsq 4 → IOA + float32 LE + QDS (`00`), IOAs 2, 3, 102, 103. The ASDU is sent even when it holds no points (`Server.cpp:308,321`).
- Periodic: every `rpoll-rate` s the same two ASDU kinds with COT 1 (periodic) go to the connected client (`reversePollSinglePoint`, `Server.cpp:147-195`), split when the payload reaches
  `MAX_ASDU_PAYLOAD_SIZE`. In double-point mode binaries go as `M_DP_TB_1` (type 31, CP56Time2a, COT 3 spontaneous) per point, interrogation as `M_DP_NA_1` (type 3);
  `convertBoolToDPValue`: false → OFF(1), true → ON(2); inbound double commands: ON(2) → true, OFF(1) → false, others → false + an error log line.
- Commands: `C_SC_NA_1` (45) COT 6 → server echoes the ASDU with COT 7 and writes the bool; `C_DC_NA_1` (46) → ACT_CON, write, ACT_TERM; `C_SE_NC_1` (50, float) → echo COT 7, writes the float.
  `O:` after a command the very next frames were the periodic data ASDUs (COT 1). Other type ids (clock sync 103, read 102, counter interrogation 101, reset process 105, test 107): handler returns false →
  library default (UNVERIFIED; probe in F4).
- APCI timers/windows (k, w, t0-t3): library defaults, UNVERIFIED; use the IEC 60870-5-104 standard values (k 12, w 8, t1 15 s, t2 10 s, t3 20 s) and compare on the wire in F4.

### 4.4 Client modes and the command interface (phase F5, optional)

`modbus-client`, `dnp3-client`, `iec60870-5-104-client` (fd-client and FEP hosts, `A:templates/fd_client.mako`, `fep_template.mako`) poll remote servers every `scan-rate`/5 s and expose
`QUERY=`/`READ=`/`WRITE=` on a ZeroMQ REP at `<command-interface>` (`MOD:comms/base/CommandInterface.cpp`: replies `ACK=tag1,tag2,` / `ACK=<tag>:<value>` / `ACK=Wrote tag <t> -- <v>` / `ERR=...`;
op names in either case). Needed only when a scenario uses `fd-client`/`fep`; not for the waterway. Deferred; the Modbus client reuses the server codec, DNP3/104 clients are separate work.

---

## 5. Rust crate and licence survey (crates.io data 2026-11-08)

### 5.1 Table

| Need | Crate | Version / date | Licence | Verdict |
|---|---|---|---|---|
| async runtime | `tokio` | 1.54.1 / 2026-10-21 | MIT | use |
| Modbus codec/server | `tokio-modbus` | 0.17.0 / 2025-10-22, 573 stars, MSRV 1.76 | MIT OR Apache-2.0 | client for tests; server API is typed (`Service` request→response), so the raw quirks Q1-Q2 cannot be produced through it (**UNVERIFIED** that no raw hook exists) → hand-roll the 300-line MBAP server |
| Modbus (alt) | `rodbus` | 1.6.0 / 2026-10-29 | **"non-standard"**: Step Function I/O agreement, non-commercial/non-production | exclude (see below) |
| DNP3 | `dnp3` (Step Function) | 1.8.0 / 2026-10-29 | **"non-standard"**, same agreement | exclude from the shipped binary (see below) |
| DNP3 | `rusty-dnp3-outstation` / `-master` | 0.0.1 / 2026-07-10, 17 downloads, "does not store any points" | MIT OR Apache-2.0 | too immature; reference only |
| DNP3 | `industrial-protocols-dnp3` | 2.0.0 / 2026-11-07, 72 downloads, part of a 38-protocol umbrella repo | MIT | unvetted, do not depend |
| IEC 104 | `iec104` | 0.6.0 / 2026-10-24, 14 stars, 7 versions, "API still evolving", server + `rtu_server` + optional rustls | MIT | candidate (spike) |
| IEC 104 | `fegrid-iec60870-{core,asdu,cs104,tokio}` | 0.1.0 / 2026-10-03, <100 downloads, sans-IO + spec-traceability tests | MIT OR Apache-2.0 | fallback candidate |
| IEC 104 | `voltage_iec104`, `igw` | 0.1.0 / 0.2.20 | MIT OR Apache-2.0 | unvetted |
| IEC 104 | `lib60870` / `lib60870-sys` (FFI to lib60870-C) | 0.4.0 / 0.5.1, 2025-12 | **GPL-3.0** | **exclude**: would make the whole binary GPL-3 (or needs MZ Automation's commercial licence) |
| IEC 104 | `iec60870-5`, `roboplc-io-iec60870-5` | 0.2.1 / 1.2.0, 2026-03 | "non-standard" (not read) | exclude until read |
| BACnet / SunSpec / 61850 | `bacnet-rs` 0.3.2, `bacnet-*` 0.15, `sunspec` 0.9.1, `iec61850-*` 0.4 | various | not checked | deferred (non-goal) |
| ZeroMQ REQ | `zeromq` | 0.7.0 / 2026-10-27, 1434 stars; REQ/REP over tcp+ipc, "tested against the reference implementation", disclaimer: not all features | MIT | candidate (spike) |
| ZeroMQ REQ | `zmq` (libzmq) | 0.10.0 / 2022-11-04 | MIT/Apache-2.0 | works but pulls a C library (breaks static builds) |
| XML | `roxmltree` 0.21.2 (read-only DOM, CDATA ok), `quick-xml` 0.42.0, `serde-xml-rs` 0.8.2 | 2026 | MIT OR Apache-2.0 / MIT / MIT | `roxmltree` |
| Serial | `tokio-serial` | 5.5.0 / 2026-06-15 | MIT | F6 |
| SHA-256 | `sha2` | 0.11.1 / 2026-10-12 | MIT OR Apache-2.0 | watcher |
| Expression | `evalexpr` | 13.1.0 | **AGPL-3.0-only** | excluded (also in `P:` §7) |

### 5.2 The Step Function (`dnp3`, `rodbus`) licence is the DNP3 decision

`LICENSE.txt` in both repos (fetched 2026-11-08; GitHub reports "other", crates.io "non-standard"; identical agreement text, only the first 40 lines of `rodbus` compared) grants a *personal, perpetual, revocable, limited, non-exclusive,
non-sublicensable, non-transferable* licence for "non-commercial and non-production" use (evaluation, research, teaching, training). It states the Developer **shall not distribute or sublicense any portion**, may not
publish benchmark results, excludes use that "generates revenue or any other form of compensation", and defines production as use "operated by end users and relied upon for organization or commercial operations";
commercial/production use needs a paid agreement (`info@stepfunc.io`). Consequences: an MIT repository cannot ship it, a release binary containing it cannot be distributed, and whether a shared cyber-range
counts as "training" or as "production" is a legal judgement call for Ryan and his organisation — not mine. It is excellent software (Rust, tokio, full outstation) and the right *measuring stick*, but not safe as a default dependency.

Open alternatives for DNP3: (A) the Step Function crate behind a default-off cargo feature that Ryan builds privately; (B) our own outstation, written against the §4.2 captures and the public protocol material;
(C) FFI to `opendnp3` 3.1.1 (Apache-2.0, the exact stock stack, vendored in the bennu tree; MIT-compatible with a NOTICE entry). C gives the highest fidelity for the least code but reintroduces a C++ build
and a memory-unsafe dependency, which is what this migration is meant to remove. B's scope is finite (§9, F3). Recommendation in Decision 2.

### 5.3 ZeroMQ without libzmq

The field device needs only a REQ client (and, for §4.4, a REP). `zeromq` 0.7 is MIT, pure Rust, tokio-based and states interop with libzmq; whether its REQ socket survives the "lazy pirate" cycle
(timeout, drop, new socket) and talks to the **stock** `bennu-simulink-provider`'s libzmq REP is **UNVERIFIED** and is F0's first spike (≤ 0.5 day). If it fails, a hand-rolled ZMTP 3.0 NULL-mechanism REQ client
(greeting, READY with `Socket-Type=REQ`, empty delimiter frame + body) is ~250 lines and testable against both libzmq (stock provider) and `ferrofire`'s own REP. The multicast receive path is plain UDP either way (`P:` Decision 2).

---

## 6. Watcher, firmware hash and brash toggle (must not break)

The exercise mechanic: players upload a "firmware" over FTP to `/home/sceptre/bennu-field-deviced.firmware`; `bennu-watcherd` accepts it **only** if its SHA-256 equals one of three hashes
computed at VM boot from the installed `/usr/bin/bennu-field-deviced` (`A:templates/sceptre_start.mako:37-43`):

```
reghash    = sha256(binary)
shellhashA = sha256(sed 's/\x5f\x53\x48\x45\x4c\x4c\x5f\x30/\x5f\x53\x48\x45\x4c\x4c\x5f\x31/g' binary)     # "_SHELL_0" -> "_SHELL_1"
shellhashB = sha256(python: binary.replace(b'SHELL_0', b'SHELL_1'))
```

The daemon holds `static std::string ss = "START_SHELL_0"` and starts the `socat TCP-LISTEN:1337,reuseaddr,fork,crlf SYSTEM:'clear && cat /etc/issue.net && /bin/login',pty,stderr,setsid,sane,echo=0`
listener (lock `/var/run/shell-server.pid`, cwd `/home/sceptre`) when `ss` contains `'1'`, and stops it otherwise (`DMN:bennu-field-deviced/main.cpp:23,164-181`, `MOD:shell/BrashServer.cpp`).
The patched firmware is therefore "the same binary with the shell enabled".

**Verified on the shipped overlay binary** (`R:images/overlays/bennu-arm64/usr/local/bin/bennu-field-deviced`, 94 256 bytes): `SHELL_0` occurs exactly once (inside `START_SHELL_0`), `_SHELL_0` once, so
shellhashA == shellhashB (`reg 6b570f92…`, A = B = `379a44db…`).

Requirements on the Rust binary (all testable, §8.5):
1. The 13 bytes `START_SHELL_0` appear **exactly once** in the file; no other occurrence of `SHELL_0` anywhere (help text, panic messages, symbol/debug names: build with `strip = true`, never name an identifier `SHELL_0`).
2. The flag is read at runtime and cannot be folded away: keep it as a `static` byte array read with `core::ptr::read_volatile` (an immutable `static` read via `contains('1')` can be const-folded by LLVM).
3. With the byte flipped to `START_SHELL_1` the process runs the same `socat` command line before interpreting the command, so port 1337 opens; with `_0` and a running listener it stops it.
4. The firmware is the **whole executable**, copied over `/usr/bin/bennu-field-deviced` and restarted by the watcher; the Rust binary must be self-contained (static, no sidecar files) so a swapped "firmware" is complete.
5. Watcher (`bennu-watcherd` personality): same flags (`--file` default `/etc/sceptre/config.xml`, `--binary` default `/home/sceptre/bennu-field-deviced.firmware`, `--watcher_config` default `/etc/sceptre/watcher.ini`,
   INI `[Hashes] reghash/shellhashA/shellhashB`), pidfile `<env>-bennu-watcherd`, logs `/etc/sceptre/log/watcher-{stdout,stderr}.log`; log lines `ctime(24) " - " message(83) "- " TYPE(10)` including
   `Firmware Version Hash: <hash>`, `Firmware Updated`, `field-device restarted`, `field-device stopped|started`, `Firmware not loaded` (brash `logShow` prints this file, `B:src/bennu/executables/bennu-brash/main.cpp:150`).
   The stock loop hashes the file every 10 ms (`:355-357`); Rust polls every 250 ms with a `stat` (mtime+size) pre-check and hashes only on change. Update path: stop device (`/usr/bin/bennu-field-deviced --c stop --env`),
   `cp` the file over the binary, start again, 1 s sleeps as stock. The branch for "Firmware not found!" is UNVERIFIED dead code in stock (the hash routine never produces that string).

---

## 7. phenix/sceptre integration

- **No scenario or generator change.** The sceptre app keeps rendering the same `config.xml` and `sceptre_start` fragment; the overlay `images/overlays/bennu-arm64/usr/local/bin/bennu-field-deviced` is replaced by the Rust binary (and
  `bennu-field-device`, `bennu-watcherd` become symlinks to it). Because the hashes are computed from whatever sits at `/usr/bin/bennu-field-deviced` at boot (§6), nothing else moves. `ln -sf /usr/local/bin/bennu-* /usr/bin/`
  in `R:images/scripts/bennu-arm64.sh` already links by glob.
- `R:images/bennu-smoke.sh` is the stock smoke test (query/read through `bennu-probe` against the DNP3 server+client pair): the Rust device must pass it unmodified (F3). Add a Modbus variant for the waterway (F1).
- Waterway: `rtu-1`/`rtu-2` are Modbus fd-servers with `logic:` and analog registers scaled over `[0,1000]` (`R:docs/plan.md:121`); the OPC gateway scales back with the same formula, so Q4 correctness is what the HMI shows.
- Ports in the ELK `fdlist` (`A:field_devices.py:150-158`) depend on metadata, not on the binary.

---

## 8. Testing / conformance plan

### 8.1 Unit and property tests (container, no VM)
Config loader (every shipped `data/configs/ep|bp/*.xml` and the generated waterway/soap configs load, with the Q11 file asserting its error); datastore (compare-and-erase, internal vs external queues); logic (§3.4 table as
a table-driven test, including each `O:` value, the delay state machine, comma/`delay` splitting, the superset cases); Modbus codec; scaling round trip over `[min,max]`; `strtod`-prefix parser; hash-marker test (§6.1-2: the built
binary contains `START_SHELL_0` once, `sed`/Python variants hash identically, flipping the byte starts the listener under a fake `socat` on `PATH`).

### 8.2 Golden transcripts from the stock device (already captured)
`conformance/stock/` holds the probes used for the `O:` facts, runnable from the Mac in one command each (`run-stock.sh modbus|dnp3|104|overflow|logic`; creates container `ff-fd-stock` from `bennu:arm64`, removes it on exit):
`probe_modbus.py` (24 requests), `probe_dnp3.py` (integrity/class poll, CROB select/direct, AO f32 direct/select/operate, unknown index, Int16), `probe_104.py` (STARTDT, GI, single command, float setpoint),
`probe_modbus_overflow.py` (Q9), `logic_corpus.xml`. Each feeds inputs by sending a hand-built RADIO datagram, so no provider is needed. Transcripts are normalised (mask timestamps, transport sequence numbers) and
checked in as `*.golden`.

### 8.3 Differential run in the Lima VM (when free)
Two containers per protocol, names `ff-fd-stock` (image `bennu:arm64`) and `ff-fd-rust` (same image or `ubuntu:22.04` plus the Rust binary), identical config and probe; compare transcripts byte for byte after normalisation. Then:
1. **Provider link**: stock `bennu-test-ep-server` (or the `ferrofire` native provider once it lands) feeds both devices; `bennu-probe --c query/read/write` against a *client-mode* stock device reads the Rust server (the smoke test pair).
2. **Output path**: write a coil/holding register via Modbus on each device, capture the `WRITE=` the device sends to a netcat-style REP, compare payload bytes (batching, `%f` formatting).
3. **Firmware flow** in a container with `watcherd`: upload a valid and an invalid firmware, observe accepted/rejected, port 1337 opens only for the shell hash.
4. **Waterway end-to-end**: swap the overlay binary for `rtu-1`/`rtu-2` in an `rb-` phenix experiment (name prefix `rb-`; never touch the running experiments); the lock cycles to the same trace as with stock RTUs (`R:tests` golden, `provider/waterway-sim` trace) and the OPC/HMI values match.
The stock baseline is the deployed `bennu:arm64` image, **not** the uncommitted tree (Decision 9): the image build predates the tree's logic/output fixes (`--log-level` missing, counter stuck at 2 `O:`).

### 8.4 Hostile input
`cargo fuzz` / `proptest` targets: MBAP stream, DNP3 link+transport+application frames, 104 APCI/ASDU stream, the RADIO datagram parser, the logic parser, the XML loader. Invariant: no panic, bounded memory, connection closed on violation.
Regression test for Q9 (3 KB frame, connection closed, listener alive).

### 8.5 Pass criteria per protocol
Modbus: all 24 probe rows byte-equal except documented Q9/`--strict` rows. DNP3: byte-equal application layer (after CRC re-computation) on the integrity poll, the class poll, all command rows, and the stock `bennu-field-device` DNP3 client reads `brkr:true` / `load-power:400.5`
from our outstation (`R:images/bennu-smoke.sh`). 104: byte-equal STARTDT/GI/command ASDUs, stock 104 client interoperates.

---

## 9. Phased plan

| # | Phase | Deliverable | Verify | Days |
|---|---|---|---|---|
| F0 | Core | workspace feature `fielddev`; `roxmltree` loader + validation; datastore; DISH-compatible UDP receiver; REQ client (zeromq spike first); logic engine (§3.4); `ferrofire field-device --file` foreground | §8.1; `O:` logic corpus equal; the `P:` verified group hash; feed a datagram, read the store; REQ round trip against stock `bennu-test-ep-server` | 2.5 |
| F1 | Modbus/TCP | MBAP server (FC 1-6, 15, 16), scaling, quirks Q1-Q4, Q9 fix, event log | 24-row differential (§8.3) + fuzz + waterway rtu config loads | 1.5 |
| F2 | Daemon, watcher, firmware | `bennu-field-deviced` personality (pidfile, daemonize, logs, prefix flags), `bennu-watcherd`, marker + brash toggle | §6.1-5 tests; firmware flow in container; **waterway RTUs on Rust in an `rb-` experiment** (first milestone) | 2 |
| F3 | DNP3 outstation | link/transport/app, Class 0/1/2/3 READ, events (100/type), IIN, CROB direct/select/operate, AO f32, Q5-Q6, 1 Hz refresh | transcript differential + stock client smoke | 5-6 (decision 2) |
| F4 | IEC 104 | spike on `iec104` vs byte goldens, then GI/periodic/commands, single + double mode | transcript differential + stock 104 client | 2-3 |
| F5 | Clients + command interface (optional) | modbus/dnp3/104 clients, REP `QUERY/READ/WRITE` | stock `bennu-probe` against ours | 3-4 |
| F6 | Serial, packaging | `tokio-serial` Modbus RTU/DNP3 serial, musl static builds (aarch64 + x86_64), overlay swap, docs | pty pair test; `file`/`ldd` static check; range experiment | 2 |

Total ≈ 15-17 days without F5 (F0-F2 ≈ 6 days for the waterway). Phases F0-F1 need no VM time beyond the §8.3 differential runs.

### Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| DNP3 outstation subtleties (IIN, event/confirm, class behaviour) | medium | goldens from stock; C-option fallback (Decision 2); scope limited to what the generator emits |
| `zeromq` crate REQ reconnect semantics | medium | F0 spike; hand-rolled ZMTP NULL REQ fallback |
| `iec104` cannot reproduce byte-exact periodic/ACT_CON/ACT_TERM sequence | medium | spike in F4; thin hand-rolled codec fallback (~1.5k lines, APCI+7 ASDU types) |
| Marker string altered by the toolchain (folded, duplicated, in panic text) | medium | §6.1-2 test is a CI gate; read via `read_volatile` |
| Stock image differs from the tree (baseline drift) | high | pin the baseline to the image digest; record bennu revision in every golden header |
| Quirk fidelity vs training value (Decision 7) | low | `--strict` flag; both modes tested |
| musl/daemonize edge cases | low | F6 test; fall back to glibc (aarch64 jammy) as `P:` does |
| Legal read of the Step Function agreement | n/a | not depended on by default |

---

## 10. Licensing

- `ferrofire` stays MIT, Copyright (c) 2026 Ryan Adams. `NOTICE` gains: PYPOWER (already), and for any code derived from the Apache-2.0 `opendnp3` (only if Decision 2 option C or a port is taken) the Apache notice;
  `cparse` (MIT, vendored in bennu) is **not** copied: our logic dialect is written from the behaviour table in §3.4.
- bennu/pybennu GPL-3: nothing translated; wire formats, config keys, CLI flags, error texts quoted only where they are interface facts (log prefixes, exception codes).
- `lib60870-C` GPL-3 and `lib60870(-sys)` are excluded; `evalexpr` AGPL excluded; Step Function crates not in the default build (§5.2).
- `roxmltree`, `tokio`, `tokio-serial`, `sha2`, `zeromq`, `iec104` (MIT or MIT/Apache) are compatible. Run `cargo deny check licenses` in CI (new file `deny.toml`).

---

## 11. Decisions for Ryan (recommendation first)

1. **Placement.** Same repo and the same multi-call `ferrofire` binary (`field-device` subcommand + `argv[0]` personalities, cargo features `provider`/`fielddev`). Alternatives: a second binary in the workspace, or its own repo.
2. **DNP3 stack.** Recommended: **our own outstation (B)**, written against the §4.2 captures, with opendnp3 as a permitted Apache-2.0 reference, ~5-6 days, differential-tested; fall back to **FFI to opendnp3 (C)** if F3 exceeds 8 days. Not the Step Function crate by default (§5.2);
   optionally add it as a private default-off feature (A) if you decide your range use is "non-commercial, non-production" and you never redistribute the binary — that is your call or your counsel's.
3. **Modbus.** Hand-rolled MBAP server on tokio (~300 lines, raw control of quirks Q1-Q2), `tokio-modbus` only as a test client. Alternative: `tokio-modbus` server (less code, loses fingerprint fidelity).
4. **IEC 104.** Adopt `iec104` (MIT) after a one-day spike against the byte goldens; otherwise hand-roll the subset. Alternative: `fegrid-iec60870-*`. Never the GPL `lib60870` bindings.
5. **ZeroMQ and static build.** `zeromq` 0.7 (pure Rust) after the F0 spike, else a hand-rolled ZMTP REQ; field-device binary built **static musl** (aarch64 first). Alternative: `zmq`/libzmq and glibc like the provider (simpler, not static).
6. **Numeric parsing.** C-`strtod` prefix semantics for published values (and for the logic literals) so odd provider output is read like stock; alternative: strict Rust `f64` parsing.
7. **Quirk policy.** Compat by default for protocol fingerprints (Q1-Q8), `--strict` for spec-correct behaviour, never replicate memory-unsafe or crashing behaviour (Q9, Q10). Alternative: strict only; or full bug-compat (not offered).
8. **Logic dialect.** New small `fd::logic` module with the cparse-compatible table (all-double math, `&&`/`||` via int64, left-assoc `**`) plus the comma-in-call superset; do **not** reuse the model-file `expr.rs` dialect (`^`, boolean `&&`).
9. **Which stock is the oracle.** The deployed image `bennu:arm64` for protocol bytes, the tree's unit tests (`test_unit_logic.cpp`) for logic/scan semantics where they disagree (the counter bug). Alternative: rebuild stock from the tree for the oracle (a few hours of arm64 build).
10. **Scope.** v1 = Modbus + DNP3 + 104 servers + daemon/watcher; clients and `fep` (F5), serial (F6), BACnet/SunSpec/61850 deferred. Alternative: pull F5 forward if a scenario needs `fd-client`.
11. **Watcher polling.** 250 ms with stat pre-check (vs stock 10 ms full hash loop): same behaviour, no CPU burn. Alternative: replicate 10 ms.
12. **Event logs.** Replicate the CSV format exactly (cheap) vs structured logging only.

## 12. Open items (UNVERIFIED, resolve in the named phase)
- Out-of-range register conversion on arm64 (Q4) — F1 probe with an out-of-range published value.
- Multi-write partial-queue behaviour on a mid-span unmapped address — F1.
- Event-log file contents of a live stock run — F1.
- DNP3 `sgvar/egvar/class` token spellings and the analog timestamp unit — F3 (only if a scenario sets them).
- 104 behaviour for type ids 101-107, APCI timer values, redundancy-group behaviour on a second connection — F4.
- `zeromq` REQ interop and `aarch64-unknown-linux-musl` daemonizing — F0/F6.
- Step Function agreement applicability to the range — Ryan.
