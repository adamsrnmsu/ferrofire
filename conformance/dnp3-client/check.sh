# shellcheck shell=bash disable=SC2154
grep -q '^DNP3-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
g=client-dnp3${SERIAL:+-serial}   # a serial run (run.sh SERIAL=connect:20000) uses its own stock goldens if any; stock's serial run gives the TCP transcript
[ -f "/conformance/stock/$g.golden" ] || g=client-dnp3
if diff "/conformance/stock/$g.golden" "$out/transcript.txt" > "$out/transcript.diff"; then pass "transcript equal to the stock master's"
else fail "transcript differs from stock/$g.golden:"; head -40 "$out/transcript.diff" >&2; fi
# With the scans and the sequence numbers left in, the frames usually match too; a clock decides where a scan falls, so this one is a note.
if diff -q "/conformance/stock/$g-raw.golden" "$out/transcript-raw.txt" >/dev/null; then pass "every frame equal to the stock master's, scans and sequence numbers included"
else echo "note: the raw transcript differs from stock/$g-raw.golden (scan timing); see transcript-raw.txt"; fi
