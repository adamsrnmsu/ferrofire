#!/usr/bin/env python3
"""fault.py MODE OUT : provider-side fault injection with a 4 Hz sampler of what the two RTUs serve over Modbus.
MODE kill (also: FC6 writes to both RTUs during the outage and after recovery, then provider READs): wait for the boat past 40 m, kill -9 the ferrofire provider, restart it after GAP s (default 5; the model restarts at t=0).
MODE stop: SIGSTOP the provider for GAP s (a hung provider: its sockets stay open), then SIGCONT.
OUT: CSV t,rtu1_boat,rtu1_lock,rtu2_lock (x = no answer) plus '# event' lines with unix times, for the report's analysis."""
import os, signal, socket, struct, subprocess, sys, time
mode, out = sys.argv[1], sys.argv[2]; gap = float(sys.argv[3]) if len(sys.argv) > 3 else 5.0
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
    def fc6(s, a, v):   # holding-register write; the RTU's logic then sends the changed output to the provider
        try:
            if s.s is None: s.s = socket.create_connection((s.ip, 502), timeout=3)
            s.s.settimeout(3); s.s.sendall(struct.pack(">HHHBBHH", 2, 0, 6, 1, 6, a, v)); r = s.s.recv(256); s.s.settimeout(1); return r.hex()
        except Exception as e: s.s = None; return "x(%s)" % e
M = {ip: Mb(ip) for ip in ("10.1.1.101", "10.1.1.102")}
def reg(ip, a): return M[ip].reg(a)
f = open(out, 'w'); t0 = time.time(); ev = lambda m: f.write("# %.3f %s\n" % (time.time(), m))
def sample():
    f.write("%.2f,%s,%s,%s\n" % (time.time() - t0, reg("10.1.1.101", 30008), reg("10.1.1.101", 30001), reg("10.1.1.102", 30001))); f.flush()
def run(sec):
    e = time.time() + sec
    while time.time() < e: sample(); time.sleep(0.2)
def probe(tag):
    try:
        o = subprocess.run(["bennu-probe", "--e", "tcp://172.16.1.2:5555", "--c", "read", "--t", tag], capture_output=True, text=True, timeout=8).stdout
        return o.split("Reply:")[1].split()[0]
    except Exception: return "x"
def writes(label):
    for ip in M: ev("%s: FC6 40001=65535 on %s -> %s" % (label, ip, M[ip].fc6(40001, 65535)))
def pid(): return int(subprocess.check_output(["pgrep", "-x", "ferrofire"]).split()[0])
def start():
    subprocess.Popen("cd /root/ff && exec ./ferrofire run waterway.yaml --server tcp://172.16.1.2:5555 --publish 'udp://*;239.0.0.1:40000' >> prov.log 2>&1", shell=True, start_new_session=True, stdin=subprocess.DEVNULL)
ev("start " + mode)
while True:   # wait for the boat to be well into the lower approach so a restart (boat back at 0) is unmistakable
    sample(); r = reg("10.1.1.101", 30008)
    if r != 'x' and 40 < r < 90: break
    time.sleep(0.2)
run(5)
p = pid()
if mode == "kill":
    ev("kill -9 provider pid %d" % p); os.kill(p, signal.SIGKILL); run(2); writes("during outage"); run(gap - 2); ev("provider start"); start()
else:
    ev("SIGSTOP provider pid %d" % p); os.kill(p, signal.SIGSTOP); run(2); writes("during hang"); run(gap - 2); ev("SIGCONT provider"); os.kill(p, signal.SIGCONT)
run(25); writes("after recovery")   # does a write reach the restarted provider? poll it (the RTU logic rewrites the output within ~1 s)
for i in range(10): ev("provider READ uppervalve.open = %s" % probe("uppervalve.open")); sample()
run(10); ev("end")
