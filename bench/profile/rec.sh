#!/bin/bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
# rec.sh WORKLOAD SECS name=pid [name=pid...]   (runs in the ffprof-perf container: --privileged --pid=host)
W=$1; S=$2; shift 2
P=$(ls /usr/lib/linux-tools/*/perf)
O=/out/$W; mkdir -p $O
echo -1 > /proc/sys/kernel/perf_event_paranoid
declare -A T0
tick() { awk '{print $14+$15}' /proc/$1/stat; }
for a in "$@"; do n=${a%%=*}; p=${a##*=}
  T0[$n]=$(tick $p)
  $P record -F 499 -g -e cpu-clock -p $p -o /tmp/$n.data -- sleep $S >/tmp/$n.rec.log 2>&1 &
done
W0=$(date +%s.%N)
wait
W1=$(date +%s.%N)
HZ=$(getconf CLK_TCK)
: > $O/cpu.txt
for a in "$@"; do n=${a%%=*}; p=${a##*=}
  t1=$(tick $p 2>/dev/null || echo ${T0[$n]})
  echo "$n pid=$p cpu_pct_of_one_core=$(awk -v a=$t1 -v b=${T0[$n]} -v h=$HZ -v w0=$W0 -v w1=$W1 'BEGIN{printf "%.1f",(a-b)/h/(w1-w0)*100}') wall_s=$(awk -v w0=$W0 -v w1=$W1 'BEGIN{printf "%.1f",w1-w0}')" >> $O/cpu.txt
  cp /tmp/$n.data $O/$n.perf.data
  $P script -i /tmp/$n.data 2>/dev/null | /opt/FlameGraph/stackcollapse-perf.pl > $O/$n.folded
  tail -2 /tmp/$n.rec.log >> $O/cpu.txt
done
cat $O/cpu.txt
