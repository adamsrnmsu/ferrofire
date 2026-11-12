#!/usr/bin/env bash
# Run iccp.lua over tests/pcaps/iccp with tshark and compare the iccp.* fields with tests/pcaps/iccp/expected/*.tsv.
#   test-iccp.sh            compare       test-iccp.sh --update   rewrite expected/ (review the git diff)
# Also checks: no Lua errors, no malformed MMS in the clean captures, nothing tagged in plain IEC 61850 captures.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
corp="$here/../../tests/pcaps/iccp"
lua="$here/iccp.lua"
upd="${1:-}"
fields=(-e frame.number -e iccp.op -e iccp.object -e iccp.class -e iccp.type -e iccp.value -e iccp.quality -e iccp.result
        -e iccp.transfer_set -e iccp.ds_conditions -e iccp.timetag -e iccp.bilateral_table_id -e iccp.tase2_version -e iccp.supported_features -e iccp.supported_blocks)
ts() { tshark -r "$1" -X "lua_script:$lua" "${@:2}" 2>"$err"; }
err="$(mktemp)"; trap 'rm -f "$err"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
mkdir -p "$corp/expected"
for pc in tase2_assoc tase2_report; do
  f="$corp/$pc.pcap"
  out="$(ts "$f" -Y iccp -T fields "${fields[@]}" -E occurrence=a -E separator=/s -E aggregator=';')"
  ! grep -qi lua "$err" || fail "$pc: $(cat "$err")"
  [ "$(ts "$f" -Y _ws.malformed | wc -l)" -eq 0 ] || fail "$pc: malformed packets"
  if [ "$upd" = --update ]; then printf '%s\n' "$out" > "$corp/expected/$pc.tsv"
  else diff <(printf '%s\n' "$out") "$corp/expected/$pc.tsv" >&2 || fail "$pc: iccp fields differ from expected/$pc.tsv"; fi
done
for f in "$corp"/iec61850/*.pcap; do
  [ "$(ts "$f" -Y mms | wc -l)" -gt 0 ] || fail "$f: no MMS decoded"
  [ "$(ts "$f" -Y iccp | wc -l)" -eq 0 ] || fail "$f: plain IEC 61850 tagged as ICCP"
  [ "$(ts "$f" -Y _ws.malformed | wc -l)" -eq 0 ] || fail "$f: malformed packets"
  ! grep -qi lua "$err" || fail "$f: $(cat "$err")"
done
for pc in malformed fuzz; do  # hostile input: tshark must finish, the Lua must not raise
  ts "$corp/$pc.pcap" -T fields -e iccp.op >/dev/null || fail "$pc: tshark exit"
  ! grep -qi lua "$err" || fail "$pc: $(cat "$err")"
done
echo "iccp dissector ok"
