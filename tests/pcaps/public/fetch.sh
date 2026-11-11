#!/usr/bin/env bash
# Fetch third-party DNP3 and IEC 61850 (MMS, GOOSE) captures from ITI/ICS-Security-Tools (repo licence CC-BY-4.0; the individual captures
# are community-sourced with no stated terms, so they are never committed: tests/pcaps/public/data/ is git-ignored) at a pinned commit and
# verify every file against ITI-ICS-Security-Tools.sha256. Then check-parsers.sh runs tshark and Zeek over them.
#   fetch.sh [DIR]      (default: tests/pcaps/public/data)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dir="${1:-$here/data}"
C=bcb93ee1cdf395a457cc08f639cf29aab5975f01
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
n=0
while read -r want path; do
  f="$dir/$path"; mkdir -p "$(dirname "$f")"
  if [ ! -f "$f" ] || [ "$(sha "$f")" != "$want" ]; then
    url="https://raw.githubusercontent.com/ITI/ICS-Security-Tools/$C/pcaps/$(python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1]))' "$path")"
    curl -fsSL --retry 3 -o "$f" "$url"; chmod 644 "$f"
    [ "$(sha "$f")" = "$want" ] || { echo "FAIL: $path sha256 differs from the pinned $want" >&2; rm -f "$f"; exit 1; }
  fi
  n=$((n+1))
done < "$here/ITI-ICS-Security-Tools.sha256"
echo "ok: $n captures in $dir"
