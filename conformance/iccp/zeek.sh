# shellcheck shell=bash
# No Zeek parser exists for MMS or ICCP (the CISA ICSNPP set has none), so there is nothing to assert here: the protocol is judged
# by tshark's mms dissector (expert info, fields) in check.sh and by libiec61850's generic client.
echo "SKIP: no Zeek MMS/ICCP parser exists; judged by tshark (check.sh)"
