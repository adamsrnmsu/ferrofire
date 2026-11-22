"""The provider a BACnet server DUT runs beside it (DUT_PRE): publishes BODY to the device's <input> group 239.0.0.1:40000 every
0.5 s, and answers the device's <output> REP 127.0.0.1:5555 (ZMTP 3.x, NULL, as conformance/scpi/provider.py) with ACK, logging
each WRITE. A `LOG=` request from the peers (port 5555 on the DUT's address) is answered with the log, so check.sh can compare what
reached the provider.   feed.py LOGFILE BODY"""
import socket, struct, sys, threading, time

LOG, BODY = sys.argv[1], sys.argv[2].encode()
open(LOG, "w").close()


def publish():
    g = b"54386972605942b"
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    u.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
    while True:
        u.sendto(bytes([len(g)]) + g + BODY + b"\0", ("239.0.0.1", 40000))
        time.sleep(0.5)


def rd(c, n):
    b = b""
    while len(b) < n:
        d = c.recv(n - len(b))
        if not d:
            raise EOFError
        b += d
    return b


def frame(c):
    fl = rd(c, 1)[0]
    return fl, rd(c, int.from_bytes(rd(c, 8 if fl & 2 else 1), "big"))


def send(c, body):
    c.sendall(bytes([1, 0, 2 if len(body) > 255 else 0]) + (len(body).to_bytes(8, "big") if len(body) > 255 else bytes([len(body)])) + body)


def serve(c):
    try:
        c.sendall(b"\xff" + bytes(8) + b"\x7f" + b"\x03\x00" + b"NULL".ljust(20, b"\0") + bytes(32)); rd(c, 64)
        ready = b"\x05READY\x0bSocket-Type" + struct.pack(">I", 3) + b"REP"
        c.sendall(bytes([4, len(ready)]) + ready); frame(c)
        while True:
            parts = []
            while True:
                fl, body = frame(c)
                parts.append(body)
                if not fl & 1:
                    break
            req = parts[-1].rstrip(b"\0")
            if req == b"LOG=":
                send(c, open(LOG, "rb").read())
                continue
            if req.startswith(b"WRITE="):
                with open(LOG, "a") as f:
                    f.write(req.decode() + "\n")
            send(c, b"ACK=ok\0")
    except (EOFError, OSError):
        pass


threading.Thread(target=publish, daemon=True).start()
srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); srv.bind(("0.0.0.0", 5555)); srv.listen(4)
while True:
    c, _ = srv.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
