#!/bin/bash
# prov.sh PUBLISH_ENDPOINT : start ferrofire provider (replaces any running one)
pkill -x ferrofire; pkill -x simulinksolver; pkill -x bennu-simulink-; sleep 1
cd /root/ff || exit 1
setsid nohup ./ferrofire run waterway.yaml --server tcp://172.16.1.2:5555 --publish "$1" > prov.log 2>&1 < /dev/null &
sleep 2; ps -eo pid,cmd | grep "[f]errofire run"; tail -3 prov.log
