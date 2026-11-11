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

# ---- the independent peers (docker/testpeers/Dockerfile): igs-iec-tase2 and telcon-tase2, both directions ------------------------------------
# They are young and possibly AI-assisted, so agreement is evidence, not proof (docs/testing.md). Each expectation below is one of:
#   ok     the operation must succeed (a disagreement fails the run)
#   xfail  a known disagreement or peer limitation: it must STILL fail; if it starts to pass, the run fails until the mark is removed
#   skip   not exercised, with the reason printed
blk() { awk -v l="### $1" '$0==l{f=1;next} /^### /{f=0} f' "$out/peer.out"; }
tc_op() { case $1 in tc-*) return 0;; *) return 1;; esac; }
op_passed() { # LABEL: telcon commands answer {"ok":true}, igs commands exit 0
  local b; b=$(blk "$1")
  if tc_op "$1"; then grep -q '"ok":true' <<<"$b"; else grep -q '^rc=0$' <<<"$b"; fi
}
op_ok() { # LABEL [EXTENDED-REGEX the output must match]
  if op_passed "$1" && { [ -z "${2:-}" ] || blk "$1" | grep -q -E -- "$2"; }; then pass "$1${2:+ ($2)}"; else fail "$1 did not succeed${2:+ with $2}: $(blk "$1" | head -3 | tr '\n' ' ')"; fi
}
op_xfail() { # LABEL BEAD-OR-REASON
  if op_passed "$1"; then fail "$1 now succeeds; it is marked as a known failure ($2): remove the mark"; else pass "$1 fails as known ($2)"; fi
}
op_fails_with() { # LABEL REGEX WHY: must fail and say REGEX (a negative control)
  if ! op_passed "$1" && blk "$1" | grep -q -E -- "$2"; then pass "$1 fails with $2 ($3)"; else fail "$1 should fail with $2 ($3): $(blk "$1" | head -3 | tr '\n' ' ')"; fi
}
skip() { echo "SKIP: $1: $2"; }
step_ok() { # BLOCK NAME [REGEX on the step's detail]
  local l; l=$(blk "$1" | grep -E "^step $2: " | head -1)
  if [[ $l == "step $2: ok"* ]] && { [ -z "${3:-}" ] || grep -q -E -- "$3" <<<"$l"; }; then pass "$1: $2${3:+ ($3)}"; else fail "$1: step $2 did not pass${3:+ with $3}: ${l:-missing}"; fi
}
step_xfail() { # BLOCK NAME WHY
  local l; l=$(blk "$1" | grep -E "^step $2: " | head -1)
  if [[ $l == "step $2: FAIL"* ]]; then pass "$1: $2 fails as known ($3)"; else fail "$1: step $2 now passes or is missing ($3): ${l:-missing}"; fi
}

echo "-- igstase2-client -> our server"
op_ok igs-names 'LOAD_MW'
op_ok igs-names-datasets
op_ok igs-get 'BRKR_1: Structure'
op_ok igs-type 'LOAD_MW: Structure'
op_ok igs-set 'SETPOINT: written'
op_ok igs-create 'created DS1'
op_ok igs-delete 'deleted'
op_fails_with igs-members-new-association 'OBJECT-NON-EXISTENT \(code 2\)' "a data set belongs to its association; ww-otj.61: ServiceError access 2, not DataAccessError 10"
op_fails_with igs-wrong-table 'Bilateral_Table_ID|table|refus|agreement|mismatch' "a client whose table id differs must not carry on"
op_xfail igs-start "ww-otj.71: igs neither defines the set nor reads Next_DSTransfer_Set, ours has no preconfigured ones"
op_xfail igs-start-rbe "ww-otj.71"
skip "igs write of Data_State" "igstase2-client set writes true/false as a boolean; Data_State is a 2-bit bit string (802 6.2): a limit of its CLI, not a disagreement"

echo "-- telcon tase2-client -> our server"
op_ok tc-blt-id 'BLT_RANGE_EMS1'
op_ok tc-name-list 'LOAD_MW'
op_ok tc-name-list-datasets
op_ok tc-read-real '"real":'
op_ok tc-read-state '"state"'
op_ok tc-read-real-q '"quality"'
op_xfail tc-read-real-qtt "ww-otj.69: telcon takes {value, flags, time}, igs and ours {time, value, flags}"
op_xfail tc-read-state-qtt "ww-otj.69"
op_ok tc-write-real
op_ok tc-dataset-define
op_ok tc-dataset-read '"ok":true'
op_ok tc-dataset-delete
op_xfail tc-transfer-set "ww-otj.70: telcon reads Next_DSTransfer_Set as a 2-member name, the field and ours send 3"
skip "telcon TASE2_Version and Supported_Features" "tase2-client has no command for them (blt-id only); the other peers read them"

echo "-- our thin client -> igstase2-server (10102)"
B=ours-to-igs
for s in associate identify "read Bilateral_Table_ID" "read data values" "write SETPOINT" "define data set" "data set members" "read data set" \
         "next transfer set" "arm interval" "report interval" "disarm interval" "arm rbe" "report rbe" "disarm rbe" "delete data set" conclude; do step_ok $B "$s"; done
step_ok $B "read TASE2_Version" 'Integer\(2000\), Integer\(8\)'
step_ok $B "read Supported_Features" 'unused: 4, bytes: \[[0-9]+, 0\]'
step_xfail $B domains "igstase2-server does not serve GetNameList (its docs: 'Not served'): a peer limit"
step_xfail $B variables "same"

