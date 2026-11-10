#!/bin/sh
# Put the glibc aarch64 binary (make linux -> dist/ferrofire) into the overlay tree. The binary is a build product, not committed.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/../dist/ferrofire"
test -x "$bin" || { echo "run 'make linux' first" >&2; exit 1; }
mkdir -p "$here/overlays/ferrofire/usr/local/bin"
install -m 0755 "$bin" "$here/overlays/ferrofire/usr/local/bin/ferrofire"
