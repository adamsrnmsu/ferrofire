# shellcheck shell=bash disable=SC2154
# The publisher's frames as tshark decodes them, and the writes the DUT's two subscribers sent the provider (peer.out).
want_field "$pcap" goose goose.gocbRef 'PUBLD0/LLN0$GO$gcbTrip'
want_field "$pcap" goose goose.datSet 'PUBLD0/LLN0$dsTrip'   # codespell:ignore datset
want_field "$pcap" goose goose.appid 0x0007
grep -q '^PUBLISH .* = true' "$out/peer.out" && pass "go-iec61850 published a TRIP" || fail "go-iec61850 did not publish"
# a write request batches tags (WRITE=a:x,b:y), so each tag:value is looked for anywhere in a PROVIDER: WRITE= line
writes() { grep '^PROVIDER: WRITE=' "$out/peer.out" | sed 's/^PROVIDER: WRITE=//' | tr ',' '\n' | grep -c -x -F -- "$1"; }
# standard ExtRef subscription: Tr.general mirrored into pub.trip-mirror, both ways
[ "$(writes pub.trip-mirror:true)" -ge 1 ] && pass "standard: mirror written true" || fail "standard: no mirror true write"
[ "$(writes pub.trip-mirror:false)" -ge 1 ] && pass "standard: mirror written false" || fail "standard: no mirror false write"
# bennu's SEL subscription: each rising edge of position 0 writes false to the trip tag once (two TRIPs published)
n=$(writes bus-1.breaker:false)
[ "$n" -ge 2 ] && pass "SEL: $n trip writes (two rising edges)" || fail "SEL: $n trip writes, want 2 or more"
[ "$(writes bus-1.breaker:true)" = 0 ] && pass "SEL: no true write" || fail "SEL: a TRIP never writes true"
