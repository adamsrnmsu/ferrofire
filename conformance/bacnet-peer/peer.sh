#!/bin/sh
# Opt-in BACnet4J peer lane (ww-ntp): an independent BACnet client (BACnet4J, GPL-3.0, so it lives outside this repo) probes the
# BACnet server DUT, and check.sh judges its transcript against expect.txt, the spec-decided expectations (ASHRAE 135).
# usage: conformance/bacnet-peer/peer.sh [rust|stock]      (make bacnet-peer WHO=rust|stock; not part of make check or CI)
# env:   BACNET4J_IMAGE  the peer image; unset or absent -> a message and exit 0. Contract: `docker run IMAGE DUT_IP BROADCAST_ADDR`
#                        prints the probe transcript on stdout (see conformance/README.md, "BACnet4J peer lane")
#        RUST_DUT_IMAGE (default ff-modbac:22.04), FF_BIN (default target-linux/debug/ferrofire), STOCK_IMAGE, CONF_NAME (container prefix)
#        OUT (default target/conformance/bacnet-peer-WHO)
set -eu
here=$(cd "$(dirname "$0")" && pwd); repo=$here/../..
who=${1:-rust}; N=${CONF_NAME:-ff-conf}-peer; image=${BACNET4J_IMAGE:-}
[ -n "$image" ] || { echo "skip: BACNET4J_IMAGE not set (the BACnet4J peer image, conformance/README.md)"; exit 0; }
docker image inspect "$image" >/dev/null 2>&1 || { echo "skip: peer image $image not found"; exit 0; }
OUT=${OUT:-$repo/target/conformance/bacnet-peer-$who}; rm -rf "$OUT"; mkdir -p "$OUT"
FEED='bus-1.gen_mw:300.7,load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,load-1_bus-1.active:true,'
cleanup() { docker rm -f $N-dut >/dev/null 2>&1; docker network rm $N-net >/dev/null 2>&1 || true; }
cleanup; trap cleanup EXIT
docker network create $N-net >/dev/null
if [ "$who" = stock ]; then
  img=${STOCK_IMAGE:-ffc-stock-2026.11.03:22.04}
  cmd='cd /tmp && BACNET_IFACE=eth0 BACNET_IP_PORT=47808 LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f /tmp/cfg.xml'
else
  img=${RUST_DUT_IMAGE:-ff-modbac:22.04}
  cmd='BACNET_IFACE=eth0 BACNET_IP_PORT=47808 ferrofire field-device --file /tmp/cfg.xml'
fi
docker run -d --name $N-dut --network $N-net "$img" sleep 900 >/dev/null
[ "$who" = stock ] || docker cp "${FF_BIN:-$repo/target-linux/debug/ferrofire}" $N-dut:/usr/local/bin/ferrofire
docker cp "$repo/conformance/stock/bacnet-server.xml" $N-dut:/tmp/cfg.xml
docker cp "$repo/conformance/bacnet/feed.py" $N-dut:/tmp/feed.py
ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' $N-dut)
docker exec -d $N-dut sh -c "python3 /tmp/feed.py /tmp/provider.log '$FEED' > /tmp/feed.out 2>&1"
docker exec -d $N-dut sh -c "$cmd > /tmp/dut.out 2>&1"
sleep 8   # the feed has given analog-input 0 its value
docker run --rm --network $N-net "$image" "$ip" "${ip%.*.*}.255.255" > "$OUT/transcript.txt" 2> "$OUT/peer.err" || true
docker exec $N-dut cat /tmp/dut.out > "$OUT/dut.out" 2>&1 || true
exec "$here/check.sh" "$who" "$OUT/transcript.txt"
