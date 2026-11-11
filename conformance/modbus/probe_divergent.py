"""Where our server deliberately differs from stock (SPEC 3.6 Q9 and the notes in conformance/README.md), recorded from stock for the record.
Stock answers most of these with exception code 4 under function byte 0x80 (or garbage from a stale buffer, bennu #81) and dies on a frame
over ~1018 bytes, so this set is NOT a golden: tests/modbus_golden.rs asserts our side of each row instead. After every row a fresh
connection checks that the server still answers."""
from modbusprobe import *
publish("load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,load-1_bus-1.active:false,bus-1.gen_mw:12.5")
def alive():
    try:
        s = socket.create_connection((host, port), timeout=2); s.settimeout(1); s.sendall(mbap(1, 2, struct.pack(">HH", 10000, 2))); r = s.recv(64); s.close()
        return "answers" if r.startswith(bytes.fromhex("000100000004010201")) else "?" + r.hex()
    except Exception as e: return "DEAD"
def case(label, data, half=True, wait=0.4):
    s = socket.create_connection((host, port), timeout=3); s.settimeout(wait)
    s.sendall(data)
    if half: s.shutdown(socket.SHUT_WR)
    buf, eof = b"", False
    try:
        while True:
            d = s.recv(65536)
            if not d: eof = True; break
            buf += d
    except socket.timeout: pass
    except OSError: eof = True
    s.close()
    print("%-42s %-24s%s  %s" % (label, buf[:12].hex() + ("..(%d bytes)" % len(buf) if len(buf) > 12 else ""), " <EOF>" if eof else "", alive()), flush=True)
H = lambda tid, pid, ln, uid: struct.pack(">HHHB", tid, pid, ln, uid)
RD = bytes([3, 0x9c, 0x40, 0, 1])
print("== MBAP length below 2")
case("length 0, then FC3 40000 x1", H(1, 0, 0, 1) + RD)
case("length 1, no PDU", H(1, 0, 1, 1))
case("length 1, then FC3 40000 x1", H(1, 0, 1, 1) + RD)
print("== protocol id")
case("protocol id 1", H(1, 1, 6, 1) + RD)
case("protocol id 0xff00", H(1, 0xFF00, 6, 1) + RD)
print("== MBAP length over 254 (stock buffer: 1024)")
for ln in (255, 256, 300, 1000, 1018):
    case("FC3 40000 x1, length %d" % ln, H(1, 0, ln, 1) + RD + bytes(ln - 6), half=False)
case("length 255 declared, 10 bytes sent, open", H(1, 0, 255, 1) + RD + bytes([0]), half=False, wait=1.0)
case("length 65535 declared, 10 bytes sent, open", H(1, 0, 65535, 1) + RD + bytes([0]), half=False, wait=1.0)
print("== a frame cut short, then the client half-closes (bennu #81: answered from the previous request in the buffer)")
case("header only (7 bytes)", H(1, 0, 6, 1))
case("cut after the function code", H(1, 0, 6, 1) + RD[:1])
case("cut after the address", H(1, 0, 6, 1) + RD[:3])
case("cut one byte short", H(1, 0, 6, 1) + RD[:4])
case("header cut at 5 bytes", H(1, 0, 6, 1)[:5])
print("== FC15/FC16 with fewer data bytes than the byte count says (stock writes whatever follows in its buffer)")
case("FC16 40000 x1 bytecount 2, no data", mbap(1, 16, struct.pack(">HHB", 40000, 1, 2)))
case("FC16 40000 x1 bytecount 2, 1 data byte", mbap(1, 16, struct.pack(">HHB", 40000, 1, 2) + b"\xff"))
case("FC15 coils 0 x2 bytecount 1, no data", mbap(1, 15, struct.pack(">HHB", 0, 2, 1)))
