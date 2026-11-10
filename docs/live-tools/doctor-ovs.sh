#!/bin/bash
# doctor-ovs.sh : `ferrofire doctor` on the live range with OVS multicast snooping off (default), on, and after a re-join.
# Run from the repo root on the Mac. Needs the provider running (prov.sh) and /root/ff/ferrofire, mbq.py on rtu-1 and scada-server.
# A "member" process on scada-server joins 239.0.0.1 before snooping is switched on, like the stock device does: the doctor started later
# on the same host does not send a new IGMP report (the kernel only counts a second member), so under snooping without a querier it is starved.
# Always restores the bridge to its default (snooping off, other_config cleared) at the end, even on error.
T=docs/live-tools; O=$T/captures/07-doctor-ovs-snooping.txt
OVS() { limactl shell phenix sudo docker exec minimega ovs-vsctl "$@"; }
on() { $T/on.sh "$@"; }
D() { echo "cd /root/ff; ./ferrofire doctor --server tcp://172.16.1.2:5555 --seconds 4 ${1:+--listen '$1'} 2>&1; echo \"exit \$?\""; }
SC="udp://172.16.2.71;239.0.0.1:40000"; R1="udp://172.16.1.101;239.0.0.1:40000"
JOIN='setsid nohup python3 -c "
import socket,struct,time
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind((\"\",40001))
s.setsockopt(socket.IPPROTO_IP,socket.IP_ADD_MEMBERSHIP,socket.inet_aton(\"239.0.0.1\")+socket.inet_aton(\"172.16.2.71\")); time.sleep(300)" >/dev/null 2>&1 < /dev/null & sleep 1; ip maddr show dev eth1 | grep 239'
MB='cd /root/ff; python3 mbq.py 10.1.1.101 read 30008 1; sleep 3; python3 mbq.py 10.1.1.101 read 30008 1'
trap 'OVS set Bridge phenix mcast_snooping_enable=false; OVS remove Bridge phenix other_config mcast-snooping-aging-time; OVS remove Bridge phenix other_config mcast-snooping-disable-flood-unregistered; on scada-server 20 "pkill -f 40001" >/dev/null' EXIT
{
echo "# $(date -u +%FT%TZ)  bridge: the OVS bridge 'phenix' inside the minimega container (every VM NIC of the range is a port of it)"
echo '$ ovs-vsctl list-br; ovs-vsctl get Bridge phenix mcast_snooping_enable other_config'; OVS list-br; OVS get Bridge phenix mcast_snooping_enable other_config
echo; echo "=== A. snooping off (default)"
echo "--- scada-server (no libzmq on it); its default multicast route is the management NIC: doctor tells you"
echo '$ ferrofire doctor --server tcp://172.16.1.2:5555'; on scada-server 60 "$(D)"
echo "--- scada-server, listening on its range NIC"
echo "\$ ferrofire doctor --listen '$SC'"; on scada-server 60 "$(D $SC)"
echo "--- rtu-1: the stock device holds UDP port 40000 without SO_REUSEADDR, so the doctor cannot share the host with it"
echo "\$ ferrofire doctor --listen '$R1'"; on rtu-1 60 "$(D $R1)"
echo "--- a member process on scada-server joins 239.0.0.1 now (stands in for a stock device that joined earlier)"
echo '$ python3 (IP_ADD_MEMBERSHIP 239.0.0.1 on eth1) ; ip maddr show dev eth1'; on scada-server 30 "$JOIN"
echo; echo "=== B. snooping on, flood-unregistered disabled, aging 7200 s (no IGMP querier on this range); the member joined before, the doctor joins as a second socket"
OVS set Bridge phenix mcast_snooping_enable=true other_config:mcast-snooping-aging-time=7200 other_config:mcast-snooping-disable-flood-unregistered=true
echo '$ ovs-vsctl set Bridge phenix mcast_snooping_enable=true other_config:mcast-snooping-aging-time=7200 other_config:mcast-snooping-disable-flood-unregistered=true'; sleep 3
echo "\$ ferrofire doctor --listen '$SC'"; on scada-server 60 "$(D $SC)"
echo "--- rtu-1 (its stock device joined before snooping was on): Modbus input register 30008 (boat.location) twice, 3 s apart; the boat moves 5 m/s"
on rtu-1 60 "$MB"
echo; echo "=== C. snooping still on; the member re-joins (new IGMP report), rtu-1's field device is restarted"
on scada-server 30 'pkill -f 40001; sleep 1; ip maddr show dev eth1 | grep -c 239'; on scada-server 30 "$JOIN"
echo '$ ovs-appctl mdb/show phenix   (port numbers are the bridge ports of the joined NICs)'; limactl shell phenix sudo docker exec minimega ovs-appctl mdb/show phenix | head
echo "\$ ferrofire doctor --listen '$SC'"; on scada-server 60 "$(D $SC)"
echo '$ restart-rtu.sh on rtu-1'; on rtu-1 90 "$(cat $T/restart-rtu.sh)"
echo "--- rtu-1 Modbus input register 30008 again"; on rtu-1 60 "$MB"
echo; echo "=== D. restored: snooping off"
} 2>&1 | tee $O
OVS set Bridge phenix mcast_snooping_enable=false; OVS remove Bridge phenix other_config mcast-snooping-aging-time; OVS remove Bridge phenix other_config mcast-snooping-disable-flood-unregistered
{ echo '$ ovs-vsctl get Bridge phenix mcast_snooping_enable other_config'; OVS get Bridge phenix mcast_snooping_enable other_config; } 2>&1 | tee -a $O
