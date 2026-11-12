# S7comm shares tcp/102 (ww-otj.52): this analyzer must not confirm it as ICCP, log it or raise weirds on it. ICSNPP's s7comm (a C++/binpac
# plugin, not built here) is the other claimant; its DPD signature needs a COTP CR first, so an S7 flow reaches both analyzers and ours must step aside.
# @TEST-EXEC: zeek -C -r $PCAPS/s7comm.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: test ! -e mms.log && test ! -e iccp.log
# @TEST-EXEC: ! grep -q iccp_ weird.log
# @TEST-EXEC: ! grep -q iccp conn.log
# And the reverse: ICCP is still ICCP with an unrelated COTP-only stream next to it.
# @TEST-EXEC: mkdir i && cd i && zeek -C -r $PCAPS/tase2_assoc.pcap $ICCP_HLTO $PACKAGE/scripts
# @TEST-EXEC: grep -q iccp i/conn.log
