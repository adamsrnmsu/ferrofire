#!/bin/sh
# Acceptance harness: run one protocol's peers against a device under test (DUT), capture, and judge the capture the way a
# blue team would (tshark expert info, decoded fields, Zeek logs). Usage: conformance/run.sh PROTO   (or: make conformance PROTO=dnp3)
# Two containers, both named ff-conf-*: the DUT, and the testpeers image sharing its network namespace (so peers talk to 127.0.0.1
# and the capture on lo sees everything). Only these two are ever removed. Layout of conformance/<PROTO>/ : proto.env, peer.sh, check.sh.
# env: DUT=stock (default, the ffc-stock-2026.11.03:22.04 oracle) | rust (our binary, FF_BIN) | DUT_IMAGE + DUT_CMD (anything else: the config is at /tmp/cfg.xml)
#      TESTPEERS=ferrofire-testpeers  OUT=target/conformance/PROTO  ZEEK_REQUIRED=1 (a missing Zeek parser fails instead of skipping)
#      BENNU_DATA=dir holding the stock sample configs (default ~/git/sceptre-bennu/data/configs/ep)
# proto.env may also set (all off by default):
#      OWN_NET=1   the DUT gets a private network $N-net and the peers join it with an address of their own instead of the DUT's
#                  network namespace (BACnet: the device answers Who-Is by broadcast, so the probe must be another host). The peers
#                  get the DUT's address as DUT_IP; inside.sh captures on CAPTURE_IFACE (default lo).
#      DUT_ENV     variables exported for the DUT command, stock or ours (DUT_ENV='BACNET_IFACE=lo BACNET_IP_PORT=47809')
#      SIDE_IMAGE + SIDE_CMD   a third container $N-side in the DUT's network namespace, started before the DUT: a client DUT's
#                  server when it needs another image than the DUT's; SIDE_EXTRA as DUT_EXTRA, SIDE_BIN=1 copies our binary in too
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
cleanup() { docker rm -f $N-dut $N-side $N-peers >/dev/null 2>&1; docker network rm $N-net >/dev/null 2>&1 || true; }
cleanup; trap cleanup EXIT
net=; peer_net="container:$N-dut"
if [ "${OWN_NET:-0}" = 1 ]; then docker network create $N-net >/dev/null; net="--network $N-net"; peer_net=$N-net; fi
# shellcheck disable=SC2086
docker run -d --name $N-dut $net ${DUT_CAPS:-} "$DUT_IMAGE" sleep 900 >/dev/null
[ -z "${DUT_BIN:-}" ] || docker cp "$DUT_BIN" $N-dut:/usr/local/bin/ferrofire
DUT_IP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' $N-dut | cut -d' ' -f1)
[ -z "${DUT_ENV:-}" ] || DUT_CMD="export $DUT_ENV; $DUT_CMD"
if [ -n "${SIDE_IMAGE:-}" ]; then
  docker run -d --name $N-side --network container:$N-dut "$SIDE_IMAGE" sleep 900 >/dev/null
  [ "${SIDE_BIN:-0}" != 1 ] || docker cp "${FF_BIN:-$here/../target-linux/debug/ferrofire}" $N-side:/usr/local/bin/ferrofire
  for pair in ${SIDE_EXTRA:-}; do docker cp "$here/../${pair%%:*}" "$N-side:${pair#*:}"; done
  docker exec -d $N-side sh -c "$SIDE_CMD"
fi
# proto.env may put more files into the DUT container (DUT_EXTRA="repo/path:/dest ...") and start something there before the DUT
# (DUT_PRE: the stock device a client DUT talks to)
for pair in ${DUT_EXTRA:-}; do docker cp "$here/../${pair%%:*}" "$N-dut:${pair#*:}"; done
[ -z "${DUT_PRE:-}" ] || docker exec -d $N-dut sh -c "$DUT_PRE"
# SERIAL=listen:PORT (a server DUT) or connect:PORT (a client DUT): the DUT's <endpoint> tcp://127.0.0.1:PORT becomes the serial port
# /tmp/ttyFF, a pty that stock/ptyrelay.py joins to TCP PORT, so the peers and the capture are those of the TCP run (ww-8k4). The relay
# models a line of SERIAL_LINE (default 9600-8N1): paced, and corrupted when the device's port settings differ; the transcript ends
# with the device's settings, `# serial line: 9600 8N1 flow=none raw` (ww-y51).
if [ -n "${SERIAL:-}" ]; then
  docker cp "$here/stock/ptyrelay.py" $N-dut:/tmp/ptyrelay.py
  docker exec -d $N-dut sh -c "python3 /tmp/ptyrelay.py ${SERIAL%%:*} ${SERIAL#*:} /tmp/ttyFF ${SERIAL_LINE:-9600-8N1} > /tmp/ptyrelay.out 2>&1"
  docker exec $N-dut sh -c 'for i in $(seq 50); do [ -e /tmp/ttyFF ] && exit 0; sleep 0.1; done; echo "no /tmp/ttyFF" >&2; exit 1'
  sed "s|tcp://127.0.0.1:${SERIAL#*:}<|/tmp/ttyFF<|" "$CFG" | docker exec -i $N-dut sh -c 'cat > /tmp/cfg.xml'
else
  docker exec -i $N-dut sh -c 'cat > /tmp/cfg.xml' < "$CFG"
fi
docker exec -d $N-dut sh -c "$DUT_CMD > /tmp/dut.out 2>&1"
sleep "${WARMUP:-4}"
rc=0
# No bind mounts: the docker daemon (TP_DOCKER_HOST, Docker Desktop by default, or a VM) may not see every host path, so files go in and out with docker cp.
# shellcheck disable=SC2086
docker create --name $N-peers --network "$peer_net" ${PEER_FLAGS:-} --cap-add NET_RAW --cap-add NET_ADMIN \
  -e ZEEK_REQUIRED="${ZEEK_REQUIRED:-0}" -e SELFTEST="${SELFTEST:-0}" -e SERIAL="${SERIAL:-}" -e DUT="$DUT" -e DUT_IP="$DUT_IP" \
  "$TESTPEERS" /conformance/inside.sh "$proto" >/dev/null
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
[ -z "${SIDE_IMAGE:-}" ] || docker exec $N-side cat /tmp/peer-device.out > "$OUT/side.out" 2>/dev/null || true   # servectl's child
# a serial run: the device's port settings end the transcript and must be SERIAL_WANT (default 9600 8N1, raw, no flow control)
if [ -n "${SERIAL:-}" ]; then
  docker exec $N-dut cat /tmp/ptyrelay.out > "$OUT/ptyrelay.out" 2>/dev/null || true
  line=$(docker exec $N-dut cat /tmp/ptyrelay.line 2>/dev/null)
  echo "# serial line: $line" >> "$OUT/transcript.txt"
  want=${SERIAL_WANT:-9600 8N1 flow=none raw}
  if [ "$line" = "$want" ]; then echo "ok:   serial line $line"; else echo "FAIL: serial line '$line', want '$want'"; rc=1; fi
fi
exit $rc
