#!/bin/sh
# Put the glibc aarch64 binary (make linux -> dist/rustbelt) into the overlay tree. The binary is a build product, not committed.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
test -x "$here/../dist/rustbelt" || { echo "run 'make linux' first" >&2; exit 1; }
install -m 0755 "$here/../dist/rustbelt" "$here/overlays/rustbelt/usr/local/bin/rustbelt"
