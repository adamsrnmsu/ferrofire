#!/usr/bin/env python3
"""trk.py SECS PERIOD OUT : on the provider, every PERIOD s compare the provider's truth (bennu-probe READ) with what the RTUs serve over Modbus.
Register = value/1000*65535 (config max-value 1000). One CSV line per sample: t,rtu1_boat,prov_boat,rtu1_lock,prov_lock,rtu2_lock (or 'x' on error)."""
import socket, struct, subprocess, sys, time
secs, per, out = float(sys.argv[1]), float(sys.argv[2]), sys.argv[3]
class Mb:
    """one persistent Modbus/TCP connection per RTU: the stock bennu modbus server leaks a socket fd per closed client connection and
    floods stderr when it reaches 1024 (found during ww-otj.19), so a poller must not connect per read."""
    def __init__(s, ip): s.ip, s.s = ip, None
    def reg(s, a):
        for _ in (0, 1):
            try:
                if s.s is None: s.s = socket.create_connection((s.ip, 502), timeout=1)
                s.s.sendall(struct.pack(">HHHBBHH", 1, 0, 6, 1, 4, a, 1)); r = s.s.recv(256)
                if len(r) < 11: raise OSError("short")
                return round(struct.unpack(">H", r[9:11])[0] / 65.535, 2)
            except Exception:
                try: s.s.close()
                except Exception: pass
                s.s = None
        return 'x'
M = {ip: Mb(ip) for ip in ("10.1.1.101", "10.1.1.102")}
def reg(ip, a): return M[ip].reg(a)
def prov(tag):
    try:
        o = subprocess.run(["bennu-probe", "--e", "tcp://172.16.1.2:5555", "--c", "read", "--t", tag], capture_output=True, text=True, timeout=5).stdout
        return float(o.split("Reply:")[1].split()[0])
    except Exception: return 'x'
f = open(out, 'w'); f.write("t,rtu1_boat,prov_boat,rtu1_lock,prov_lock,rtu2_lock\n"); t0 = time.time()
while time.time() - t0 < secs:
    row = [round(time.time() - t0, 1), reg("10.1.1.101", 30008), prov("boat.location"), reg("10.1.1.101", 30001), prov("lock.height"), reg("10.1.1.102", 30001)]
    f.write(",".join(str(round(x, 2)) if isinstance(x, float) else str(x) for x in row) + "\n"); f.flush(); time.sleep(per)
