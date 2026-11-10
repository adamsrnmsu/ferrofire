#!/bin/bash
# usage: cap.sh label secs ; run on provider, rtu-1, rtu-2
# Besides the pcap in /root, the summary (plus the process list and provider log) goes to /root/ff/live-out/, which
# collect.sh copies into docs/bench/results/live/ in the repo: a number quoted in the docs must have its capture there.
L=$1; S=$2; H=$(hostname)
case $H in waterway-provider|rtu-1|rtu-2) ;; *) exit 0;; esac
IF=$(ip -br a | awk '/172\.16\.1\./{print $1}')
tck=$(getconf CLK_TCK)
cpu(){ for p in $(pgrep -f "bennu-simulink-provider|simulinksolver|ferrofire (run|pull)|bennu-field-device"); do awk '{s+=$14+$15} END{printf "%d ",s}' /proc/$p/stat; done; echo; }
OUT=/root/ff/live-out; mkdir -p $OUT
c0=$(cpu)
timeout $S tcpdump -nn -q -i $IF -w /root/cap-$L-$H.pcap 'udp port 40000 or tcp port 5555 or igmp' 2>/dev/null
c1=$(cpu)
{
echo "== $H $L cpu ticks before: $c0 after: $c1 (CLK_TCK $tck, over ${S}s)"
date -u +"captured %Y-%m-%dT%H:%M:%SZ"; ps -eo pid,cmd | grep -E "[f]errofire|[b]ennu-"
python3 /root/ff/cap.py /root/cap-$L-$H.pcap
python3 /root/ff/seqgap.py /root/cap-$L-$H.pcap
[ -f /root/ff/prov.log ] && { echo "-- prov.log (tail)"; tail -20 /root/ff/prov.log; }
} 2>&1 | tee $OUT/cap-$L-$H.txt
