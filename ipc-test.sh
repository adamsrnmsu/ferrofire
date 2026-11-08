#!/bin/bash
# IPC self-check: our simulinksolver vs stock bennu-simulink-provider inside bennu:arm64 (run in the Lima VM).
# usage: limactl shell phenix sudo bash <abs>/ipc-test.sh
set -u
D=$(cd "$(dirname "$0")" && pwd)
docker run --rm --name ff-ipc-$$ --ipc=private -v "$D/dist:/sim:ro" bennu:arm64 bash -c '
mkdir -p /tmp/w && cd /tmp/w && cp /sim/simulinksolver .
bennu-simulink-provider --server-endpoint tcp://127.0.0.1:5555 --publish-endpoint udp://127.0.0.1:5556 >prov.log 2>&1 &
sleep 1; ./simulinksolver -tf inf >solver.log 2>&1 &
sleep 3
for f in prov.log solver.log; do echo "== $f"; head -20 $f; done
Q=$(timeout 15 bennu-probe --e tcp://127.0.0.1:5555 --c query); n=$(echo "$Q" | grep -c "^\s[a-z]*\.[a-z]*$"); echo "POINTS: $n"
r() { timeout 15 bennu-probe --e tcp://127.0.0.1:5555 --c read --t "$1" 2>&1 | grep -A1 Reply | tail -1 | tr -d "\t"; }
echo "start lock.height: $(r lock.height)"
w() { timeout 15 bennu-probe --e tcp://127.0.0.1:5555 --c write --t "$1" --v "$2" >/dev/null 2>&1; }
w uppervalve.open 1; sleep 4; echo "up-valve  lock.height: $(r lock.height)"; w uppervalve.open 0
w lowervalve.open 1; sleep 2; echo "low-valve lock.height: $(r lock.height)"; sleep 3; echo "low-valve lock.height: $(r lock.height)"
echo "valve: $(r lowervalve.open)"
for f in solver.log prov.log; do echo "== $f"; tail -n 5 $f; done'
