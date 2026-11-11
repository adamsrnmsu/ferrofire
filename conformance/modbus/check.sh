# shellcheck shell=bash disable=SC2154
# tshark field assertions on $pcap (see peer.sh for what was sent). Function codes: 1 read coils, 2 read discrete inputs, 3 read holding
# registers, 4 read input registers, 5/6 write single, 15/16 write multiple, 17 report slave id (nmap), 43 encapsulated interface (nmap).
for fc in 1 2 3 4 5 6 15 16; do want_field "$pcap" "modbus.func_code == $fc" modbus.func_code $fc; done
want_field "$pcap" 'modbus.func_code == 4'  modbus.reference_num 30000
want_field "$pcap" 'modbus.func_code == 4 && mbtcp.len == 5' modbus.regval_uint16 16416   # the reply: 250.5 over [0,1000]
want_field "$pcap" 'modbus.func_code == 3'  modbus.reference_num 40000
want_field "$pcap" 'modbus.func_code == 2'  modbus.reference_num 10000
want_field "$pcap" 'modbus.exception_code' modbus.exception_code 2                                  # read input register 29999
want_field "$pcap" 'modbus.exception_code' modbus.exception_code 3                                  # unmapped coil: 3, not 2 (SPEC Q3)
want_field "$pcap" 'mbtcp.unit_id == 7' mbtcp.unit_id 7                                        # the unit id is echoed
grep -q '502/tcp open' /tmp/nmap-modbus.txt && pass "nmap sees 502/tcp open" || fail "nmap: 502/tcp not open"
grep -q -P "\[10001\]:\s+1" /tmp/mbpoll-di.txt && pass "mbpoll di reads 10001 = 1" || fail "mbpoll di did not read 10001 = 1"
for t in ir:16416 hr:16416; do grep -q "${t#*:}" "/tmp/mbpoll-${t%:*}.txt" && pass "mbpoll ${t%:*} reads ${t#*:}" || fail "mbpoll ${t%:*} did not read ${t#*:}"; done
grep -q -i 'Written' /tmp/mbpoll-wr.txt && pass "mbpoll wrote a register" || fail "mbpoll register write not acknowledged"
grep -q -i 'Written' /tmp/mbpoll-wc.txt && pass "mbpoll wrote a coil" || fail "mbpoll coil write not acknowledged"
