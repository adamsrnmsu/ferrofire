"""A stand-in provider REP socket on 127.0.0.1:5555 (ZMTP 3.x, NULL mechanism, as conformance/dnp3/dnp3probe.py's): it acknowledges
every request and prints each one as `PROVIDER: <request>`, so check.sh can see which writes the GOOSE subscriber caused."""
import socket, struct, sys, threading

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
    n = int.from_bytes(rd(c, 8 if fl & 2 else 1), "big")
    return fl, rd(c, n)

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
            print("PROVIDER: " + parts[-1].rstrip(b"\0").decode(errors="replace"), flush=True)
            reply = b"ACK=ok\0"
            c.sendall(bytes([1, 0, 0, len(reply)]) + reply)
    except (EOFError, OSError):
        pass

srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); srv.bind(("127.0.0.1", 5555)); srv.listen(4)
while True:
    c, _ = srv.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
