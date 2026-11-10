#!/bin/bash
# fetch.sh GLOB... : from the repo root on the Mac, copy /root/ff/live-out/GLOB from provider, rtu-1 and rtu-2 (through minimega cc, no ssh
# on this range) into docs/bench/results/live/<host>/ . This range has no host route to the guests, so collect.sh (scp) is not usable here.
for h in waterway-provider rtu-1 rtu-2; do
  mkdir -p docs/bench/results/live/$h
  "$(dirname "$0")"/rng.sh $h 60 "cd /root/ff/live-out && for f in $*; do [ -f \$f ] && { echo \"@@@ \$f\"; cat \$f; echo \"@@@ end\"; }; done" | python3 -c "
import sys,re
t=sys.stdin.read(); d='docs/bench/results/live/$h/'
for m in re.finditer(r'@@@ (\S+)\n(.*?)\n?@@@ end',t,re.S):
    if m.group(1)!='end': open(d+m.group(1),'w').write(m.group(2)+'\n'); print('$h',m.group(1),len(m.group(2)))
"
done
