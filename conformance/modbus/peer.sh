# shellcheck shell=bash disable=SC2154
# One RADIO datagram gives the registers values; then pymodbus (an independent client), libmodbus' mbpoll and nmap's modbus-discover.
d=$(dirname "$0")
python3 - <<'PY'
import socket
g = b"54386972605942b"
body = b"load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,load-1_bus-1.active:false,bus-1.gen_mw:12.5,\0"
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
u.sendto(bytes([len(g)]) + g + body, ("239.0.0.1", 40000))
PY
sleep 2
echo "== pymodbus"; python3 "$d/client.py" || exit 1
echo "== mbpoll (0-based addresses: -0)"
mbpoll -0 -1 -m tcp -a 1 -t 3 -r 30000 -c 1 127.0.0.1 | tee /tmp/mbpoll-ir.txt
mbpoll -0 -1 -m tcp -a 1 -t 1 -r 10000 -c 2 127.0.0.1 | tee /tmp/mbpoll-di.txt
mbpoll -0 -1 -m tcp -a 1 -t 4 -r 40000 -c 1 127.0.0.1 | tee /tmp/mbpoll-hr.txt
mbpoll -0 -1 -m tcp -a 1 -t 0 -r 0 -c 2 127.0.0.1 | tee /tmp/mbpoll-co.txt
mbpoll -0 -1 -m tcp -a 1 -t 4 -r 40000 127.0.0.1 1234 | tee /tmp/mbpoll-wr.txt
mbpoll -0 -1 -m tcp -a 1 -t 0 -r 0 127.0.0.1 1 | tee /tmp/mbpoll-wc.txt
echo "== nmap modbus-discover"
nmap -Pn -p 502 --script modbus-discover 127.0.0.1 | tee /tmp/nmap-modbus.txt
