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
