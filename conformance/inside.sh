#!/bin/bash
# Runs inside the testpeers container (started by run.sh). Capture on lo, drive the peers, then judge the pcap.
set -u
proto=$1; d=/conformance/$proto; out=/out
# shellcheck source=conformance/lib.sh
. /conformance/lib.sh
. "$d/proto.env"
mkdir -p "$out"; tcpdump -U -i lo -w "$out/$proto.pcap" "$CAPTURE_FILTER" 2>"$out/tcpdump.err" &
tdpid=$!; sleep 1
bash "$d/peer.sh" > "$out/peer.out" 2>&1; peer_rc=$?
sleep 1; kill "$tdpid"; wait "$tdpid" 2>/dev/null
cat "$out/peer.out"
[ "$peer_rc" = 0 ] || fail "peer.sh exited $peer_rc"
pcap="$out/$proto.pcap"
expert_clean "$pcap"
. "$d/check.sh"
zeek_run "$pcap" "$out"
. "$d/zeek.sh"
[ "${SELFTEST:-0}" != 1 ] || bash /conformance/selftest.sh "$pcap" || FAILS=$((FAILS+1))
finish
