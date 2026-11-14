#!/bin/bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
# mm.sh WORKLOAD BIN [TAG] -> one line: workload bin cpu%  (provider CPU, no perf). BIN = name in /var/tmp/ff-prof/bin (NAME-ferrofire)
source /var/tmp/ff-prof/scripts/lib.sh
IMG=ffprof-perf:22.04; W=$1; B=/out/bin/$2-ferrofire; T=${3:-$2}; D=/src/target-linux/control-grid/R10
WIN=${WIN:-25}
guard || exit 1; cleanup
mkdir -p /var/tmp/ff-prof/m
cpu() { docker exec ffp-prov awk '{print $14+$15}' /proc/1/stat; }
measure() { sleep ${WARM:-6}; a=$(cpu); s=$(date +%s.%N); sleep $WIN; b=$(cpu); e=$(date +%s.%N)
  echo "$W $2 cpu=$(awk -v a=$a -v b=$b -v s=$s -v e=$e 'BEGIN{printf "%.1f",(b-a)/100/(e-s)*100}')"; }
grid() { # dt rate mode
  sed -e "s/^dt: .*/dt: $1/" -e "s/^publish_rate: .*/publish_rate: $2/" -e "s#case: ../case.py#case: /src/target-linux/control-grid/case.py#" $R/target-linux/control-grid/R10/model.yaml > /var/tmp/ff-prof/m/model-$1-$2.yaml
  dr --name ffp-prov --network-alias provider $IMG $B run /out/m/model-$1-$2.yaml --stamp-writes >/dev/null
  for n in 1 2 3 4 5 6 7 8 9 10; do dr --name ffp-rtu-$n --network-alias rtu-$n $IMG /out/bin/base-ferrofire field-device --file $D/ferrofire/rtu-$n.xml >/dev/null; done
  sleep 6
  if [ $3 = storm ]; then
    CELLS=$(python3 -c "print(','.join(['breaker:sim:1000']*400))")
    rm -f /var/tmp/ff-prof/m/ffc-$T-$W.cells.csv
    dr --name ffp-drv $IMG /out/bin/base-ffcontrol --scada $D/scada.json --provider tcp://provider:5555 --stream 'udp://*;239.0.1.{n}:40000' --cells $CELLS --stack ferrofire --settle-s 0.05 --out /out/m/ffc-$T-$W >/dev/null
    WARM=12
  fi
  measure
  if [ $3 = storm ]; then python3 - /var/tmp/ff-prof/m/ffc-$T-$W.cells.csv <<'PY'
import csv,sys,statistics as st
r=list(csv.DictReader(open(sys.argv[1])))
ok=[x for x in r if x['ok']==x['issued']]
sp=[float(x['span_s']) for x in r]; wl=[float(x['wall_s']) for x in r]
print("  cells",len(r),"ok",len(ok),"span_s p50 %.3f p90 %.3f  wall_s p50 %.3f"%(st.median(sp),sorted(sp)[int(len(sp)*.9)],st.median(wl)))
PY
  fi
}
case $W in
 big) dr --name ffp-prov --network-alias provider $IMG $B run /out/m8112.yaml --rate 0.1 >/dev/null; measure ;;
 big01) dr --name ffp-prov --network-alias provider $IMG $B run /out/m8112.yaml --rate 0.01 >/dev/null; measure ;;
 m800) dr --name ffp-prov --network-alias provider $IMG $B run /src/bench/models/m800.yaml --rate 0.01 >/dev/null; measure ;;
 query) dr --name ffp-prov --network-alias provider $IMG $B run /src/bench/models/m800.yaml --rate 0.01 >/dev/null; sleep 4
   dr --name ffp-cl --network-alias cl $IMG /out/bin/base-ffbench client --transport zmq --host provider --n 800 --period 0.01 --warmup 1 --pub-secs 1 --req-secs 10 >/dev/null
   : > /var/tmp/ff-prof/m/cpu-$T.log
   for i in $(seq 40); do echo "$(date +%s%N) $(cpu)" >> /var/tmp/ff-prof/m/cpu-$T.log; sleep 1; done
   docker logs ffp-cl 2>&1 > /var/tmp/ff-prof/m/cl-$T.json
   python3 - /var/tmp/ff-prof/m/cl-$T.json /var/tmp/ff-prof/m/cpu-$T.log "$T" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); L=[tuple(map(int,l.split())) for l in open(sys.argv[2]) if len(l.split())==2]
def cpu(w):
    s,e=w['start_ns'],w['end_ns']
    pts=[p for p in L if s<=p[0]<=e]
    if len(pts)<2: return -1
    return (pts[-1][1]-pts[0][1])/100/((pts[-1][0]-pts[0][0])/1e9)*100
for k in ('read','query'):
    print("  %s %s cpu=%.1f rate=%.0f/s p50=%.1fus p99=%.1fus"%(sys.argv[3],k,cpu(d['phases'][k]),d[k]['rate_per_s'],d[k]['p50_us'],d[k]['p99_us']))
PY
   ;;
 gridfast-idle) grid 0.005 0.01 idle ;;
 gridfast-storm) grid 0.005 0.01 storm ;;
 grid-storm) grid 0.05 0.1 storm ;;
 grid-idle) grid 0.05 0.1 idle ;;
esac
cleanup
