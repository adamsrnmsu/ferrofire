# ferrofire field-device image: design

Date: 2026-11-13. Status: draft, builder-checked, awaiting approval.

## Intent

The waterway range's RTUs are bennu VMs on a full Debian/Kali-style image. We want our own image for the
ferrofire field device that looks and behaves more like real field equipment: small embedded firmware, a
read-only root, BusyBox, nothing extra installed.

The main reason is instruction with PEAT. An RTU on this image is deliberately vulnerable: anyone with its
login can pull its configuration and firmware and push replacement firmware. That gives PEAT's pull and push
something real to work against, and the effect of a push can be watched from the blue side in parseus. Real
devices vary on this; this one is vulnerable on purpose.

The image must run in the range the way bennu RTUs do today: phenix injects its config, phenix's SCEPTRE app
(sceptre-phenix-apps) drives it, and miniccc is available for orchestration (`cc exec`).

## Scope: two variants on one base

- **Shared base** (this design): the Buildroot image, phenix integration, persistence.
- **Variant 1, the SCEPTRE RTU** (this design): the experiment workhorse. Simple logic, normal shell.
- **Variant 2, the programmable PLC** (later, its own design): for teaching real PLC programming. Recorded
  at the end with what is decided so far; not designed here.
- **Variant 3, the automation controller** (later, its own design): an RTAC-class box with a lot of compute,
  on NixOS rather than the shared Buildroot base. Recorded at the end; not designed here.

## Goals (variant 1)

Done means this demo works on the waterway range:

1. **Swap**: `rtu-1` and `rtu-2` move from `bennu-arm64.qc2` to the variant 1 image. The provider, OPC, SCADA,
   HMI, historian and engineer workstation are unchanged, and the RTUs serve the same tags over the same
   protocols (Modbus and DNP3 today).
