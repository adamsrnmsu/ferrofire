#!/usr/bin/env bash
# Decode a generated datagram with tshark and the Lua dissector. The pcap is built here from scratch, with its own
# libstdc++ hash, so this also cross-checks the group against the Rust implementation. Needs tshark and python3 (host).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
python3 -I "$here/make_pcap.py" "$tmp/v1.pcap"
out="$(tshark -r "$tmp/v1.pcap" -X "lua_script:$here/ferrofire_datagram.lua" -T fields \
  -e ferrofire.group -e ferrofire.tag -e ferrofire.value -e ferrofire.seq -e ferrofire.cyc -e ferrofire.t -e ferrofire.nul \
  -E separator=/s)"
want='54386972605942b sim.t,lock.height,lowervalve.open 0.1,5,0 7 3 1234567890 True'
[ "$out" = "$want" ] || { echo "got:  $out" >&2; echo "want: $want" >&2; exit 1; }
echo "dissector ok"
