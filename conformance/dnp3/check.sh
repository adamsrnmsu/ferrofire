# shellcheck shell=bash disable=SC2154
# tshark field assertions on $pcap (see peer.sh for what was sent). Object ids are group<<8|variation: 0x3c02 = g60v2 class 1 poll.
want_field "$pcap" 'dnp3.al.func == 1'   dnp3.al.obj 0x3c01   # integrity poll asks class 0
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.obj 0x1e06   # response carries analog input g30v6 ...
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.obj 0x0102   # ... and binary input with flags g1v2
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.index 2      # binary-input address 2 from the config
want_field "$pcap" 'dnp3.al.func == 5'   dnp3.al.obj 0x0c01   # DIRECT_OPERATE CROB g12v1 ...
want_field "$pcap" 'dnp3.al.func == 5'   dnp3.al.index 5      # ... on binary-output address 5
want_field "$pcap" 'dnp3.al.func == 129' dnp3.al.obj 0x0c01   # echoed in the response
want_field "$pcap" 'dnp3.al.func == 5'   dnp3.al.obj 0x2903   # analog output float g41v3
want_field "$pcap" 'dnp3.al.func == 129' dnp3.src 10          # responses come from outstation address 10
# what the master reported for each command (peer.out): CROB accepted, SBO analog refused without a select, direct analog accepted
grep -q 'Index: 5 State: SUCCESS Status: SUCCESS' "$out/peer.out" && pass "master: CROB idx 5 SUCCESS" || fail "master: CROB idx 5 not SUCCESS"
grep -q 'Index: 1 State: SUCCESS Status: NO_SELECT' "$out/peer.out" && pass "master: SBO analog idx 1 NO_SELECT" || fail "master: SBO analog idx 1 not refused"
grep -q 'Index: 2 State: SUCCESS Status: SUCCESS' "$out/peer.out" && pass "master: analog idx 2 SUCCESS" || fail "master: analog idx 2 not SUCCESS"
# dnp3py 0.6.0 (independent of opendnp3) is the second TCP stream; it exposes no control statuses, so read them off the wire: its direct
# operate (g41 idx 2), select and operate (g41 idx 1) and CROB (g12 idx 5) must all come back with control status 0 (SUCCESS)
st=$(fields "$pcap" 'tcp.stream == 1 && dnp3.al.func == 129 && dnp3.al.ctrlstatus' dnp3.al.ctrlstatus | tr -s ' ,\n' '\n\n' | grep -v '^$' | sort | uniq -c | tr -s ' ' | tr '\n' ';')
[ "$st" = " 4 0;" ] && pass "dnp3py: 4 control responses, all status SUCCESS" || fail "dnp3py control statuses were: ${st:-none} (want four 0)"
want_field "$pcap" 'tcp.stream == 1 && dnp3.al.func == 129' dnp3.al.obj 0x1e06   # its integrity poll got g30v6 too