2. **Pull**: from a kali-purple VM in the experiment (the arm64 kali-purple image with PEAT and parseus,
   sandialabs/sceptre-phenix-images#50), PEAT identifies each RTU and pulls its `config.xml` and firmware
   over SSH and/or telnet, and parses device name, logic, tags and endpoints as it does for bennu today.
3. **Modify and push**: a modified firmware or `config.xml` logic is pushed back with PEAT over SSH and/or
   telnet. The device accepts it, restarts the field device on it, and the change survives a reboot of
   the VM.
4. **See it**: parseus on the kali-purple VM shows the change in that RTU's register data.

The waterway topology does not have a kali-purple VM today; adding one (to waterway or a waterway variant)
is part of the demo work.

## Non-goals

- Variant 2 (designed separately, after variant 1).
- x86_64 images now. The build config should not rule them out (same config, other arch, later).
- Changing the provider, the control-system VMs, or how SCADA/HMI talk to the RTUs.
- Adding arm64 or Buildroot support to phenix's own image builder.

## Anti-goals (fails even if the demo works)

- The live waterway experiment is broken or blocked for other sessions during development. All testing
  happens in an isolated experiment until the swap demo, and the swap is coordinated.
- The root filesystem is writable at runtime.
- PEAT needs anything beyond a new SCEPTRE pull/push capability to work with it (no special-casing hacks,
  no manual FTP client steps standing in for PEAT).

## Decided

These were decided by the user:

- **Base OS**: Buildroot, BusyBox init (not systemd), read-only root filesystem.
- **Variant 1 access**: telnet and/or SSH, landing in a **normal shell** (BusyBox `ash` is fine; brash is not
  required for variant 1).
- **Variant 1 logic**: the simple SCEPTRE-style `<logic>` block in `config.xml`.
- **Firmware persistence**: a pushed firmware persists across reboots. The device is deliberately granted a
  vulnerability that lets firmware be pulled and pushed.
- **Any pushed firmware is accepted**: no hash allow-list (unlike bennu's watcher, which accepts only the
  stock and "shell" builds). A genuinely modified firmware runs, which is what demo step 3 pushes.
- **Writable storage**: some part of the filesystem must be writable and persistent to allow this. Its exact
  shape is left open for now (see "Left to the builder").
- **miniccc** stays in for orchestration, and the image must work under phenix's SCEPTRE app.
- **Success** is the four-step demo above.

## Calls made in this design (not yet confirmed by the user)

- **arm64 first**, matching the range.
- **Code lives in the ferrofire repo** (Buildroot external tree / config and overlay next to the binary it
  packages), plus a recipe in phenix-waterway-range to build and test it.
- **Variant 1 before variant 2**, with mixed bennu and ferrofire RTUs allowed in one experiment.
- **FTP is not offered** on variant 1 (access is SSH/telnet). PEAT's existing FTP path stays for bennu RTUs.
- **PEAT changes go upstream**: SCEPTRE identify/pull/push over SSH and telnet added to PEAT's
  `sandia/sceptre_fcd.py` (today it identifies and pulls over FTP only and has no push), as a PR to
  sandialabs/PEAT.
- **SCEPTRE app support goes upstream** as a PR to sceptre-phenix-apps (see constraints): the field-device
  startup it generates today cannot run on this image.
- **Range reset**: a fresh experiment start from phenix's snapshot brings back factory firmware; persistence
  only has to hold across reboots within one experiment run.
- **Credentials**: lab defaults in the style of bennu (`sceptre`/`sceptre`), documented, not secret.
- **Firmware is the ferrofire executable** that runs the field device (`ferrofire field-device --file
  /etc/sceptre/config.xml`), stored in the persistent area under its own name, `ferrofire.firmware` (user:
  "leave our mark"; the name itself is my call). PEAT's SCEPTRE module already treats any `*.firmware` file
  in the listing as the firmware; the PEAT PR makes it recognize ferrofire explicitly and report the device
  as ferrofire on Buildroot instead of bennu on Ubuntu.
- **PEAT/device contract** (one spec both PRs follow): user `sceptre`, files at fixed paths in its home
  (`/home/sceptre/config.xml`, `/home/sceptre/ferrofire.firmware`), the same paths over SSH and
  telnet. Identify by logging in and finding those files (the SSH/telnet analogue of today's FTP listing
  check), not by a banner alone. The `sceptre` user gets a normal unprivileged shell.
- **Boot order of state**: phenix runs `/etc/phenix/startup/*` on every boot, so the ferrofire startup seeds
  the persistent area from the factory copies on the read-only root only when it is empty, then runs the
  current firmware and config. A pushed firmware or config is never overwritten by a reboot.
- **"Reboot" means a guest reboot** (or a minimega VM restart inside the same running experiment, which keeps
  the VM's disk). Stopping and starting the experiment makes a new snapshot and reverts to factory.
- **Field-device parity first**: before the image work, confirm ferrofire's field device serves waterway's
  `rtu-1`/`rtu-2` configs (Modbus and DNP3, same tags, provider endpoints from `config.xml`) as stock bennu
  does; any gap is the first task.
- **Demo PEAT**: the demo may run a PEAT built from our branch installed on the kali-purple VM while the
  upstream PR is open. The kali-purple arm64 image is rebuilt from #50's script for the demo (the earlier
  test image was deleted) and added to the waterway topology.

## Constraints

### phenix

- The image is a qcow2 that phenix can inject into: a root filesystem partition phenix can mount and write
  before boot (`inject_partition`). Read-only applies at runtime; offline injection still writes the root.
- phenix and its apps inject at least: `/etc/phenix/startup/*` scripts (phenix's interface setup, the SCEPTRE
  app's `sceptre-start.sh`), `/etc/sceptre/config.xml`, `/etc/ntp.conf`, hostname data. Something must run
  `/etc/phenix/startup/*` in order at boot. phenix's generated scripts use `ip`, so iproute2 (or a compatible
  `ip`) is required; current images run them with bash, so either bash is present or each injected script is
  confirmed to run under `ash`.
- miniccc runs at boot over the virtio-serial `cc` port (`/opt/minimega/bin/miniccc -serial
  /dev/virtio-ports/cc`), injected with `phenix image inject-miniexe` as for other images.
- phenix's `ntp` app writes `/etc/ntp.conf`; something must use it (BusyBox ntpd reads its own options, so a
  small adapter or a compatible daemon is needed).
- arm64 VMs in the range boot UEFI (AAVMF) on `virt` with virtio disk and network, as `bennu-arm64` does.
- phenix's image builder (vmdb2/debootstrap) cannot build this image; Buildroot produces the qcow2 and the
  range copies it to `/phenix/images`.

### SCEPTRE app (sceptre-phenix-apps)

For a node named `field-device`, `templates/sceptre_start.mako` generates a startup script that:

- writes `/etc/sceptre/watcher.ini` with hashes of `/usr/bin/bennu-field-deviced`, including a shell-variant
  hash computed with `python3` and `sed`;
