# ferrofire RTU image (shared base + variant 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Buildroot arm64 RTU image running ferrofire's field device, which PEAT on a kali-purple VM can identify, pull and push firmware/config to over SSH and telnet, with the change visible in parseus, demonstrated on waterway's `rtu-1`/`rtu-2`.

**Architecture:** The factory `ferrofire` binary on a read-only root runs a new `ferrofire watch` supervisor. The supervisor executes whatever firmware sits in the `sceptre` user's home on a persistent data partition. PEAT gains SSH/telnet identify, pull and push for SCEPTRE devices. The SCEPTRE app gains a `firmware: ferrofire` startup branch. The range repo gets an install script, an isolated test topology and generator flags for the demo.

**Tech Stack:** Rust (ferrofire), Buildroot (BusyBox init, glibc, OpenSSH, GRUB2 arm64-efi, genimage), Python (PEAT: paramiko, telnetlib; sceptre-phenix-apps: mako, pytest), phenix/minimega on the Lima range.

**Spec:** `docs/superpowers/specs/2026-11-13-field-device-image-design.md` (approved 2026-11-13).

## Global Constraints

- arm64 first; the image boots UEFI (AAVMF) on `virt` with virtio disk and net, like `bennu-arm64`.
- Buildroot, BusyBox init (no systemd), root filesystem mounted read-only at runtime.
- Partitions: 1 = EFI (vfat), 2 = root (ext4; topology `inject_partition: 2`), 3 = data (ext4, mounted rw at `/data`).
- Toolchain glibc >= 2.35. ferrofire's `make linux` binary targets jammy glibc 2.35 on purpose (named semaphores must match the glibc provider), so it is used as-is.
- miniccc: `/opt/minimega/bin/miniccc -v=false -serial /dev/virtio-ports/cc -logfile /var/log/miniccc.log`, injected with `phenix image inject-miniexe <miniccc> <img>:2`.
- `/etc/phenix/startup/*` run in name order at every boot, with bash.
- Access: SSH on 22 (OpenSSH, SFTP subsystem) and telnet on 23 (BusyBox telnetd). No FTP. User `sceptre`, password `sceptre`, uid 1001, shell `/bin/sh` (BusyBox ash), home `/home/sceptre`.
- Device contract (PEAT and image): `/home/sceptre/config.xml` and `/home/sceptre/ferrofire.firmware`, the same paths over SSH and telnet. `/home/sceptre` is `/data/sceptre`.
- Any pushed firmware is accepted (no hash allow-list).
- Factory files are seeded into `/home/sceptre` only when absent; a reboot never overwrites a pushed file.
- Image name `ferrofire-rtu-arm64.qc2`. Never reuse an existing image name in `/phenix/images`.
- Test in an isolated experiment (own topology and namespace, no gateway) until Task 9. Waterway changes ship behind generator flags that default off.
- Per repo: one squashed conventional commit per PR, with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. No claude.ai session links anywhere.

## Review Focus

1. **A firmware caught mid-upload** (scp or SFTP writing in place) must not be executed half-written. The watcher switches only after the hash is unchanged across two polls, and PEAT writes to `<name>.tmp` and renames. Pinned in Task 1 (`waits_for_stable_firmware`) and Task 7 (`push_writes_temp_then_renames`).
2. **A pushed firmware without the exec bit** (SFTP and base64 uploads arrive 0644) must still run. The watcher copies it to `<run-dir>/active` with mode 0755. Pinned in Task 1 (`runs_firmware_without_exec_bit`).
3. **Garbage or crashing firmware** must not kill the watcher. The watcher logs, retries with backoff, and recovers immediately on the next push. Pinned in Task 1 (`survives_garbage_firmware_then_recovers`).
4. **A pushed `config.xml` that is invalid XML**: the field device exits, the watcher logs, and the next good push recovers. Pinned in Task 1 (`config_change_restarts_and_bad_config_recovers`).
5. **Reboot after a push** when phenix has injected a different `/etc/sceptre/config.xml`: the persisted pushed files win. Pinned in Task 3 (smoke reboot check) and Task 4 (phenix reboot check).

---

### Task 1: `ferrofire watch` supervisor

