#!/usr/bin/env python3
"""mbq.py IP read|readh|write|watch|watchh ADDR [N|VALUE|N SECS] : minimal Modbus/TCP client (FC4 input registers, FC3 holding registers, FC6 write one holding register).
Prints the raw 16-bit registers and the value scaled over [0,1000] as the stock field device does."""
import socket, struct, sys
ip, op, a = sys.argv[1], sys.argv[2], int(sys.argv[3])
s = socket.create_connection((ip, 502), timeout=3)
if op in ("watch", "watchh"):   # poll FC4 (watchh: FC3) ADDR..ADDR+N every 50 ms for SECS seconds, print each change with its offset in ms
    import time; n, secs = int(sys.argv[4]), float(sys.argv[5]); t0 = time.time(); last = None
    while time.time() - t0 < secs:
        s.sendall(struct.pack(">HHHBBHH", 1, 0, 6, 1, 4 if op == "watch" else 3, a, n)); r = s.recv(256)
        regs = struct.unpack(">%dH" % n, r[9:9 + 2 * n])
        if regs != last: print("+%5d ms  %s" % ((time.time() - t0) * 1000, " ".join("%d:%d" % (a + i, x) for i, x in enumerate(regs))), flush=True); last = regs
        time.sleep(0.05)
elif op == "write":
    v = int(sys.argv[4]); s.sendall(struct.pack(">HHHBBHH", 1, 0, 6, 1, 6, a, v)); r = s.recv(256)
    print("FC6 %s:%d = %d  (%.1f of 1000)  reply %s" % (ip, a, v, v / 65.535, "ok" if len(r) >= 12 else r.hex()))
else:
    n = int(sys.argv[4]) if len(sys.argv) > 4 else 1
    s.sendall(struct.pack(">HHHBBHH", 1, 0, 6, 1, 4 if op == "read" else 3, a, n)); r = s.recv(256)
    regs = struct.unpack(">%dH" % n, r[9:9 + 2 * n])
    print(" ".join("%d:%d(%.2f)" % (a + i, x, x / 65.535) for i, x in enumerate(regs)))
