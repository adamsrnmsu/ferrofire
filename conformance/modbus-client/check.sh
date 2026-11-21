# shellcheck shell=bash disable=SC2154
# The transcript (command-interface replies, and the bytes and connection events on the Modbus port after each command) must be the
# stock client's, line for line.
grep -q '^MODBUS-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
if diff /conformance/stock/client-modbus.golden "$out/transcript.txt" > "$out/transcript.diff"; then pass "transcript equal to the stock client's"
else fail "transcript differs from stock/client-modbus.golden:"; head -40 "$out/transcript.diff" >&2; fi
want_field "$pcap" 'mbtcp && tcp.dstport == 5502' modbus.func_code 4
want_field "$pcap" 'mbtcp && tcp.dstport == 5502' modbus.func_code 6
