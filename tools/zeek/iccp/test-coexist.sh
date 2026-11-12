#!/usr/bin/env bash
# ICCP and CISA ICSNPP s7comm both claim tcp/102 (ww-otj.52). Load both in the ferrofire-testpeers image (it carries icsnpp-s7comm, make testpeers)
# and check DPD gives each its own traffic: ICCP flows are "cotp,iccp" (ICSNPP's COTP logger sees every COTP flow), S7comm flows "cotp,s7comm",
# no weird.log rows. Runs on the Lima docker daemon like the other testpeers targets (TP_DOCKER_HOST).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
export DOCKER_HOST="${TP_DOCKER_HOST:-unix://$HOME/.lima/phenix/sock/docker.sock}"
tar -C "$repo" -c tools/zeek/iccp tests/pcaps/iccp | docker run --rm -i --entrypoint bash ferrofire-testpeers -c '
  set -e; mkdir /w; tar -x -C /w; cd /w; spicyz -o /tmp/iccp.hlto tools/zeek/iccp/analyzer/iccp.spicy tools/zeek/iccp/analyzer/iccp.evt
  rc=0
  check() { # pcap, expected service
    rm -rf /o; mkdir /o; cd /o
    zeek -C -r "/w/tests/pcaps/iccp/$1" /tmp/iccp.hlto /w/tools/zeek/iccp/scripts packages/icsnpp-s7comm >/dev/null 2>stderr
    got=$(zeek-cut service < conn.log | tr , "\n" | sort -u | tr "\n" " ")
    if [ "$got" = "$2 " ] && [ ! -s weird.log ] && [ ! -s stderr ]; then echo "ok: $1 -> $got"; else echo "FAIL: $1: service [$got] want [$2]; weird: $(cat weird.log 2>/dev/null | head -3)"; rc=1; fi
    cd /w
  }
  check tase2_assoc.pcap "cotp iccp"; check tase2_report.pcap "cotp iccp"; check s7comm.pcap "cotp s7comm"
  exit $rc'
