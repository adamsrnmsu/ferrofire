#!/bin/bash
# usage: ovsb.sh before|after LABEL ; run from the repo root on the Mac: per-port OVS counters of the range bridge (the minimega container's 'phenix'),
# saved to docs/bench/results/live/ovs-LABEL-{before,after}.txt with port -> VM names from `mm vm info`; `ovsb.sh diff LABEL` prints the delta table.
set -e
D=docs/bench/results/live; mkdir -p $D
X="limactl shell phenix sudo docker exec minimega"
case $1 in
before|after)
  { $X ovs-ofctl dump-ports phenix; echo "-- ports"; $X ovs-vsctl --columns=name,ofport,external_ids list Interface | grep -E "^(name|ofport)" | paste - - ; echo "-- snooping"; $X ovs-vsctl get Bridge phenix mcast_snooping_enable; echo "-- vms"; $X mm namespace waterway .columns name,tap vm info 2>&1 | grep -v INFO; echo "-- end"; $X ovs-appctl mdb/show phenix 2>&1 | head -20; } > $D/ovs-$2-$1.txt ;;
diff)
  python3 - "$D/ovs-$2-before.txt" "$D/ovs-$2-after.txt" <<'PY'
import re,sys
def load(f):
    t=open(f).read(); a,_,rest=t.partition("-- ports"); ports,_,_=rest.partition("-- snooping")
    vm={}
    for l in t.partition("-- vms")[2].partition("-- end")[0].splitlines():
        m=re.match(r'.*\|\s*(\S+)\s*\|\s*\[(.*)\]',l)
        if m:
            for i,tp in enumerate(m.group(2).split(", ")): vm[tp.strip()]=m.group(1)+"/"+str(i)
    names={}
    for l in ports.strip().splitlines():
        m=re.search(r'name\s*:\s*"?([^"\s]+)"?\s+ofport\s*:\s*(\d+)',l)
        if m: names[m.group(2)]=vm.get(m.group(1),m.group(1))
    st={}; cur=None
    for l in a.splitlines():
        m=re.match(r'\s+port\s+(\S+?):\s+rx pkts=(\d+), bytes=(\d+).*',l)
        if m: cur=m.group(1); st[cur]=[int(m.group(2)),int(m.group(3)),0,0]; continue
        m=re.match(r'\s+tx pkts=(\d+), bytes=(\d+)',l)
        if m and cur: st[cur][2]=int(m.group(1)); st[cur][3]=int(m.group(2))
    return st,names
b,n=load(sys.argv[1]); a,_=load(sys.argv[2])
print("%-14s %-22s %12s %12s %12s %12s"%("port","name","rx pkts","rx bytes","tx pkts","tx bytes"))
for p in sorted(a,key=lambda x:(len(x),x)):
    d=[x-y for x,y in zip(a[p],b.get(p,[0]*4))]
    print("%-14s %-22s %12d %12d %12d %12d"%(p,n.get(p,""),*d))
PY
;;
esac
