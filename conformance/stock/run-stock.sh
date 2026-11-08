#!/bin/sh
# Run one probe against the STOCK bennu field device in bennu:arm64 (Lima VM "phenix").
# Usage (from the Mac):  conformance/stock/run-stock.sh modbus|104|dnp3|overflow|logic
# Creates and removes its own container ff-fd-stock; touches nothing else. Spec: docs/field-device-spec.md section 8.
set -e
here=$(cd "$(dirname "$0")" && pwd)
bennu=${BENNU_DATA:-$HOME/git/sceptre-bennu/data/configs/ep}
case "$1" in
  modbus)   cfg=$bennu/modbus-server.xml;   probe=probe_modbus.py ;;
  overflow) cfg=$bennu/modbus-server.xml;   probe=probe_modbus_overflow.py ;;
  dnp3)     cfg=$bennu/dnp3-server.xml;     probe=probe_dnp3.py ;;
  104)      cfg=$here/iec104-server.xml;    probe=probe_104.py ;;   # shipped sample lacks <subtype>, so we carry a fixed copy
  logic)    cfg=$here/logic_corpus.xml;     probe= ;;
  *) echo "usage: $0 modbus|104|dnp3|overflow|logic" >&2; exit 2 ;;
esac
L="limactl shell phenix --"
$L docker rm -f ff-fd-stock >/dev/null 2>&1 || true
$L docker run -d --name ff-fd-stock bennu:arm64 sleep 120 >/dev/null
trap '$L docker rm -f ff-fd-stock >/dev/null 2>&1' EXIT
cat "$cfg" | $L docker exec -i ff-fd-stock sh -c 'cat > /tmp/cfg.xml'
$L docker exec -d ff-fd-stock sh -c 'LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f /tmp/cfg.xml > /tmp/fd.out 2>&1'
sleep 4
if [ -n "$probe" ]; then
  cat "$here/$probe" | $L docker exec -i ff-fd-stock sh -c 'cat > /tmp/probe.py'
  $L docker exec ff-fd-stock python3 /tmp/probe.py
else
  sleep 4; $L docker exec ff-fd-stock grep -E '^LOGIC|ERROR|WARN|Delaying' /tmp/fd.out | head -80
fi
