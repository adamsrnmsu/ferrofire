#!/bin/bash
# matrix.sh : the ww-2s0 live matrix, run from the repo root on the Mac. For each of push stock / push ferrofire / pull 100 ms / pull 1000 ms:
# 3 independent reps (provider and RTU processes restarted each time), 30 s captures (cap.sh) on provider, rtu-1 and rtu-2, labels
# <cfg>-<n>. Finishes with the RTUs and the provider back on the original stock wiring (rtudg.sh restore).
R="$(dirname "$0")/rng.sh"
prov() { $R waterway-provider 40 "$1" >/dev/null; }
rtus() { RNG_N=2 $R all 90 "case \$(hostname) in rtu-1|rtu-2) $1; echo ok;; esac" >/dev/null; }
cap() { RNG_N=3 $R all 30 "case \$(hostname) in waterway-provider|rtu-1|rtu-2) setsid nohup /root/ff/cap.sh $1 30 >/dev/null 2>&1 </dev/null & sleep 1; echo started;; esac" | grep -c started; sleep 38; }
# the stock provider and its solver are two processes (the range's startup script launches both); a backgrounded `cd && cmd &` would keep the cc pipe open
STOCK='pkill -x ferrofire; pkill -x simulinksolver; pkill -f bennu-simulink-provider; sleep 1; cd /etc/sceptre; setsid nohup bennu-simulink-provider --server-endpoint tcp://172.16.1.2:5555 --publish-endpoint "udp://*;239.0.0.1:40000" > /root/ff/stock.log 2>&1 </dev/null & sleep 1; cd /etc/sceptre; setsid nohup ./simulinksolver -tf inf > /root/ff/solver.log 2>&1 </dev/null & sleep 3; ps -eo pid,cmd | grep "[b]ennu-simulink\|[s]imulinksolver" | cut -c1-110'
want() { [ -z "$CFGS" ] || [[ " $CFGS " == *" $1 "* ]]; }   # CFGS="pull100 pull1000" reruns a subset
for n in 1 2 3; do
  want pushstock && { prov "$STOCK"; rtus '/root/ff/rtudg.sh restore >/dev/null'; sleep 10; cap pushstock-$n; }
  want pushff && { prov '/root/ff/prov.sh "udp://*;239.0.0.1:40000"'; rtus '/root/ff/rtudg.sh restore >/dev/null'; sleep 10; cap pushff-$n; }
  want pull100 && { prov '/root/ff/prov.sh "udp://127.0.0.1:40000"'; rtus 'RATE=100 /root/ff/rtu2.sh start udp://127.0.0.1:40000 >/dev/null'; sleep 8; cap pull100-$n; }
  want pull1000 && { prov '/root/ff/prov.sh "udp://127.0.0.1:40000"'; rtus 'RATE=1000 /root/ff/rtu2.sh start udp://127.0.0.1:40000 >/dev/null'; sleep 8; cap pull1000-$n; }
done
rtus '/root/ff/rtudg.sh restore >/dev/null'
prov '/root/ff/prov.sh "udp://*;239.0.0.1:40000"'
