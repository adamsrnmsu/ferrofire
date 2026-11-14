#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
source /var/tmp/ff-prof/scripts/lib.sh
guard && cleanup; D=/src/target-linux/control-grid/R10
dr --name ffp-prov --network-alias provider ffprof-perf:22.04 $FF run $D/model.yaml --stamp-writes >/dev/null
for n in 1 2 3 4 5 6 7 8 9 10; do dr --name ffp-rtu-$n --network-alias rtu-$n ffprof-perf:22.04 $FF field-device --file $D/ferrofire/rtu-$n.xml >/dev/null; done
sleep 8; docker logs ffp-prov 2>&1 | tail -3
perfrec w1a-grid-idle 45 prov=ffp-prov rtu=ffp-rtu-1
docker stats --no-stream --format '{{.Name}} {{.CPUPerc}} {{.MemUsage}}'
cleanup
