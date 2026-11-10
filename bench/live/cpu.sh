#!/bin/bash
[ "$(hostname)" = waterway-provider ] || exit 0
P=$(pgrep -f "[f]errofire run|[b]ennu-simulink-provider|[s]imulinksolver")
t(){ s=0; for p in $P; do s=$((s+$(awk '{print $14+$15}' /proc/$p/stat))); done; echo $s; }
a=$(t); sleep $1; b=$(t)
echo "provider pids $P ticks $a -> $b = $((b-a)) over ${1}s ($(( (b-a)*100/$1/100 )).$(( (b-a)*100/$1%100 ))% core, ticks/s*)"
