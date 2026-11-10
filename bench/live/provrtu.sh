#!/bin/bash
# provrtu.sh : provider with one slice endpoint per RTU (each carries only that RTU's <input> tags), no whole-model stream
pkill -x ferrofire; sleep 1; cd /root/ff || exit 1
R1=lowerwater.height,lock.height,lowergate.open,lowervalve.open,lowersensor.active,lowersensor.direction,locksensor.active,locksensor.direction,boat.location,boat.direction
R2=upperwater.height,lock.height,uppergate.open,uppervalve.open,uppersensor.active,uppersensor.direction,locksensor.active,locksensor.direction
setsid nohup ./ferrofire run waterway.yaml --server tcp://172.16.1.2:5555 --publish-tags "udp://*;239.0.1.1:40000=$R1" --publish-tags "udp://*;239.0.1.2:40000=$R2" > prov.log 2>&1 < /dev/null &
sleep 2; ps -eo pid,cmd | grep "[f]errofire run" | cut -c1-120; head -3 prov.log | cut -c1-250
