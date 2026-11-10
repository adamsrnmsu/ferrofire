#!/bin/bash
# top-live.sh : run ON waterway-provider. `ferrofire top` in two tmux panes (attached to the local provider, and on the multicast stream), captured as
# text screens while the boat moves, while the provider is SIGSTOPped for 5 s (the tags go STALE on the viewer's clock), and after SIGCONT; plus --once dumps.
cd /root/ff || exit; S="--server tcp://172.16.1.2:5555"; PID=$(pgrep -x ferrofire | head -1)
tmux kill-server 2>/dev/null; tmux new-session -d -s top -x 100 -y 30 "./ferrofire top --name run --stale-after 2 2>/tmp/top.err; echo exited \$? >>/tmp/top.err; sleep 30"
tmux new-session -d -s strm -x 100 -y 30 "./ferrofire top --stream 'udp://172.16.1.2;239.0.0.1:40000' $S --stale-after 2 2>/tmp/strm.err; sleep 30"
shot() { echo; echo "=== $1  ($(date -u +%T))"; echo "--- ferrofire top --name run"; tmux capture-pane -p -t top:0.0 | sed -e 's/[[:space:]]*$//' | sed '/^$/d' | $F; echo "--- ferrofire top --stream"; tmux capture-pane -p -t strm:0.0 | sed -e 's/[[:space:]]*$//' | sed '/^$/d' | $F; }
F="cat"; sleep 2; ./ferrofire probe $S write lowervalve.opn=1 --confirm >/dev/null 2>&1; sleep 3; shot "boat moving, a rejected WRITE in the log pane of the first view"
F="grep -E ^(/run|stream|tag|boat.location|lock.height|uppergate|uppervalve|locksensor.active|log|│.*[a-z])"
sleep 2; shot "2 s later: boat.location and ages have moved"
kill -STOP $PID; echo; echo "##### provider SIGSTOP at $(date -u +%T.%N | cut -c1-12)"
sleep 1; shot "stopped +1 s"
sleep 3; shot "stopped +4 s: no update for more than 2 s"
kill -CONT $PID; echo; echo "##### provider SIGCONT at $(date -u +%T.%N | cut -c1-12)"
sleep 1.5; shot "resumed +1.5 s"
tmux send-keys -t top:0.0 q; tmux send-keys -t strm:0.0 q; sleep 1; cat /tmp/top.err; tmux kill-server
echo; echo "=== --once dumps (stale-after 2): text, then json"
./ferrofire top --name run --once --filter 'boat.*'; sleep 2; ./ferrofire top --name run --once --filter 'boat.location'
timeout 5 ./ferrofire top --name run --once --json --filter 'lock.*'
echo "--- provider stopped 3 s: --stream --once with no QUERY needed (--tags); the control-socket view cannot ask a stopped process"
kill -STOP $PID; sleep 3; timeout 10 ./ferrofire top --stream 'udp://172.16.1.2;239.0.0.1:40000' --tags lock.height --once 2>&1 | head; echo "exit ${PIPESTATUS[0]}"; kill -CONT $PID
