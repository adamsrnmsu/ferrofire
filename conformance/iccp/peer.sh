# shellcheck shell=bash
# Four legs, one capture (tcp 102, 10102, 10103):
#   1. libiec61850 mms_utility and our thin client against our server (the original acceptance)
#   2. igstase2-client (aklira/igs-iec-tase2)  -> our server on 102
#   3. telcon-tase2 tase2-client daemon        -> our server on 102
#   4. our thin client (ICCP_STEPS=1)          -> igstase2-server on 10102 and telcon tase2-server on 10103
# Every operation prints "### label", its output and "rc=N"; check.sh judges them. The peers are separate processes; nothing links them.
H=127.0.0.1
op() { local label=$1; shift; echo "### $label"; timeout 40 "$@" 2>&1; echo "rc=$?"; }

echo "== 1. mms_utility: domains, identity, domain directory, reads"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -d
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -i
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -t EMS1
echo "-- mms_utility read LOAD_MW"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -a EMS1 -r LOAD_MW
echo "-- mms_utility read BRKR_1"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -a EMS1 -r BRKR_1
echo "-- mms_utility read Bilateral_Table_ID"
timeout 15 stdbuf -oL mms_utility -h $H -p 102 -a EMS1 -r Bilateral_Table_ID
echo "== 1. iccp-client: browse, read, write, data set, transfer set, reports"
timeout 40 iccp-client $H:102 EMS1 LOAD_MW,BRKR_1,BUS_KV,SETPOINT SETPOINT LOAD_MW,BRKR_1,SETPOINT
echo "iccp-client exit: $?"

echo "== 2. igstase2-client -> our server"
IGS0="igstase2-client --server $H:102 --domain EMS1 --timeout-s 8"
IGS="$IGS0 --table-id BLT_RANGE_EMS1"
op igs-names $IGS names
op igs-names-datasets $IGS names --data-sets
op igs-get $IGS get LOAD_MW BRKR_1 SETPOINT CMD_1 BUS_KV
op igs-type $IGS type LOAD_MW
op igs-set $IGS set SETPOINT=12.5
op igs-create $IGS create DS1 LOAD_MW BRKR_1 SETPOINT
op igs-members-new-association $IGS members DS1
op igs-start $IGS start DSTrans_1 PRESET_DS --interval-s 1 --reports 2
op igs-start-rbe $IGS start DSTrans_1 PRESET_DS --rbe --interval-s 1 --reports 2
op igs-delete $IGS delete DS1
op igs-wrong-table $IGS0 --table-id OTHER_TABLE get LOAD_MW

echo "== 3. telcon tase2-client -> our server"
rm -f /tmp/tase2-test-client.sock
tase2-client daemon --addr $H:102 > /tmp/telcon-client.log 2>&1 &
for _ in $(seq 1 50); do [ -S /tmp/tase2-test-client.sock ] && break; sleep 0.1; done; sleep 1
TC="tase2-client"
op tc-blt-id $TC blt-id
op tc-name-list $TC name-list --domain EMS1
op tc-name-list-datasets $TC name-list --class dataset --domain EMS1
op tc-read-real $TC read --domain EMS1 --name SETPOINT --kind real
op tc-read-state $TC read --domain EMS1 --name CMD_1 --kind state
op tc-read-real-q $TC read --domain EMS1 --name BUS_KV --kind real_q
op tc-read-real-qtt $TC read --domain EMS1 --name LOAD_MW --kind real_q_time_tag
op tc-read-state-qtt $TC read --domain EMS1 --name BRKR_1 --kind state_q_time_tag
op tc-write-real $TC write --domain EMS1 --name SETPOINT --value '{"kind":"real","real":12.5}'
op tc-dataset-define $TC dataset define --domain EMS1 --name DS1 --member EMS1/SETPOINT --member EMS1/CMD_1
op tc-dataset-read $TC dataset read --domain EMS1 --name DS1 --kind real --kind state
op tc-transfer-set $TC transfer-set --domain EMS1 --dataset DS1 --member EMS1/SETPOINT --member EMS1/CMD_1 --interval 1
op tc-dataset-delete $TC dataset delete --domain EMS1 --name DS1
op tc-shutdown $TC shutdown
sleep 1

echo "== 4. our thin client -> igstase2-server (10102) and telcon tase2-server (10103)"
igstase2-server --listen $H:10102 --config /conformance/iccp/igs-server.toml > /tmp/igs-server.log 2>&1 &
rm -f /tmp/tase2-test-server.sock
tase2-server daemon --addr $H:10103 > /tmp/telcon-server.log 2>&1 &
for _ in $(seq 1 50); do [ -S /tmp/tase2-test-server.sock ] && break; sleep 0.1; done; sleep 1
echo "### ours-to-igs"
ICCP_STEPS=1 ICCP_CALLING_AP_TITLE=1.3.9999.1 timeout 90 iccp-client $H:10102 EMS1 LOAD_MW,BRKR_1,BUS_KV,SETPOINT SETPOINT LOAD_MW,BRKR_1,SETPOINT 2>&1
echo "rc=$?"
echo "### ours-to-telcon"
ICCP_STEPS=1 timeout 90 iccp-client $H:10103 ICC1 Real_001,Disc_001,State_001 Real_002 Real_001,Disc_001,State_001 2>&1
echo "rc=$?"
op telcon-server-shutdown tase2-server shutdown
pkill igstase2-server || true
exit 0
