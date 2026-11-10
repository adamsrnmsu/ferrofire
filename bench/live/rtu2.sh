#!/bin/bash
cd /root/ff || exit 1
pkill -f "[b]ennu-watcherd"; pkill -f "[b]ennu-field-device"; pkill -f "[f]errofire pull"; sleep 1
if [ "$1" = restore ]; then
  setsid nohup bennu-watcherd restart --f /etc/sceptre/config.xml --b /home/sceptre/bennu-field-deviced.firmware --w /etc/sceptre/watcher.ini >/dev/null 2>&1 </dev/null &
  sleep 4; ps -eo pid,cmd | grep "[b]ennu"; exit 0
fi
EP=$2
sed "s#<endpoint>udp://[^<]*</endpoint>#<endpoint>$EP</endpoint>#" /etc/sceptre/config.xml > rtu.xml
grep -n "<endpoint>" rtu.xml
setsid nohup ./ferrofire pull-bridge --server tcp://172.16.1.2:5555 --transport zmq --rate ${RATE:-100} --tags-from rtu.xml --publish "$EP" > bridge.log 2>&1 </dev/null &
sleep 1
setsid nohup bennu-field-device --file rtu.xml > fd.log 2>&1 </dev/null &
sleep 4
ps -eo pid,cmd | grep "[f]errofire\|[b]ennu"; tail -n 3 bridge.log; tail -n 3 fd.log