**Files:**
- Create: `src/watch.rs`
- Modify: `src/main.rs` (add `Cmd::Watch(watch::Cli)` to the `Cmd` enum at :18-62 and a dispatch arm at ~:104-120), `src/lib.rs` (`pub mod watch;`, if modules are declared there)
- Test: `tests/watch.rs`

**Interfaces:**
- Produces: CLI `ferrofire watch --firmware <PATH> --file <PATH> [--poll-ms <u64, default 500>] [--run-dir <PATH, default /run/ferrofire>]`. It runs in the foreground and exits on SIGTERM/SIGINT after stopping the child. stderr log lines (exact prefixes, which tests grep):
  - `watch: firmware sha256=<64 hex>`
  - `watch: started pid=<n>`
  - `watch: exited status=<code|signal>`
  - `watch: restart: <firmware|config> changed`
  - `watch: start failed: <error>`
- Behavior:
  - Copy the firmware to `<run-dir>/active` with mode 0755, then spawn `<run-dir>/active field-device --file <config>`.
  - Hash the firmware and config every poll. A change counts only when the new hash is seen on two consecutive polls. On a change, send SIGTERM to the child, wait up to 5 s, SIGKILL, then restart.
  - If the child exits on its own, or the exec fails, restart with backoff (1 s doubling, capped at 30 s). Any file change resets the backoff and restarts at once.

- [ ] **Step 1: Write the failing tests** in `tests/watch.rs`. Use fake firmware written as shell scripts, for example `#!/bin/sh\necho "v1 $*" >> "$MARK"; exec sleep 1000`, with `--poll-ms 50`:
  - `starts_firmware_with_config`: the marker shows `v1 field-device --file <config>`, and stderr has `watch: firmware sha256=` and `watch: started pid=`.
  - `restarts_on_firmware_change`: overwrite with a v2 script. Within 2 s the marker shows `v2`, stderr has `watch: restart: firmware changed`, and the v1 process is gone.
  - `runs_firmware_without_exec_bit`: a firmware file with mode 0644 still starts.
  - `waits_for_stable_firmware`: write half a script, then the rest after 30 ms (less than 2 polls). The half version never runs (no marker line from it).
  - `survives_garbage_firmware_then_recovers`: firmware of 4 KiB of random bytes gives `watch: start failed` or `watch: exited`, and the watcher is still alive after 1 s. Pushing the v2 script then starts within 2 s.
  - `config_change_restarts_and_bad_config_recovers`: rewriting the config gives `watch: restart: config changed`. Using the real `ferrofire` binary as firmware with an invalid XML config gives `watch: exited`. Restoring a valid config (`tests/conformance/rtu-1.xml`) gives `watch: started` again.
  - `sigterm_stops_child`: SIGTERM to the watcher terminates the child too.
