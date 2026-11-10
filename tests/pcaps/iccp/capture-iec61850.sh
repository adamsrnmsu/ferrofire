#!/usr/bin/env bash
# Regenerate the libiec61850 MMS captures (iec61850/*.pcap) in a throwaway container. libiec61850 is GPL-3.0 and is
# built and run there only; the pcaps are data. Needs docker (DOCKER_HOST as for the rest of the repo).
# No bind mount: the Lima VM only shares $HOME, so the pcaps come back as a tar stream.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$here/iec61850"
docker build -q -t ff-iec61850 "$out" >/dev/null
rm -f "$out"/*.pcap
docker run --rm ff-iec61850 bash -c '
set -e
mkdir /out
ex=/lib61850/examples
cap() {  # cap NAME SERVER CLIENT: server + client over lo, tcpdump on tcp/102
  n=$1; srv=$2; cli=$3
  tcpdump -i lo -U -w /out/$n.pcap tcp port 102 2>/dev/null & tp=$!
  sleep 1; $ex/$srv/$srv >/dev/null 2>&1 & sp=$!
  sleep 1; timeout 8 $ex/$cli/$cli >/dev/null 2>&1 || true
  kill $sp 2>/dev/null || true; sleep 1; kill $tp; wait 2>/dev/null || true
}
cap read_names server_example_basic_io iec61850_client_example1
cap reporting server_example_basic_io iec61850_client_example_reporting
cap datasets server_example_basic_io iec61850_client_example2
tar -C /out -c .
' | tar -x -C "$out"
echo "libiec61850 commit: $(docker run --rm ff-iec61850 cat /lib61850/COMMIT)"
ls -l "$out"/*.pcap
