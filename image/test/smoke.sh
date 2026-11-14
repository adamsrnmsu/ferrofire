#!/usr/bin/env bash
# Boot image/out/ferrofire-rtu-arm64.qc2 in QEMU (-M virt -cpu max, AAVMF UEFI, virtio disk/net, user-net
# hostfwd 2222->22 and 2323->23) and check it over SSH/telnet. Each check prints `ok <name>` or exits 1.
# The host needs only docker: the script re-runs itself in the ff-buildroot container (TCG, no KVM there).
# The shipped qc2 is never written: the VM boots a raw copy.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
if [ -z "${IN_SMOKE:-}" ]; then
  docker build -q -t ff-buildroot:bookworm "$repo/image" >/dev/null
  exec docker run --rm -e IN_SMOKE=1 -v "$repo":/src -w /src ff-buildroot:bookworm image/test/smoke.sh
fi

qc2=/src/image/out/ferrofire-rtu-arm64.qc2
tmp=$(mktemp -d)
serial=$tmp/serial.log
fail() { echo "FAIL $1"; echo "--- serial log tail"; tail -n 40 "$serial" 2>/dev/null; exit 1; }
[ -f "$qc2" ] || fail "ssh_login: no image at $qc2 (make rtu-image)"

# The disk under test is a raw copy, so the guest's /data survives the reboots below and partition 2 can be
# written offline with debugfs, the way phenix injects (partition 2, as root, run by S20phenix at boot).
raw=$tmp/disk.raw
qemu-img convert -O raw "$qc2" "$raw"
off=$(( 512 * $(sfdisk -d "$raw" | sed -n '/2 : start=/ s/.*start= *\([0-9]*\),.*/\1/p') ))
# inject GUEST_PATH:CONTENT_FILE... (replaces any existing file)
inject() {
  : >"$tmp/dbg"
  for a in "$@"; do printf 'rm %s\nwrite %s %s\n' "${a%%:*}" "${a#*:}" "${a%%:*}" >>"$tmp/dbg"; done
  debugfs -w -f "$tmp/dbg" "$raw?offset=$off" >/dev/null 2>&1 || true
  for a in "$@"; do
    debugfs -R "cat ${a%%:*}" "$raw?offset=$off" 2>/dev/null | cmp -s - "${a#*:}" || fail "inject ${a%%:*}"
  done
}

opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5)
r() { sshpass -p sceptre ssh "${opts[@]}" -p 2222 sceptre@127.0.0.1 "$@"; }

cp /usr/share/AAVMF/AAVMF_VARS.fd "$tmp/vars.fd"
boot() {
  qemu-system-aarch64 -M virt -cpu max,pauth-impdef=on -smp 2 -m 1024 -display none -monitor none \
    -drive if=pflash,format=raw,readonly=on,file=/usr/share/AAVMF/AAVMF_CODE.fd \
    -drive if=pflash,format=raw,file="$tmp/vars.fd" \
    -drive if=none,id=hd0,format=raw,file="$raw" -device virtio-blk-pci,drive=hd0 \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:2222-:22,hostfwd=tcp:127.0.0.1:2323-:23 -device virtio-net-pci,netdev=n0,romfile= \
    -netdev user,id=n1 -device e1000,netdev=n1,romfile= \
    -serial file:"$serial" &
  qemu=$!
  for _ in $(seq 120); do r true 2>/dev/null && return; kill -0 $qemu 2>/dev/null || fail "ssh_login: qemu exited"; sleep 3; done
  fail ssh_login
}
stop() { r sync; kill $qemu; wait $qemu 2>/dev/null || true; }
trap 'kill $qemu 2>/dev/null || true; rm -rf "$tmp"' EXIT

cp /src/tests/conformance/rtu-1.xml "$tmp/factory.xml"
sed 's/<name>rtu-1<\/name>/<!-- injected -->&/' "$tmp/factory.xml" >"$tmp/injected.xml"
echo '/usr/sbin/ferrofire-rtu-start /etc/sceptre/config.xml' >"$tmp/start.sh"
# miniccc_path stub: what `phenix image inject-miniexe` puts at /usr/local/bin/miniccc (removed after the check).
printf '#!/bin/sh\necho "$@" >/run/miniccc.args\nexec sleep 100000\n' >"$tmp/miniccc"
chmod +x "$tmp/miniccc"
# The image has no /usr/local; phenix's injector creates it, so do the same.
debugfs -w -R "mkdir /usr/local" "$raw?offset=$off" >/dev/null 2>&1 || true
debugfs -w -R "mkdir /usr/local/bin" "$raw?offset=$off" >/dev/null 2>&1 || true
# phenix owns all addressing (its interfaces script runs from /etc/phenix/startup): the image runs no DHCP client,
# so give the guest the user-net address the way phenix would.
printf 'ip link set eth0 up\nip addr add 10.0.2.15/24 dev eth0\nip route add default via 10.0.2.2\n' >"$tmp/net.sh"
inject /etc/sceptre/config.xml:"$tmp/factory.xml" /etc/phenix/startup/00-net.sh:"$tmp/net.sh" /etc/phenix/startup/sceptre-start.sh:"$tmp/start.sh" /usr/local/bin/miniccc:"$tmp/miniccc"
boot
# ssh_login: wait for a real login (QEMU's hostfwd accepts TCP before the guest listens).
r true || fail ssh_login
echo "ok ssh_login"

