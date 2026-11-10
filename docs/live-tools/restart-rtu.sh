#!/bin/bash
# restart-rtu.sh : on an RTU VM, restart the stock field device exactly as /etc/phenix/startup/sceptre-start.sh starts it (new IGMP join, same config).
# (pkill -f bennu-field-deviced also kills the watcher, whose arguments name the binary, and leaves a stale pid file that makes the next watcher refuse to start.)
pkill -x bennu-watcherd; pkill -x bennu-field-dev; sleep 1; rm -f /run/default-bennu-field-deviced.pid
setsid nohup bennu-watcherd restart --f /etc/sceptre/config.xml --b /home/sceptre/bennu-field-deviced.firmware --w /etc/sceptre/watcher.ini >/dev/null 2>&1 < /dev/null &
sleep 12; ps -eo pid,user,etime,cmd | grep "[b]ennu-field-deviced --c"
