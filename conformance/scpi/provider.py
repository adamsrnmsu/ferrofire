"""A stand-in provider REP socket (ZMTP 3.x, NULL mechanism, from conformance/dnp3/dnp3probe.py): acknowledges every WRITE and appends
it to the file in argv[1], so a command that went SCPI -> store write queue -> scan cycle is visible as a line `WRITE=point:value,...`."""
import socket, struct, sys, threading

LOG = sys.argv[1]
srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); srv.bind(("127.0.0.1", 5555)); srv.listen(4)

def rd(c, n):
    b = b""
    while len(b) < n:
        d = c.recv(n - len(b))
        if not d: raise EOFError
        b += d
    return b

def frame(c):
    fl = rd(c, 1)[0]
    return fl, rd(c, int.from_bytes(rd(c, 8 if fl & 2 else 1), "big"))

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
                if not fl & 1: break
            req = parts[-1]
            if req.startswith(b"WRITE="):
                with open(LOG, "a") as f: f.write(req.rstrip(b"\0").decode() + "\n")
            reply = b"ACK=ok\0" if req[:6] in (b"WRITE=", b"READ=q", b"QUERY=") or req.startswith(b"READ=") else b"ERR=unknown\0"
            c.sendall(bytes([1, 0, 0, len(reply)]) + reply)
    except (EOFError, OSError):
        pass

while True:
    c, _ = srv.accept(); threading.Thread(target=serve, args=(c,), daemon=True).start()
