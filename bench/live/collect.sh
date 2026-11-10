#!/bin/bash
# usage: collect.sh host... ; run from the repo root after a live capture: copies each VM's /root/ff/live-out/ (the
# cap.sh summaries) into docs/bench/results/live/<host>/ so the numbers quoted in the docs have their evidence committed.
for h in "$@"; do
  mkdir -p "docs/bench/results/live/$h"
  scp -q "root@$h:/root/ff/live-out/*" "docs/bench/results/live/$h/" || echo "collect: nothing from $h" >&2
done
