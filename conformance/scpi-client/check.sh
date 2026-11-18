# shellcheck shell=bash disable=SC2154
# The capture on 5025 is the controller talking to scpi-parser: plain TCP text, so assert on the payload bytes both ways (the set, the
# query sent as QUERY;*OPC? and its joined answer, the error-queue reads and their "no error" answers) and that no error answer came back.
hex() { printf '%s' "$1" | od -An -tx1 | tr -d ' \n'; }
to=$(fields "$pcap" 'tcp.dstport == 5025 && tcp.len > 0' tcp.payload | tr -d ':\n')
from=$(fields "$pcap" 'tcp.srcport == 5025 && tcp.len > 0' tcp.payload | tr -d ':\n')
sent() { echo "$to" | grep -q -F -- "$(hex "$1")" && pass "the controller sent $1" || fail "the controller never sent $1"; }
answered() { echo "$from" | grep -q -F -- "$(hex "$1")" && pass "the instrument answered $1" || fail "the instrument never answered $1"; }
sent '*ESE 36.0'
sent '*ESE 4.0'
sent '*ESE?;*OPC?'
sent 'MEAS:VOLT:DC?;*OPC?'
sent 'SYST:ERR?'
answered '36;1'
answered '4;1'
answered '0,"No error"'
# The same on VXI-11: tshark decodes the portmapper's GETPORT answer and from it the core channel on its ephemeral port, so the writes
# (device_write, procedure 11, no error field: a request) and the reads (device_read, 12, with error 0) are checked as decoded fields.
vto=$(fields "$pcap" 'vxi11_core.procedure_v1 == 11 && !vxi11_core.error' vxi11_core.data | tr -d ':\n')
vfrom=$(fields "$pcap" 'vxi11_core.procedure_v1 == 12 && vxi11_core.error == 0' vxi11_core.data | tr -d ':\n')
vsent() { echo "$vto" | grep -q -F -- "$(hex "$1")" && pass "VXI-11: the controller wrote $1" || fail "VXI-11: the controller never wrote $1"; }
vanswered() { echo "$vfrom" | grep -q -F -- "$(hex "$1")" && pass "VXI-11: the instrument answered $1" || fail "VXI-11: the instrument never answered $1"; }
n=$(fields "$pcap" 'vxi11_core.procedure_v1 == 10 && vxi11_core.error == 0' vxi11_core.lid | grep -c .)
[ "$n" -ge 1 ] && pass "VXI-11: create_link answered without error" || fail "VXI-11: no create_link answered without error"
vsent '*ESE 36.0'
vsent '*ESE 4.0'
vsent '*ESE?;*OPC?'
vsent 'MEAS:VOLT:DC?;*OPC?'
vanswered '36;1'
vanswered '4;1'
text=$(printf '%s' "$from" | python3 -c 'import sys; print(bytes.fromhex(sys.stdin.read()).decode(errors="replace"))')
if printf '%s\n' "$text" | grep -q -E '^-[0-9]+,'; then fail "the instrument reported an error:"; printf '%s\n' "$text" | grep -E '^-[0-9]+,' >&2
else pass "no error in the instrument's queue"; fi
for p in "scpi-parser: \*ESE 36 set by the controller" "scpi-parser: \*ESE 4 set by the controller" "scpi-parser: MEAS:VOLT:DC[?] answer"; do
  grep -q "^PASS $p" "$out/peer.out" && pass "$p" || fail "$p"
done
for p in "scpi-parser-vxi11: \*ESE 36 set by the controller" "scpi-parser-vxi11: \*ESE 4 set by the controller" "scpi-parser-vxi11: MEAS:VOLT:DC[?] answer"; do
  grep -q "^PASS $p" "$out/peer.out" && pass "$p" || fail "$p"
done
grep -q '^SCPI-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
