"""Extended master probe: prints every APDU decoded. usage: probe_ext.py [port]. Used against stock and c104 alike."""
import socket, struct, sys, time
s = socket.create_connection(("127.0.0.1", 2404), timeout=4)
ns = 0; t0 = time.time()
nr = 0  # I-frames received, echoed as N(R) so the server's window never stalls
def drain(t=1.2, tag=""):
    global nr
    s.settimeout(t); buf = b""
    try:
        while True:
            d = s.recv(4096)
            if not d: break
            buf += d
    except Exception: pass
    i = 0; per = []
    while i < len(buf) and buf[i] == 0x68:
        n = buf[i+1]
        if buf[i+2] & 1 == 0:
            nr += 1; asdu = buf[i+6:i+2+n]
            if asdu[2] == 1: per.append(asdu[0])  # COT 1 periodic: summarised, not listed
            else: print(tag, "t=%.1f" % (time.time()-t0), asdu.hex(" "))
        else: print(tag, "U/S", buf[i:i+2+n].hex(" "))
        i += 2+n
    if per: print(tag, "periodic COT1 ASDU types:", per)
    if i < len(buf): print(tag, "TRAILING", buf[i:].hex(" "))
def send(asdu, tag):
    global ns
    s.sendall(bytes([0x68, 4+len(asdu), (ns<<1)&0xff, (ns<<1)>>8, (nr<<1)&0xff, (nr<<1)>>8]) + asdu); ns += 1
    drain(1.2, tag)
def a(typ, cot, ioa, body, vsq=1): return bytes([typ, vsq, cot, 0, 1, 0, ioa&255, ioa>>8, 0]) + body
s.sendall(bytes.fromhex("680407000000")); drain(0.3, "STARTDT")
send(a(100, 6, 0, b"\x14"), "GI20")
send(a(100, 6, 0, b"\x15"), "GI21(other QOI)")
send(a(45, 6, 0, b"\x01"), "C_SC 45 ioa0 ON")
send(a(45, 6, 1, b"\x00"), "C_SC 45 ioa1 OFF")
send(a(46, 6, 0, b"\x02"), "C_DC 46 ioa0 ON")
send(a(46, 6, 1, b"\x01"), "C_DC 46 ioa1 OFF")
send(a(50, 6, 3, struct.pack("<f", 77.5) + b"\x00"), "C_SE 50 ioa3 77.5")
send(a(45, 6, 50, b"\x01"), "C_SC 45 unknown ioa 50")
send(a(103, 6, 0, bytes(7)), "C_CS 103 clock sync")
send(a(101, 6, 0, b"\x05"), "C_CI 101 counter GI")
send(a(102, 5, 100, b""), "C_RD 102 read ioa100")
send(a(107, 6, 0, b"\xaa\x55" + bytes(7)), "C_TS 107 test")
print("-- idle 5.5s: periodic timing")
s.settimeout(0.05); end = time.time()+5.5
while time.time() < end:
    try: d = s.recv(4096)
    except Exception: continue
    if d: print("IDLE t=%.2f" % (time.time()-t0), "apdus:", [(d[j+6], d[j+8]) for j in range(0, len(d), 0) ] if 0 else d.hex(" ")[:60]); nr += 1