# Nothing at boot (S40network's udhcpc deconfig) may undo the address phenix set.
sleep 10
[ "$(r "ip -o -4 addr show dev eth0 | awk '{print \$4}'")" = 10.0.2.15/24 ] && ! r 'ps | grep -q "[u]dhcpc"' ||
  { r 'ip -o -4 addr; ps'; fail static_ip_kept; }
echo "ok static_ip_kept"

r "awk '\$2==\"/\" {o=\$4} END {exit o !~ /^ro(,|\$)/}' /proc/mounts" || fail root_ro
echo "ok root_ro"

# EROFS, not just EACCES: sceptre could never write /etc, so insist on the read-only error.
r 'touch /etc/x 2>&1; test ! -e /etc/x' | grep -q 'Read-only file system' || fail etc_not_writable
echo "ok etc_not_writable"

r 'touch /home/sceptre/x && rm /home/sceptre/x' || fail data_rw
echo "ok data_rw"

# /home/sceptre is the bind of /data/sceptre: same device as /data, and a file shows up on both paths.
r "awk '\$2==\"/data\" {d=\$1} \$2==\"/home/sceptre\" {h=\$1} END {exit !(d != \"\" && d == h)}' /proc/mounts &&
   touch /home/sceptre/y && test -e /data/sceptre/y && rm /home/sceptre/y" || fail home_is_data
echo "ok home_is_data"

# A bare connect would pass with no telnetd behind hostfwd: require the login prompt.
timeout 10 bash -c 'exec 3<>/dev/tcp/127.0.0.1/2323; timeout 5 cat <&3' >"$tmp/telnet" 2>/dev/null || true
grep -aq 'login:' "$tmp/telnet" || fail telnet_open
echo "ok telnet_open"

# Telnet login transcript: user, password (an empty one is a bare Enter), then exit.
tn() {
  # shellcheck disable=SC2016 # $1/$2 expand in the inner bash
  timeout 20 bash -c 'exec 3<>/dev/tcp/127.0.0.1/2323; sleep 2; printf "%s\r\n" "$1" >&3; sleep 2
    printf "%s\r\n" "$2" >&3; sleep 4; printf "exit\r\n" >&3; timeout 2 cat <&3' _ "$1" "$2" 2>/dev/null || true
}
# The positive control keeps root_telnet_denied honest, and pins the `$ ` prompt PEAT's telnet client expects.
tn sceptre sceptre | grep -aq '\$ ' || fail telnet_login
echo "ok telnet_login"

# Root has an empty password, so login gives it a `# ` shell without asking for one unless /etc/securetty
# (console, ttyAMA0 only) refuses root on the telnet pty first.
tn root '' >"$tmp/root-telnet"
{ grep -aq 'Login incorrect' "$tmp/root-telnet" && ! grep -aq '# ' "$tmp/root-telnet"; } ||
  { cat -v "$tmp/root-telnet"; fail root_telnet_denied; }
echo "ok root_telnet_denied"

# An empty root password is refused by sshd's PermitEmptyPasswords default anyway, so also pin the config.
{ ! sshpass -p '' ssh "${opts[@]}" -p 2222 root@127.0.0.1 true 2>/dev/null &&
  r 'grep -qx "PermitRootLogin no" /etc/ssh/sshd_config'; } || fail root_ssh_denied
echo "ok root_ssh_denied"

# phenix assigns addresses by link order: the non-lo links must be exactly the two NICs, with no dummy0/sit0/etc.
[ "$(r "ip -o link | awk -F': ' '\$2 != \"lo\" {sub(/@.*/, \"\", \$2); printf \"%s \", \$2}'")" = "eth0 eth1 " ] ||
  { r 'ip -o link'; fail only_nic_links; }
echo "ok only_nic_links"

