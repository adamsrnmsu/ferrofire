# Zeek over tase2_report.pcap: the logs must match the baseline.
# @TEST-EXEC: zeek -C -r $PCAPS/tase2_report.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: btest-diff mms.log
# @TEST-EXEC: btest-diff iccp.log