- copies the binary and `config.xml` into `/home/sceptre/` and runs `bennu-watcherd restart`;
- also runs `service ssh restart` and other Debian assumptions.

None of `python3`, `service`, `bennu-watcherd` or bennu binaries exist on this image. The app needs a
ferrofire field-device path (selected by image or metadata) that generates a startup script this image can
run. The `config.xml` it injects is unchanged: ferrofire's field device already reads bennu's config.

### PEAT

- PEAT's SSH support is paramiko (interactive session; paramiko also offers SFTP). Its telnet support is
  command/response. The device must allow pull and push over both in a way PEAT can drive: over SSH, SFTP or
  scp; over telnet, shell-only transfer (for example base64 through the shell).
- PEAT must still identify the device as a SCEPTRE RTU and parse `config.xml` as it does today.

### Firmware and watcher

- Pushed firmware lands in the persistent writable area; the device starts the field device from the
  current firmware at boot.
- A watcher notices a new firmware (any content; bennu's C++ watcher polls every 10 ms but only accepts
  allow-listed hashes, which this design drops), restarts the field device on it, and logs the firmware
  hash. The SCEPTRE app's `watcher.ini` hash generation is not needed on this image.
- `config.xml` changes (logic) must also take effect when pushed; how (watch, restart command, reboot) is
  the builder's call, but step 3 of the demo must show it.

## Left to the builder

- The writable persistent area: separate partition, overlay on a data partition, or other, as long as root
  stays read-only and a pushed firmware is what boots next.
- Buildroot toolchain (glibc or musl) and how ferrofire is built for it.
- SSH server (Dropbear or OpenSSH) and how SFTP/scp is provided.
- Whether the watcher is part of the ferrofire binary or a separate small tool.
- What happens when a pushed firmware fails to start (stay down like a bricked device, or fall back to the
  factory copy); either way it is logged.
- The exact PEAT pull/push commands and file paths over SSH and telnet.
- How the range builds and installs the image (recipe and script shape).

## Testing

- Boot and phenix integration in an isolated experiment (own namespace, no gateway): `cc exec` works, injected
  startup scripts run, the field device serves Modbus and DNP3 from an injected `config.xml`.
- PEAT identify/pull/push against the isolated RTU before the swap.
- Then the four-step demo on waterway, coordinated with other users of the range.

## Variant 2: the programmable PLC (recorded, not designed)

- For teaching real PLC programming; a separate use case from variant 1.
- Access lands in a **device console** (vendor-style restricted commands, like bennu's brash), not a shell.
- From PEAT's side it should look like OpenPLC Runtime v4 (HTTPS JSON API on 8443: login, status, users,
  logs, compile status, and a program upload that PEAT's `openplcv4` module already pushes).
- Open: how IEC 61131-3 programs run on a small read-only device (interpret, compile off-device, or run
  OpenPLC's runtime), and how it relates to ferrofire's field device.

## Variant 3: the automation controller (recorded, not designed)

- User: instead of a small field device, something that handles a large amount of compute, like an RTAC
  (real-time automation controller).
- Guesses to confirm when it is designed: a substation/site controller that polls many field devices as a
  client (DNP3, Modbus, IEC 61850) and serves the aggregate upstream to SCADA, runs heavier logic, and gets a
  bigger VM (more vCPU and RAM) than an RTU.
- **Base OS: NixOS** (user decision), not the Buildroot base. Variants 1 and 2 stay on Buildroot. Reasons
  recorded at the time: an RTAC-class box is a full appliance (database, web front end, many protocol
  clients, all supervised), which NixOS declares in one reproducible config; the read-only Nix store plus a
  root reset to declared state on boot (with one persistent data area) matches the firmware/factory-reset
  model; one config builds aarch64 and x86_64 qcow2s; miniccc and the `/etc/phenix/startup` runner are plain
  systemd units. Costs accepted: a second image toolchain, Nix builds in the Lima VM, larger images.
- PEAT has an SEL RTAC module (`sel/sel_rtac.py`: HTTPS 443 and PostgreSQL 5432, configuration as XML pages),
  so there is prior art for what PEAT pulls from this class of device. Whether variant 3 should look like that
  to PEAT, or present as ferrofire with its own PEAT support, is open.
- ferrofire's field device lists client modes as next on its roadmap; variant 3 depends on them.
