# bennu-simulink-provider <-> solver contract

Citations: `P` = `~/git/sceptre-bennu/src/bennu/executables/bennu-simulink-provider/main.cpp`,
`H` = `.../solver/modelhooks/modelHooks.h`, `C` = `.../solver/modelhooks/modelHooks.c`.
`modelhooksRealtime/modelHooks.c` differs only by an extra time-shm (`TIME_SHM_KEY`, jitter stats) and a
`(void*)-1` semaphore-failure check; the provider does not use it, so it is not part of the contract.
`R:` = this crate (`src/contract.rs`, `src/ipc.rs`, `src/main.rs`).

## Constants (must match on both sides: P:25)
| Item | Value | Source |
| --- | --- | --- |
| Count shm key | `10610` (SysV) | P:28, H:42 |
| Points shm key | `10613` (SysV) | P:29, H:43 |
| Updates FIFO | `/tmp/updates_fifo` | P:32, H:46 |
| Publish semaphore | POSIX named `publish_sem` | P:36, H:49 |
| Updates semaphore | POSIX named `updates_sem` | P:37, H:50 |
| Slot / record size | `MAX_MSG_LEN = 256` bytes | P:35, H:98 |
| Point list file | `publishPoints.txt`, relative to solver cwd | H:57, C:79 |
| Solver CLI | `./simulinksolver -tf inf` (Simulink-generated main; hooks ignore argv) | plan Phase 4 |

## Who creates what
Solver (`init()`, C:7-121) is the owner; the provider only attaches.
1. Read `publishPoints.txt` from cwd; one tag per line, trailing char stripped (`C:440-487`, `C:479`). Fatal if missing (C:450).
   `numPublishPoints` = line count (C:79-81).
2. `sem_open(name, O_CREAT, 0644, 0)` for both semaphores -> created **locked** (count 0) (C:84-93).
   The provider also does `sem_open(O_CREAT, 0644, 0)` (P:104-115), so whichever starts first creates it.
   No `sem_unlink` before create (would orphan a provider that already holds it).
3. `unlink` + `mkfifo(0666)` the FIFO, then open **read end** `O_RDONLY|O_NONBLOCK` (C:96-105).
4. Count shm: `shmget(10610, sizeof(unsigned int)=4, IPC_CREAT|0666)`, `shmat`, `sprintf("%u", n)` -> decimal text + NUL (C:108-110).
   Only 4 bytes: counts up to 999.
5. Points shm: `shmget(10613, n*256, IPC_CREAT|0666)`, `shmat`; `publishState()` fills it (C:113-116).
6. `sem_post(publish_sem)` and `sem_post(updates_sem)` -> unlocked (C:119-120).

Provider (constructor, P:99-140): `sem_open` both; `sem_wait(publish_sem)` (blocks until step 6 -> this is the startup
handshake); `shmget(10610, 4, IPC_CREAT|0666)`, parse the text count (P:127-131); `shmget(10613, n*256, IPC_CREAT|0666)`
+ `shmat` (P:135-137); `sem_post(publish_sem)`; then `open("/tmp/updates_fifo", O_WRONLY|O_NONBLOCK)` (P:142) --
fails (ENXIO) unless the solver's read end is already open, hence the ordering in step 3.

## Steady state
`step()` hook, once per solver step (C:124-157), strictly in this order:
1. `sem_wait(publish_sem)`; `publishState()` overwrites every slot; (every `GT_PRINT_INTERVAL` s dump groundTruth.txt); `sem_post` (C:141-150).
2. `sem_wait(updates_sem)`; `applyUpdates()` drains the FIFO; `sem_post` (C:152-154).
3. Simulink then advances the model one fixed step.

Provider read paths (`query` P:149-170, `read` P:173-208, `publishData` P:290-312, default 0.1 s) take
`publish_sem` around a linear scan of the n slots. `write` (P:211-275) takes `updates_sem` around FIFO writes.

## Point encoding (publish side)
Slot i is bytes `[i*256, (i+1)*256)`, one NUL-terminated string, **in publishPoints.txt line order** (C:200-210):

    <match>:BOOLEAN:<0|1>          e.g.  lowergate.open:BOOLEAN:1        (C:299 "%d")
    <match>:DOUBLE:<%f>            e.g.  lock.height:DOUBLE:5.500000     (C:305 "%f", 6 decimals)

