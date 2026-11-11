"""Shared helpers for the differential DNP3 probes (SPEC 8): the same requests go to the stock bennu outstation and to ours, and the
transcript must be byte-equal after the masking in mask(). Master 1 -> outstation 10. Used by probe_diff.py and probe_big.py,
which take [host] [port]."""
import os, socket, struct, sys, time

def mk():
    t = []
    for i in range(256):
        c = i
        for _ in range(8):
            c = (c >> 1) ^ 0xA6BC if c & 1 else c >> 1
        t.append(c)
    return t
T = mk()
def crc(b):
    c = 0
    for x in b:
        c = (c >> 8) ^ T[(c ^ x) & 0xFF]
    return (~c) & 0xFFFF
def frame(dst, src, ctrl, user=b""):
    hdr = bytes([0x05, 0x64, 5 + len(user), ctrl]) + struct.pack("<HH", dst, src)
    out = hdr + struct.pack("<H", crc(hdr))
    for i in range(0, len(user), 16):
        blk = user[i:i + 16]
        out += blk + struct.pack("<H", crc(blk))
    return out
def parse(buf):
    res = []; i = 0
    while i + 10 <= len(buf) and buf[i] == 5 and buf[i + 1] == 0x64:
        ln = buf[i + 2]; ctrl = buf[i + 3]; dst, src = struct.unpack("<HH", buf[i + 4:i + 8])
        ul = ln - 5; data = b""; j = i + 10
        while ul > 0:
            n = min(16, ul); data += buf[j:j + n]; j += n + 2; ul -= n
        res.append((ctrl, dst, src, data)); i = j
    return res

import threading, struct as _st
def _zmtp_rep(port=5555):
    """A stand-in provider REP socket (ZMTP 3.x, NULL mechanism): acknowledges every WRITE so that commands reach the device's
    datastore the way they do with a live provider. Without one, stock bennu applies a queued write only after its retries give
    up, at a time that depends on its history, which no transcript can pin."""
    srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); srv.bind(("127.0.0.1", port)); srv.listen(4)
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
            ready = b"\x05READY\x0bSocket-Type" + _st.pack(">I", 3) + b"REP"
            c.sendall(bytes([4, len(ready)]) + ready); frame(c)
            while True:
                parts = []
                while True:
                    fl, body = frame(c)
                    parts.append(body)
                    if not fl & 1: break
                if parts[-1].startswith(b"WRITE=") or parts[-1].startswith(b"READ=") or parts[-1].startswith(b"QUERY="):
                    reply = b"ACK=ok\0"
                else:
                    reply = b"ERR=unknown\0"
                c.sendall(bytes([1, 0, 0, len(reply)]) + reply)
        except (EOFError, OSError):
            pass
    def loop():
        while True:
            c, _ = srv.accept(); threading.Thread(target=serve, args=(c,), daemon=True).start()
    threading.Thread(target=loop, daemon=True).start()
if os.environ.get("NO_PROVIDER") != "1": _zmtp_rep()

host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
port = int(sys.argv[2]) if len(sys.argv) > 2 else 20000
import json
def trace(*a): print("#T " + json.dumps(a), flush=True)   # machine-readable twin of the transcript: replayed by the Rust golden test
class Sock:
    def __init__(self, real): self.real = real
    def sendall(self, b): trace("send", b.hex()); self.real.sendall(b)
    def __getattr__(self, n): return getattr(self.real, n)
def connect():
    global s, tpseq
    trace("connect"); s = Sock(socket.create_connection((host, port), timeout=3)); tpseq = 0
connect()
def nap(t): trace("sleep", t); time.sleep(t)
tpseq = 0
aseq = 0

def drain(wait=0.6):
    nap(wait); s.settimeout(0.4); buf = b""
    try:
        while True:
            d = s.recv(65536)
            if not d: buf += b"<EOF>"; break
            buf += d
    except Exception:
        pass
    trace("recv", buf.replace(b"<EOF>", b"").hex(), buf.endswith(b"<EOF>")); trace("sleep", 0.4)
    return buf

