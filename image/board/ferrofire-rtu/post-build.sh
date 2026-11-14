#!/bin/sh
# $1 = TARGET_DIR. Install the factory ferrofire (make linux) and finish the read-only root layout.
set -eu
BOARD_DIR=$(dirname "$0")
install -m 0755 "$BOARD_DIR/../../../dist/ferrofire" "$1/usr/bin/ferrofire"
# Mount points: the skeleton's /var/log -> ../tmp symlink becomes its own tmpfs (etc/fstab); /data (S01data).
[ ! -L "$1/var/log" ] || rm "$1/var/log"
mkdir -p "$1/var/log" "$1/data"
# BusyBox's own telnetd script; ours is S51telnetd.
rm -f "$1/etc/init.d/S50telnet"
