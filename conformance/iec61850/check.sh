# shellcheck shell=bash disable=SC2154
# tshark field assertions on $pcap, and what libiec61850's clients and GOOSE subscriber reported (peer.out). Values are not asserted:
# the DUT's inputs come from whatever publishes on the multicast group, so only the shape of the conversation is.
need() { # need DESCRIPTION FILTER : at least one packet matches
  [ "$(count "$pcap" "$2")" -gt 0 ] && pass "$1 ($2)" || fail "$1: no packet matches $2"
}
need "MMS Initiate request" 'mms.initiate_RequestPDU_element'
need "MMS Initiate response (ACSE/presentation/session came up)" 'mms.initiate_ResponsePDU_element'
need "GetNameList (server directory)" 'mms.getNameList_element'
need "Read of named variables" 'mms.read_element'
need "Write (a control's Oper, a report control block)" 'mms.write_element'
need "InformationReport (a report to the subscribed client)" 'mms.informationReport_element'
need "GOOSE frames" 'goose'
want_field "$pcap" goose goose.appid 0x03e8                                         # appid 1000, libiec61850's example
want_field "$pcap" goose goose.gocbRef 'simpleIOGenericIO/LLN0$GO$gcbAnalogValues'
want_field "$pcap" goose goose.datSet  'simpleIOGenericIO/LLN0$Events'   # codespell:ignore datset
want_field "$pcap" goose goose.numDatSetEntries 12                                  # value and quality of 6 points
want_field "$pcap" goose goose.confRev 1
# libiec61850's side of the conversation
grep -q '^read float value:' "$out/peer.out" && pass "client_example1 read AnIn1.mag.f" || fail "client_example1 did not read AnIn1.mag.f"
grep -q 'GGIO1.SPCSO1 operated successfully' "$out/peer.out" && pass "control: SPCSO1 direct operate" || fail "control: SPCSO1 not operated"
grep -q 'GGIO1.SPCSO2 operated successfully' "$out/peer.out" && pass "control: SPCSO2 select before operate" || fail "control: SPCSO2 not operated"
grep -q 'GGIO1.AnIn1.mag.f\[MX\].*: [0-9.-]' "$out/peer.out" && pass "report carries AnIn1.mag.f" || fail "reporting client got no AnIn1 value in a report"
[ "$(grep -c 'message is valid' "$out/peer.out")" -ge 3 ] && pass "GOOSE subscriber accepted the stream" || fail "GOOSE subscriber saw fewer than 3 valid messages"
grep -q 'simpleIOGenericIO$' "$out/peer.out" && pass "mms_utility lists the logical device" || fail "mms_utility did not list simpleIOGenericIO"
