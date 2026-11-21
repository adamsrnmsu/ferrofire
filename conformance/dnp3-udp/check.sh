# shellcheck shell=bash disable=SC2154
# tshark field assertions on $pcap, as conformance/dnp3/check.sh, with every DNP3 frame on UDP port 20000.
want_field "$pcap" 'dnp3.al.func == 1'   dnp3.al.obj 0x3c01   # integrity poll asks class 0
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.obj 0x1e06   # response carries analog input g30v6 ...
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.obj 0x0102   # ... and binary input with flags g1v2
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.index 2      # binary-input address 2 from the config
want_field "$pcap" 'dnp3.al.func == 5'   dnp3.al.obj 0x0c01   # DIRECT_OPERATE CROB g12v1 ...
want_field "$pcap" 'dnp3.al.func == 5'   dnp3.al.index 5      # ... on binary-output address 5
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.obj 0x0c01   # echoed in the response
want_field "$pcap" 'dnp3.al.func == 5'   dnp3.al.obj 0x2903   # analog output float g41v3
want_field "$pcap" 'dnp3.al.func == 129' dnp3.src 10          # responses come from outstation address 10
want_field "$pcap" 'dnp3.al.func == 129' udp.srcport 20000    # ... as UDP datagrams from the outstation's port
n=$(count "$pcap" 'dnp3 && tcp'); [ "$n" = 0 ] && pass "no DNP3 over TCP" || fail "$n DNP3 packets over TCP"
grep -q 'Index: 5 State: SUCCESS Status: SUCCESS' "$out/peer.out" && pass "master: CROB idx 5 SUCCESS" || fail "master: CROB idx 5 not SUCCESS"
grep -q 'Index: 1 State: SUCCESS Status: NO_SELECT' "$out/peer.out" && pass "master: SBO analog idx 1 NO_SELECT" || fail "master: SBO analog idx 1 not refused"
grep -q 'Index: 2 State: SUCCESS Status: SUCCESS' "$out/peer.out" && pass "master: analog idx 2 SUCCESS" || fail "master: analog idx 2 not SUCCESS"
