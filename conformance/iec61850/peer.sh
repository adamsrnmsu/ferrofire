# shellcheck shell=bash disable=SC2154
# libiec61850's unmodified example clients and GOOSE subscriber (GPL-3.0, test peers only, never linked) against server.xml.
# Their hard-coded names (simpleIOGenericIO/GGIO1.AnIn1, LLN0.Events, LLN0.RP.EventsRCB01, gcbAnalogValues, appid 1000) are the
# server's defaults, so no peer is patched.
H=127.0.0.1
echo "== client_example1: read AnIn1, the Events data set, the report control block"
timeout 20 stdbuf -oL iec61850_client_example1 $H 102
echo "== control: operate SPCSO1 (direct), SPCSO2 (select before operate), SPCSO3"
timeout 20 stdbuf -oL client_example_control $H 102
echo "== reporting: general interrogation of EventsRCB01"
timeout 15 stdbuf -oL iec61850_client_example_reporting $H 102
echo "== goose_subscriber on lo (5 s)"
timeout 5 stdbuf -oL goose_subscriber_example lo
echo "== mms_utility: directory"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -d
exit 0
