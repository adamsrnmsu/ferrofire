#!/bin/bash
# fdlog.sh [label]: on an RTU, is the stock field device alive and what did it log (error-ish lines, unknown/ignored points, last lines)?
H=$(hostname); L=/etc/sceptre/log; mkdir -p /root/ff/live-out; exec > >(tee /root/ff/live-out/fdlog-$1-$H.txt) 2>&1; [ -f /root/ff/fd.log ] && F=/root/ff/fd.log || F=$L/default-bennu-field-deviced-stdout.log
echo "== $H fdlog $1 $(date -u +%FT%TZ) log=$F"
ps -eo pid,etimes,cmd | grep -E "[b]ennu-field-device|[b]ennu-watcherd"
echo "lines $(wc -l < $F), error-ish: $(grep -c -i -E 'error|exception|abort|terminate|fail|segv|malformed|invalid' $F), timeouts/retries/offline: $(grep -c -i -E "timeout|timed out|retry|retrying|retries|lazy|reconnect|offline|abandon" $F), unknown: $(grep -c -i unknown $F), seq/_ff mentions: $(grep -c '_ff' $F)"
grep -i -E 'error|exception|abort|terminate|fail|segv|malformed|invalid|timeout|retry|retrying|offline|abandon|reconnect|_ff' $F | sort | uniq -c | sort -rn | head -8 | cut -c1-200
for g in $L/default-bennu-field-deviced-stderr.log $L/watcher-stderr.log $L/watcher-stdout.log /root/ff/fd.log; do [ -f $g ] && echo "stderr/other $g: $(grep -c -i -E 'error|exception|abort|fail' $g) error-ish of $(wc -l < $g) lines"; done
dmesg 2>/dev/null | grep -i -E "segfault|bennu" | tail -3
echo "-- message mix of the device log (LOGIC lines dropped, numbers masked)"; grep -v LOGIC $F | sed 's/[0-9.]\+/N/g' | sort | uniq -c | sort -rn | head -6 | cut -c1-160
