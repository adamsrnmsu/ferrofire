#!/bin/sh
# Host side of smoke-rust.sh: the stock bennu dnp3-client against our outstation (SPEC 8.5). usage: smoke.sh [stock|rust]
# (stock = the unmodified images/bennu-smoke.sh, for comparison). Needs the Lima docker (bennu:arm64); BENNU_DATA as for run.sh.
set -eu
who=${1:-rust}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
DOCKER_HOST=${DOCKER_HOST:-unix://$HOME/.lima/phenix/sock/docker.sock}; export DOCKER_HOST
BENNU_DATA=${BENNU_DATA:-$HOME/git/sceptre-bennu/data/configs/ep}
trap 'docker rm -f ff-smoke >/dev/null 2>&1 || true' EXIT
docker rm -f ff-smoke >/dev/null 2>&1 || true
docker run -d --name ff-smoke bennu:arm64 sleep 120 >/dev/null
docker exec ff-smoke mkdir -p /data/configs
docker cp "$BENNU_DATA" ff-smoke:/data/configs/ep
case $who in
  rust)  docker cp "${FF_BIN:-$root/target-linux/debug/ferrofire}" ff-smoke:/usr/local/bin/ferrofire
         docker cp "$here/smoke-rust.sh" ff-smoke:/tmp/smoke.sh ;;
  stock) docker cp "$HOME/git/phenix-waterway-range/images/bennu-smoke.sh" ff-smoke:/tmp/smoke.sh ;;
esac
docker exec ff-smoke sh /tmp/smoke.sh
