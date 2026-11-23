# shellcheck shell=bash disable=SC2154
# tshark field assertions on $pcap (peer.sh: probe_bacnet.py, nmap), then the transcript against stock's. Object types: 0 analog-input,
# 1 analog-output, 3 binary-input, 4 binary-output, 8 device; property 85 present-value; error code 31 unknown-object.
grep -q '^BACNET PEERS DONE' "$out/peer.out" && pass "peer.sh finished" || fail "peer.sh did not finish"
want_field "$pcap" 'bacapp.unconfirmed_service == 0' bacapp.instance_number 1234        # I-Am names device 1234
want_field "$pcap" 'bacapp.unconfirmed_service == 0' bacapp.vendor_identifier 260       # ... vendor 260 (bacnet-stack)
rp='bacapp.type == 3 && bacapp.confirmed_service == 12 && bacapp.property_identifier == 85'
# the fed values (proto.env FEED), as the configured objects serve them (AO: stock's uint8)
want_field "$pcap" "$rp && bacapp.objectType == 0 && bacapp.instance_number == 0" bacapp.present_value.real 250.5
want_field "$pcap" "$rp && bacapp.objectType == 0 && bacapp.instance_number == 1" bacapp.present_value.real 300.700012207031   # 300.7 as a REAL
want_field "$pcap" "$rp && bacapp.objectType == 1 && bacapp.instance_number == 2" bacapp.present_value.real 250
want_field "$pcap" "$rp && bacapp.objectType == 1 && bacapp.instance_number == 3" bacapp.present_value.real 44
for o in '3 && bacapp.instance_number == 0' '3 && bacapp.instance_number == 1' '4 && bacapp.instance_number == 2' '4 && bacapp.instance_number == 3'; do
  want_field "$pcap" "$rp && bacapp.objectType == $o" bacapp.present_value.enum_index 1
done
want_field "$pcap" 'bacapp.type == 0 && bacapp.confirmed_service == 15 && bacapp.objectType == 1 && bacapp.instance_number == 2' bacapp.present_value.real 42.5
want_field "$pcap" "$rp && bacapp.objectType == 1 && bacapp.instance_number == 2" bacapp.present_value.real 42   # read back as stock keeps it
grep -A1 -x 'WP  analog-output,2 present-value=42.5 prio=8 -> ACK' "$out/transcript.txt" | grep -q -x 'RP  analog-output,2 present-value -> 42.0000' \
  && pass "WP analog-output,2 42.5 at priority 8, then RP reads 42" || fail "WP analog-output,2 not followed by a read of 42"
want_field "$pcap" 'bacapp.type == 5 && bacapp.confirmed_service == 12' bacapp.error_code 31   # unknown-object
want_field "$pcap" 'bacapp.type == 5 && bacapp.confirmed_service == 12' bacapp.error_class 1   # ... class object
diff /conformance/stock/bacnet-server.nmap.golden "$out/nmap.txt" > "$out/nmap.diff" && pass "nmap bacnet-info equal to stock's" \
  || { fail "nmap bacnet-info differs from stock's:"; cat "$out/nmap.diff" >&2; }
# stock's transcript with the same feed; ours must differ from it by exactly the listed divergences (conformance/bacnet/rust-feed.diff)
want_transcript /conformance/stock/bacnet-server-feed.golden "$out/transcript.txt" "$([ "$DUT" = rust ] && echo /conformance/bacnet/rust-feed.diff)"
# Who-Is to 255.255.255.255 is answered (ww-o17); a BO 2 inactive write at priority 5 reaches the provider (ww-s31): extra.txt, peer.sh
grep -q -x 'Who-Is to 255.255.255.255 -> I-Am device 1234' "$out/extra.txt" && pass "Who-Is to 255.255.255.255 answered" || fail "Who-Is to 255.255.255.255 not answered"
grep -q 'ACK; provider got: .*load-1_bus-1.active:false' "$out/extra.txt" && pass "BO 2 inactive at priority 5 reaches the provider" || fail "BO 2 inactive at priority 5 did not reach the provider"
