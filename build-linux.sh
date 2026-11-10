#!/usr/bin/env bash
# Build ferrofire for aarch64-unknown-linux-gnu (glibc 2.35, jammy) in a container; outputs dist/ferrofire and dist/simulinksolver.
# Must be glibc, not musl: POSIX named semaphores in /dev/shm are shared with the glibc bennu-simulink-provider and
# musl's sem_t layout is incompatible. jammy = the guest images' glibc, so the binary runs on every guest.
# On an Apple-silicon Mac Docker Desktop runs linux/arm64 natively; set PLATFORM=linux/amd64 for an x86-64 build.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
plat="${PLATFORM:-linux/arm64}"
docker image inspect ff-rust:22.04 >/dev/null 2>&1 || docker build --platform "$plat" -t ff-rust:22.04 "$here/docker"
docker run --rm --platform "$plat" -v "$here":/src -w /src -e CARGO_TARGET_DIR=/src/target-linux -e CARGO_FLAGS="${CARGO_FLAGS:-}" ff-rust:22.04 bash -c '
  cargo build --release $CARGO_FLAGS
  mkdir -p dist && cp target-linux/release/ferrofire dist/ferrofire && cp target-linux/release/ferrofire dist/simulinksolver
  file dist/ferrofire; objdump -T dist/ferrofire | grep -o "GLIBC_[0-9.]*" | sort -uV | tail -1; echo "needed: $(readelf -d dist/ferrofire | grep -o "lib[a-z0-9_.+-]*\.so[0-9.]*" | tr "\n" " ")"'
