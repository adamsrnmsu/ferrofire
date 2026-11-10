# 150 seeded mutants of valid PDUs, one connection each: Zeek must read all of them (exit 0) and still log the clean part.
# @TEST-EXEC: zeek -C -r $PCAPS/fuzz.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: test "$(grep -vc '^#' conn.log)" = 150
# @TEST-EXEC: test "$(grep -c initiate_req mms.log)" = 150
