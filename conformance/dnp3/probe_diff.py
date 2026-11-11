from dnp3probe import *
import dnp3probe as P

print("== a fresh connection that never resets the link")
P.s.sendall(frame(10, 1, 0xF2)); show("test-link before reset", drain(0.4))
P.s.sendall(frame(10, 1, 0xF3, bytes([0xC0 | 0]) + bytes([0xC0 | 0, 1, 60, 2, 6]))); show("confirmed data before reset", drain(0.6))
P.s.sendall(frame(10, 1, 0xC4, bytes([0xC0 | 1]) + bytes([0xC0 | 1, 1, 60, 2, 6]))); show("unconfirmed data before reset", drain(0.6))
print("== link layer")
P.s.sendall(frame(10, 1, 0xC9)); show("req-link-status", drain(0.4))
P.s.sendall(frame(10, 1, 0xC0)); show("reset-link", drain(0.4))
P.s.sendall(frame(10, 1, 0xD2)); show("test-link(fcv fcb=0)", drain(0.4))
P.s.sendall(frame(10, 1, 0xC2)); show("test-link(no fcv)", drain(0.4))
print("== initial state")
req("integrity", 1, INTEG)
confirm("confirm integrity", 0)
req("integrity-again", 1, INTEG)
req("class1-after", 1, bytes([60, 2, 6]))
req("class2", 1, bytes([60, 3, 6]))
req("class3", 1, bytes([60, 4, 6]))
req("class0", 1, bytes([60, 1, 6]))
print("== publish and events")
publish("pb0:true,pb1:false,pb2:true,pb3:true,pb10:false,pa0:12.5,pa1:-3,pa2:70000.25,pa7:1e10")
req("class1-events", 1, bytes([60, 2, 6]))
confirm("confirm events")
req("class1-empty", 1, bytes([60, 2, 6]))
publish("pb0:false,pa0:12.75,pa1:-3")
req("class123", 1, CLS)
confirm("confirm")
req("class0-values", 1, bytes([60, 1, 6]))
print("== reads by group and variation")
for lbl, objs in [
    ("g1v0 all", hdr(1, 0, 6)), ("g1v1 all", hdr(1, 1, 6)), ("g1v2 all", hdr(1, 2, 6)),
    ("g1v2 range 0-3", hdr(1, 2, 0, 0, 3)), ("g1v2 range 2-10", hdr(1, 2, 0, 2, 10)),
    ("g1v1 range 2-2", hdr(1, 1, 0, 2, 2)), ("g1v2 range16 0-3", hdr(1, 2, 1, 0, 0, 3, 0)),
    ("g1v2 range 20-30", hdr(1, 2, 0, 20, 30)), ("g1v2 range 3-1", hdr(1, 2, 0, 3, 1)),
    ("g1v2 count1", hdr(1, 2, 7, 2)), ("g1v2 count16", hdr(1, 2, 8, 2, 0)),
    ("g1v2 list idx", bytes([1, 2, 0x17, 2, 10, 2])), ("g1v2 list idx unknown", bytes([1, 2, 0x17, 2, 99, 2])),
    ("g2v0", hdr(2, 0, 6)), ("g2v1", hdr(2, 1, 6)), ("g2v2", hdr(2, 2, 6)), ("g2v3", hdr(2, 3, 6)), ("g2v2 count1", hdr(2, 2, 7, 1)),
    ("g10v0", hdr(10, 0, 6)), ("g10v2", hdr(10, 2, 6)),
    ("g30v0", hdr(30, 0, 6)), ("g30v1", hdr(30, 1, 6)), ("g30v2", hdr(30, 2, 6)), ("g30v3", hdr(30, 3, 6)), ("g30v4", hdr(30, 4, 6)),
    ("g30v5", hdr(30, 5, 6)), ("g30v6", hdr(30, 6, 6)), ("g30v6 range 1-2", hdr(30, 6, 0, 1, 2)), ("g30v6 range 5-6", hdr(30, 6, 0, 5, 6)),
    ("g32v0", hdr(32, 0, 6)), ("g32v1", hdr(32, 1, 6)), ("g32v3", hdr(32, 3, 6)), ("g32v5", hdr(32, 5, 6)), ("g32v7", hdr(32, 7, 6)), ("g32v8", hdr(32, 8, 6)),
    ("g40v0", hdr(40, 0, 6)), ("g40v1", hdr(40, 1, 6)), ("g20v0", hdr(20, 0, 6)), ("g21v0", hdr(21, 0, 6)), ("g99v1", hdr(99, 1, 6)),
    ("g60v1+g1v2", hdr(60, 1, 6) + hdr(1, 2, 6)), ("g60v2 count", hdr(60, 2, 7, 1)), ("g60v9", hdr(60, 9, 6)),
    ("g50v1", hdr(50, 1, 6)), ("g80v1", hdr(80, 1, 6)), ("empty read", b""), ("truncated header", bytes([1, 2])),
    ("truncated range", bytes([1, 2, 0, 1])), ("g1v2 q99", bytes([1, 2, 0x99, 1])),
]:
    req(lbl, 1, objs)
