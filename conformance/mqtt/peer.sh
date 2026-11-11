# shellcheck shell=bash disable=SC2154
# Broker, stand-in provider, one RADIO datagram, then mosquitto_sub/mosquitto_pub, Eclipse Paho (python) and the Tahu host application.
d=$(dirname "$0")
cat > /tmp/mosquitto.conf <<'C'
user root
listener 1883 127.0.0.1
allow_anonymous true
C
mosquitto -c /tmp/mosquitto.conf > /tmp/mosquitto.log 2>&1 &
python3 "$d/provider.py" /tmp/provider.log &
python3 - <<'PY'
import socket, time
g = b"54386972605942b"
body = b"load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,\0"
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
for _ in range(10):   # the device may still be waiting for the broker: keep telling it the values
    u.sendto(bytes([len(g)]) + g + body, ("239.0.0.1", 40000)); time.sleep(0.5)
PY
echo "== the device is online"
mosquitto_sub -h 127.0.0.1 -t ferrofire/rtu-1/status -C 1 -W 30 | tee /tmp/status.txt
grep -q online /tmp/status.txt || { echo "FAIL: no online status within 30 s"; exit 1; }
echo "== mosquitto_sub (independent client): retained plain topics"
mosquitto_sub -h 127.0.0.1 -v -W 3 -t 'ferrofire/rtu-1/#' | tee /tmp/sub.txt
echo "== mosquitto_pub: a command with QoS 1, then read the provider log"
mosquitto_pub -h 127.0.0.1 -q 1 -t ferrofire/rtu-1/setpoint/set -m 300
sleep 2; cat /tmp/provider.log
echo "== paho"
python3 "$d/paho_client.py" /tmp/provider.log || exit 1
echo "== Tahu host application (Sparkplug B)"
java -cp '/opt/tahu-host:/opt/tahu-host/lib/*' TahuHost tcp://127.0.0.1:1883 waterway rtu-1 | tee /tmp/tahu.txt
grep -q 'TAHU HOST DONE' /tmp/tahu.txt || { echo "FAIL: the Tahu host did not finish"; exit 1; }
sleep 1; cat /tmp/provider.log
echo "== the device is killed (SIGKILL): the broker must publish its wills (NDEATH, status offline)"
mosquitto_sub -h 127.0.0.1 -v -t 'spBv1.0/waterway/NDEATH/rtu-1' -t ferrofire/rtu-1/status -W 8 > /tmp/will.txt &
wsub=$!
sleep 1; pkill -9 -f 'ferrofire field-device' || echo 'FAIL: could not kill the device'
wait "$wsub"; cat /tmp/will.txt
