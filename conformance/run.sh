#!/bin/sh
# Acceptance harness: run one protocol's peers against a device under test (DUT), capture, and judge the capture the way a
# blue team would (tshark expert info, decoded fields, Zeek logs). Usage: conformance/run.sh PROTO   (or: make conformance PROTO=dnp3)
# Two containers, both named ff-conf-*: the DUT, and the testpeers image sharing its network namespace (so peers talk to 127.0.0.1
# and the capture on lo sees everything). Only these two are ever removed. Layout of conformance/<PROTO>/ : proto.env, peer.sh, check.sh.
# env: DUT=stock (default, bennu:arm64 oracle) | rust (our binary, FF_BIN) | DUT_IMAGE + DUT_CMD (anything else: the config is at /tmp/cfg.xml)
#      TESTPEERS=ferrofire-testpeers  OUT=target/conformance/PROTO  ZEEK_REQUIRED=1 (a missing Zeek parser fails instead of skipping)
#      BENNU_DATA=dir holding the stock sample configs (default ~/git/sceptre-bennu/data/configs/ep)
set -eu
proto=${1:?usage: conformance/run.sh PROTO}
here=$(cd "$(dirname "$0")" && pwd)
[ -f "$here/$proto/proto.env" ] || { echo "no conformance/$proto/proto.env" >&2; exit 2; }
BENNU_DATA=${BENNU_DATA:-$HOME/git/sceptre-bennu/data/configs/ep}
# shellcheck source=/dev/null
. "$here/$proto/proto.env"   # sets CFG (path), STOCK_CMD, WARMUP; optional DUT_DEFAULT (rust where bennu has no such server), DUT_CAPS
DUT=${DUT:-${DUT_DEFAULT:-stock}}
if [ "$DUT" = stock ]; then DUT_IMAGE=bennu:arm64; DUT_CMD=$STOCK_CMD; fi
# DUT=rust: our binary (FF_BIN, default target-linux/debug/ferrofire from make test/linux) is copied into a bennu:arm64 container
if [ "$DUT" = rust ]; then
  DUT_IMAGE=bennu:arm64; DUT_BIN=${FF_BIN:-$here/../target-linux/debug/ferrofire}
  DUT_CMD='ferrofire field-device --file /tmp/cfg.xml'
fi
: "${DUT_IMAGE:?set DUT_IMAGE and DUT_CMD}" "${DUT_CMD:?}"
TESTPEERS=${TESTPEERS:-ferrofire-testpeers}
OUT=${OUT:-$here/../target/conformance/$proto}
rm -rf "$OUT"; mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
cleanup() { docker rm -f ff-conf-dut ff-conf-peers >/dev/null 2>&1 || true; }
cleanup; trap cleanup EXIT
# shellcheck disable=SC2086
docker run -d --name ff-conf-dut ${DUT_CAPS:-} "$DUT_IMAGE" sleep 900 >/dev/null
[ -z "${DUT_BIN:-}" ] || docker cp "$DUT_BIN" ff-conf-dut:/usr/local/bin/ferrofire
docker exec -i ff-conf-dut sh -c 'cat > /tmp/cfg.xml' < "$CFG"
docker exec -d ff-conf-dut sh -c "$DUT_CMD > /tmp/dut.out 2>&1"
sleep "${WARMUP:-4}"
rc=0
# No bind mounts: the docker daemon (a Lima VM on the Mac) cannot see every host path, so files go in and out with docker cp.
docker create --name ff-conf-peers --network container:ff-conf-dut --cap-add NET_RAW --cap-add NET_ADMIN \
  -e ZEEK_REQUIRED="${ZEEK_REQUIRED:-0}" -e SELFTEST="${SELFTEST:-0}" "$TESTPEERS" /conformance/inside.sh "$proto" >/dev/null
docker cp "$here/." ff-conf-peers:/conformance
# proto.env may name a repo-relative binary the peers need (PEER_BIN: our own thin test client, for protocols with no open peer)
[ -z "${PEER_BIN:-}" ] || docker cp "$here/../$PEER_BIN" "ff-conf-peers:/usr/local/bin/$(basename "$PEER_BIN")"
# ... and PEER_EXTRA="repo/path:/dest ...": more files or directories
for pair in ${PEER_EXTRA:-}; do docker cp "$here/../${pair%%:*}" "ff-conf-peers:${pair#*:}"; done
# captures fetched by tests/pcaps/iccp/fetch-real.sh (never committed) go to /real; the ICCP check uses them when present
[ ! -d "$here/../tests/pcaps/iccp/real" ] || docker cp "$here/../tests/pcaps/iccp/real" ff-conf-peers:/real
docker start -a ff-conf-peers || rc=$?
docker cp ff-conf-peers:/out/. "$OUT" 2>/dev/null || true
docker exec ff-conf-dut cat /tmp/dut.out > "$OUT/dut.out" 2>/dev/null || true
exit $rc
