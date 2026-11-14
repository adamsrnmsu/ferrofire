#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
source /var/tmp/ff-prof/scripts/lib.sh
IMG=ffprof-perf:22.04
guard && cleanup; D=/src/target-linux/control-grid/R10
dr --name ffp-prov --network-alias provider $IMG $FF run $D/model.yaml --stamp-writes >/dev/null
for n in 1 2 3 4 5 6 7 8 9 10; do dr --name ffp-rtu-$n --network-alias rtu-$n $IMG $FF field-device --file $D/ferrofire/rtu-$n.xml >/dev/null; done
sleep 8
CELLS=$(python3 -c "print(','.join(['breaker:sim:1000']*400))")
dr --name ffp-drv $IMG /src/target-prof/release/ffcontrol --scada $D/scada.json --provider tcp://provider:5555 --stream 'udp://*;239.0.1.{n}:40000' --cells $CELLS --stack ferrofire --settle-s 0.05 --out /out/w1b2-ffc >/dev/null
sleep 20; docker logs ffp-drv 2>&1 | tail -3
perfrec w1b2-grid-storm-fast 45 prov=ffp-prov rtu=ffp-rtu-1 drv=ffp-drv
docker logs ffp-drv 2>&1 | tail -3; wc -l /var/tmp/ff-prof/w1b2-ffc.cells.csv
docker stats --no-stream --format '{{.Name}} {{.CPUPerc}} {{.MemUsage}}' | grep -E "prov|rtu-1 |drv"
cleanup
