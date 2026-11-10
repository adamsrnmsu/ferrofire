# Zeek over tase2_assoc.pcap: the logs must match the baseline.
# @TEST-EXEC: zeek -C -r $PCAPS/tase2_assoc.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: btest-diff mms.log
# @TEST-EXEC: btest-diff iccp.log
