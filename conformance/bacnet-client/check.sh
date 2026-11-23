# shellcheck shell=bash disable=SC2154
# The transcript (command-interface replies, and the client's BACnet frames with the invoke id masked) must be the stock client's
# against the stock server: the same for the stock client against our server (SERVER=rust) and for our client (DUT=rust).
grep -q '^BACNET-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
want_transcript /conformance/stock/client-bacnet.golden "$out/transcript.txt"
rp='bacapp.type == 0 && bacapp.confirmed_service == 12 && bacapp.property_identifier == 85'
# the scan, as stock's maps make it: binary-output 0..3 (its binary-inputs 0 and 1 too), analog-input 0, analog-output 1..3
want_field "$pcap" "$rp && udp.srcport == 47809" bacapp.objectType 4
want_field "$pcap" "$rp && udp.srcport == 47809" bacapp.objectType 0
want_field "$pcap" "$rp && udp.srcport == 47809" bacapp.objectType 1
want_field "$pcap" 'bacapp.type == 0 && bacapp.confirmed_service == 15 && udp.srcport == 47809' bacapp.present_value.real 42.5   # WRITE: WP REAL
want_field "$pcap" 'bacapp.type == 0 && bacapp.confirmed_service == 15 && udp.srcport == 47809' bacapp.present_value.enum_index 0   # ... ENUMERATED
