#!/bin/sh
# usage: run.sh stock|c104 single|double  -> prints probe_ext.py output. Containers ff-sdd-t0-*, removed on exit.
set -e
export DOCKER_HOST=unix://$HOME/.docker/run/docker.sock
here=$(cd "$(dirname "$0")" && pwd); who=$1; mode=${2:-single}
N=ff-sdd-t0-$who
docker rm -f $N >/dev/null 2>&1 || true
trap '[ -n "$KEEP" ] || docker rm -f $N >/dev/null 2>&1' EXIT
if [ $who = stock ]; then
  docker run -d --name $N ffc-stock-2026.11.03:22.04 sleep 120 >/dev/null
  sed "s#<subtype>single</subtype>#<subtype>$mode</subtype>#" $here/../../../conformance/stock/iec104-server.xml | docker exec -i $N sh -c 'cat > /tmp/cfg.xml'
  docker exec -d $N sh -c 'LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f /tmp/cfg.xml > /tmp/srv.out 2>&1'
else
  docker run -d --name $N ff-mod104:22.04 sleep 120 >/dev/null
  docker cp $here/server.py $N:/tmp/server.py
  docker exec -d -e PYTHONUNBUFFERED=1 $N sh -c "python3 /tmp/server.py $mode 1 > /tmp/srv.out 2>&1"
fi
docker cp ${PROBE:-$here/probe_ext.py} $N:/tmp/probe_ext.py
sleep 4
docker exec $N python3 /tmp/probe_ext.py
echo "== server log"; docker exec $N tail -n ${LOGN:-60} /tmp/srv.out
