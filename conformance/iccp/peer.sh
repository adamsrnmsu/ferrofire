# shellcheck shell=bash
# A generic MMS client (libiec61850 mms_utility, GPL, test peer only) and our thin ICCP client against server.xml on 127.0.0.1:102.
H=127.0.0.1
echo "== mms_utility: domains, identity, domain directory, reads"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -d
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -i
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -t EMS1
echo "-- mms_utility read LOAD_MW"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -a EMS1 -r LOAD_MW
echo "-- mms_utility read BRKR_1"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -a EMS1 -r BRKR_1
echo "-- mms_utility read Bilateral_Table_ID"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -a EMS1 -r Bilateral_Table_ID
echo "== iccp-client: browse, read, write, data set, transfer set, reports"
timeout 40 iccp-client $H:102 EMS1 LOAD_MW,BRKR_1,BUS_KV,SETPOINT SETPOINT LOAD_MW,BRKR_1,SETPOINT
echo "iccp-client exit: $?"
exit 0
