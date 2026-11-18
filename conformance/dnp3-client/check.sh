# shellcheck shell=bash disable=SC2154
grep -q '^DNP3-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
if diff /conformance/stock/client-dnp3.golden "$out/transcript.txt" > "$out/transcript.diff"; then pass "transcript equal to the stock master's"
else fail "transcript differs from stock/client-dnp3.golden:"; head -40 "$out/transcript.diff" >&2; fi
