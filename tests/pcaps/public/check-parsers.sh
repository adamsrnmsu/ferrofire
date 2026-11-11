#!/usr/bin/env bash
# Run tshark (malformed frames, expert Error/Warn) and Zeek (icsnpp-dnp3 weird.log) over the fetched ITI captures, in the testpeers image.
# Informational: these come from unknown tools, so odd frames are expected; the point is that our parsers' stack (tshark 4.4, Zeek 9 + ICSNPP)
# decodes real third-party traffic, and a regression shows as a changed count. Needs fetch.sh first and a docker daemon with the image.
#   check-parsers.sh [DIR]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"; dir="${1:-$here/data}"
[ -d "$dir" ] || { echo "run fetch.sh first" >&2; exit 2; }
cid=$(docker create "${TESTPEERS:-ferrofire-testpeers}" sleep 600); trap 'docker rm -f "$cid" >/dev/null' EXIT
docker start "$cid" >/dev/null; docker cp "$dir" "$cid:/pub"
docker exec "$cid" bash -c '
cd /pub; tot=0; mal=0; warn=0
for f in $(find . -name "*.pcap" | sort); do
  n=$(tshark -r "$f" 2>/dev/null | wc -l); m=$(tshark -r "$f" -Y _ws.malformed 2>/dev/null | wc -l)
  w=$(tshark -r "$f" -q -z expert,warn 2>/dev/null | grep -cE "^ +[0-9]+ " || true)
  tot=$((tot+n)); mal=$((mal+m)); warn=$((warn+w)); [ "$m" = 0 ] || echo "malformed $m/$n: $f"
done
echo "tshark: $(find . -name "*.pcap" | wc -l) captures, $tot packets, $mal malformed, $warn expert Error/Warn rows"
mkdir -p /z && cd /z; zw=0; zr=0
for f in $(find /pub/dnp3 -name "*.pcap" | sort); do rm -f *.log; zeek -C -r "$f" icsnpp-dnp3 >/dev/null 2>&1 || zr=$((zr+1)); [ ! -f weird.log ] || zw=$((zw+$(grep -vc "^#" weird.log))); done
echo "zeek icsnpp-dnp3: $(find /pub/dnp3 -name "*.pcap" | wc -l) captures, $zr zeek failures, $zw weird.log rows"'
