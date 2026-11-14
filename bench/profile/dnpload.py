#!/usr/bin/env python3
"""dnpload.py HOST SECS POLL_MS : DNP3 master address 1 -> outstation 10, integrity poll (g60v1-4) every POLL_MS; counts responses."""
import socket, struct, sys, time, json
host, secs, pms = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
def crc(d):
    c = 0
    for b in d:
        c ^= b
        for _ in range(8): c = (c >> 1) ^ 0xA6BC if c & 1 else c >> 1
    return struct.pack("<H", ~c & 0xffff)
def frame(user):
    hdr = struct.pack("<BBBBHH", 5, 0x64, 5 + len(user), 0xC4, 10, 1)
    out = hdr + crc(hdr)
    for i in range(0, len(user), 16):
        blk = user[i:i + 16]; out += blk + crc(blk)
    return out
s = socket.create_connection((host, 20000)); s.settimeout(0.2); s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
end = time.time() + secs; seq = 0; polls = resp = byts = to = 0; nxt = time.time()
while time.time() < end:
    app = bytes([0xC0 | (seq & 15), 0x01, 0x3C, 1, 6, 0x3C, 2, 6, 0x3C, 3, 6, 0x3C, 4, 6])
    s.sendall(frame(b"\xC0" + app)); seq += 1; polls += 1
    try:
        d = s.recv(65536)
        if d: resp += 1; byts += len(d)
        s.settimeout(0.005)
        while True:
            d = s.recv(65536)
            if not d: break
            byts += len(d)
    except socket.timeout: pass
    s.settimeout(0.2)
    nxt += pms / 1000; time.sleep(max(0, nxt - time.time()))
print(json.dumps({"polls": polls, "responses": resp, "bytes": byts}))
