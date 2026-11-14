#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
source /var/tmp/ff-prof/scripts/lib.sh
IMG=ffprof-perf:22.04
guard && cleanup
dr --name ffp-prov --network-alias provider $IMG $FF run /out/m8112.yaml --rate 0.1 >/dev/null; sleep 3
dr --name ffp-fd --network-alias rtu $IMG $FF field-device --file /out/rtu8112-iccp.xml >/dev/null; sleep 8
docker logs ffp-fd 2>&1 | grep -iE "iccp|error|warn" | head -3 | cut -c1-200
dr --name ffp-iccp $IMG /src/target-prof/release/iccpload rtu:102 60 100 >/dev/null; sleep 6
perfrec w5-iccp-mms-poll 40 fd=ffp-fd iccp=ffp-iccp prov=ffp-prov
sleep 16; docker logs ffp-iccp 2>&1 | tail -3 | tee /var/tmp/ff-prof/w5-iccp-mms-poll/client.json
cleanup