- [ ] **Step 2: Run them and confirm they fail.** Run: `make test` (or `cargo test --test watch` in the container). Expected: FAIL, because the `watch` subcommand does not exist.
- [ ] **Step 3: Implement `pub fn main(cli: Cli) -> anyhow::Result<()>` (or the crate's existing error type) in `src/watch.rs`, and wire up `Cmd::Watch`.** Use `std::process::Command` for the child, `libc::kill` for signals (libc is already a dependency), and a SHA-256 crate only if the crate graph lacks one (check `Cargo.lock` first; otherwise add `sha2`). Use a plain poll loop: no inotify.
- [ ] **Step 4: Run and confirm they pass.** Run: `make test`. Expected: all `tests/watch.rs` tests PASS, and the rest of the suite stays green. `make check` also passes.
- [ ] **Step 5: Commit** `feat(watch): supervise the field device from a pushable firmware file`.

### Task 2: Buildroot base image that boots read-only

**Files (all new, under the ferrofire repo):**
- `image/external.desc`, `image/Config.in`, `image/external.mk`: the Buildroot external tree, name `FERROFIRE`.
- `image/BUILDROOT_VERSION`: one line, a pinned Buildroot LTS tag (use the newest LTS at implementation time and record it).
- `image/configs/ferrofire_rtu_aarch64_defconfig`: start from Buildroot's `qemu_aarch64_virt_defconfig` and add:
  - glibc toolchain, GRUB2 `arm64-efi`, and a kernel with virtio blk, net, console and serial;
  - BusyBox with `telnetd`, `ntpd`, `base64`, `sha256sum`;
  - `openssh` (server plus `sftp-server`), `bash`, `iproute2`, `e2fsprogs`;
  - `BR2_ROOTFS_USERS_TABLES=$(BR2_EXTERNAL_FERROFIRE_PATH)/board/ferrofire-rtu/users.table`;
  - `BR2_ROOTFS_OVERLAY` set to the overlay below;
  - post-build and post-image scripts, as below.
- `image/board/ferrofire-rtu/users.table`: `sceptre 1001 sceptre 1001 =sceptre /home/sceptre /bin/sh - ferrofire RTU`.
- `image/board/ferrofire-rtu/genimage.cfg`: GPT. Partition 1 `efi` vfat 64M, partition 2 `rootfs` ext4 256M, partition 3 `data` ext4 128M (empty, label `data`).
- `image/board/ferrofire-rtu/post-build.sh`: copies `dist/ferrofire` (from `make linux`) to `$TARGET_DIR/usr/bin/ferrofire`.
- `image/board/ferrofire-rtu/post-image.sh`: runs genimage, then `qemu-img convert -O qcow2` to `image/out/ferrofire-rtu-arm64.qc2`.
- Overlay `image/board/ferrofire-rtu/rootfs-overlay/`:
  - `etc/fstab`: root ro; `/data` rw; tmpfs on `/tmp`, `/run`, `/var/log`.
  - `etc/inittab`: BusyBox sysinit runs `/etc/init.d/rcS`, plus a serial getty.
  - `etc/init.d/S01data`: mount `/data`, `mkdir -p /data/sceptre /data/ssh`, chown sceptre, bind-mount `/data/sceptre` on `/home/sceptre`.
  - `etc/init.d/S20phenix`: `for f in /etc/phenix/startup/*; do bash "$f"; done`, logging to `/var/log/phenix-startup.log`.
  - `etc/init.d/S30miniccc`: start miniccc with the Global Constraints command line if the binary exists.
  - `etc/init.d/S40ntpd`: turn `server` lines in `/etc/ntp.conf` into `ntpd -p` arguments, if the file exists.
  - `etc/init.d/S50sshd`: host keys in `/data/ssh`, generated on first boot.
  - `etc/init.d/S51telnetd`.
  - `etc/ssh/sshd_config`: `HostKey /data/ssh/...`, `Subsystem sftp /usr/libexec/sftp-server`, `PasswordAuthentication yes`, `PermitRootLogin no`.
- `image/build.sh`: runs the Buildroot build in a Debian container on the Mac's Docker (native arm64), with a download/ccache volume.
- Root `Makefile`: target `image` (depends on `linux`, runs `image/build.sh`) and target `image-smoke`.
- `image/test/smoke.sh`: boots the qcow2 in `qemu-system-aarch64 -M virt -cpu max` with AAVMF and user-net hostfwd 2222->22 and 2323->23, then checks over SSH with `sshpass -p sceptre`.
- `.gitignore`: `image/out/`.

**Interfaces:**
- Consumes: `dist/ferrofire` (from `make linux`).
- Produces: `image/out/ferrofire-rtu-arm64.qc2`, root on partition 2.

- [ ] **Step 1: Write the smoke checks** in `image/test/smoke.sh`. Each one prints `ok <name>` or exits 1:
  - `ssh_login`: `sshpass -p sceptre ssh -p 2222 sceptre@localhost true`
  - `root_ro`: `grep ' / ' /proc/mounts` shows `ro`
  - `etc_not_writable`: `touch /etc/x` fails
  - `data_rw`: `touch /home/sceptre/x && rm /home/sceptre/x`
  - `telnet_open`: port 2323 accepts a connection
  - `ferrofire_runs`: `ferrofire --version` exits 0
  - `sftp_works`: an `sftp -P 2222` batch `put`/`get` of a 1 MiB file round-trips with the same sha256
- [ ] **Step 2: Run them and confirm they fail.** Run: `make image-smoke`. Expected: FAIL (no image yet).
- [ ] **Step 3: Write the external tree, defconfig, overlay and scripts** listed above.
- [ ] **Step 4: Build and smoke-test.** Run: `make image && make image-smoke`. Expected: every check prints `ok`, and the qcow2 is under 200 MB.
- [ ] **Step 5: Commit** `feat(image): Buildroot arm64 RTU base with read-only root and data partition`.

### Task 3: RTU start-up and persistence in the image

**Files:**
- Create: `image/board/ferrofire-rtu/rootfs-overlay/usr/sbin/ferrofire-rtu-start` (POSIX sh)
- Modify: `image/test/smoke.sh` (add the RTU checks)

**Interfaces:**
- Consumes: `ferrofire watch` (Task 1).
- Produces: `ferrofire-rtu-start [CONFIG]`, where CONFIG defaults to `/etc/sceptre/config.xml`. It is idempotent:
  - If `/home/sceptre/ferrofire.firmware` is absent, copy `/usr/bin/ferrofire` there.
  - If `/home/sceptre/config.xml` is absent, copy CONFIG there.
  - chown both to sceptre.
  - If no watcher is running (pidfile `/run/ferrofire-watch.pid`), start `ferrofire watch --firmware /home/sceptre/ferrofire.firmware --file /home/sceptre/config.xml` detached, with stderr going to `/var/log/ferrofire-watch.log`.

- [ ] **Step 1: Add the smoke checks.** Before boot, the smoke script injects the way phenix does. It works on a raw copy of the qcow2 and writes into partition 2 with `debugfs -w`:
  - `tests/conformance/rtu-1.xml` to `/etc/sceptre/config.xml`;
  - a one-line `/etc/phenix/startup/sceptre-start.sh` (`/usr/sbin/ferrofire-rtu-start /etc/sceptre/config.xml`).

  `S20phenix` then runs it as root at boot. The checks:
  - `rtu_seeds`: both files exist in `/home/sceptre`, and `cmp /usr/bin/ferrofire /home/sceptre/ferrofire.firmware` succeeds.
  - `rtu_serves`: the Modbus endpoint from rtu-1.xml (127.0.0.1:5502) accepts a TCP connection within 10 s.
  - `rtu_push_persists`: as `sceptre`, upload a modified firmware (a `#!/bin/sh` script that `exec /usr/bin/ferrofire "$@"`) to `ferrofire.firmware.tmp`, `mv` it into place, and see a new `watch: firmware sha256=` line. Also replace `config.xml` with one whose comment differs. Power the VM off, inject a different `/etc/sceptre/config.xml` with `debugfs` (as phenix would), and boot again. The firmware sha256 and `config.xml` in `/home/sceptre` are the pushed ones, not the factory or newly injected ones.
  - `rtu_start_idempotent`: inject a second startup script, `/etc/phenix/startup/sceptre-start2.sh`, with the same line. After boot, `ps` shows exactly one `ferrofire watch`.
- [ ] **Step 2: Run them and confirm they fail.** Run: `make image-smoke`. Expected: the new checks FAIL.
- [ ] **Step 3: Write `ferrofire-rtu-start`** as specified.
- [ ] **Step 4: Rebuild and smoke-test.** Run: `make image && make image-smoke`. Expected: all checks `ok`.
- [ ] **Step 5: Commit** `feat(image): seed and supervise the RTU firmware from the data partition`. Then squash the ferrofire branch (Tasks 1-3) into one commit for its PR.

### Task 4: Install on the range and boot under phenix (isolated)

**Files (phenix-waterway-range):**
- Create: `images/ferrofire-rtu.sh`. It copies `~/git/ferrofire/image/out/ferrofire-rtu-arm64.qc2` into Lima at `/phenix/images/ferrofire-rtu-arm64.qc2`, then runs `phenix image inject-miniexe /phenix/miniccc /phenix/images/ferrofire-rtu-arm64.qc2:2` (miniccc copied from the minimega container, as `images/build.sh` does). It refuses to overwrite a running experiment's image.
- Create: `topologies/ferrofire-rtu-test/ferrofire-rtu-test.yaml`. One VM `rtu-t`, `inject_partition: 2`, AAVMF and `virt` as for `rtu-1`, one NIC on vlan `FFRT` with no gateway. Injections:
  - the range's generated `rtu-1` `config.xml` to `/etc/sceptre/config.xml`;
  - a one-line `sceptre-start.sh` (`/usr/sbin/ferrofire-rtu-start /etc/sceptre/config.xml`) to `/etc/phenix/startup/sceptre-start.sh`.
- Modify: `images/recipes/README.md`: one paragraph on building and installing.

- [ ] **Step 1: Install and start the test.** Run: `images/ferrofire-rtu.sh`, then `px cfg create` the topology, `px exp create ffrt -t ferrofire-rtu-test` and `px exp start ffrt`.
- [ ] **Step 2: Verify through miniccc** (`mm namespace ffrt cc exec ...`, matching response lines exactly, not the echoed command):
  - `uname -m` is `aarch64`.
  - `/var/log/phenix-startup.log` shows the interface script and `sceptre-start.sh` ran.
  - `ip addr` shows the topology address.
  - `/var/log/ferrofire-watch.log` has `watch: started`.
  - Ports 502 and 20000 are listening (`netstat -ltn`).
  - The `ntp.conf` adapter did not error.
- [ ] **Step 3: Check persistence under phenix.** Push a modified firmware over SSH from inside the guest (`cc exec`), then `cc exec reboot`. After reboot, the firmware hash in the watch log is the pushed one.
- [ ] **Step 4: Tear down.** Stop and delete `ffrt`. Waterway is untouched (`px exp list`).
- [ ] **Step 5: Commit** in the range repo `feat(images): install ferrofire RTU image and isolated boot test`.

### Task 5: SCEPTRE app `firmware: ferrofire` branch

**Files (sceptre-phenix-apps, `src/python/phenix_apps/apps/sceptre/`):**
- Modify: `templates/sceptre_start.mako`:
  - Wrap the ssh `ListenAddress` / `service ssh restart` / `sleep 10s` preamble (lines 2-8) in `% if metadata.get('firmware', '').lower() != 'ferrofire':`.
  - Add, before `% elif name.lower() == 'field-device':` (line 37), the branch `% elif name.lower() == 'field-device' and metadata.get('firmware', '').lower() == 'ferrofire':` with body `/usr/sbin/ferrofire-rtu-start /etc/sceptre/config.xml`.
- Test: `tests/test_sceptre_start_ferrofire.py`. Render the template with `mako.template.Template(filename=...)` and `name='field-device', os='linux', ips=[{'address': '10.1.1.101', 'vlan': 'ICS'}], needrestart=False, needsleep=False`.

- [ ] **Step 1: Write the failing tests:**
  - `test_ferrofire_branch`: with `metadata={'firmware': 'ferrofire'}`, the output contains `/usr/sbin/ferrofire-rtu-start /etc/sceptre/config.xml` and contains neither `bennu-watcherd` nor `service ssh restart`.
  - `test_bennu_unchanged`: with `metadata={}`, the output contains `bennu-watcherd` and `service ssh restart`.
- [ ] **Step 2: Run and confirm failure.** Run: `make -C src/python test TEST=phenix_apps/apps/sceptre/tests`. Expected: `test_ferrofire_branch` FAILS.
- [ ] **Step 3: Edit the template** as above.
- [ ] **Step 4: Run and confirm everything passes.** Same command. Expected: all PASS, and `test_sceptre_golden.py` is unchanged (its input has no `firmware` key).
- [ ] **Step 5: Commit** `feat(sceptre): start ferrofire RTU images selected by firmware metadata`. The PR goes to sceptre-phenix-apps upstream, from a fork if the account cannot push there.

### Task 6: PEAT identify and pull over SSH and telnet

**Files (PEAT, a branch from `origin/main`, not the arm64 branch):**
- Create: `peat/modules/sandia/sceptre_shell.py`
- Modify: `peat/modules/sandia/sceptre_fcd.py` (options, IPMethods, `_pull`, device annotation)
- Test: `tests/modules/sandia/test_sceptre_shell.py`
- Create: `newsfragments/<PR>.feature`

**Interfaces:**
- Produces (in `sceptre_shell.py`):
  - `class SceptreShell` with `open(dev: DeviceData, protocol: Literal["ssh", "telnet"]) -> SceptreShell` (classmethod).
  - Methods: `list_home() -> list[str]`, `read_file(name: str) -> bytes`, `write_file(name: str, data: bytes) -> None`, `close() -> None`.
  - SSH uses `peat.protocols.SSH` and its `open_sftp()` / `sftp_conn` for reads and writes. Telnet uses `peat.protocols.Telnet`: log in at `login: ` / `Password: `, match the prompt with the regex `\$ $`, read with `base64 <file>`, write with the Task 7 rule.
  - Paths are relative to `dev.options["sceptre"]["home"]`, default `/home/sceptre`.
- Produces (in `sceptre_fcd.py`):
  - `default_options` gains `"ssh": {"user": "sceptre", "pass": "sceptre"}`, `"telnet": {"user": "sceptre", "pass": "sceptre"}`, `"sceptre": {..., "home": "/home/sceptre", "firmware_filename": "ferrofire.firmware"}`.
  - `_verify_ssh(cls, dev) -> bool` and `_verify_telnet(cls, dev) -> bool`: log in, then `list_home()` must contain `config.xml` and a `*.firmware` file, whose name is stored in `dev._runtime_options["sceptre"]["firmware_filename"]`.
  - IPMethods `"SCEPTRE SSH"` (port 22, reliability 8) and `"SCEPTRE Telnet"` (port 23, reliability 7).
  - `_pull` uses the first verified service among ftp, ssh and telnet. When the firmware filename starts with `ferrofire`, it sets the device's OS name to `Buildroot` and its model to `ferrofire` (use the `DeviceData` fields `annotate_fields` already sets).

- [ ] **Step 1: Write the failing tests**, using in-memory fakes. Fake the SSH SFTP as a dict of path to bytes. Fake telnet as a tiny shell that interprets `ls`, `base64 <f>`, `base64 -d > <f> <<'EOF'` and `mv`.
  - `test_ssh_identify_finds_firmware`: home `{config.xml, ferrofire.firmware}` gives True and records the filename.
  - `test_identify_rejects_without_files`: an empty home gives False, over both protocols.
  - `test_ssh_pull_round_trips`: pull returns the exact bytes of both files and parses the config (`examples/devices/sceptre/` XML).
  - `test_telnet_pull_round_trips_binary`: 1 MiB of random firmware bytes come back byte-exact.
  - `test_pull_annotates_ferrofire`: OS `Buildroot`, model `ferrofire`.
- [ ] **Step 2: Run and confirm failure.** Run: `pdm run pytest tests/modules/sandia/test_sceptre_shell.py`. Expected: FAIL (import error).
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run and confirm they pass.** Same command, then `pdm run test`. Expected: PASS, with the existing suite unchanged.
- [ ] **Step 5: Commit** `feat(sceptre): identify and pull SCEPTRE devices over SSH and telnet`.

### Task 7: PEAT push over SSH and telnet

**Files:** `peat/modules/sandia/sceptre_fcd.py`, `peat/modules/sandia/sceptre_shell.py`, `tests/modules/sandia/test_sceptre_shell.py`

**Interfaces:**
- Produces: `SCEPTRE._push(cls, dev: DeviceData, to_push: bytes, push_type: PushType) -> bool`. `"config"` writes `config.xml`; `"firmware"` writes the recorded firmware filename (default `ferrofire.firmware`). It returns False, with an error log, when neither SSH nor telnet is verified.
- Write rule (both protocols): write `<name>.tmp`, then rename onto `<name>`. On SFTP use `posix_rename`. On telnet, send base64 in 76-character lines into `base64 -d > <name>.tmp`, then `mv`.

- [ ] **Step 1: Write the failing tests:**
  - `test_push_writes_temp_then_renames`: the fake records operations `write <name>.tmp` then `rename`, never a direct write to `<name>` (SSH and telnet).
  - `test_push_firmware_round_trips`: push 1 MiB of random bytes, then pull them back byte-exact (both protocols).
  - `test_push_config`: `config.xml` content is replaced.
  - `test_push_without_shell_service_fails`: returns False.
- [ ] **Step 2: Run and confirm failure.** `pdm run pytest tests/modules/sandia/test_sceptre_shell.py`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement `_push`** and the write rule.
- [ ] **Step 4: Run and confirm they pass.** Same command, then `pdm run test`. Expected: PASS.
- [ ] **Step 5: Commit and open the PR.** Squash Tasks 6-7 into one commit, `feat(sceptre): identify, pull and push SCEPTRE devices over SSH and telnet`, then open the PR to sandialabs/PEAT and rename the newsfragment to the PR number. Track the separate bug found in `peat/api/push_api.py` (the `except` logs an undefined `dev`) as its own bead, not this PR.

### Task 8: kali-purple demo box with the PEAT branch

**Files (phenix-waterway-range):**
- Recreate `images/recipes/kali-purple-parseus-arm64.env` as before (the #50 configured script, plus an arm64 parseus bundle from the parseus#16 branch, staged with `DOCKER_PLATFORM=linux/arm64`).
- Modify `topologies/waterway-arm64/gen_topology.py`: a `--purple` flag (default off) adds VM `purple` with image `kali-purple-parseus-arm64.qc2`, 8 GiB, 4 vCPU, and three NICs: MGMT, ICS at `10.1.1.250/24`, and a capture NIC on vlan `MIRROR`. This uses the AAVMF/virt settings of the existing kali-purple nodes.

- [ ] **Step 1: Build the image.** Run: `images/build.sh kali-purple-parseus-arm64`. Expected: the qc2 exists, and the build log shows `PEAT 2026.2.11`.
- [ ] **Step 2: Build a PEAT wheel** from the Task 6-7 branch (`pdm build`), to inject into `purple`.
- [ ] **Step 3: Check the generator.** Run `gen_topology.py` with and without `--purple`. Expected: without the flag, the output is byte-identical to today's `waterway-topology.yaml`.
- [ ] **Step 4: Commit** `feat(waterway): optional kali-purple box with PEAT on the ICS vlan`.

### Task 9: Swap waterway's RTUs and run the demo

**Files (phenix-waterway-range):**
- Modify `topologies/waterway-arm64/gen_topology.py`: a `--ferrofire-rtu` flag (default off) sets the `rtu-1`/`rtu-2` image to `ferrofire-rtu-arm64.qc2` and drops the `ferrofire` and shim injections.
- Modify `gen_scenario.py`: the same flag adds `firmware: ferrofire` to both RTUs' metadata.
- Create `docs/ferrofire-rtu-demo.md`: commands and captured output for the four demo steps.

- [ ] **Step 1: Check generator parity.** Without the flags, the generated topology and scenario are byte-identical to today's. With both flags, the diff shows only the image, injection and metadata changes.
- [ ] **Step 2: Check the range is free.** Run `px exp list`, and check open range beads for other sessions' use of `waterway`. Ask the user before stopping or restarting it.
- [ ] **Step 3: Start the demo waterway** with `--ferrofire-rtu --purple` (Task 5's sceptre-phenix-apps branch installed in the phenix container). Install the PEAT wheel on `purple` (`cc exec /opt/peat/bin/pip install --no-deps --force-reinstall <wheel>`). Run `scripts/mirror.sh waterway purple 2`.
- [ ] **Step 4: Swap.** SCADA/HMI values update as with bennu RTUs, and the provider is unchanged.
- [ ] **Step 5: Pull.** On `purple`, run `peat scan -i 10.1.1.101-102` and `peat pull -d sceptre -i 10.1.1.101`. Expected: identified over SSH, with the device name, logic, tags and endpoints parsed, and model `ferrofire`.
- [ ] **Step 6: Modify and push.** Push in two ways:
  - (a) a `config.xml` with one changed `<logic>` constant (`peat push -d sceptre -t config`);
  - (b) a modified firmware: a `#!/bin/sh` script that rewrites that constant in a copy of the config and execs `/usr/bin/ferrofire field-device --file <copy>` (`peat push -d sceptre -t firmware`).
  Each gives a new `watch: firmware sha256=` or `restart: config changed` line. Then `cc exec reboot` rtu-1, and the pushed state is still active.
- [ ] **Step 7: See it.** On `purple`, parseus shows the affected rtu-1 register changing to the new logic, from mirrored traffic captured and loaded into parseus (capture on the MIRROR NIC; load the pcap through parseus's pcap ingest).
- [ ] **Step 8: Record the evidence** in `docs/ferrofire-rtu-demo.md`, restore the default waterway (flags off) if other sessions need it, and commit `docs(waterway): ferrofire RTU PEAT pull/push demo`.
