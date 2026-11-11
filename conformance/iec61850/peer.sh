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
# No libiec61850 goose_subscriber_example here (ww-otj.72): its kernel filter is `ether proto 0x88b8` at offset 12, which drops our (and its own
# publisher's) VLAN priority-tagged frames when the tag is inline, as on lo; a NIC with VLAN offload strips the tag first. go-iec61850 below
# subscribes instead and handles the tag.
echo "== mms_utility: directory"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -d
# Independent stacks (docker/testpeers): iec61850bean (Java, own ASN.1 codec) and otfabric/go-iec61850 (Go); neither shares code with
# libiec61850 or with the iec61850-rs crate under the server. They fail the run by printing ERROR / missing markers (check.sh).
echo "== iec61850bean ConsoleClient: m (model), ga (read all data)"
printf 'm\nga\nq\n' | timeout 60 java -cp '/opt/iec61850bean/*' com.beanit.iec61850bean.app.ConsoleClient -h $H -p 102
echo "== go-iec61850: browse, data sets, GoCB, reads, GOOSE subscriber on lo (8 s)"
timeout 45 ffgoiec $H:102 lo 'simpleIOGenericIO/GGIO1.AnIn1.mag.f[MX]'
exit 0
