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
grep -q 'simpleIOGenericIO$' "$out/peer.out" && pass "mms_utility lists the logical device" || fail "mms_utility did not list simpleIOGenericIO"
# iec61850bean 1.9.0 (Java, own ASN.1 codec): model discovery, data set read, ga (read all data); any exception or lost association fails
bean=$(sed -n '/== iec61850bean/,/== go-iec61850/p' "$out/peer.out")
echo "$bean" | grep -q 'successfully read model' && pass "bean: association and model discovery" || fail "bean: no model (association or GetNameList/GetVariableAccessAttributes failed)"
echo "$bean" | grep -q '^simpleIOGenericIO/GGIO1.SPCSO2.Oper.ctlVal' && pass "bean: model carries the control objects" || fail "bean: SPCSO2.Oper.ctlVal not in the model"
echo "$bean" | grep -q 'simpleIOGenericIO/LLN0.Events' && pass "bean: data set LLN0.Events" || fail "bean: data set missing"
[ "$(echo "$bean" | grep -c 'GGIO1.*\.q: ')" -ge 6 ] && pass "bean: data set values and qualities decoded" || fail "bean: fewer than 6 quality fields decoded"
echo "$bean" | grep -q 'Reading all data...done' && pass "bean: ga (Read of the whole model) done" || fail "bean: ga did not complete"
echo "$bean" | grep -qiE 'exception|error|timed out|aborted' && fail "bean: reported an error" || pass "bean: no errors"
# otfabric/go-iec61850 v1.16.2 (Go): browse, data set, GoCB, a typed read, and a GOOSE subscriber that decodes the VLAN-tagged stream
grep -q '^FFGOIEC OK' "$out/peer.out" && pass "go-iec61850: no errors" || fail "go-iec61850: ERROR lines or missing FFGOIEC OK"
grep -q '^DATASET: LLN0$Events members: 12' "$out/peer.out" && pass "go-iec61850: data set of 12 members" || fail "go-iec61850: data set read"
grep -q '^GOCB: goena=true' "$out/peer.out" && pass "go-iec61850: GoCB read back, GoEna" || fail "go-iec61850: GoCB"
grep -q '^READ: .*AnIn1.mag.f\[MX\] = ' "$out/peer.out" && pass "go-iec61850: typed read of AnIn1.mag.f" || fail "go-iec61850: read"
grep -q '^GOOSE: gocbRef="simpleIOGenericIO/LLN0$GO$gcbAnalogValues".*appid=1000 confRev=1 entries=12' "$out/peer.out" && pass "go-iec61850: GOOSE gocbRef, appid, confRev, 12 entries decoded" || fail "go-iec61850: GOOSE frame not decoded"
