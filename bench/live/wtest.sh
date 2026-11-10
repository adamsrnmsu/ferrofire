#!/bin/bash
r(){ timeout 5 bennu-probe --e tcp://172.16.1.2:5555 --c read --t uppervalve.open 2>&1 | grep -A1 Reply | tail -n 1 | tr -d '\t'; }
echo "provider uppervalve.open before: $(r)"
python3 - <<'PY'
import socket,struct
s=socket.create_connection(("10.1.1.102",502),timeout=5)
s.sendall(struct.pack(">HHHBBHH",2,0,6,1,6,40001,65535)); print("FC6 write 40001=65535 ->",s.recv(256).hex())
PY
for i in 1 2 3 4 5 6; do echo "t+${i}s provider uppervalve.open: $(r)"; sleep 1; done
