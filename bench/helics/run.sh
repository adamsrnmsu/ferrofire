#!/bin/sh
# ferrofire's own engine as a HELICS federate next to a second federate (ww-7mw.4): sh bench/helics/run.sh
# One container: helics_broker -f 2, `ferrofire run example.yaml` (the federate "tank") and controller.py (the federate "ctl",
# a bang-bang level controller). controller.py prints the exchange and judges it; this script keeps the transcripts.
# env: FF_BIN (default target-linux/debug/ferrofire: `make dut`), IMAGE (ff-physics:22.04: HELICS 3.6.1 Python wheel, helics_broker,
#      pyzmq; built from bench/physics/Dockerfile when missing), OUT (target/helics), PFX (container name prefix, ff-helics),
#      GRANTS (600). Only the container <PFX>-run is created and removed.
set -eu
here=$(cd "$(dirname "$0")" && pwd); root=$here/../..
FF_BIN=${FF_BIN:-$root/target-linux/debug/ferrofire}; IMAGE=${IMAGE:-ff-physics:22.04}; OUT=${OUT:-$root/target/helics}
C=${PFX:-ff-helics}-run
[ -x "$FF_BIN" ] || { echo "no $FF_BIN: run make dut or set FF_BIN" >&2; exit 2; }
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build -t "$IMAGE" "$here/../physics"
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)

docker rm -f "$C" >/dev/null 2>&1 || true
docker run -d --name "$C" "$IMAGE" sleep 300 >/dev/null
docker cp "$here/." "$C:/helics"; docker cp "$FF_BIN" "$C:/usr/local/bin/ferrofire"
docker exec -d "$C" sh -c 'helics_broker -f 2 --local_interface=127.0.0.1 > /tmp/broker.out 2>&1'; sleep 1
docker exec -d "$C" sh -c 'cd /helics && FERROFIRE_HELICS_LIB=/usr/local/lib/libhelics.so ferrofire run example.yaml --server tcp://127.0.0.1:5555 --publish "udp://*;127.0.0.1:45999" > /tmp/ferrofire.out 2>&1'
rc=0; docker exec -e LD_LIBRARY_PATH=/usr/local/lib "$C" python3 /helics/controller.py "${GRANTS:-600}" > "$OUT/controller.txt" 2>&1 || rc=$?
# HELICS does not tell a lone federate its peers have left: stop ferrofire ourselves (SIGTERM, a clean exit)
docker exec "$C" sh -c 'pkill -TERM ferrofire; sleep 1; pgrep ferrofire' >/dev/null 2>&1 && { echo "FAIL: ferrofire did not stop on SIGTERM"; rc=1; }
for f in ferrofire broker; do docker exec "$C" cat /tmp/$f.out > "$OUT/$f.out" 2>/dev/null || true; done
docker rm -f "$C" >/dev/null
grep -E '^(ok|FAIL):' "$OUT/controller.txt" || true
sed -n '1,3p;$p' "$OUT/controller.txt" | grep '^t=' || true
[ $rc = 0 ] && echo "HELICS PASS" || { echo "HELICS FAIL (see $OUT)"; exit 1; }
