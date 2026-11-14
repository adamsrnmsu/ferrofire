#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
source /var/tmp/ff-prof/scripts/lib.sh
IMG=ffprof-perf:22.04; CLI=/src/target-prof/release/ffbench
guard && cleanup
dr --name ffp-prov --network-alias provider $IMG $FF run /src/bench/models/m800.yaml --rate 0.01 >/dev/null
sleep 3; docker logs ffp-prov 2>&1 | tail -1 | cut -c1-160
# 4a: datagram encode (provider) + decode (ffbench dg --whole) at 800 tags 100 Hz
dr --name ffp-dg --network-alias dg $IMG $CLI dg --whole --period 0.01 --secs 55 --host provider >/dev/null
sleep 6
perfrec w4a-datagram-800-100hz 40 prov=ffp-prov dg=ffp-dg
docker logs ffp-dg 2>&1 | head -c 1500
docker rm -f ffp-dg >/dev/null
# 4b: REQ/REP READ / QUERY / WRITE loops (ffbench client), provider still publishing 800 tags at 100 Hz
dr --name ffp-cl --network-alias cl $IMG $CLI client --transport zmq --host provider --n 800 --period 0.01 --warmup 1 --pub-secs 1 --req-secs 14 >/dev/null
sleep 3
perfrec w4b-reqrep-800-100hz 42 prov=ffp-prov cl=ffp-cl
sleep 4; docker logs ffp-cl 2>&1 | python3 -c "
import sys,json
d=json.load(sys.stdin)
for k in ('read','query','write'): print(k, json.dumps(d[k])[:300])
" | tee /var/tmp/ff-prof/w4b-reqrep-800-100hz/client.txt
cleanup
