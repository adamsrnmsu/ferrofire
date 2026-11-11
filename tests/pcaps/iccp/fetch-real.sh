#!/usr/bin/env bash
# Fetch the two public real-world ICCP/TASE.2 captures into tests/pcaps/iccp/real/ (git-ignored: they are other people's data and are never
# committed or redistributed) and verify their sha256. make conformance PROTO=iccp runs our parsers over them when the directory exists.
#   fetch-real.sh [DIR]      (default: tests/pcaps/iccp/real)
#   mms.pcap     Wireshark wiki SampleCaptures, ICCP between two utilities, 2005, TASE2_Version 1996.8, 12-member DSTransferSet
#   iccp.pcap    chaugan/wireshark-iccp-dissector pcaps/iccp_with_switchover_anon.pcap (pinned commit), 9 associations, TASE2_Version 2000.8
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dir="${1:-$here/real}"
CHAUGAN=332a84fa012146d4d1c51680b07ae91539820e44
SHA_MMS=9a84e82e438a599d51d3651f188999a2f1f141ee6edb51c3b507558bf4992410            # of the decompressed mms.pcap
SHA_ICCP=26488ed9b8a5f81167d476b0745505a941a8d46a48e81a0d26bc501cc2e56b9e
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
mkdir -p "$dir"
get() { # get NAME URL SHA [gz]
  local name=$1 url=$2 want=$3 gz=${4:-}
  if [ -f "$dir/$name" ] && [ "$(sha "$dir/$name")" = "$want" ]; then echo "ok: $name (already here)"; return; fi
  local tmp; tmp="$(mktemp)"; trap 'rm -f "$tmp"' RETURN
  curl -fsSL --retry 3 -o "$tmp" "$url"
  if [ -n "$gz" ]; then gunzip -c "$tmp" > "$dir/$name"; else cp "$tmp" "$dir/$name"; fi
  chmod 644 "$dir/$name"   # mktemp is 0600; the conformance container reads them as an unprivileged user
  [ "$(sha "$dir/$name")" = "$want" ] || { echo "FAIL: $name sha256 differs from the pinned $want" >&2; rm -f "$dir/$name"; exit 1; }
  echo "ok: $name"
}
get mms.pcap  https://wiki.wireshark.org/uploads/__moin_import__/attachments/SampleCaptures/mms.pcap.gz "$SHA_MMS" gz
get iccp.pcap "https://raw.githubusercontent.com/chaugan/wireshark-iccp-dissector/$CHAUGAN/pcaps/iccp_with_switchover_anon.pcap" "$SHA_ICCP"
