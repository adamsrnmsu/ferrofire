# Parse failures are not silent (ww-otj.50): a message Spicy drops raises iccp_parse_error / iccp_tpkt_error / iccp_unparsed_spdu in weird.log,
# and valid traffic raises none. The fuzz and malformed corpora are fixed, so the counts per weird name are the baseline.
# @TEST-EXEC: zeek -C -r $PCAPS/fuzz.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: grep -v '^#' weird.log | cut -f7 | grep '^iccp_' | sort | uniq -c > fuzz.weirds
# @TEST-EXEC: btest-diff fuzz.weirds
# @TEST-EXEC: mkdir m && cd m && zeek -C -r $PCAPS/malformed.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: grep -v '^#' m/weird.log | cut -f7 | grep '^iccp_' | sort | uniq -c > malformed.weirds
# @TEST-EXEC: btest-diff malformed.weirds
# @TEST-EXEC: for f in $PCAPS/tase2_assoc.pcap $PCAPS/tase2_report.pcap $PCAPS/iec61850/*.pcap; do rm -rf v; mkdir v; (cd v && zeek -C -r $f $ICCP_HLTO $PACKAGE/scripts); ! grep -q iccp_ v/weird.log 2>/dev/null || { echo "weird on $f"; exit 1; }; done
