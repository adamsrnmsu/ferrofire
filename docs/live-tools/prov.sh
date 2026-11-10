#!/bin/bash
# prov.sh : (re)start `ferrofire run` as the range's provider on waterway-provider, replacing the stock bennu-simulink-provider
pkill -x ferrofire; pkill -x simulinksolver; pkill -x bennu-simulink-; sleep 1
cd /root/ff || exit 1
setsid nohup ./ferrofire run waterway.yaml --server tcp://172.16.1.2:5555 --publish 'udp://*;239.0.0.1:40000' > prov.log 2>&1 < /dev/null &
sleep 2; ps -eo pid,cmd | grep "[f]errofire run"; tail -3 prov.log
