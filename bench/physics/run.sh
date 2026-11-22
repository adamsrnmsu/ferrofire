#!/bin/sh
# bennu's own physics providers driving a field device (ww-82n): sh bench/physics/run.sh [opendss|helics|helics-opendss|all] (default all).
#   opendss: pybennu-power-solver, solver OpenDSS, on bennu's sunspec feeder ($BENNU/src/gobennu/testing/sunspec/system.dss).
#   helics-opendss: helics_broker + dss_federate.py (our own HELICS federate around OpenDSS, ww-dkf) + pybennu-power-solver, solver Helics.
#   helics:  helics_broker + bennu's test power federate (pybennu-test-ep-server-helics) + pybennu-power-solver, solver Helics.
# Each runs twice in a fresh ff-phys-* container, once with ferrofire's field device and once with stock bennu-field-device, and
# check.py drives the device over Modbus. PASS when both DUTs pass every check and their `state:` lines match (compare.py: registers within 2 counts).
# env: FF_BIN (default target-linux/release/ferrofire) IMAGE (ff-physics:22.04, built from bench/physics/Dockerfile when missing)
#      OUT (target/physics) BENNU (~/git/sceptre-bennu: system.dss is copied from it, not vendored: bennu is GPL-3.0).
#      Only ff-phys-* containers are created and removed.
set -eu
here=$(cd "$(dirname "$0")" && pwd); root=$here/../..
BENNU=${BENNU:-$HOME/git/sceptre-bennu}; DSS=$BENNU/src/gobennu/testing/sunspec/system.dss
FF_BIN=${FF_BIN:-$root/target-linux/release/ferrofire}; IMAGE=${IMAGE:-ff-physics:22.04}; OUT=${OUT:-$root/target/physics}
[ -f "$DSS" ] || { echo "no $DSS: set BENNU to a sceptre-bennu checkout" >&2; exit 2; }
[ -x "$FF_BIN" ] || { echo "no $FF_BIN: build it (cargo build --release in ff-rust:22.04) or set FF_BIN" >&2; exit 2; }
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build -t "$IMAGE" "$here"
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)

# the provider stack of a scenario, started in container $1
stack() {
  case $2 in
    opendss) docker exec -d "$1" sh -c 'cd /physics && pybennu-power-solver -c solver.ini start > /tmp/prov.out 2>&1' ;;
    helics)
      docker exec -d "$1" sh -c 'cd /physics && helics_broker -f 2 > /tmp/broker.out 2>&1'
      docker exec -d "$1" sh -c 'cd /physics && pybennu-test-ep-server-helics -d -c federate.json > /tmp/fed.out 2>&1'
      docker exec -d "$1" sh -c 'cd /physics && pybennu-power-solver -c solver.ini start > /tmp/prov.out 2>&1' ;;
    helics-opendss)
      docker exec -d "$1" sh -c 'cd /physics && helics_broker -f 2 > /tmp/broker.out 2>&1'
      docker exec -d "$1" sh -c 'cd /physics && python3 dss_federate.py system.dss dss_federate.json > /tmp/fed.out 2>&1'
      docker exec -d "$1" sh -c 'cd /physics && pybennu-power-solver -c solver.ini start > /tmp/prov.out 2>&1' ;;
  esac
}

one() {   # scenario dut -> $OUT/<scenario>-<dut>.{txt,dut.out,prov.out,fed.out}
  c=ff-phys-$1-$2; docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" "$IMAGE" sleep 900 >/dev/null
  docker cp "$here/$1/." "$c:/physics"; docker cp "$here/check.py" "$c:/physics/check.py"; docker cp "$DSS" "$c:/physics/system.dss"
  stack "$c" "$1"; sleep 4
  if [ "$2" = rust ]; then
    docker cp "$FF_BIN" "$c:/usr/local/bin/ferrofire"
    docker exec -d "$c" sh -c 'cd /physics && ferrofire field-device --file device.xml > /tmp/dut.out 2>&1'
  else
    docker exec -d "$c" sh -c 'cd /physics && LD_LIBRARY_PATH=/usr/local/lib bennu-field-device --f device.xml > /tmp/dut.out 2>&1'
  fi
  rc=0; docker exec "$c" python3 /physics/check.py "$1" > "$OUT/$1-$2.txt" 2>&1 || rc=$?
  for f in dut prov fed broker; do docker exec "$c" cat /tmp/$f.out > "$OUT/$1-$2.$f.out" 2>/dev/null || rm -f "$OUT/$1-$2.$f.out"; done
  docker rm -f "$c" >/dev/null
  sed "s/^/$1 $2: /" "$OUT/$1-$2.txt"
  return $rc
}

fail=0
for s in $(if [ "${1:-all}" = all ]; then echo opendss helics helics-opendss; else echo "$1"; fi); do
  one "$s" rust || fail=1; one "$s" stock || fail=1
  if python3 "$here/compare.py" "$OUT/$s-rust.txt" "$OUT/$s-stock.txt"; then
    echo "ok:   $s: ferrofire and stock read the same registers at every step"
  else
    echo "FAIL: $s: ferrofire and stock differ (above)"; fail=1
  fi
done
[ $fail = 0 ] && echo "PHYSICS PASS" || echo "PHYSICS FAIL"
exit $fail
