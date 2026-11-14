#!/bin/sh
# $1 = BINARIES_DIR. GPT disk (genimage.cfg), then the qcow2 that phenix boots.
set -eu
BOARD_DIR=$(cd "$(dirname "$0")" && pwd)
OUT="$BOARD_DIR/../../out"
cp -f "$BOARD_DIR/grub.cfg" "$1/efi-part/EFI/BOOT/grub.cfg"
support/scripts/genimage.sh -c "$BOARD_DIR/genimage.cfg"
mkdir -p "$OUT"
qemu-img convert -O qcow2 "$1/disk.img" "$OUT/ferrofire-rtu-arm64.qc2"