# phenix's default NIC model is e1000: some interface must be bound to that driver.
r 'for d in /sys/class/net/*/device/driver; do [ "$(basename "$(readlink "$d")")" = e1000 ] && exit 0; done; exit 1' || fail e1000_driver
echo "ok e1000_driver"

# S30miniccc runs the injected /usr/local/bin/miniccc with the cc serial port.
r 'for _ in $(seq 20); do grep -q -- "-serial /dev/virtio-ports/cc" /run/miniccc.args 2>/dev/null && exit 0; sleep 0.5; done; exit 1' || fail miniccc_path
echo "ok miniccc_path"

r 'ferrofire --version' >/dev/null || fail ferrofire_runs
echo "ok ferrofire_runs"

head -c 1048576 /dev/urandom >"$tmp/put.bin"
printf 'put %s sftp.bin\nget sftp.bin %s\nrm sftp.bin\n' "$tmp/put.bin" "$tmp/get.bin" >"$tmp/batch"
sshpass -p sceptre sftp "${opts[@]}" -o BatchMode=no -P 2222 -b "$tmp/batch" sceptre@127.0.0.1 >/dev/null || fail sftp_works
[ "$(sha256sum <"$tmp/put.bin")" = "$(sha256sum <"$tmp/get.bin")" ] || fail sftp_works
echo "ok sftp_works"

# RTU: S20phenix ran sceptre-start.sh at boot, which seeded /home/sceptre and started the watcher.
r 'cmp /usr/bin/ferrofire /home/sceptre/ferrofire.firmware && test -f /home/sceptre/config.xml' || { r 'cat /var/log/phenix-startup.log'; fail rtu_seeds; }
echo "ok rtu_seeds"

# nc exits 0 on connect (stdin is at EOF); the watcher may still be starting, so poll.
r 'for _ in $(seq 20); do nc 127.0.0.1 5502 </dev/null && exit 0; sleep 0.5; done; exit 1' || fail rtu_serves
echo "ok rtu_serves"

# Push as sceptre the way an operator would: upload to .tmp, rename into place, then replace the config.
printf '#!/bin/sh\n# pushed\nexec /usr/bin/ferrofire "$@"\n' >"$tmp/pushed.fw"
sed 's/<name>rtu-1<\/name>/<!-- pushed -->&/' "$tmp/factory.xml" >"$tmp/pushed.xml"
r 'grep -c "watch: firmware sha256=" /var/log/ferrofire-watch.log' >"$tmp/n0"
r 'cat >ferrofire.firmware.tmp && mv ferrofire.firmware.tmp /home/sceptre/ferrofire.firmware' <"$tmp/pushed.fw"
r 'cat >config.xml.tmp && mv config.xml.tmp /home/sceptre/config.xml' <"$tmp/pushed.xml"
r "for _ in \$(seq 20); do [ \$(grep -c 'watch: firmware sha256=' /var/log/ferrofire-watch.log) -gt $(cat "$tmp/n0") ] && exit 0; sleep 1; done; exit 1" ||
  fail "rtu_push_persists: watcher did not see the new firmware"
stop
inject /etc/sceptre/config.xml:"$tmp/injected.xml"
boot
want=$(sha256sum <"$tmp/pushed.fw" | cut -d' ' -f1)
r "cd /home/sceptre && [ \$(sha256sum <ferrofire.firmware | cut -d' ' -f1) = $want ] && grep -q 'pushed' config.xml && ! grep -q injected config.xml &&
   grep -q 'watch: firmware sha256=$want' /var/log/ferrofire-watch.log" || fail rtu_push_persists
r 'for _ in $(seq 20); do nc 127.0.0.1 5502 </dev/null && exit 0; sleep 0.5; done; exit 1' || fail "rtu_push_persists: pushed firmware does not serve"
echo "ok rtu_push_persists"

# A second startup script runs the same line: still exactly one watcher.
stop
inject /etc/phenix/startup/sceptre-start2.sh:"$tmp/start.sh"
boot
sleep 3
r 'grep -q sceptre-start2.sh /var/log/phenix-startup.log' || fail "rtu_start_idempotent: start2 never ran"
[ "$(r "ps | grep -c '[f]errofire watch'")" = 1 ] || fail rtu_start_idempotent
echo "ok rtu_start_idempotent"

# Remove the miniccc stub again (the disk is a throwaway copy, but leave it as found).
stop
debugfs -w -R "rm /usr/local/bin/miniccc" "$raw?offset=$off" >/dev/null 2>&1 || true
debugfs -R "stat /usr/local/bin/miniccc" "$raw?offset=$off" 2>&1 | grep -q 'not found' || fail "miniccc_path: stub not removed"
