#!/bin/bash
# (rtu-1.xml must not contain XML comments: bennu-field-device silently hangs on them.)
# Native-mode conformance (spec 6.5): ferrofire run vs stock bennu tools inside bennu:arm64. Run in the Lima VM:
#   limactl shell phenix bash <abs>/tests/conformance.sh        (needs dist/ferrofire from `make linux`)
set -u
D=$(cd "$(dirname "$0")/.." && pwd)
docker run --rm --name ff-conf-$$ -v "$D/dist:/sim:ro" -v "$D/tests/conformance:/conf:ro" -v "$D/models:/models:ro" bennu:arm64 bash -c '
mkdir -p /tmp/w && cd /tmp/w && cp /sim/ferrofire . && cp /conf/rtu-1.xml .
echo "ldd libzmq: $(ldd ./ferrofire | grep -c zmq) (0 = statically linked)"
./ferrofire run /models/waterway.yaml >rb.log 2>&1 &
sleep 1
python3 /conf/dish.py >sub.log 2>&1 &
bennu-field-device --file rtu-1.xml >fd.log 2>&1 &
sleep 4
E=tcp://127.0.0.1:5555
echo "== query"; Q=$(timeout 15 bennu-probe --e $E --c query); echo "$Q" | head -30; echo "POINTS: $(echo "$Q" | grep -c "^\s[a-z]*\.[a-z]*$")"
r() { timeout 15 bennu-probe --e $E --c read --t "$1" 2>&1 | grep -A1 Reply | tail -1 | tr -d "\t"; }
w() { timeout 15 bennu-probe --e $E --c write --t "$1" --v "$2" 2>&1 | grep -A1 Reply | tail -1 | tr -d "\t"; }
echo "== read/write"
echo "lock.height: $(r lock.height)"; echo "uppervalve.open: $(r uppervalve.open)"
echo "write uppervalve.open=1: $(w uppervalve.open 1)"; sleep 4
echo "uppervalve.open: $(r uppervalve.open)"; echo "lock.height (filling): $(r lock.height)"
for a in "no.such 1" "lock.height 3"; do set -- $a; echo "write $1: $(timeout 15 bennu-probe --e $E --c write --t $1 --v $2 2>&1 | tail -1)"; done
echo "== subscriber (head)"; sleep 2; cat sub.log
echo "== modbus (field device reading ferrofire, raw frames)"
python3 - <<PY
import socket,struct
def rd(fc,addr,n):
    s=socket.create_connection(("127.0.0.1",5502),timeout=5)
    s.sendall(struct.pack(">HHHBBHH",1,0,6,1,fc,addr,n)); r=s.recv(256); s.close(); return r
def wr(addr,val):
    s=socket.create_connection(("127.0.0.1",5502),timeout=5)
    s.sendall(struct.pack(">HHHBBHH",2,0,6,1,6,addr,val)); r=s.recv(256); s.close(); return r
for a in (0,30000):
    r=rd(4,a,2); print("FC4 addr",a,r.hex(), struct.unpack(">HH",r[9:13]) if len(r)>=13 and r[7]==4 else "")
r=wr(40000,0); print("FC6 close valve ->",r.hex())
PY
sleep 2; echo "uppervalve.open after modbus write 0: $(r uppervalve.open)"
for f in rb.log fd.log; do echo "== $f"; tail -n 8 $f; done'
