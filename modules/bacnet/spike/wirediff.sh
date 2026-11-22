#!/bin/sh
# Compare what the server sent, frame by frame, stock pcap vs spike pcap (both servers are the first host of their network).
# Usage: wirediff.sh STOCK.pcap[.gz] SPIKE.pcap[.gz]   -> lists server->probe BACnet payloads present in one capture only.
export DOCKER_HOST=${DOCKER_HOST:-unix://$HOME/.docker/run/docker.sock}
dump() {   # server = the source of the first I-Am; its BACnet payloads, in order
  f="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
  case "$f" in *.gz) gunzip -c "$f" > /tmp/ff-t0b-wd.pcap; f=/tmp/ff-t0b-wd.pcap ;; esac
  srv=$(docker run --rm -v "$f":/x.pcap:ro --entrypoint tshark ferrofire-testpeers -r /x.pcap -Y 'bacapp.unconfirmed_service==0' -T fields -e ip.src 2>/dev/null | head -1)
  docker run --rm -v "$f":/x.pcap:ro --entrypoint tshark ferrofire-testpeers -r /x.pcap -Y "ip.src==$srv && udp.srcport==47808 && bacapp" -T fields -e udp.payload 2>/dev/null
}
dump "$1" > /tmp/ff-t0b-wire1.txt; dump "$2" > /tmp/ff-t0b-wire2.txt
python3 - <<'P'
import collections
a = open('/tmp/ff-t0b-wire1.txt').read().split(); b = open('/tmp/ff-t0b-wire2.txt').read().split()
print("frames stock=%d spike=%d" % (len(a), len(b)))
ca, cb = collections.Counter(a), collections.Counter(b)
for tag, c in (("stock only", ca - cb), ("spike only", cb - ca)):
    for k, v in c.items(): print(tag, v, k)
P
