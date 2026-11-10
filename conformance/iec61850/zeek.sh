# shellcheck shell=bash
# No Zeek parser exists for MMS or GOOSE (the CISA ICSNPP set has none), so there is nothing to assert here: the protocol is judged
# by tshark's mms and goose dissectors (expert info, fields) in check.sh and by libiec61850's own clients.
echo "SKIP: no Zeek MMS/GOOSE parser exists; judged by tshark (check.sh)"