def mask(app):
    """Normalise what a wall clock decides: the 6-byte provider time in time-bearing objects (a little-endian value near
    1.79e9 seconds followed by two zero octets) and IIN1.4 NEED_TIME, which comes back 30 s after a time write."""
    b = bytearray(app)
    if len(b) >= 4 and b[1] == 0x81: b[2] &= ~0x10
    for i in range(len(b) - 5):
        if b[i + 3] in (0x6a, 0x6b) and b[i + 4] == 0 and b[i + 5] == 0: b[i:i + 6] = bytes(6)
    return bytes(b)

lastfrag = None   # app control octet of the last fragment seen, None if the reply was empty
def show(label, buf):
    global lastfrag
    lastfrag = None
    trace("label", label)
    # reassemble transport segments per (src) into app fragments
    if buf.endswith(b"<EOF>"): print(label, "EOF")
    frames = parse(buf.replace(b"<EOF>", b""))
    if not frames: print(label, "-- no reply")
    cur = b""; n = 0
    for ctrl, dst, src, data in frames:
        n += 1
        if not data:
            print(label, "link ctrl=%02x %d<-%d" % (ctrl, dst, src)); continue
        tph = data[0]
        cur = (cur if not tph & 0x40 else b"") + data[1:]
        if tph & 0x80:
            lastfrag = cur[0]
            print(label, "link ctrl=%02x %d<-%d tp=%02x frames=%d app:" % (ctrl, dst, src, tph, n), mask(cur).hex(" ")); cur = b""; n = 0

def send(label, app, ctrl=0xC4, wait=0.6, dst=10, src=1, raw_tp=None):
    global tpseq
    user = (bytes([0xC0 | tpseq]) + app) if raw_tp is None else raw_tp
    tpseq = (tpseq + 1) & 63
    s.sendall(frame(dst, src, ctrl, user))
    show(label, drain(wait))

last = 0
def req(label, fc, objs=b"", ctl=0xC0, wait=0.6, **kw):
    global aseq, last
    last = aseq
    a = bytes([ctl | aseq, fc]) + objs
    send(label, a, wait=wait, **kw)
    aseq = (aseq + 1) & 15
def confirm(label, seq=None, wait=0.4):
    global tpseq
    seq = (last if lastfrag is None else lastfrag & 0x0F) if seq is None else seq
    s.sendall(frame(10, 1, 0xC4, bytes([0xC0 | tpseq]) + bytes([0xC0 | seq, 0x00]))); tpseq = (tpseq + 1) & 63
    show(label, drain(wait))

def publish(body):
    g = b"54386972605942b"
    body += ","   # the provider ends every entry with a comma; ours treats a body without one as truncated
    d = bytes([len(g)]) + g + body.encode() + b"\0"
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    trace("publish", body); u.sendto(d, ("239.0.0.1", 40000)); u.close()
    nap(1.6)

hdr = lambda g, v, q, *rest: bytes([g, v, q]) + bytes(rest)
CROB = lambda idx, code, cnt=1, on=100, off=0, st=0, q=0x17: bytes([12, 1, q, 1, idx]) + bytes([code, cnt]) + struct.pack("<II", on, off) + bytes([st])
AO32 = lambda idx, v: bytes([41, 1, 0x17, 1, idx]) + struct.pack("<i", v) + b"\0"
AOF = lambda idx, v: bytes([41, 3, 0x17, 1, idx]) + struct.pack("<f", v) + b"\0"
AOD = lambda idx, v: bytes([41, 4, 0x17, 1, idx]) + struct.pack("<d", v) + b"\0"
CLS = bytes([60, 2, 6, 60, 3, 6, 60, 4, 6])
INTEG = bytes([60, 2, 6, 60, 3, 6, 60, 4, 6, 60, 1, 6])

