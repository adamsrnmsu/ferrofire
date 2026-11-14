#!/bin/bash
# bench/stock_capacity.sh > docs/bench/results/control/stock-capacity.txt   (ww-1df.35)
# Can the stock provider (bench/stock_provider.py, 1 250-branch grid) keep its publish interval? One idle provider container per interval (no writes, no RTUs),
# 55 s, from the `Publish-stats:` lines (one per 10 s): pack + send time of one publish, publishes per 10 s window (effective period), process CPU % of one core
# (delta of cpu_s over the lines after the first, so start-up is left out). Needs target-linux/control-grid (bench/gen_grid.py) and the ffc-stock:22.04 image.
cd "$(dirname "$0")/.." || exit
echo "date $(date -u +%FT%TZ)"; echo "host_load $(uptime | sed 's/.*load averages*: //')"; docker info --format 'docker_vm cpus={{.NCPU}} mem={{.MemTotal}}'
for P in 1 0.5 0.2 0.1 0.05; do
  docker rm -f ffcap >/dev/null 2>&1
  docker run -d --name ffcap --platform linux/arm64 -v "$PWD":/src -v "$PWD/target-linux/control-grid/case.py":/etc/sceptre/case_control.py:ro -e STOCK_PUBLISH_S=$P -e STOCK_PUBLISH_STATS=1 \
    ffc-stock:22.04 python3 /src/bench/stock_provider.py start -c /src/target-linux/control-grid/R1/config.ini >/dev/null
  sleep 55
  docker logs ffcap 2>&1 | grep -a 'Publish-stats' | python3 -c '
import re, sys
L = [re.findall(r"(\w+)=([\d.]+)", l) for l in sys.stdin]
L = [{k: float(v) for k, v in l} for l in L][1:]
n = len(L)
dt = (L[-1]["t_ns"] - L[0]["t_ns"]) / 1e9
pubs = sum(l["publishes"] for l in L[1:])
print("interval %s windows %d pack_ms_mean %.1f send_ms_mean %.1f work_ms %.1f effective_period_ms %.0f publishes_per_s %.1f cpu_pct %.1f" % (sys.argv[1], n,
      sum(l["pack_ms_mean"] for l in L) / n, sum(l["send_ms_mean"] for l in L) / n, sum(l["pack_ms_mean"] + l["send_ms_mean"] for l in L) / n,
      dt * 1000 / pubs, pubs / dt, 100 * (L[-1]["cpu_s"] - L[0]["cpu_s"]) / dt))' $P
  docker rm -f ffcap >/dev/null
done