`<match>` is the line from publishPoints.txt (C:292) -- in Simulink it is a substring matched against signal block paths
(C:269); here it is the tag itself. Unknown types are written as `<match>:` only (C:310-311).
Provider decode (`Dto`, P:47-117): split at first `:` -> tag; split at the first `.` of tag -> `tag` + `field`
(default field `processModelIO`, P:52); next `:` -> type; rest -> data. The published/queried name is
`tag.field`, or just `tag` when field is `processModelIO` (P:156-160). Hence `lock.height` -> tag `lock`, field `height`,
name `lock.height`. `getDataString` (P:62-92): `DOUBLE`->number (precision 10), `BOOLEAN`->`true`/`false`,
`INITIALIZED`->`Null`, else `Unexpected`.
Provider answers: `query` -> `ACK=tag1,tag2,...,` (P:147, trailing comma); `read` -> `ACK=<value>` or `ERR=Tag not found`.

## Update encoding (write side)
Provider `write(tags)` (P:211-275): value `true`/`false` -> type `BOOLEAN`, data `1`/`0`; anything else -> type `DOUBLE`,
data = the string as given. Builds `"<tag>:<type>:<data>"` (P:245), `snprintf` into a zeroed 256-byte buffer
(error if >= 256, P:246), `::write(fifo, buf, 256)` -- always whole 256-byte records (P:257), so atomic
(<= PIPE_BUF). `<tag>` is the tag name sent by the RTU, e.g. `lowergate.open`.
Solver `applyUpdates()` (C:216-242): `read(fifo, buf, 256)` until it returns <= 0; `strtok` on `:` -> tag, type, val;
`updateModelParameterValue(tag, val)` (C:318-394) matches tag as a **substring** of block-parameter paths; DOUBLE ->
`strtod`, BOOLEAN -> `strtol(val, NULL, 2)`. Unmatched tags are dropped silently (C:347-352). The type field is not trusted:
the target parameter's own type decides.

## Teardown
`term()` (C:160-190, never called on ^C/SIGKILL): take both semaphores, `shmdt`+`IPC_RMID` both segments, `sem_close`+`sem_unlink`
both, close+unlink FIFO. The Rust solver handles SIGINT/SIGTERM and does the same. On start it removes a stale segment of a
different size and drains stale semaphore counts (R: `ipc.rs`).

## Rust implementation notes (deviations)
- publishPoints.txt missing -> built-in 15-tag list (C is fatal). Lines are trimmed (CRLF safe).
- Update tags are matched **exactly**, not by substring (only the 4 inputs exist). Non-zero value = true.
- Fixed 50 ms step, real time; one boat; publish happens each step, updates applied the same step before physics advances.
- Types chosen (the original model is not available): every tag DOUBLE (0/1 for open/active): bennu field devices declare all tags analog and abort (std::bad_variant_access) on a BOOLEAN.
  RTU logic compares `direction == 1/-1`, so direction is DOUBLE. If a Modbus mapping needs another type, change
  `physics::read_point` only.

## Tag set / physics (from `injects/publishPoints.txt`, `waterway-scenario.yaml`)
Outputs (15): lowerwater.height, lock.height, upperwater.height, lowergate.open, lowervalve.open, uppergate.open,
uppervalve.open, boat.direction, lowersensor.direction, locksensor.direction, uppersensor.direction, boat.location,
lowersensor.active, locksensor.active, uppersensor.active.
Inputs (RTU-written): lowergate.open, lowervalve.open (rtu-1 `logic:`), uppergate.open, uppervalve.open (rtu-2).
Semantics inferred from the RTU logic: lowervalve open drains the lock toward `lowerwater.height`, uppervalve fills toward
`upperwater.height`; a gate opens when a boat is at its sensor and the lock level is within 0.1 m of that pool; sensor
`direction` is the boat direction while active (0 otherwise). Defaults: lower pool 5 m, upper 15 m, 0.5 m/s, boat 5 m/s on a
300 m course (gates at 100 and 200), reverses at both ends. `physics::tests::full_transit_with_rtu_logic` closes the loop with a
transcription of the RTU logic.

## Build target: glibc, not musl (found by the IPC test)
The binary must be `aarch64-unknown-linux-gnu`, built against jammy glibc (2.35, see `build-linux.sh`). A static musl build
starts and publishes shm fine, but the POSIX semaphore files in `/dev/shm` use a different `sem_t` layout in musl than in
glibc, so the glibc `bennu-simulink-provider` blocks forever in its constructor `sem_wait(publish_sem)` (P:123).
`ipc-test.sh` runs solver + stock provider in `bennu:arm64` and checks 15 points and lock.height changes.
