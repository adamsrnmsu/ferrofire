# shellcheck shell=bash disable=SC2154
# The Zeek Spicy analyzer (tools/zeek/iccp, built by prepare.sh) over this run's capture, which includes the peers' own servers on 10102/10103,
# and over the real captures when tests/pcaps/iccp/fetch-real.sh has fetched them. zero parser errors = no weird.log / reporter.log rows.
# Known gap (bead ww-otj.84): the analyzer rejects BER indefinite length (0x80), which the vendor stack in the real iccp.pcap uses; the weird is
# tolerated for that capture only, so a new kind of parse failure still fails the run.
zeek_clean() { # OUTDIR LABEL: Zeek ran, wrote iccp.log and mms.log, and complained about nothing
  local z=$1/zeek l=$2 known='^$'
  [ "$l" = "real iccp" ] && known='iccp_parse_error.*\$\$ != 128'
  [ -s "$z/iccp.log" ] && pass "$l: zeek iccp.log has $(grep -vc '^#' "$z/iccp.log") rows" || fail "$l: zeek wrote no iccp.log"
  [ -s "$z/mms.log" ] && pass "$l: zeek mms.log has $(grep -vc '^#' "$z/mms.log") rows" || fail "$l: zeek wrote no mms.log"
  if [ -s "$z/weird.log" ] && [ "$(grep -v '^#' "$z/weird.log" | grep -vc "$known")" != 0 ]; then fail "$l: zeek weird.log: $(grep -v '^#' "$z/weird.log" | grep -v "$known" | head -3)"; else pass "$l: zeek weird.log empty"; fi
  if [ -s "$z/reporter.log" ] && [ "$(grep -vc '^#' "$z/reporter.log")" != 0 ]; then fail "$l: zeek reporter.log: $(grep -v '^#' "$z/reporter.log" | head -3)"; else pass "$l: zeek reporter.log empty"; fi
  if grep -qi 'error' "$z/zeek.stderr" 2>/dev/null; then fail "$l: zeek stderr: $(head -3 "$z/zeek.stderr")"; fi
}
if [ "${ZEEK_OK:-0}" = 1 ]; then
  zeek_clean "$out" "this run"
  want_zeek iccp class supported_features
  want_zeek iccp class tase2_version
  want_zeek iccp value '{2000;8}'
  [ "$(zeek-cut supported_features < "$out/zeek/iccp.log" | grep -v -x -- '-' | awk '{print length($0)}' | sort -u | tr '\n' ' ')" = "12 " ] \
    && pass "zeek: Supported_Features is 12 bits on every association" || fail "zeek: Supported_Features is not always 12 bits"
  zeek-cut supported_blocks < "$out/zeek/iccp.log" | grep -q 'BasicServices' && pass "zeek: Supported_Features decoded to block names" || fail "zeek: no block names in supported_blocks"
  # the peers' servers (10102, 10103) are parsed too
  zeek-cut id.resp_p < "$out/zeek/iccp.log" | grep -q -x 10102 && pass "zeek parses the igs server's port (10102)" || fail "zeek logged nothing for port 10102"
  zeek-cut id.resp_p < "$out/zeek/iccp.log" | grep -q -x 10103 && pass "zeek parses the telcon server's port (10103)" || fail "zeek logged nothing for port 10103"
  for rp in /real/*.pcap; do
    [ -f "$rp" ] || continue
    nm=$(basename "$rp" .pcap); mkdir -p "$out/zeek-real-$nm/zeek"
    (cd "$out/zeek-real-$nm/zeek" && zeek -C -r "$rp" ${ZEEK_LOAD} >zeek.stdout 2>zeek.stderr) || fail "real $nm: zeek -r failed"
    zeek_clean "$out/zeek-real-$nm" "real $nm"
    f=$(zeek-cut supported_features < "$out/zeek-real-$nm/zeek/iccp.log" | grep -v -x -- '-' | awk '{print length($0)}' | sort -u | tr '\n' ' ')
    [ "$f" = "12 " ] && pass "real $nm: zeek Supported_Features is 12 bits" || fail "real $nm: zeek Supported_Features lengths: ${f:-none}"
    case $nm in mms) v='{1996;8}';; iccp) v='{2000;8}';; *) v='';; esac
    [ -z "$v" ] || { zeek-cut tase2_version < "$out/zeek-real-$nm/zeek/iccp.log" | grep -q -x -F -- "$v" && pass "real $nm: zeek TASE2_Version $v" || fail "real $nm: zeek TASE2_Version $v not seen"; }
  done
fi
