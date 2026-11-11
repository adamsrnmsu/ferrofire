#!/bin/sh
# SPEC 8 differential: one probe against the stock oracle (bennu:arm64) or ours, transcript to stdout.
# usage: conformance/differential.sh PROTO stock|rust [diff|big]     (conformance/<PROTO>/probe_diff.py, diff-server.xml)
#   rust: FF_BIN=path to a linux ferrofire binary (default target-linux/debug/ferrofire); it is docker-cp'd into a bennu:arm64 container.
# TRACE=file also saves the probe trace. Compare: differential.sh dnp3 stock > stock.txt; differential.sh dnp3 rust > rust.txt; diff stock.txt rust.txt  (or: make diff-dnp3)
# Containers ff-diff-$set-dut and ff-diff-$set-peers only; they are the only ones removed.
set -eu
proto=${1:?usage: differential.sh PROTO stock|rust}; who=${2:?stock|rust}; set=${3:-diff}   # set: diff (probe_diff.py, diff-server.xml) or big (probe_big.py, diff-big-server.xml)
cfg=diff-server.xml; [ "$set" = diff ] || cfg=diff-$set-server.xml
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
DOCKER_HOST=${DOCKER_HOST:-unix://$HOME/.lima/phenix/sock/docker.sock}; export DOCKER_HOST
cleanup() { docker rm -f ff-diff-$set-dut ff-diff-$set-peers >/dev/null 2>&1 || true; }
cleanup; trap cleanup EXIT
docker run -d --name ff-diff-$set-dut bennu:arm64 sleep 300 >/dev/null
docker cp "$here/$proto/$cfg" ff-diff-$set-dut:/tmp/cfg.xml
case $who in
  stock) docker exec -d ff-diff-$set-dut sh -c 'cd /tmp && LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f /tmp/cfg.xml > /tmp/dut.out 2>&1' ;;
  rust)  docker cp "${FF_BIN:-$root/target-linux/debug/ferrofire}" ff-diff-$set-dut:/usr/local/bin/ferrofire
         docker exec -d ff-diff-$set-dut sh -c 'cd /tmp && ferrofire field-device --file /tmp/cfg.xml > /tmp/dut.out 2>&1' ;;
  *) echo "stock|rust" >&2; exit 2 ;;
esac
sleep 6
docker create --name ff-diff-$set-peers --network container:ff-diff-$set-dut ferrofire-testpeers python3 /conformance/probe_$set.py >/dev/null
docker cp "$here/$proto" ff-diff-$set-peers:/conformance
# probe lines starting "#T " are the machine-readable trace (replayed by the Rust golden test); the rest is the transcript
docker start -a ff-diff-$set-peers | awk -v t="${TRACE:-/dev/null}" '/^#T /{print substr($0,4) > t; next} {print}'
# LOGS=file also saves the device's event log (the *.log files in its cwd)
if [ -n "${LOGS:-}" ]; then
  docker exec ff-diff-$set-dut sh -c 'cat /tmp/*.log' >"$LOGS" 2>&1 || true
fi
# NOTE: runs must not overlap (not even the diff and big sets): both devices join the multicast group 239.0.0.1:40000 on the
# same bridge, so one probe's publishes land on the other device.
