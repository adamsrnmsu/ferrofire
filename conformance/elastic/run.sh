#!/bin/sh
# Elasticsearch/OpenSearch conformance (make conformance PROTO=elastic; called by conformance/run.sh). Brings up, on the Lima docker
# daemon, an Elasticsearch 8, an OpenSearch 2 and an Elasticsearch 8 with security on (TLS, basic auth, API key), then runs
#   check.py        in a bennu:arm64 container next to stock pybennu: provider export, `ferrofire groundtruth`, the differential
#                   against the stock groundtruth_monitor.py, the intended differences, TLS/auth enforcement
#   scorch_check.py in a Python 3.12 container: the SCORCH providerdata component's own check against the provider export
# All containers are named ff-conf-*, are only ever created and removed here, and share the network ff-conf-es-net. No bind mounts
# (the daemon is a VM): files go in with docker cp. The binary is FF_BIN (default target-linux/debug/ferrofire from `make test`).
# Needs the phenix_apps source: SCORCH_SRC (default ~/git/sceptre-phenix-apps/src/python/phenix_apps). Images used, not built from
# source: opensearchproject/opensearch:2 (Apache-2.0), docker.elastic.co/elasticsearch/elasticsearch:8.19.3 (test peer only).
# env: KEEP=1 leaves the containers up (rerun with the same env to reuse them), OUT=dir for the artefacts (default target/conformance/elastic).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
FF_BIN=${FF_BIN:-$here/../../target-linux/debug/ferrofire}
SCORCH_SRC=${SCORCH_SRC:-$HOME/git/sceptre-phenix-apps/src/python/phenix_apps}
OUT=${OUT:-$here/../../target/conformance/elastic}
ES_IMAGE=${ES_IMAGE:-docker.elastic.co/elasticsearch/elasticsearch:8.19.3}
OS_IMAGE=${OS_IMAGE:-opensearchproject/opensearch:2}
NET=ff-conf-es-net
[ -x "$FF_BIN" ] || { echo "no $FF_BIN: run make test first (or set FF_BIN)" >&2; exit 2; }
[ -d "$SCORCH_SRC" ] || { echo "no $SCORCH_SRC: set SCORCH_SRC to sceptre-phenix-apps/src/python/phenix_apps" >&2; exit 2; }
rm -rf "$OUT"; mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
cleanup() { [ -n "${KEEP:-}" ] || { docker rm -f ff-conf-es8 ff-conf-os2 ff-conf-essec ff-conf-dut ff-conf-scorch >/dev/null 2>&1; docker network rm $NET >/dev/null 2>&1; } || true; }
trap cleanup EXIT
up() { docker inspect "$1" >/dev/null 2>&1 || docker run -d --name "$@" >/dev/null; }
docker network inspect $NET >/dev/null 2>&1 || docker network create $NET >/dev/null
# Disk thresholds off: a nearly full VM disk would otherwise keep every shard unassigned. 512 MB heaps: three servers share one VM.
JAVA="-Xms512m -Xmx512m"
up ff-conf-es8 --network $NET --network-alias es8 -e discovery.type=single-node -e xpack.security.enabled=false \
  -e cluster.routing.allocation.disk.threshold_enabled=false -e ES_JAVA_OPTS="$JAVA" "$ES_IMAGE"
up ff-conf-os2 --network $NET --network-alias os2 -e discovery.type=single-node -e DISABLE_SECURITY_PLUGIN=true \
  -e DISABLE_INSTALL_DEMO_CONFIG=true -e cluster.routing.allocation.disk.threshold_enabled=false -e OPENSEARCH_JAVA_OPTS="$JAVA" "$OS_IMAGE"
up ff-conf-essec --hostname essec --network $NET --network-alias essec -e discovery.type=single-node -e ELASTIC_PASSWORD=ffconf-Pass-1 \
  -e cluster.routing.allocation.disk.threshold_enabled=false -e ES_JAVA_OPTS="$JAVA" "$ES_IMAGE"
docker image inspect ff-conf-scorch-env >/dev/null 2>&1 || docker build -q -f "$here/Dockerfile.scorch" -t ff-conf-scorch-env "$here" >/dev/null
docker rm -f ff-conf-dut ff-conf-scorch >/dev/null 2>&1 || true
docker run -d --name ff-conf-dut --network $NET bennu:arm64 sleep 3600 >/dev/null
docker run -d --name ff-conf-scorch --network $NET ff-conf-scorch-env sleep 3600 >/dev/null
# the TLS CA of the secured server appears once it has started (poll; it is created on first boot)
for _ in $(seq 1 90); do
  docker cp ff-conf-essec:/usr/share/elasticsearch/config/certs/http_ca.crt "$OUT/http_ca.crt" >/dev/null 2>&1 && break || sleep 2
done
[ -s "$OUT/http_ca.crt" ] || { echo "the secured Elasticsearch did not produce a CA certificate" >&2; exit 1; }
for c in ff-conf-dut ff-conf-scorch; do
  docker cp "$FF_BIN" $c:/usr/local/bin/ferrofire
  docker cp "$here/." $c:/conf
  docker cp "$OUT/http_ca.crt" $c:/conf/http_ca.crt
done
docker cp "$SCORCH_SRC" ff-conf-scorch:/phenix_apps
rc=0
echo "== bennu:arm64: provider export, groundtruth, differential vs stock pybennu"
docker exec ff-conf-dut python3 /conf/check.py || rc=1
docker cp ff-conf-dut:/out/. "$OUT" 2>/dev/null || true
echo "== SCORCH providerdata check against the provider export"
docker exec ff-conf-scorch python -I /conf/scorch_check.py || rc=1
exit $rc
