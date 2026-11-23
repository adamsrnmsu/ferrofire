#!/bin/sh
# usage: check.sh rust|stock TRANSCRIPT. Each expect.txt line (ID|text) must be a line of the BACnet4J peer's transcript, except the IDs
# in xfail-WHO.txt, the DUT's known failures, which must still fail (a pass means the divergence went away: delete the line).
set -eu
here=$(cd "$(dirname "$0")" && pwd); who=$1; t=$2; rc=0
[ -s "$t" ] || { echo "FAIL: empty transcript $t"; exit 1; }
n=0
while IFS='|' read -r id text; do
  case $id in ''|'#'*) continue;; esac
  n=$((n+1))
  if grep -Fq -- "$text" "$t"; then ok=1; else ok=0; fi
  if grep -qx -- "$id" "$here/xfail-$who.txt"; then
    [ $ok = 0 ] && echo "xfail: $id" || { echo "FAIL: $id now passes, remove it from xfail-$who.txt"; rc=1; }
  else
    [ $ok = 1 ] && echo "ok:   $id" || { echo "FAIL: $id: no line has '$text'"; rc=1; }
  fi
done < "$here/expect.txt"
# an xfail entry that names no expectation is a typo
for id in $(grep -v '^#' "$here/xfail-$who.txt"); do grep -q "^$id|" "$here/expect.txt" || { echo "FAIL: xfail-$who.txt names unknown $id"; rc=1; }; done
echo "bacnet-peer $who: $n expectations, $([ $rc = 0 ] && echo pass || echo FAIL)"
exit $rc
