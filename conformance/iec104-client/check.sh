# shellcheck shell=bash disable=SC2154
# The transcript (command-interface replies and the client's frames, periodic data listed once) must be the stock client's against the
# stock server (stock/client-iec104.golden): the same for the stock client against our server (SERVER=rust); our client (DUT=rust)
# differs from it by exactly the divergences in iec104-client/rust-client.diff, once one is listed.
grep -q '^IEC104-CLIENT PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
rd=/conformance/iec104-client/rust-client.diff; { [ "$DUT" = rust ] && [ -f "$rd" ]; } || rd=
want_transcript /conformance/stock/client-iec104.golden "$out/transcript.txt" "$rd"
want_104_sequence "$pcap"
want_field "$pcap" 'iec60870_asdu.typeid == 100 && tcp.dstport == 2404' iec60870_asdu.oa 3     # the client's GI, originator 3
want_field "$pcap" 'iec60870_asdu.typeid == 100 && tcp.dstport == 2404' iec60870_asdu.causetx 6
want_field "$pcap" 'tcp.dstport == 2404' iec60870_asdu.typeid 46                               # binary writes: C_DC_NA_1
want_field "$pcap" 'tcp.dstport == 2404' iec60870_asdu.typeid 50                               # analog writes: C_SE_NC_1
