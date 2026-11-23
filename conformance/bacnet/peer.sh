# shellcheck shell=bash disable=SC2154
# conformance/stock/probe_bacnet.py (a bacpypes3 client: Who-Is, the device and each object by RP/RPM, WP, errors, DCC) against the
# DUT at DUT_IP, once its feed has given analog-input 0 its value; then nmap's bacnet-info; then what reached the provider.
me=$(python3 -c 'import fcntl, socket, struct, ipaddress
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); r = struct.pack("256s", b"eth0")
ip = socket.inet_ntoa(fcntl.ioctl(s.fileno(), 0x8915, r)[20:24]); m = socket.inet_ntoa(fcntl.ioctl(s.fileno(), 0x891B, r)[20:24])
print("%s/%d" % (ip, ipaddress.IPv4Network("0.0.0.0/" + m).prefixlen))')
/opt/v/bin/python - "$DUT_IP" "$me" <<'PY'
import asyncio, sys
from bacpypes3.app import Application
from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.pdu import Address
async def main():
    app = Application.from_args(SimpleArgumentParser().parse_args(["--name", "wait", "--instance", "598", "--address", sys.argv[2] + ":47809"]))
    for _ in range(60):
        try:
            if await asyncio.wait_for(app.read_property(Address(sys.argv[1]), "analog-input,0", "present-value"), 1) == 250.5:
                print("feed: analog-input,0 reads 250.5"); break
        except Exception:
            pass
        await asyncio.sleep(0.5)
    else:
        print("feed: analog-input,0 never read 250.5")
    app.close()
asyncio.run(main())
PY
DCC_WAIT=70 /opt/v/bin/python /conformance/stock/probe_bacnet.py "$DUT_IP" "$me:47808" > /out/transcript.txt 2>&1
nmap -sU -p 47808 --script bacnet-info "$DUT_IP" 2>&1 | grep -v "^Starting Nmap\|^Nmap done\|scan report\|Host is up\|latency\|MAC Address" > /out/nmap.txt
echo "## provider" >> /out/transcript.txt
python3 - "$DUT_IP" >> /out/transcript.txt <<'PY'
import sys, zmq
s = zmq.Context.instance().socket(zmq.REQ); s.setsockopt(zmq.RCVTIMEO, 5000); s.connect("tcp://%s:5555" % sys.argv[1])
s.send(b"LOG=\0"); sys.stdout.write(s.recv().decode())
PY
# Beyond the probe above (its transcript is stock's golden): a Who-Is sent to 255.255.255.255 (ww-o17), then a BO 2 write of inactive at
# priority 5, and the WRITE= line it adds to the provider's log (ww-s31). Each prints one line to /out/extra.txt, which check.sh reads.
python3 - "$DUT_IP" > /out/extra.txt <<'PY'
import socket, sys, time
dut = sys.argv[1]
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1); s.bind(("0.0.0.0", 47808)); s.settimeout(0.5)
s.sendto(bytes.fromhex("810b000c0120ffff00ff1008"), ("255.255.255.255", 47808))     # BVLC original-broadcast, NPDU global, Who-Is
iam, end = False, time.time() + 4
while time.time() < end and not iam:
    try:
        data, addr = s.recvfrom(1500)
    except socket.timeout:
        continue
    iam = addr[0] == dut and bytes.fromhex("1000c4020004d2") in data
print("Who-Is to 255.255.255.255 -> " + ("I-Am device 1234" if iam else "(no I-Am)"))
PY
plog() { python3 - "$DUT_IP" <<'PY'
import sys, zmq
s = zmq.Context.instance().socket(zmq.REQ); s.setsockopt(zmq.RCVTIMEO, 5000); s.connect("tcp://%s:5555" % sys.argv[1])
s.send(b"LOG=\0"); sys.stdout.write(s.recv().decode())
PY
}
plog > /tmp/log.before
r=$(/opt/v/bin/python - "$DUT_IP" "$me" <<'PY'
import asyncio, sys
from bacpypes3.app import Application
from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.basetypes import BinaryPV
from bacpypes3.pdu import Address
async def main():
    app = Application.from_args(SimpleArgumentParser().parse_args(["--name", "bo5", "--instance", "597", "--address", sys.argv[2] + ":47810"]))
    r = await asyncio.wait_for(app.write_property(Address(sys.argv[1]), "binary-output,2", "present-value", BinaryPV("inactive"), priority=5), 5)
    print("ACK" if r is None else r)
    app.close()
asyncio.run(main())
PY
)
sleep 3
echo "WP binary-output,2 inactive prio 5 -> $r; provider got: $(plog | tail -n +"$(($(wc -l < /tmp/log.before) + 1))" | tr '\n' ' ')" >> /out/extra.txt
cat /out/extra.txt
cat /out/transcript.txt
echo "BACNET PEERS DONE"
