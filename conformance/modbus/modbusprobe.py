"""Shared helpers for the differential Modbus probes (SPEC 8): the same requests go to the stock bennu server and to ours, and the
transcript must be byte-equal. Every request is one line of transcript and a `#T` trace record (replayed by tests/modbus_golden.rs).
Usage in a probe: `from modbusprobe import *`. Optional argv: [host] [port]."""
import json, socket, struct, sys, threading, time

host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
port = int(sys.argv[2]) if len(sys.argv) > 2 else 5502

def trace(*a): print("#T " + json.dumps(a), flush=True)

# ---- a stand-in provider: ZMTP REP that acknowledges every WRITE and remembers what it was sent --------------------------------
seen, up = [], []
def start_provider(p=5555):
    """Without one, stock bennu applies a queued write only after its retries give up, which no transcript can pin. With one the
    write lands within a cycle, and the payload of each WRITE= is itself evidence (scaling, %f formatting, batching)."""
    trace("provider", 1); up.append(1)
    srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); srv.bind(("127.0.0.1", p)); srv.listen(4)
    def rd(c, n):
        b = b""
        while len(b) < n:
            d = c.recv(n - len(b))
            if not d: raise EOFError
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
                    if not fl & 1: break
                msg = parts[-1].rstrip(b"\0").decode()
                if msg.startswith("WRITE="): seen.append(msg[6:])
                reply = b"ACK=ok\0" if msg.split("=")[0] in ("WRITE", "READ", "QUERY") else b"ERR=unknown\0"
                c.sendall(bytes([1, 0, 0, len(reply)]) + reply)
        except (EOFError, OSError):
            pass
    def loop():
        while True:
            c, _ = srv.accept(); threading.Thread(target=serve, args=(c,), daemon=True).start()
    threading.Thread(target=loop, daemon=True).start()

def nap(t): trace("sleep", t); time.sleep(t)

def wrote(quiet=1.0, cap=10.0):
    """Wait until the stock device has sent the provider everything queued (it drains a multi-point write over several cycles, so
    one wait of fixed length mixes writes up), then report it. Sorted: batch order and size are not wire-relevant."""
    if not up: return
    t0 = t = time.time(); n = len(seen)
    while time.time() - t0 < cap and time.time() - t < quiet:
        time.sleep(0.1)
        if len(seen) != n: n, t = len(seen), time.time()
    trace("sleep", round(time.time() - t0, 1))
    got = sorted(p for m in seen for p in m.split(","))
    seen.clear()
    trace("provider-saw", got)
    print("  provider saw:", ",".join(got) or "nothing")

def publish(body):
    g = b"54386972605942b"
    d = bytes([len(g)]) + g + (body + ",").encode() + b"\0"
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    trace("publish", body); u.sendto(d, ("239.0.0.1", 40000)); u.close()
    nap(1.6)

# ---- requests ---------------------------------------------------------------------------------------------------------------
def mbap(uid, fc, data=b"", tid=1, pid=0, length=None):
    pdu = bytes([fc]) + data
    return struct.pack(">HHHB", tid, pid, len(pdu) + 1 if length is None else length, uid) + pdu

class Sess:
    """One TCP connection; `send` writes bytes, then reads for `wait` seconds (or until the server closes)."""
    def __init__(self):
        trace("connect"); self.s = socket.create_connection((host, port), timeout=3)
    def send(self, label, data, wait=0.2):
        trace("label", label); trace("send", data.hex())
        try: self.s.sendall(data)
        except OSError: pass
        buf, eof = b"", False
        self.s.settimeout(wait)
        try:
            while True:
                d = self.s.recv(65536)
                if not d: eof = True; break
                buf += d
        except socket.timeout: pass
        except OSError: eof = True
        trace("recv", buf.hex(), eof)
        print("%-34s %s%s" % (label, buf.hex(), " <EOF>" if eof else ""), flush=True)
        return buf
    def close(self): self.s.close()

def raw(label, data, wait=0.2):
    s = Sess(); r = s.send(label, data, wait); s.close(); return r
def x(label, fc, data=b"", wait=0.2, **kw): return raw(label, mbap(fc=fc, data=data, uid=kw.pop("uid", 1), **kw), wait)
def rd(label, fc, a, n, **kw): return x(label, fc, struct.pack(">HH", a, n), **kw)
def wr(label, fc, a, v, **kw):
    r = x(label, fc, struct.pack(">HH", a, v), **kw); wrote(); return r
def wm(label, fc, a, n, payload, **kw):
    """FC15 / FC16: address, quantity, byte count, then the payload."""
    r = x(label, fc, struct.pack(">HHB", a, n, len(payload)) + payload, **kw); wrote(); return r
