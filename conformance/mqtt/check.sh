# shellcheck shell=bash disable=SC2154
# tshark field assertions on $pcap, and what the peers recorded (peer.out holds paho's PASS lines and the Tahu host's decode).
want_field "$pcap" 'mqtt.msgtype == 1' mqtt.protoname MQTT
want_field "$pcap" 'mqtt.msgtype == 1' mqtt.clientid ff-plain
want_field "$pcap" 'mqtt.msgtype == 1' mqtt.clientid ff-spb
want_field "$pcap" 'mqtt.msgtype == 1' mqtt.willtopic ferrofire/rtu-1/status
want_field "$pcap" 'mqtt.msgtype == 1' mqtt.willtopic spBv1.0/waterway/NDEATH/rtu-1
want_field "$pcap" 'mqtt.msgtype == 8' mqtt.topic ferrofire/rtu-1/+/set
want_field "$pcap" 'mqtt.msgtype == 8' mqtt.topic spBv1.0/waterway/NCMD/rtu-1
want_field "$pcap" 'mqtt.msgtype == 3' mqtt.topic ferrofire/rtu-1/level
want_field "$pcap" 'mqtt.msgtype == 3' mqtt.topic spBv1.0/waterway/NBIRTH/rtu-1
want_field "$pcap" 'mqtt.msgtype == 3' mqtt.topic spBv1.0/waterway/NDATA/rtu-1
want_field "$pcap" 'mqtt.msgtype == 3' mqtt.topic spBv1.0/waterway/NCMD/rtu-1
want_field "$pcap" 'mqtt.msgtype == 4' mqtt.msgtype 4                  # PUBACK: QoS 1 publishes were acknowledged
for p in "paho: status online" "paho: level 250.5 read" "paho: setpoint command reached the provider" \
         "paho: breaker command reached the provider" "paho: out-of-range command refused"; do
  grep -q "^PASS $p" "$out/peer.out" && pass "$p" || fail "$p"
done
grep -q 'setpoint:300.000000\|load-mw-setpoint:300.000000' "$out/peer.out" && pass "mosquitto_pub command reached the provider" || fail "mosquitto_pub command did not reach the provider"
grep -q '^ferrofire/rtu-1/level 250.5' "$out/peer.out" && pass "mosquitto_sub read level 250.5" || fail "mosquitto_sub did not read level 250.5"
grep -q '^NBIRTH seq=0 bdSeq=' "$out/peer.out" && pass "Tahu decoded our NBIRTH" || fail "Tahu did not decode the NBIRTH"
grep -q '^NBIRTH .*level=250.5(alias 1, Double)' "$out/peer.out" && pass "Tahu: NBIRTH level metric, alias and type" || fail "Tahu: NBIRTH level metric"
grep -q '^NDATA seq=' "$out/peer.out" && pass "Tahu decoded our NDATA" || fail "Tahu did not decode an NDATA"
grep -q '^REBIRTH OK' "$out/peer.out" && pass "Tahu NCMD Node Control/Rebirth made us send a new NBIRTH" || fail "rebirth did not work"
grep -q 'load-mw-setpoint:55.000000' "$out/peer.out" && pass "Tahu NCMD write reached the provider" || fail "Tahu NCMD write did not reach the provider"
grep -q '^spBv1.0/waterway/NDEATH/rtu-1' "$out/peer.out" && pass "NDEATH published when the device went away" || fail "no NDEATH"
