#!/bin/sh
# Acceptance harness: run one protocol's peers against a device under test (DUT), capture, and judge the capture the way a
# blue team would (tshark expert info, decoded fields, Zeek logs). Usage: conformance/run.sh PROTO   (or: make conformance PROTO=dnp3)
# Two containers, both named ff-conf-*: the DUT, and the testpeers image sharing its network namespace (so peers talk to 127.0.0.1
# and the capture on lo sees everything). Only these two are ever removed. Layout of conformance/<PROTO>/ : proto.env, peer.sh, check.sh.
# env: DUT=stock (default, the ffc-stock-2026.11.03:22.04 oracle) | rust (our binary, FF_BIN) | DUT_IMAGE + DUT_CMD (anything else: the config is at /tmp/cfg.xml)
#      TESTPEERS=ferrofire-testpeers  OUT=target/conformance/PROTO  ZEEK_REQUIRED=1 (a missing Zeek parser fails instead of skipping)
#      BENNU_DATA=dir holding the stock sample configs (default ~/git/sceptre-bennu/data/configs/ep)
set -eu
proto=${1:?usage: conformance/run.sh PROTO}
here=$(cd "$(dirname "$0")" && pwd)
[ "$proto" != elastic ] || exec "$here/elastic/run.sh"   # real Elasticsearch/OpenSearch containers: its own script
[ -f "$here/$proto/proto.env" ] || { echo "no conformance/$proto/proto.env" >&2; exit 2; }
BENNU_DATA=${BENNU_DATA:-$HOME/git/sceptre-bennu/data/configs/ep}
# shellcheck source=/dev/null
. "$here/$proto/proto.env"   # sets CFG (path), STOCK_CMD, WARMUP; optional DUT_DEFAULT (rust where bennu has no such server), DUT_CAPS
DUT=${DUT:-${DUT_DEFAULT:-stock}}
# STOCK_IMAGE: the stock oracle image (default ffc-stock-2026.11.03:22.04, bennu v2026.11.03). CONF_NAME: the container name prefix.
STOCK_IMAGE=${STOCK_IMAGE:-ffc-stock-2026.11.03:22.04}; N=${CONF_NAME:-ff-conf}
PEER_FLAGS=$(printf %s "${PEER_FLAGS:-}" | sed "s/ff-conf-dut/$N-dut/g")   # proto.env names the default DUT container; follow CONF_NAME
if [ "$DUT" = stock ]; then DUT_IMAGE=$STOCK_IMAGE; DUT_CMD=$STOCK_CMD; fi
# DUT=rust: our binary (FF_BIN, default target-linux/debug/ferrofire from make test/linux) is copied into a stock-image container, or into
# RUST_DUT_IMAGE (the binary needs only glibc; a protocol bennu has no part in can name the testpeers image there)
if [ "$DUT" = rust ]; then
  DUT_IMAGE=${RUST_DUT_IMAGE:-$STOCK_IMAGE}; DUT_BIN=${FF_BIN:-$here/../target-linux/debug/ferrofire}
  DUT_CMD='ferrofire field-device --file /tmp/cfg.xml'
fi
: "${DUT_IMAGE:?set DUT_IMAGE and DUT_CMD}" "${DUT_CMD:?}"
TESTPEERS=${TESTPEERS:-ferrofire-testpeers}
OUT=${OUT:-$here/../target/conformance/$proto}
rm -rf "$OUT"; mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
cleanup() { docker rm -f $N-dut $N-peers >/dev/null 2>&1 || true; }
cleanup; trap cleanup EXIT
# shellcheck disable=SC2086
docker run -d --name $N-dut ${DUT_CAPS:-} "$DUT_IMAGE" sleep 900 >/dev/null
[ -z "${DUT_BIN:-}" ] || docker cp "$DUT_BIN" $N-dut:/usr/local/bin/ferrofire
# proto.env may put more files into the DUT container (DUT_EXTRA="repo/path:/dest ...") and start something there before the DUT
# (DUT_PRE: the stock device a client DUT talks to)
for pair in ${DUT_EXTRA:-}; do docker cp "$here/../${pair%%:*}" "$N-dut:${pair#*:}"; done
[ -z "${DUT_PRE:-}" ] || docker exec -d $N-dut sh -c "$DUT_PRE"
docker exec -i $N-dut sh -c 'cat > /tmp/cfg.xml' < "$CFG"
docker exec -d $N-dut sh -c "$DUT_CMD > /tmp/dut.out 2>&1"
sleep "${WARMUP:-4}"
rc=0
# No bind mounts: the docker daemon (a Lima VM on the Mac) cannot see every host path, so files go in and out with docker cp.
# shellcheck disable=SC2086
docker create --name $N-peers --network container:$N-dut ${PEER_FLAGS:-} --cap-add NET_RAW --cap-add NET_ADMIN \
  -e ZEEK_REQUIRED="${ZEEK_REQUIRED:-0}" -e SELFTEST="${SELFTEST:-0}" "$TESTPEERS" /conformance/inside.sh "$proto" >/dev/null
docker cp "$here/." $N-peers:/conformance
# proto.env may name a repo-relative binary the peers need (PEER_BIN: our own thin test client, for protocols with no open peer)
[ -z "${PEER_BIN:-}" ] || docker cp "$here/../$PEER_BIN" "$N-peers:/usr/local/bin/$(basename "$PEER_BIN")"
# ... and PEER_EXTRA="repo/path:/dest ...": more files or directories
for pair in ${PEER_EXTRA:-}; do docker cp "$here/../${pair%%:*}" "$N-peers:${pair#*:}"; done
# captures fetched by tests/pcaps/iccp/fetch-real.sh (never committed) go to /real; the ICCP check uses them when present
[ ! -d "$here/../tests/pcaps/iccp/real" ] || docker cp "$here/../tests/pcaps/iccp/real" $N-peers:/real
docker start -a $N-peers || rc=$?
docker cp $N-peers:/out/. "$OUT" 2>/dev/null || true
docker exec $N-dut cat /tmp/dut.out > "$OUT/dut.out" 2>/dev/null || true
exit $rc
