#!/usr/bin/env bash
# Build the Spicy analyzer and run the btest suite in the ff-zeek container (Zeek + Spicy + g++; docker/Dockerfile.zeek).
# test.sh            compare against tests/Baseline
# test.sh --update   rewrite tests/Baseline from the current output (review the git diff before committing)
# The Lima docker VM shares only $HOME, so sources go in and baselines come out as tar streams, not bind mounts.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
docker image inspect ff-zeek >/dev/null 2>&1 || docker build -q -t ff-zeek -f "$repo/docker/Dockerfile.zeek" "$repo/docker" >/dev/null
flag=""; [ "${1:-}" = "--update" ] && flag="-U"
tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
tar -C "$repo" -c tools/zeek/iccp tests/pcaps/iccp | docker run --rm -i ff-zeek bash -c "
  set -e; mkdir /w; tar -x -C /w; cd /w/tools/zeek/iccp
  rm -rf build tests/.tmp; mkdir build; (cd build && spicyz -o iccp.hlto ../analyzer/iccp.spicy ../analyzer/iccp.evt) >&2
  cd tests; rc=0; btest -d -j $flag >&2 || rc=\$?
  [ -z '$flag' ] || tar -C Baseline -c .
  exit \$rc" > "$tmp"
if [ -n "$flag" ]; then mkdir -p "$here/tests/Baseline"; tar -x -C "$here/tests/Baseline" < "$tmp"; echo "baselines updated"; else echo "zeek-iccp ok"; fi
