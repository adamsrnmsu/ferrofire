# shellcheck shell=bash disable=SC2154
# The transcript (command-interface replies, and the bytes and connection events on the Modbus port after each command) must be the
# stock client's, line for line.
grep -q '^MODBUS-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
g=client-modbus${SERIAL:+-serial}.golden   # a serial run (run.sh SERIAL=connect:5502) has its own stock golden
if diff "/conformance/stock/$g" "$out/transcript.txt" > "$out/transcript.diff"; then pass "transcript equal to the stock client's"
else fail "transcript differs from stock/$g:"; head -40 "$out/transcript.diff" >&2; fi
want_field "$pcap" 'mbtcp && tcp.dstport == 5502' modbus.func_code 4
want_field "$pcap" 'mbtcp && tcp.dstport == 5502' modbus.func_code 6
