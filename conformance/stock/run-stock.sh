#!/bin/sh
# Run one probe against the STOCK bennu field device in $STOCK_IMAGE (default ffc-stock-2026.11.03:22.04) on Docker Desktop.
# Usage (from the Mac):  conformance/stock/run-stock.sh modbus|104|dnp3|overflow|logic
#                        conformance/stock/run-stock.sh modbus-client|dnp3-client   re-record client-<proto>.golden and .pcap: the
#                        stock fd-client against the stock server, through conformance/run.sh (containers ff-conf-*, or CONF_NAME-*;
#                        STOCK_IMAGE names the stock image, default ffc-stock-2026.11.03:22.04). DNP3 also gets client-dnp3-raw.golden: every frame as sent, scans included.
#                        conformance/stock/run-stock.sh bacnet-client   the same for <bacnet-client>; bacnet-feed: the fed server
#                        transcript of conformance/bacnet (stock/bacnet-server-feed.golden)
#                        conformance/stock/run-stock.sh bacnet   BACnet server golden: DUT and probe in private network ff-sdd-t0b-net (the
#                        probe needs its own IP: the stock device answers Who-Is by broadcast); OUT=dir keeps probe output and pcap.
# Creates and removes its own container ff-fd-stock; touches nothing else. Spec: docs/field-device-spec.md section 8.
set -e
DOCKER_HOST=${DOCKER_HOST:-unix://$HOME/.docker/run/docker.sock}; export DOCKER_HOST
STOCK_IMAGE=${STOCK_IMAGE:-ffc-stock-2026.11.03:22.04}; export STOCK_IMAGE
here=$(cd "$(dirname "$0")" && pwd)
bennu=${BENNU_DATA:-$HOME/git/sceptre-bennu/data/configs/ep}
case "$1" in
  bacnet)
    N=${CONF_NAME:-ff-sdd-t0b}; out=${OUT:-$here/../../target/conformance/bacnet-stock}; mkdir -p "$out"
    cleanup() { docker rm -f $N-dut $N-probe >/dev/null 2>&1; docker network rm $N-net >/dev/null 2>&1 || true; }
    cleanup; trap cleanup EXIT
    docker network create $N-net >/dev/null
    docker run -d --name $N-dut --network $N-net $STOCK_IMAGE sleep 900 >/dev/null
    docker run -d --name $N-probe --network $N-net --cap-add NET_RAW --cap-add NET_ADMIN ferrofire-testpeers sleep 900 >/dev/null
    docker exec $N-probe sh -c 'python3 -m venv /opt/v && /opt/v/bin/pip install -q bacpypes3==0.1.0' >/dev/null 2>&1
    dut=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$N-net\").IPAddress}}" $N-dut)
    me=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$N-net\").IPAddress}}" $N-probe)
    docker exec -d $N-probe sh -c 'tcpdump -i eth0 -U -w /tmp/b.pcap udp port 47808 2>/dev/null'; sleep 1
    cat "$here/bacnet-server.xml" | docker exec -i $N-dut sh -c 'cat > /tmp/cfg.xml'
    docker exec -d $N-dut sh -c 'BACNET_IFACE=eth0 BACNET_IP_PORT=47808 LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f /tmp/cfg.xml > /tmp/fd.out 2>&1'
    sleep 4
    docker cp "$here/probe_bacnet.py" $N-probe:/tmp/probe.py
    docker exec $N-probe /opt/v/bin/python /tmp/probe.py "$dut" "$me/16:47808" > "$out/transcript.txt" 2>&1 || true
    docker exec $N-probe nmap -sU -p 47808 --script bacnet-info "$dut" 2>&1 | grep -v "^Starting Nmap\|^Nmap done\|scan report\|Host is up\|latency\|MAC Address" > "$out/nmap.txt" || true
    cp "$out/nmap.txt" "$here/bacnet-server.nmap.golden"
    sleep 1; docker exec $N-probe pkill tcpdump; sleep 1; docker cp $N-probe:/tmp/b.pcap "$out/bacnet-server.pcap"; gzip -kf "$out/bacnet-server.pcap"; cp "$out/bacnet-server.pcap.gz" "$here/"   # .gz: the 1 MB large-file hook
    sed -e "s/$dut/DUT/g" -e "s/$me/PROBE/g" "$out/transcript.txt" > "$here/bacnet-server.golden"; exit ;;
  bacnet-feed)
    # stock/bacnet-server-feed.golden: conformance/bacnet with its provider (FEED), through conformance/run.sh (containers ff-conf-*)
    : > "$here/bacnet-server-feed.golden"; DUT=stock "$here/../run.sh" bacnet || true
    cp "${OUT:-$here/../../target/conformance/bacnet}/transcript.txt" "$here/bacnet-server-feed.golden"; exit ;;
  modbus-client|dnp3-client|bacnet-client)
    # SERIAL=connect:PORT records the serial goldens instead (client-<proto>-serial*, run.sh SERIAL)
    p=${1%-client}${SERIAL:+-serial}; : > "$here/client-$p.golden"   # check.sh compares with the golden: empty while recording
    DUT=stock "$here/../run.sh" "$1" || true
    out=${OUT:-$here/../../target/conformance/$1}
    grep -v '^# serial line:' "$out/transcript.txt" > "$here/client-$p.golden"; cp "$out/$1.pcap" "$here/client-$p.pcap"   # run.sh checks the line itself
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
