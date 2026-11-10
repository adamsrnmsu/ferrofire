# shellcheck shell=bash disable=SC2154
# tshark field assertions on $pcap, and what the two peers reported (peer.out). Values are not asserted from the wire: the DUT's
# inputs come from whatever publishes on the multicast group, so only the shape of the conversation is.
need() { # need DESCRIPTION FILTER : at least one packet matches
  [ "$(count "$pcap" "$2")" -gt 0 ] && pass "$1 ($2)" || fail "$1: no packet matches $2"
}
need "MMS Initiate request" 'mms.initiate_RequestPDU_element'
need "MMS Initiate response (ACSE/presentation/session came up)" 'mms.initiate_ResponsePDU_element'
need "GetNameList" 'mms.getNameList_element'
need "Read of named variables" 'mms.read_element'
need "Write (a data value, a transfer set)" 'mms.write_element'
need "DefineNamedVariableList (a data set)" 'mms.defineNamedVariableList_element'
need "InformationReport (a transfer set report)" 'mms.informationReport_element'
want_field "$pcap" mms mms.vmd_specific 'Transfer_Set_Time_Stamp'     # the report names its own variables as plain MMS text
want_field "$pcap" mms mms.vmd_specific 'DSConditions_Detected'
want_field "$pcap" mms mms.vmd_specific 'Bilateral_Table_ID'
want_field "$pcap" mms mms.domainId 'EMS1'
want_field "$pcap" mms mms.itemId 'LOAD_MW'
want_field "$pcap" mms mms.itemId 'DSTrans_1'
# the generic MMS client's side
grep -q '^EMS1$' "$out/peer.out" && pass "mms_utility lists the ICCP domain EMS1" || fail "mms_utility did not list EMS1"
grep -q 'LOAD_MW' "$out/peer.out" && pass "mms_utility browses the data values" || fail "mms_utility did not browse LOAD_MW"
[ "$(grep -c '^Read SUCCESS' "$out/peer.out")" = 3 ] && pass "mms_utility read 3 ICCP variables (Read SUCCESS)" || fail "mms_utility did not read all 3 variables"
grep -q -x 'BLT_RANGE_EMS1' "$out/peer.out" && pass "mms_utility reads Bilateral_Table_ID" || fail "mms_utility did not read the bilateral table id"
grep -q -E '^\{[0-9]+,[0-9.]+,[0-9]+\}$' "$out/peer.out" && pass "mms_utility reads a Data_RealQTimeTag as {time,value,flags}" || fail "no {time,value,flags} read"
# our thin ICCP client's side
for k in 'identify: ferrofire iccp-server' 'write SETPOINT: Ok(())' 'arm DSTrans_1: Ok(())' 'report 3:' 'released'; do
  grep -q -F -- "$k" "$out/peer.out" && pass "iccp-client: $k" || fail "iccp-client did not print: $k"
done
grep -q 'FAILED' "$out/peer.out" && fail "iccp-client reported a failure" || true
