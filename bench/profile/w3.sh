#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
source /var/tmp/ff-prof/scripts/lib.sh
IMG=ffprof-perf:22.04
RATE=${RATE:-0.05}; W=${W:-w3-fd-8112-20hz-modbus}
guard && cleanup
dr --name ffp-prov --network-alias provider $IMG $FF run /out/m8112.yaml --rate $RATE >/dev/null
sleep 3
dr --name ffp-fd --network-alias rtu $IMG $FF field-device --file /out/rtu8112.xml >/dev/null
sleep 8; docker logs ffp-fd 2>&1 | tail -3 | cut -c1-200
docker run -d $LBL --network $NET -v /var/tmp/ff-prof:/out --name ffp-load $IMG python3 /out/scripts/mbload.py rtu 62 4 4 2 >/dev/null
docker run -d $LBL --network $NET -v /var/tmp/ff-prof:/out --name ffp-dnp $IMG python3 /out/scripts/dnpload.py rtu 62 50 >/dev/null
sleep 8
perfrec $W 45 prov=ffp-prov fd=ffp-fd load=ffp-load dnp=ffp-dnp
sleep 12; docker logs ffp-load 2>&1 | tail -2 | tee /var/tmp/ff-prof/$W/load.json; docker logs ffp-dnp 2>&1 | tail -2 | tee /var/tmp/ff-prof/$W/dnp.json
cleanup
