#!/bin/sh
# Put the glibc aarch64 binary (make linux -> dist/ferrofire) into the overlay tree. The binary is a build product, not committed.
#   build-overlay.sh        zmq image:  CARGO_FLAGS= (or "--no-default-features --features zmq") ./build-linux.sh first
#   build-overlay.sh nng    nng image:  CARGO_FLAGS="--no-default-features --features nng" ./build-linux.sh first
# The build is checked against the variant: the nng image's binary must not link libzmq, the zmq image's must.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/../dist/ferrofire"
test -x "$bin" || { echo "run 'make linux' first" >&2; exit 1; }
needs_zmq=0
if command -v readelf >/dev/null 2>&1; then
	readelf -d "$bin" | grep -q 'libzmq' && needs_zmq=1
else
	docker run --rm --platform linux/arm64 -v "$here/..":/src ff-rust:22.04 sh -c 'readelf -d /src/dist/ferrofire | grep -q libzmq' && needs_zmq=1 || true
fi
case "${1:-zmq}" in
nng)
	[ "$needs_zmq" = 0 ] || { echo "dist/ferrofire links libzmq: rebuild with CARGO_FLAGS='--no-default-features --features nng'" >&2; exit 1; }
	dest=ferrofire-nng ;;
zmq)
	[ "$needs_zmq" = 1 ] || { echo "dist/ferrofire has no zmq: rebuild without CARGO_FLAGS (or --features zmq)" >&2; exit 1; }
	dest=ferrofire ;;
*) echo "usage: build-overlay.sh [zmq|nng]" >&2; exit 2 ;;
esac
mkdir -p "$here/overlays/$dest/usr/local/bin"
install -m 0755 "$bin" "$here/overlays/$dest/usr/local/bin/ferrofire"
