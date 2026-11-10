#!/bin/bash
# Negative control for the harness itself (runs inside testpeers): a deliberately damaged DNP3 capture must FAIL expert_clean,
# and an empty one must FAIL too. Usage: docker run --rm -v ...  or via `make conformance-selftest`.
set -u
. "$(dirname "$0")/lib.sh"
export PROTO_FILTER=dnp3
src=${1:?pcap}
t=$(mktemp -d)
# damage: a response's analog range stop byte (1e 06 00 00 00 -> stop 0x20) no longer matches its CRC; Wireshark only warns.
python3 -I - "$src" "$t/bad.pcap" <<'PY'
import sys
b = bytearray(open(sys.argv[1], 'rb').read())
i = b.find(bytes([0x1e, 0x06, 0x00, 0x00, 0x00]))
assert i != -1, 'no analog object in the capture'
b[i + 4] = 0x20
open(sys.argv[2], 'wb').write(b)
PY
tcpdump -r "$src" -w "$t/empty.pcap" 'port 1' 2>/dev/null
expect_fail() { local out; out=$( (expert_clean "$1") 2>&1 ); if echo "$out" | grep -q FAIL; then echo "ok:   $2 is rejected"; else echo "FAIL: $2 was accepted" >&2; echo "$out" >&2; FAILS=$((FAILS+1)); fi; }
FAILS=0
expect_fail "$t/bad.pcap" "damaged capture"
expect_fail "$t/empty.pcap" "empty capture"
[ "$FAILS" = 0 ] && echo "SELFTEST PASS" || { echo "SELFTEST FAIL" >&2; exit 1; }
