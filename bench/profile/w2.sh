#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
source /var/tmp/ff-prof/scripts/lib.sh
IMG=ffprof-perf:22.04
guard && cleanup
dr --name ffp-prov --network-alias provider $IMG $FF run /src/models/waterway.yaml >/dev/null
sleep 5; docker logs ffp-prov 2>&1 | tail -1 | cut -c1-200
perfrec w2a-waterway 40 prov=ffp-prov
docker rm -f ffp-prov >/dev/null
dr --name ffp-prov --network-alias provider $IMG $FF run /out/m8112.yaml --rate 0.1 >/dev/null
sleep 6; docker logs ffp-prov 2>&1 | tail -1 | cut -c1-200
perfrec w2b-big8112-10hz 45 prov=ffp-prov
cleanup