echo "-- our thin client -> telcon tase2-server (10103)"
B=ours-to-telcon
for s in associate domains variables "read data values" "write Real_002" "define data set" "read data set" "next transfer set" "delete data set" conclude; do step_ok $B "$s"; done
step_xfail $B identify "tase2-server answers Identify with service error 12: a peer limit"
for s in Bilateral_Table_ID TASE2_Version Supported_Features; do step_xfail $B "read $s" "the tase2-server test daemon does not serve it (its own blt-id fails with DataAccessError 10): a peer limit"; done
step_xfail $B "data set members" "tase2-server does not implement GetNamedVariableListAttributes: a peer limit"
step_xfail $B "arm interval" "ww-otj.70: DSTransferSet write with a 3-member Transfer_Set_Name is refused (DataAccessError 7)"
step_xfail $B "arm rbe" "ww-otj.70"

echo "-- what the peers put on the wire"
need "igs and telcon conclude the association" 'mms.conclude_RequestPDU_element'
need "a peer deletes a data set" 'mms.deleteNamedVariableList_element'
need "reports come from the peers' servers (port 10102)" 'tcp.srcport == 10102 && mms.informationReport_element'
want_field "$pcap" 'tcp.dstport == 10102' mms.vmd_specific 'TASE2_Version'
# ww-otj.61 (F1): the negotiated nesting level never exceeds the proposed one, on any association, and a proposal of 2 is answered 2
nest=$(tshark -r "$pcap" -Y 'mms.initiate_RequestPDU_element || mms.initiate_ResponsePDU_element' -T fields -e tcp.stream \
  -e mms.proposedDataStructureNestingLevel -e mms.negociatedDataStructureNestingLevel 2>/dev/null)
bad=$(awk -F'\t' '$2!=""{p[$1]=$2} $3!=""{if(!($1 in p)||$3>p[$1])print $1,p[$1],$3; if(p[$1]==2&&$3==2)two++} END{if(!two)print "no proposal of 2 answered with 2"}' <<<"$nest")
[ -z "$bad" ] && pass "negotiated nesting level <= proposed on every association; a proposal of 2 is answered 2" || fail "nesting level negotiated up: $bad"
# ww-otj.62 / .63 (F2, F3): iccp.lua over the capture: Supported_Features is 12 bits everywhere (ours and igs), versions are 2000;8
# (tshark does not run Lua as root; the Wireshark field is spelled "negociated")
lua() { setpriv --reuid=65534 --regid=65534 --clear-groups env HOME=/tmp tshark ${TSHARK_EXTRA:-} -X lua_script:/usr/local/share/iccp.lua "$@" 2>"$out/lua.err.all"
        grep -v -e 'matched 2 dissectors' -e 'Running as user' "$out/lua.err.all" > "$out/lua.err"; }
rows=$(lua -r "$pcap" -Y 'iccp.supported_features || iccp.tase2_version' -T fields -e iccp.supported_features -e iccp.tase2_version | sort -u)
[ ! -s "$out/lua.err" ] || fail "iccp.lua raised: $(head -3 "$out/lua.err")"
[ -n "$rows" ] && pass "iccp.lua decodes Supported_Features and TASE2_Version" || fail "iccp.lua found no Supported_Features / TASE2_Version"
lens=$(awk -F'\t' '$1 ~ /^[01]+$/ {print length($1)}' <<<"$rows" | sort -u | tr '\n' ' ')
[ "$lens" = "12 " ] && pass "Supported_Features is 12 bits on every read (ours and igs)" || fail "Supported_Features lengths: ${lens:-none}: $rows"
vers=$(awk -F'\t' '$2 ~ /^[{]/ {print $2}' <<<"$rows" | sort -u | tr '\n' ' ')
[ "$vers" = "{2000;8} " ] && pass "TASE2_Version is {2000;8} on every read" || fail "TASE2_Version is not only {2000;8}: ${vers:-none}"
n=$(lua -r "$pcap" -Y iccp | wc -l); [ "$n" -gt 0 ] && pass "iccp.lua tags $n packets" || fail "iccp.lua tagged nothing"

# ---- the real captures (tests/pcaps/iccp/fetch-real.sh; never committed) ---------------------------------------------------------------------
if ls /real/*.pcap >/dev/null 2>&1; then
  for rp in /real/*.pcap; do
    nm=$(basename "$rp" .pcap)
    m=$(tshark -r "$rp" -Y '_ws.malformed' 2>/dev/null | wc -l); [ "$m" = 0 ] && pass "real $nm: 0 malformed" || fail "real $nm: $m malformed"
    r=$(lua -r "$rp" -Y 'iccp.supported_features || iccp.tase2_version' -T fields -e iccp.supported_features -e iccp.tase2_version | sort -u)
    [ ! -s "$out/lua.err" ] && pass "real $nm: iccp.lua runs without Lua errors" || fail "real $nm: iccp.lua raised: $(head -3 "$out/lua.err")"
    feats=$(awk -F'\t' '$1 ~ /^[01]+$/ {print length($1)}' <<<"$r" | sort -u | tr '\n' ' ')
    [ "$feats" = "12 " ] && pass "real $nm: Supported_Features is 12 bits" || fail "real $nm: Supported_Features lengths: ${feats:-none}"
    case $nm in
      mms)  want='{1996;8}';; iccp) want='{2000;8}';; *) want='';;
    esac
    [ -z "$want" ] || { grep -q -F "$want" <<<"$r" && pass "real $nm: TASE2_Version $want" || fail "real $nm: TASE2_Version $want not seen: $r"; }
  done
else
  skip "real captures" "tests/pcaps/iccp/real is absent: run tests/pcaps/iccp/fetch-real.sh, then make conformance PROTO=iccp"
fi
