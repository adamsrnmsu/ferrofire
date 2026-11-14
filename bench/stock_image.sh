#!/bin/sh
# Build the bench's stock image from a committed sceptre-bennu ref (ww-1df.47): sh bench/stock_image.sh [REF=v2026.11.03] [TAG=ffc-stock-2026.11.03:22.04]
# Same recipe as ffc-stock:22.04 (bennu 6.0.0 = the phenix bennu-arm64 overlay): docker build of bench/Dockerfile.stock-build from `git archive REF`, copy /usr/local out,
# rebuild pybennu's vendored x86 swig _Endpoint.so for aarch64 (as phenix images/build_endpoint_so.sh), drop _Endpoint.pyd, then bench/Dockerfile.stock on top. The old image is kept.
# Heavy (make -j2; ~1 h on the 7.7 GB docker VM): check `docker ps` first. Scratch in $WORK (default /tmp/ffstock).
set -ex
REF=${1:-v2026.11.03}; TAG=${2:-ffc-stock-2026.11.03:22.04}; WORK=${WORK:-/tmp/ffstock}
BENNU=${BENNU:-$HOME/git/sceptre-bennu}; HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$WORK/src" "$WORK/ctx/local" && rm -rf "$WORK/src" && mkdir "$WORK/src"
git -C "$BENNU" archive "$REF" | tar -x -C "$WORK/src"   # committed refs only: the working tree has uncommitted WIP
docker build --platform linux/arm64 -f "$HERE/Dockerfile.stock-build" -t "bennu-build:$REF-arm64" "$WORK/src"
id=$(docker create --platform linux/arm64 "bennu-build:$REF-arm64")
rm -rf "$WORK/ctx/local" && docker cp "$id:/usr/local" "$WORK/ctx/local" && docker rm "$id"
docker run --rm --platform linux/arm64 -v "$WORK/ctx/local/lib/python3.10/dist-packages/pybennu/distributed/swig:/out" "bennu-build:$REF-arm64" sh -ec \
  'apt-get update && apt-get install -y --no-install-recommends swig && cd /tmp && cp /usr/local/lib/python3.10/dist-packages/pybennu/distributed/swig/Endpoint.i . && swig -c++ -python Endpoint.i && g++ -O2 -fPIC -shared Endpoint_wrap.cxx -I/usr/include/python3.10 -o /out/_Endpoint.so'
rm -f "$WORK/ctx/local/lib/python3.10/dist-packages/pybennu/distributed/swig/_Endpoint.pyd"
cp "$HERE/Dockerfile.stock" "$WORK/ctx/Dockerfile"
docker build --platform linux/arm64 -t "$TAG" "$WORK/ctx"