print("== other function codes")
req("write clear restart", 2, bytes([80, 1, 0, 7, 7, 0]))
req("after clear restart", 1, bytes([60, 1, 6]))
req("write time", 2, bytes([50, 1, 7, 1]) + struct.pack("<Q", 1700000000000)[:6])
req("write bad", 2, bytes([80, 1, 0, 3, 3, 0]))
req("cold restart", 13)
req("warm restart", 14)
req("delay measure", 23)
req("record time", 24)
req("enable unsol", 20, bytes([60, 2, 6, 60, 3, 6, 60, 4, 6]))
req("disable unsol", 21, bytes([60, 2, 6, 60, 3, 6, 60, 4, 6]))
req("enable unsol no objs", 20)
req("assign class", 22, bytes([60, 2, 6, 1, 0, 0, 2, 2]))
req("freeze", 7, hdr(20, 0, 6))
req("fc 0x55", 0x55)
req("fc response(129)", 129)
req("confirm via req", 0)
req("direct op no ack", 6, CROB(5, 3), wait=0.4)
req("sol null-ish read class1 multi", 1, CLS)
print("== commands")
for lbl, fc, objs in [
    ("direct crob 5 latch-on", 5, CROB(5, 3)), ("direct crob 5 latch-off", 5, CROB(5, 4)), ("direct crob 5 pulse-on", 5, CROB(5, 1)),
    ("direct crob 5 op 0x0f", 5, CROB(5, 0x0f)), ("direct crob 5 tcc", 5, CROB(5, 0x43)), ("direct crob 5 cnt0", 5, CROB(5, 3, 0)),
    ("direct crob 9 unmapped", 5, CROB(9, 3)), ("direct crob 2 (input point)", 5, CROB(2, 3)), ("direct crob 4 sbo", 5, CROB(4, 3)),
    ("select crob 4", 3, CROB(4, 3)), ("operate crob 4", 4, CROB(4, 3)), ("operate crob 4 again", 4, CROB(4, 3)),
    ("operate crob 5 no select", 4, CROB(5, 3)), ("select crob 9", 3, CROB(9, 3)), ("operate crob 9", 4, CROB(9, 3)),
    ("select crob 5", 3, CROB(5, 3)), ("operate crob 5 different op", 4, CROB(5, 4)), ("operate crob 5 matching", 4, CROB(5, 3)),
    ("direct crob 5+9", 5, bytes([12, 1, 0x17, 2, 5]) + bytes([3, 1]) + struct.pack("<II", 100, 0) + b"\0" + bytes([9]) + bytes([3, 1]) + struct.pack("<II", 100, 0) + b"\0"),
    ("direct crob q28", 5, CROB(5, 3, q=0x28)[:3] + bytes([1, 0, 5, 0]) + CROB(5, 3)[5:]),
    ("direct crob bad len", 5, CROB(5, 3)[:-3]),
    ("direct ao32 8", 5, AO32(8, 77)), ("direct aof 8", 5, AOF(8, 1.5)), ("direct aod 8", 5, AOD(8, 2.5)),
    ("direct aof 3 sbo", 5, AOF(3, 1.5)), ("select aof 3", 3, AOF(3, 1.5)), ("operate aof 3", 4, AOF(3, 1.5)), ("operate aof 3 again", 4, AOF(3, 1.5)),
    ("select aof 3 / operate other val", 3, AOF(3, 1.5)), ("operate aof 3 other val", 4, AOF(3, 9.5)),
    ("direct aof 99", 5, AOF(99, 1.5)), ("direct aof 4 (binary idx)", 5, AOF(4, 1.5)), ("select aof 99", 3, AOF(99, 1.5)),
    ("select ao32 8", 3, AO32(8, 5)), ("operate ao32 8", 4, AO32(8, 5)),
    ("direct g41v2", 5, bytes([41, 2, 0x17, 1, 8]) + struct.pack("<h", 7) + b"\0"),
    ("direct g41v3 q28", 5, bytes([41, 3, 0x28, 1, 0, 8, 0]) + struct.pack("<f", 3.5) + b"\0"),
    ("direct unknown obj g12v2", 5, bytes([12, 2, 0x17, 1, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
    ("direct g10v2 write", 5, bytes([10, 2, 0x17, 1, 5, 0x81])),
    ("direct empty", 5, b""),
]:
    req(lbl, fc, objs)
print("== event order, selection and variations")
req("drain events", 1, CLS); confirm("confirm drain")
publish("pa0:20.5")                       # analog first ...
publish("pb1:true")                       # ... then binary
req("class1 insertion order", 1, bytes([60, 2, 6])); confirm("confirm order")
publish("pa0:21.5"); publish("pb1:false")
req("g2v0+g32v0 request order", 1, hdr(2, 0, 6) + hdr(32, 0, 6)); confirm("confirm order2")
publish("pa0:22.5"); publish("pb1:true")
req("g32v0+g2v0 request order", 1, hdr(32, 0, 6) + hdr(2, 0, 6)); confirm("confirm order3")
publish("pa0:23.5"); publish("pb1:false")
req("class0 then class1", 1, bytes([60, 1, 6, 60, 2, 6])); confirm("confirm c0c1")
publish("pa0:24.5"); publish("pb1:true")
req("class1 then class0", 1, bytes([60, 2, 6, 60, 1, 6])); confirm("confirm c1c0")
publish("pa0:25.5"); publish("pb1:false")
req("class0 only, events pending (iin class bit)", 1, bytes([60, 1, 6]))
req("g1v2 range + g2v0", 1, hdr(1, 2, 0, 0, 1) + hdr(2, 0, 6)); confirm("confirm rg")
publish("pa0:26.5,pa1:5.5"); publish("pb1:true,pb2:false")
req("g2v0 count 0", 1, hdr(2, 0, 7, 0))
req("g2v0 count 1", 1, hdr(2, 0, 7, 1))
confirm("confirm count1")
req("g32v0 count 1", 1, hdr(32, 0, 7, 1)); confirm("confirm g32 count1")
req("class1 rest", 1, bytes([60, 2, 6])); confirm("confirm rest")
for v in (1, 2, 3):
    publish("pb1:%s,pb2:%s,pa0:%d.5,pa1:-2.99,pa2:0.9" % (["false", "true"][v % 2], ["true", "false"][v % 2], 30 + v))
    req("events g2v%d g32v%d" % (v, v), 1, hdr(2, v, 6) + hdr(32, v, 6))
    confirm("confirm var")
publish("pb1:true,pa0:41.25,pa1:1e10,pa2:-1e10,pa7:-0.5")
req("events g2v1+g32v2", 1, hdr(2, 1, 6) + hdr(32, 2, 6)); confirm("c")
publish("pb1:false,pa0:42.25,pa1:3e9,pa2:1.5")
req("events g2v2+g32v4", 1, hdr(2, 2, 6) + hdr(32, 4, 6)); confirm("c")
publish("pb1:true,pa0:43.25,pa7:2.5")
req("events g32v5", 1, hdr(32, 5, 6)); confirm("c")
req("events g2v3 g32v7", 1, hdr(2, 3, 6) + hdr(32, 7, 6)); confirm("c")
publish("pb1:false,pa0:44.25")
req("events g32v8 g2v0", 1, hdr(32, 8, 6) + hdr(2, 0, 6)); confirm("c")
req("events g32v6 q28 indices", 1, bytes([32, 6, 0x17, 1, 0]))
req("mixed unknown", 1, hdr(1, 2, 6) + hdr(99, 1, 6) + hdr(30, 6, 6))
print("== event variations, one change at a time")
req("drain", 1, CLS); confirm("confirm drain")
pb1 = [True]
def flip():
    pb1[0] = not pb1[0]
    return "pb1:" + ("true" if pb1[0] else "false")
publish("pb1:true"); req("drain", 1, CLS); confirm("confirm drain")
for g2v, g32v in [(1, 1), (1, 2), (2, 4), (3, 7), (1, 5), (2, 8), (3, 3), (0, 0)]:
    publish("%s,pa0:%d.75,pa1:%d.25" % (flip(), 50 + g32v, -60 - g32v))
    req("events g2v%d g32v%d" % (g2v, g32v), 1, hdr(2, g2v, 6) + hdr(32, g32v, 6)); confirm("confirm var")
print("== pipelined and fragmented requests")
a = bytes([0xC0 | 3, 1, 60, 1, 6]); b = bytes([0xC0 | 4, 1, 1, 2, 6])
P.s.sendall(frame(10, 1, 0xC4, bytes([0xC0 | 20]) + a) + frame(10, 1, 0xC4, bytes([0xC0 | 21]) + b)); show("two frames one write", drain(0.6))
f = frame(10, 1, 0xC4, bytes([0xC0 | 22]) + bytes([0xC0 | 5, 1, 1, 2, 6]))
for i in range(0, len(f), 3):
    P.s.sendall(f[i:i + 3]); nap(0.01)
show("three-byte dribble", drain(0.6))
u = bytes([0xC0 | 5, 1, 1, 2, 6])
P.s.sendall(frame(10, 1, 0xC4, bytes([0x40 | 23]) + u[:3]) + frame(10, 1, 0xC4, bytes([0x80 | 24]) + u[3:])); show("fragmented request", drain(0.6))
print("== link layer after reset")
P.s.sendall(frame(10, 1, 0xF2)); show("test-link fcb=1 fcv", drain(0.4))
P.s.sendall(frame(10, 1, 0xF2)); show("test-link fcb=1 again", drain(0.4))
P.s.sendall(frame(10, 1, 0xD2)); show("test-link fcb=0", drain(0.4))
P.s.sendall(frame(10, 1, 0xF3, bytes([0xC0 | 25]) + bytes([0xC0 | 6, 1, 60, 2, 6]))); show("confirmed user data fcb=1", drain(0.6))
P.s.sendall(frame(10, 1, 0xD3, bytes([0xC0 | 26]) + bytes([0xC0 | 7, 1, 60, 2, 6]))); show("confirmed user data fcb=0", drain(0.6))
P.s.sendall(frame(10, 1, 0xD3, bytes([0xC0 | 27]) + bytes([0xC0 | 8, 1, 60, 2, 6]))); show("confirmed user data fcb=0 again", drain(0.6))
P.s.sendall(frame(10, 1, 0xC0)); show("reset-link again", drain(0.4))
P.s.sendall(frame(10, 1, 0xFA)); show("link func 10 (req status? )", drain(0.4))
P.s.sendall(frame(10, 1, 0x4B)); show("secondary-originated frame", drain(0.4))
P.s.sendall(frame(10, 1, 0x04, bytes([0xC0 | 28]) + bytes([0xC0 | 9, 1, 60, 2, 6]))); show("DIR=0 unconfirmed", drain(0.6))
print("== routing")
send("wrong dst", bytes([0xC0 | 5, 1, 60, 1, 6]), dst=11)
send("broadcast", bytes([0xC0 | 5, 1, 60, 1, 6]), dst=0xFFFF)
send("other master", bytes([0xC0 | 6, 1, 60, 1, 6]), src=7)
send("no FIR/FIN", bytes([0xC0 | 7, 1, 60, 1, 6]), raw_tp=bytes([0x00 | 9, 0xC7, 1, 60, 1, 6]))
send("empty tp", b"", raw_tp=b"")
print("== one master at a time (CloseNew), and state outlives a connection")
second = socket.create_connection((host, port), timeout=3)
second.sendall(frame(10, 1, 0xC4, bytes([0xC0 | 0]) + bytes([0xC0 | 2, 1, 60, 1, 6])))
time.sleep(0.6); second.settimeout(0.4)
try:
    d = second.recv(4096); print("second connection: closed" if not d else "second connection: %d bytes" % len(d))
except socket.timeout: print("second connection: open, no reply")
except OSError: print("second connection: closed")   # a reset and an orderly close are both a refusal
second.close()
req("first still served", 1, bytes([60, 2, 6]))
trace("close"); P.s.real.close(); time.sleep(0.5); connect()
req("after reconnect: class1", 1, bytes([60, 2, 6]))
req("after reconnect: class0 iin", 1, bytes([60, 1, 6]))
print("== done")
