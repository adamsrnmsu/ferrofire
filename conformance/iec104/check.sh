# shellcheck shell=bash disable=SC2154
# The transcript (stock/probe_104x.py, see peer.sh) must be stock's for the same SET (stock/iec104-server[-SET].golden); a rust DUT
# may differ from it by exactly the divergences in iec104/rust-SET.diff, once one is listed. Then tshark: the interrogation and its
# answer, data from common address 1, the command echoes, and the APCI sequence numbers.
grep -q '^IEC104 PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
g=iec104-server; [ "${SET:-single}" = single ] || g=iec104-server-$SET
rd=/conformance/iec104/rust-${SET:-single}.diff; { [ "$DUT" = rust ] && [ -f "$rd" ]; } || rd=
want_transcript "/conformance/stock/$g.golden" "$out/transcript.txt" "$rd"
want_104_sequence "$pcap"
want_field "$pcap" 'iec60870_asdu.typeid == 100' iec60870_asdu.causetx 7    # ACT_CON
want_field "$pcap" 'iec60870_asdu.typeid == 100' iec60870_asdu.causetx 10   # ACT_TERM
want_field "$pcap" 'iec60870_asdu.causetx == 20' iec60870_asdu.addr 1       # interrogated data from CA 1
want_field "$pcap" 'iec60870_asdu.causetx == 20' iec60870_asdu.typeid 13    # M_ME_NC_1
if [ "${SET:-single}" != big ]; then
  for t in 45 46 50; do want_field "$pcap" "iec60870_asdu.typeid == $t && iec60870_asdu.causetx == 7" iec60870_asdu.typeid $t; done
fi
