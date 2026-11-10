#!/bin/bash
# rtudg.sh ENDPOINT : on an RTU, stop the daemon/watcher and run the stock bennu-field-device on a COPY of /etc/sceptre/config.xml whose
# <input><endpoint> is ENDPOINT (per-RTU group run); the original config.xml is never touched. rtudg.sh restore brings the originals back.
cd /root/ff || exit 1
pkill -f "[b]ennu-watcherd"; pkill -f "[b]ennu-field-device"; pkill -f "[f]errofire pull"; sleep 1
if [ "$1" = restore ]; then
  rm -f rtu.xml fd.log
  setsid nohup bennu-watcherd restart --f /etc/sceptre/config.xml --b /home/sceptre/bennu-field-deviced.firmware --w /etc/sceptre/watcher.ini >/dev/null 2>&1 </dev/null &
  sleep 3; bennu-field-deviced --c restart --env default --file /etc/sceptre/config.xml >/dev/null 2>&1; sleep 5
  ps -eo pid,cmd | grep "[b]ennu"; exit 0
fi
sed "s#<endpoint>udp://[^<]*</endpoint>#<endpoint>$1</endpoint>#" /etc/sceptre/config.xml > rtu.xml
grep -n "<endpoint>" rtu.xml
setsid nohup bennu-field-device --file rtu.xml > fd.log 2>&1 </dev/null &
sleep 4; ps -eo pid,cmd | grep "[b]ennu"; tail -n 2 fd.log | cut -c1-160
