#!/usr/bin/env bash
# Build image/out/ferrofire-rtu-arm64.qc2 with Buildroot (BUILDROOT_VERSION) in a Debian container, native arm64
# on Apple silicon. The tarball must match BUILDROOT_VERSION.sha256 (from buildroot.org's .sign file).
# Buildroot's source, downloads, ccache and output tree live in the ff-buildroot volume, not the bind mount
# (macOS is case-insensitive and slow), so rebuilds are incremental. A package change in the defconfig
# may need a clean output tree: docker run --rm -v ff-buildroot:/br debian:bookworm rm -rf /br/output
# Needs dist/ferrofire (make linux). JOBS caps parallelism (Docker Desktop memory).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"
br="$(cat "$here/BUILDROOT_VERSION")"
test -x "$repo/dist/ferrofire" || { echo "missing dist/ferrofire: run make linux" >&2; exit 1; }
docker build -q -t ff-buildroot:bookworm "$here" >/dev/null
docker run --rm -v ff-buildroot:/br -v "$repo":/src -e BR="$br" -e JOBS="${JOBS:-8}" \
  -e FORCE_UNSAFE_CONFIGURE=1 -e BR2_DL_DIR=/br/dl -e BR2_CCACHE_DIR=/br/ccache ff-buildroot:bookworm bash -euc '
  src=/br/buildroot-$BR
  if [ ! -d "$src" ]; then
    wget -q -O "/tmp/buildroot-$BR.tar.gz" "https://buildroot.org/downloads/buildroot-$BR.tar.gz"
    (cd /tmp && sha256sum -c /src/image/BUILDROOT_VERSION.sha256)
    x=$(mktemp -d /br/extract.XXXXXX)
    tar -C "$x" -xzf "/tmp/buildroot-$BR.tar.gz"
    mv "$x/buildroot-$BR" "$src" && rmdir "$x"
  fi
  make -C "$src" O=/br/output BR2_EXTERNAL=/src/image ferrofire_rtu_aarch64_defconfig
  make -C "$src" O=/br/output BR2_JLEVEL="$JOBS"
  ls -l /src/image/out/ferrofire-rtu-arm64.qc2'
