#!/bin/bash
# usage: rng.sh <host|all> <max wait secs> <cmd...>  : run a bash snippet on range VMs through minimega cc (base64-wrapped so minimega does not
# expand $vars or eat quotes), poll until every targeted client answered, print the responses
F=$1; W=$2; shift 2
mm() { limactl shell phenix sudo docker exec minimega mm "$@" 2>&1 | grep -v "INFO main.go"; }
N0=${RNG_N:-}
B=$(printf '%s' "$*" | base64 | tr -d '\n')
mm namespace waterway cc delete command all >/dev/null; mm namespace waterway cc delete response all >/dev/null
if [ "$F" = all ]; then mm namespace waterway clear cc filter >/dev/null; N=9; else mm namespace waterway cc filter hostname=$F >/dev/null; N=1; fi
mm namespace waterway cc exec bash -c "echo $B | base64 -d | bash" >/dev/null
[ -n "$N0" ] && N=$N0
for _ in $(seq 1 "$W"); do
  sleep 1; o=$(mm namespace waterway cc responses all | grep -v "minirouter-router")
  [ "$(echo "$o" | grep -c -E '^[0-9]+/[0-9a-f-]+/stdout|^lima-phenix: [0-9]+/[0-9a-f-]+/stdout')" -ge $N ] && break
done
echo "$o"
true
