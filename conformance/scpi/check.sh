# shellcheck shell=bash disable=SC2154
# SCPI is plain TCP text: no dissector, so assert on TCP (port 5025 traffic both ways, handshakes, no malformed frames: expert_clean
# above) and on the transcript in the capture (the payload bytes of the requests and the responses), then on what the peers recorded.
hex() { printf '%s' "$1" | od -An -tx1 | tr -d ' \n'; }
payloads=$(fields "$pcap" 'tcp.len > 0' tcp.dstport tcp.srcport tcp.payload | tr -d ':')
seen() { echo "$payloads" | grep -q -F -- "$(hex "$1")" && pass "capture holds $2" || fail "capture lacks $2"; }
[ "$(count "$pcap" 'tcp.dstport == 5025 && tcp.flags.syn == 1 && tcp.flags.ack == 0')" -ge 2000 ] && pass "at least 2000 TCP handshakes to 5025" || fail "fewer than 2000 handshakes"
[ "$(count "$pcap" 'tcp.srcport == 5025 && tcp.len > 0')" -gt 0 ] && pass "the instrument answered" || fail "no payload from port 5025"
seen '*IDN?' 'the request *IDN?'
seen 'ferrofire,rtu-1,CONF-0001,' 'the *IDN? response'
seen 'MEAS:SCAL:LEVEL?' 'the request MEAS:SCAL:LEVEL?'
seen 'SOUR:SETPOINT 420.5' 'the request SOUR:SETPOINT 420.5'
seen 'SYST:ERR?' 'the request SYST:ERR?'
seen '-222,"Data out of range;setpoint: 99999 is outside the limits"' 'the -222 response'
seen '-363,"Input buffer overrun"' 'the -363 response to the overlong line'
seen '250.5;-12.25;1' 'the joined response of a compound query'
for p in "pyvisa: \*IDN? names" "pyvisa: MEAS:SCAL:LEVEL[?] = 250.5" "pyvisa: SOUR:SETPOINT 420.5 reached the provider write queue" \
         "pyvisa: SOUR:BREAKER_CMD ON reached the provider write queue" "pyvisa: \*STB[?] after the error" "pyvisa: \*ESR[?] after the error" \
         "pyvisa: SYST:ERR[?] is -222" "pyvisa: the out-of-range write never reached the provider" \
         "lxi: \*IDN[?]" "lxi: MEAS:SCAL:LEVEL[?] = 250.5" "ferrofire scpi: --check script exit 0" "ferrofire scpi: --check exits 1" \
         "lxi vxi11: \*IDN[?]" "lxi vxi11: MEAS:SCAL:LEVEL[?] = 250.5" "lxi discover: found the device" \
         "ferrofire scpi: TCPIP::127.0.0.1::hislip0::INSTR --check" "ferrofire scpi: TCPIP::127.0.0.1::INSTR --check" \
         "ferrofire scpi: TCPIP::127.0.0.1::hislip0::INSTR rejected query found at once" "ferrofire scpi: TCPIP::127.0.0.1::INSTR rejected query found at once" \
         "churn: descriptors flat" "plain socket: overlong and garbage"; do
  grep -q "^PASS $p" "$out/peer.out" && pass "$p" || fail "$p"
done
for t in hislip vxi11; do
  for p in "pyvisa: $t: \*IDN[?] names" "pyvisa: $t: MEAS:SCAL:LEVEL[?] = 250.5" "pyvisa: $t: compound query joins" "pyvisa: $t: status byte after the error" \
           "pyvisa: $t: \*ESR[?] is the execution error bit" "pyvisa: $t: SYST:ERR[?] is -222" "pyvisa: $t: after a device clear the session still answers" \
           "pyvisa: $t: a device clear is no error"; do
    grep -q "^PASS $p" "$out/peer.out" && pass "$p" || fail "$p"
  done
done
for p in "pyvisa: shared status: an error made over HiSLIP is read over VXI-11" "pyvisa: shared status: and is gone for HiSLIP" \
         "pyvisa: shared status: [*]ESE and the error made over VXI-11 are seen on the raw socket" \
         "pyvisa: hislip: a message over a response that was not read is INTERRUPTED" "pyvisa: hislip: and only once"; do
  grep -q "^PASS $p" "$out/peer.out" && pass "$p" || fail "$p"
done
# HiSLIP, portmapper and the VXI-11 core channel are in the capture, and tshark's own dissectors (judged malformed/expert-clean above) decoded them
for f in hislip portmap vxi11_core; do
  [ "$(count "$pcap" "$f")" -gt 0 ] && pass "tshark decoded $f" || fail "tshark decoded no $f"
done
[ "$(count "$pcap" 'tcp.dstport == 4880 && tcp.flags.syn == 1 && tcp.flags.ack == 0')" -ge 4 ] && pass "HiSLIP handshakes to 4880 (two channels per session)" || fail "fewer than 4 HiSLIP handshakes"
seen 'hislip0' 'the HiSLIP sub-address'
seen 'inst0' 'the VXI-11 device name'
grep -q '^PYVISA DONE' "$out/peer.out" && pass "PyVISA finished" || fail "PyVISA did not finish"
grep -q 'load-mw-setpoint:300.000000' "$out/peer.out" && pass "ferrofire scpi write reached the provider" || fail "ferrofire scpi write did not reach the provider"
grep -q '^SCPI PEERS DONE' "$out/peer.out" && pass "the device was still serving at the end" || fail "the device was not serving at the end"
