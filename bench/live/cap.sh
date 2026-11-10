#!/bin/bash
# usage: cap.sh label secs ; run on provider, rtu-1, rtu-2
L=$1; S=$2; H=$(hostname)
case $H in waterway-provider|rtu-1|rtu-2) ;; *) exit 0;; esac
IF=$(ip -br a | awk '/172\.16\.1\./{print $1}')
tck=$(getconf CLK_TCK)
cpu(){ for p in $(pgrep -f "bennu-simulink-provider|simulinksolver|ferrofire (run|pull)|bennu-field-device"); do awk '{s+=$14+$15} END{printf "%d ",s}' /proc/$p/stat; done; echo; }
c0=$(cpu)
timeout $S tcpdump -nn -q -i $IF -w /root/cap-$L-$H.pcap 'udp port 40000 or tcp port 5555' 2>/dev/null
c1=$(cpu)
echo "== $H $L cpu ticks before: $c0 after: $c1 (CLK_TCK $tck, over ${S}s)"
python3 /root/ff/cap.py /root/cap-$L-$H.pcap
