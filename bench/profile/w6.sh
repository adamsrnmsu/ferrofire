#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
source /var/tmp/ff-prof/scripts/lib.sh
IMG=ffprof-perf:22.04; D=/src/target-linux/control-grid/R10; OUTD=/var/tmp/ff-prof/w6-sweep; mkdir -p $OUTD
guard && cleanup
grid() { # dt rate mode(idle|storm)
  dt=$1; rate=$2; mode=$3; W=w6a-grid-$mode-dt$dt-pub$rate
  sed -e "s/^dt: .*/dt: $dt/" -e "s/^publish_rate: .*/publish_rate: $rate/" -e "s#case: ../case.py#case: /src/target-linux/control-grid/case.py#" /Users/ryanadams/git/ferrofire/target-linux/control-grid/R10/model.yaml > /var/tmp/ff-prof/model-sweep.yaml
  dr --name ffp-prov --network-alias provider $IMG $FF run /out/model-sweep.yaml --stamp-writes >/dev/null
  for n in 1 2 3 4 5 6 7 8 9 10; do dr --name ffp-rtu-$n --network-alias rtu-$n $IMG $FF field-device --file $D/ferrofire/rtu-$n.xml >/dev/null; done
  sleep 6
  if [ $mode = storm ]; then
    CELLS=$(python3 -c "print(','.join(['breaker:sim:1000']*400))")
    dr --name ffp-drv $IMG /src/target-prof/release/ffcontrol --scada $D/scada.json --provider tcp://provider:5555 --stream 'udp://*;239.0.1.{n}:40000' --cells $CELLS --stack ferrofire --settle-s 0.05 --out /out/w6-sweep/ffc-$W >/dev/null
    sleep 12
  fi
  perfrec $W 25 prov=ffp-prov | grep pct
  docker ps -aq --filter label=ffprof=1 | xargs -r docker rm -f >/dev/null
}
for p in "0.05 0.1" "0.02 0.05" "0.01 0.02" "0.005 0.01"; do for m in idle storm; do grid $p $m; done; done
big() { rate=$1; W=w6b-big8112-pub$rate
  dr --name ffp-prov --network-alias provider $IMG $FF run /out/m8112.yaml --rate $rate >/dev/null; sleep 5
  perfrec $W 25 prov=ffp-prov | grep pct; cleanup; }
for r in 0.1 0.05 0.02 0.01; do big $r; done
m800() { rate=$1; W=w6c-m800-pub$rate
  dr --name ffp-prov --network-alias provider $IMG $FF run /src/bench/models/m800.yaml --rate $rate >/dev/null; sleep 5
  perfrec $W 25 prov=ffp-prov | grep pct; cleanup; }
for r in 0.1 0.05 0.02 0.01; do m800 $r; done
