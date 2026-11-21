# shellcheck shell=bash disable=SC2154
# The stand-in provider, then go-iec61850 publishing gcbTrip: Tr.general false, then true, false, true, one state a second.
python3 /conformance/iec61850-rtu/provider.py & prov=$!
sleep 2
echo "== go-iec61850 publishes PUB/LD0 gcbTrip from substation.scd on lo"
timeout 30 ffgoiec publish lo /conformance/iec61850-rtu/substation.scd PUB LD0 gcbTrip 3
sleep 2
kill "$prov"
exit 0
