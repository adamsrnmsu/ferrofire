# Zeek over iec61850/reporting.pcap: the logs must match the baseline.
# @TEST-EXEC: zeek -C -r $PCAPS/iec61850/reporting.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: btest-diff mms.log
# @TEST-EXEC: test ! -e iccp.log
