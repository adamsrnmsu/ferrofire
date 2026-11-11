# shellcheck shell=bash
# Sourced by inside.sh before the capture: build the ICCP Spicy analyzer (tools/zeek/iccp, copied in by PEER_EXTRA) for zeek.sh and the zeek_run step.
a=/usr/local/share/zeek-iccp/analyzer
if command -v spicyz >/dev/null && spicyz -o /tmp/iccp.hlto "$a/iccp.spicy" "$a/iccp.evt" >/tmp/spicyz.log 2>&1; then :
else echo "FAIL: spicyz could not build the ICCP analyzer (see out/spicyz.log)" >&2; mkdir -p /out; cp /tmp/spicyz.log /out/spicyz.log 2>/dev/null; FAILS=$((FAILS+1)); fi
# The analyzer claims tcp/102; the peers' servers listen on 10102 and 10103
cat > /tmp/ports.zeek <<'ZEEK'
event zeek_init() { Analyzer::register_for_ports(Analyzer::ANALYZER_ICCP, set(10102/tcp, 10103/tcp)); }
ZEEK
