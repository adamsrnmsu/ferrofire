#!/bin/sh
# bacpypes3 spike: run server.py in a python container, run conformance/stock/probe_bacnet.py against it from a second container in a
# private network, capture, and diff the transcript against the stock golden. Containers/network ff-sdd-t0b-spike-*, removed on exit.
# Usage: modules/bacnet/spike/run.sh   (DCC_WAIT=0 skips the 70 s DCC-duration wait, then the last lines differ)
set -e
export DOCKER_HOST=${DOCKER_HOST:-unix://$HOME/.docker/run/docker.sock}
here=$(cd "$(dirname "$0")" && pwd); root=$here/../../..; N=ff-sdd-t0b-spike; out=$here/out; mkdir -p "$out"
cleanup() { docker rm -f $N-dut $N-probe >/dev/null 2>&1; docker network rm $N-net >/dev/null 2>&1 || true; }
cleanup; trap cleanup EXIT
docker network create $N-net >/dev/null
docker run -d --name $N-dut --network $N-net ff-physics:22.04 sleep 900 >/dev/null
docker run -d --name $N-probe --network $N-net --cap-add NET_RAW --cap-add NET_ADMIN ferrofire-testpeers sleep 900 >/dev/null
docker exec $N-dut pip install -q bacpypes3==0.1.0 >/dev/null 2>&1
docker exec $N-probe sh -c 'python3 -m venv /opt/v && /opt/v/bin/pip install -q bacpypes3==0.1.0' >/dev/null 2>&1
dut=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$N-net\").IPAddress}}" $N-dut)
me=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$N-net\").IPAddress}}" $N-probe)
docker cp "$here/server.py" $N-dut:/tmp/server.py; docker cp "$root/conformance/stock/bacnet-server.xml" $N-dut:/tmp/cfg.xml
docker exec -d $N-probe sh -c 'tcpdump -i eth0 -U -w /tmp/b.pcap udp port 47808 2>/dev/null'; sleep 1
docker exec -d $N-dut sh -c "python3 /tmp/server.py /tmp/cfg.xml $dut/16:47808 > /tmp/srv.out 2>&1"
sleep 8
docker cp "$root/conformance/stock/probe_bacnet.py" $N-probe:/tmp/probe.py
docker exec -e DCC_WAIT=${DCC_WAIT:-70} $N-probe /opt/v/bin/python /tmp/probe.py "$dut" "$me/16:47808" > "$out/transcript.txt" 2>&1 || true
docker exec $N-probe nmap -sU -p 47808 --script bacnet-info "$dut" 2>&1 | grep -v "^Starting Nmap\|^Nmap done\|scan report\|Host is up\|latency\|MAC Address" > "$out/nmap.txt" || true
sleep 1; docker exec $N-probe pkill tcpdump; sleep 1; docker cp $N-probe:/tmp/b.pcap "$out/bacnet-spike.pcap" && gzip -f "$out/bacnet-spike.pcap"   # the pre-commit large-file hook refuses 1 MB
docker cp $N-dut:/tmp/srv.out "$out/server.log"
sed -e "s/$dut/DUT/g" -e "s/$me/PROBE/g" "$out/transcript.txt" > "$out/spike.txt"
diff "$root/conformance/stock/bacnet-server.nmap.golden" "$out/nmap.txt" > "$out/nmap.diff" && echo "nmap bacnet-info == stock" || echo "nmap differs, see $out/nmap.diff"
if diff "$root/conformance/stock/bacnet-server.golden" "$out/spike.txt" > "$out/diff.txt"; then echo "transcript == stock golden"; else echo "differs, see $out/diff.txt"; fi
