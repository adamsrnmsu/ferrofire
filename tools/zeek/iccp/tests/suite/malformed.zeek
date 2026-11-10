# Malformed input must not crash Zeek or lose the valid traffic that follows it on the same connection.
# @TEST-EXEC: zeek -C -r $PCAPS/malformed.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: btest-diff mms.log
# @TEST-EXEC: btest-diff iccp.log
