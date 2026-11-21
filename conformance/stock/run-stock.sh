#!/bin/sh
# Run one probe against the STOCK bennu field device in $STOCK_IMAGE (default ffc-stock-2026.11.03:22.04) on Docker Desktop.
# Usage (from the Mac):  conformance/stock/run-stock.sh modbus|104|dnp3|overflow|logic
#                        conformance/stock/run-stock.sh modbus-client|dnp3-client   re-record client-<proto>.golden and .pcap: the
#                        stock fd-client against the stock server, through conformance/run.sh (containers ff-conf-*, or CONF_NAME-*;
#                        STOCK_IMAGE names the stock image, default ffc-stock-2026.11.03:22.04). DNP3 also gets client-dnp3-raw.golden: every frame as sent, scans included.
# Creates and removes its own container ff-fd-stock; touches nothing else. Spec: docs/field-device-spec.md section 8.
set -e
DOCKER_HOST=${DOCKER_HOST:-unix://$HOME/.docker/run/docker.sock}; export DOCKER_HOST
STOCK_IMAGE=${STOCK_IMAGE:-ffc-stock-2026.11.03:22.04}; export STOCK_IMAGE
here=$(cd "$(dirname "$0")" && pwd)
bennu=${BENNU_DATA:-$HOME/git/sceptre-bennu/data/configs/ep}
case "$1" in
  modbus-client|dnp3-client)
    # SERIAL=connect:PORT records the serial goldens instead (client-<proto>-serial*, run.sh SERIAL)
    p=${1%-client}${SERIAL:+-serial}; : > "$here/client-$p.golden"   # check.sh compares with the golden: empty while recording
    DUT=stock "$here/../run.sh" "$1" || true
    out=${OUT:-$here/../../target/conformance/$1}
    cp "$out/transcript.txt" "$here/client-$p.golden"; cp "$out/$1.pcap" "$here/client-$p.pcap"
    [ ! -f "$out/transcript-raw.txt" ] || cp "$out/transcript-raw.txt" "$here/client-$p-raw.golden"; exit ;;
  modbus)   cfg=$bennu/modbus-server.xml;   probe=probe_modbus.py ;;
  overflow) cfg=$bennu/modbus-server.xml;   probe=probe_modbus_overflow.py ;;
  dnp3)     cfg=$bennu/dnp3-server.xml;     probe=probe_dnp3.py ;;
  104)      cfg=$here/iec104-server.xml;    probe=probe_104.py ;;   # shipped sample lacks <subtype>, so we carry a fixed copy
  logic)    cfg=$here/logic_corpus.xml;     probe= ;;
  *) echo "usage: $0 modbus|104|dnp3|overflow|logic" >&2; exit 2 ;;
esac
docker rm -f ff-fd-stock >/dev/null 2>&1 || true
docker run -d --name ff-fd-stock $STOCK_IMAGE sleep 120 >/dev/null
trap 'docker rm -f ff-fd-stock >/dev/null 2>&1' EXIT
cat "$cfg" | docker exec -i ff-fd-stock sh -c 'cat > /tmp/cfg.xml'
docker exec -d ff-fd-stock sh -c 'LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f /tmp/cfg.xml > /tmp/fd.out 2>&1'
sleep 4
if [ -n "$probe" ]; then
  cat "$here/$probe" | docker exec -i ff-fd-stock sh -c 'cat > /tmp/probe.py'
  docker exec ff-fd-stock python3 /tmp/probe.py
else
  sleep 4; docker exec ff-fd-stock grep -E '^LOGIC|ERROR|WARN|Delaying' /tmp/fd.out | head -80
fi
