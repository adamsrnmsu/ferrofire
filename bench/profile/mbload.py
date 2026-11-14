#!/usr/bin/env python3
"""mbload.py HOST SECS WRITERS POLLERS POLL_MS : Modbus/TCP FC6 closed-loop write storm (WRITERS conns) + FC3/FC4 polling every POLL_MS (POLLERS conns). JSON counts."""
import socket, struct, sys, threading, time, json
host, secs, nw, npl, pms = sys.argv[1], float(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), float(sys.argv[5])
end = time.time() + secs
cnt = {"writes": 0, "polls": 0, "errors": 0}
lk = threading.Lock()
def conn():
    s = socket.create_connection((host, 502)); s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1); return s
def rx(s):
    h = b""
    while len(h) < 7:
        d = s.recv(7 - len(h)); 
        if not d: raise EOFError
        h += d
    n = struct.unpack(">H", h[4:6])[0] - 1; b = b""
    while len(b) < n:
        d = s.recv(n - len(b)); 
        if not d: raise EOFError
        b += d
    return b
def req(s, tid, pdu): s.sendall(struct.pack(">HHHB", tid, 0, len(pdu) + 1, 1) + pdu); return rx(s)
def writer(k):
    s = conn(); i = 0; c = e = 0
    while time.time() < end:
        r = req(s, i & 0xffff, struct.pack(">BHH", 6, i % 4, (i * 7) & 0x3ff)); i += 1
        c += 1; e += r[0] & 0x80 != 0
    with lk: cnt["writes"] += c; cnt["errors"] += e
def poller(k):
    s = conn(); i = 0; c = e = 0; nxt = time.time()
    while time.time() < end:
        for pdu in (struct.pack(">BHH", 3, 0, 4), struct.pack(">BHH", 4, 0, 100)):
            r = req(s, i & 0xffff, pdu); i += 1; c += 1; e += r[0] & 0x80 != 0
        nxt += pms / 1000; time.sleep(max(0, nxt - time.time()))
    with lk: cnt["polls"] += c; cnt["errors"] += e
ts = [threading.Thread(target=writer, args=(k,)) for k in range(nw)] + [threading.Thread(target=poller, args=(k,)) for k in range(npl)]
[t.start() for t in ts]; [t.join() for t in ts]
cnt["secs"] = secs; print(json.dumps(cnt))
