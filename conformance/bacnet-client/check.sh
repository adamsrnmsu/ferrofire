# shellcheck shell=bash disable=SC2154
# The transcript (command-interface replies, and the client's BACnet frames with the invoke id masked) must be the stock client's
# against the stock server, whichever client and server ran: SERVER=rust (the stock client against our server) holds; DUT=rust (our
# client) does not yet, on the stock sample or with its tags declared (CFG=conformance/bacnet-client/client-tags.xml): the open
# divergences are in conformance/README.md, BACnet/IP.
grep -q '^BACNET-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
want_transcript /conformance/stock/client-bacnet.golden "$out/transcript.txt"
rp='bacapp.type == 0 && bacapp.confirmed_service == 12 && bacapp.property_identifier == 85'
# the scan, as stock's maps make it: binary-output 0..3 (its binary-inputs 0 and 1 too), analog-input 0, analog-output 1..3
want_field "$pcap" "$rp && udp.srcport == 47809" bacapp.objectType 4
want_field "$pcap" "$rp && udp.srcport == 47809" bacapp.objectType 0
want_field "$pcap" "$rp && udp.srcport == 47809" bacapp.objectType 1
want_field "$pcap" 'bacapp.type == 0 && bacapp.confirmed_service == 15 && udp.srcport == 47809' bacapp.present_value.real 42.5   # WRITE: WP REAL
want_field "$pcap" 'bacapp.type == 0 && bacapp.confirmed_service == 15 && udp.srcport == 47809' bacapp.present_value.enum_index 1   # ... ENUMERATED
