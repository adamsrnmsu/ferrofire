#!/bin/bash
# bench/solve_time.sh FERROFIRE_LINUX_BINARY GRIDDIR > docs/bench/results/control/solve-time.txt
# Power-flow re-solve time of the control-bench grid (GRIDDIR from `python3 bench/gen_grid.py GRIDDIR`), ferrofire `check` in an
# ubuntu:22.04 arm64 container on the default Docker daemon. Re-solve cost = (200 sequential breaker writes - none) / 200,
# each write at its own model time so each one triggers a solve. Host load is recorded; pybennu is not installed offline (not measured).
set -e
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1"); GD=$(cd "$2" && pwd)
python3 - "$GD" <<'P'
import json, sys
d = sys.argv[1]; g = json.load(open(d + '/grid.json'))
w = lambda k: f"--write {g['breakers'][k]}"
open(d + '/w_seq.args', 'w').write(' '.join(f"{w(g['flip_order'][i // 2 % 500])}={i % 2}@{0.05 * (i + 1):.2f}" for i in range(200)))
open(d + '/w_open.args', 'w').write(' '.join(f"{w(k)}=0@0.1" for k in g['flip_order']))
open(d + '/w_gens.args', 'w').write(' '.join(f"--write generator-1_bus-{b}.active=0@0.1" for b in g['gen_buses']))
P
echo "date $(date -u +%FT%TZ)"; echo "host_load $(uptime | sed 's/.*load averages*: //')"; docker info --format 'docker_vm cpus={{.NCPU}} mem={{.MemTotal}}'
echo "binary sha256 $(shasum -a 256 "$BIN" | cut -c1-16) mtime $(date -r "$BIN" +%FT%T) (built outside this script; the commit is not recorded)"
out=$(docker run --rm --platform linux/arm64 -v "$BIN":/ff:ro -v "$GD":/g -w /g ubuntu:22.04 bash -c '
TIMEFORMAT=%R
s() { local a=$(date +%s%N); "$@" >/dev/null 2>&1; echo $(( ($(date +%s%N) - a) / 1000000 )); }
M=R1/model.yaml
for i in 1 2 3 4 5; do
  echo "rep $i none_ms $(s /ff check $M --steps 205) seq200_ms $(s /ff check $M --steps 205 $(cat w_seq.args)) open1001_ms $(s /ff check $M --steps 4 $(cat w_open.args)) gens_off_ms $(s /ff check $M --steps 4 $(cat w_gens.args))"
done')
echo "$out"
echo "$out" | awk '/^rep/ {n++; a+=$4; b+=$6} END {printf "per_solve_ms %.1f (mean seq200 minus mean none, over 200 solves, %d reps)\n", (b-a)/n/200, n}'
