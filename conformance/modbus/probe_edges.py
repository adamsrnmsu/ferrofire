"""Scaling, bit packing, address-space corners and writes landing, against diff-edges-server.xml (made by gen_edges.py) with a
stand-in provider that acknowledges every WRITE. Registers 30000+k / 40000+k (k = 0..11) have the ranges listed in gen_edges.py."""
from modbusprobe import *
start_provider()
N = 12
def hexs(b): return b.hex()
print("== initial state: everything reads as the zero of its kind")
rd("FC4 30000 x12", 4, 30000, N); rd("FC3 40000 x12", 3, 40000, N); rd("FC1 coils 10 x10", 1, 10, 10); rd("FC2 10000 x12", 2, 10000, 12)
rd("FC4 @0", 4, 0, 1); rd("FC3 @0", 3, 0, 1); rd("FC1 @0", 1, 0, 1); rd("FC2 @0", 2, 0, 1)
rd("FC4 @65535", 4, 65535, 1); rd("FC3 @65535", 3, 65535, 1); rd("FC1 @65535", 1, 65535, 1); rd("FC2 @65535", 2, 65535, 1)
print("== the top of the address space: where does addr + qty become an error, and which one")
for fc, name in ((1, "FC1"), (2, "FC2"), (3, "FC3"), (4, "FC4")):
    for a, n in ((65534, 1), (65534, 2), (65533, 2), (65533, 3), (65535, 1)): rd("%s @%d x%d" % (name, a, n), fc, a, n)
print("== in-range values: each input register gets the same fraction f of its own range (min + f * (max - min); the zero-span one gets 100)")
from gen_edges import RANGES
span = [r if None not in r else (-1000, 2000) for r in RANGES]   # one key alone leaves the default
for f in (0, 1 / 65535, 0.25, 0.5, 12345 / 65535, 0.75, 65534 / 65535, 1):
    publish(",".join("ia%d:%r" % (30000 + k, 100 if lo == hi else lo + (hi - lo) * f) for k, (lo, hi) in enumerate(span)) + ",ia0:%r,ia65535:%r" % (1000 * f, 1000 * f))
    rd("FC4 x12 at f=%.5f" % f, 4, 30000, N); rd("FC4 @0 at f=%.5f" % f, 4, 0, 1)
print("== binary inputs: bit packing, LSB first, unaligned starts")
publish(",".join("ib%d:%s" % (10000 + k, "true" if k in (0, 2, 3, 5, 8, 11) else "false") for k in range(12)) + ",ib0:true,ib65535:true")
for a, n in [(10000, 12), (10000, 8), (10000, 9), (10003, 7), (10008, 4), (10011, 1), (10000, 1), (10005, 2), (10011, 2)]: rd("FC2 %d x%d" % (a, n), 2, a, n)
rd("FC2 @0", 2, 0, 1); rd("FC2 @65535", 2, 65535, 1); rd("FC2 @65535 x2", 2, 65535, 2)
print("== holding registers: write raw values, read back, and see what the provider was sent")
for k in range(N):
    for raw in (0, 12345, 65535):
        wr("FC6 4%04d=%d" % (k, raw), 6, 40000 + k, raw)
    rd("FC3 4%04d" % k, 3, 40000 + k, 1)
for raw in (1, 32767, 32768, 65534): wr("FC6 40000=%d" % raw, 6, 40000, raw); rd("FC3 40000", 3, 40000, 1)
wr("FC6 @0 =65535", 6, 0, 65535); rd("FC3 @0", 3, 0, 1)
# FC6/FC5 at address 65535 (mapped here) are not probed: stock answers exception 2 but has already queued the write, plus garbage writes to the
# wrapped addresses 0.. (SPEC Q9 family; recorded by hand in conformance/README.md)
wr("FC6 @65534 =1", 6, 65534, 1)
wm("FC16 @65535 x1", 16, 65535, 1, bytes([0, 5])); wm("FC16 @65534 x2", 16, 65534, 2, bytes(4)); wm("FC16 @65534 x1", 16, 65534, 1, bytes(2))
wm("FC16 40000 x12 pattern", 16, 40000, N, b"".join(struct.pack(">H", (i * 5957 + 1) & 0xFFFF) for i in range(N)))
rd("FC3 40000 x12", 3, 40000, N)
wm("FC16 40010 x3 (last unmapped: first two still queue)", 16, 40010, 3, bytes([0x10, 0, 0x20, 0, 0x30, 0]))
rd("FC3 40010 x2", 3, 40010, 2)
wm("FC16 65535 x2 (wraps?)", 16, 65535, 2, bytes([0x11, 0x11, 0x22, 0x22]))
rd("FC3 @65535", 3, 65535, 1); rd("FC3 @0", 3, 0, 1)
wm("FC16 tid=abcd", 16, 40001, 1, bytes([0x80, 0]), tid=0xABCD)
rd("FC3 40001", 3, 40001, 1)
print("== coils: what lands for each FC5 value, FC15 bit order and spill-over")
for v in (0xFF00, 0, 0x00FF, 0x0001, 0xFFFF, 1234, 0x8000, 0xFF01, 0xFEFF, 0xFF80):
    wr("FC5 coil1 =%04x" % v, 5, 1, v); rd("FC1 coil 1", 1, 1, 1)
wr("FC5 coil 0 ON uid=7", 5, 0, 0xFF00, uid=7); rd("FC1 coils 0..3", 1, 0, 4)
rd("FC1 @65535", 1, 65535, 1)
wr("FC5 coil 7 ON", 5, 7, 0xFF00); rd("FC1 coil 7", 1, 7, 1)
wr("FC5 coil 65534 (unmapped)", 5, 65534, 0xFF00)
wm("FC15 @65535 x1", 15, 65535, 1, bytes([1])); wm("FC15 @65534 x1", 15, 65534, 1, bytes([1])); wm("FC15 @65534 x2", 15, 65534, 2, bytes([3]))
wm("FC15 coils 10..19 = 0b10_1100_1101", 15, 10, 10, bytes([0xCD, 0x02]))
rd("FC1 coils 10..19", 1, 10, 10); rd("FC1 coils 12 x5", 1, 12, 5); rd("FC1 coils 10 x8", 1, 10, 8); rd("FC1 coils 10 x9", 1, 10, 9)
wm("FC15 coils 10..14 high bits set in the last byte", 15, 10, 5, bytes([0xFF]))
rd("FC1 coils 10..19", 1, 10, 10)
wm("FC15 coils 18..21 (20 unmapped: 18 and 19 still queue)", 15, 18, 4, bytes([0x0F]))
rd("FC1 coils 18 x2", 1, 18, 2)
wm("FC15 65535 x2 (wraps?)", 15, 65535, 2, bytes([3]))
rd("FC1 @65535", 1, 65535, 1); rd("FC1 @0", 1, 0, 1)
wm("FC15 coil 3 = 0 (one bit)", 15, 3, 1, bytes([0]))
wm("FC15 coil 2 = 1", 15, 2, 1, bytes([1]))
rd("FC1 coils 0..3", 1, 0, 4)
print("== a write to an unmapped address queues nothing")
wr("FC5 coil 4 (unmapped)", 5, 4, 0xFF00); wr("FC6 40012 (unmapped)", 6, 40012, 5)
print("== two writes to one tag before the provider answers: sent in one segment the later value wins")
s = Sess()
s.send("FC6 40002 = 100 and = 200 in one segment", mbap(1, 6, struct.pack(">HH", 40002, 100), tid=1) + mbap(1, 6, struct.pack(">HH", 40002, 200), tid=2)); s.close(); wrote()
rd("FC3 40002", 3, 40002, 1)
# not probed: two writes 0.2 s apart. Stock sometimes keeps the first (a write that arrives while the first is in flight is erased by
# the ACK); ours keeps the newer one (compare-and-erase). A race in stock, not a behaviour to copy (conformance/README.md).
wr("FC6 40003 = 300", 6, 40003, 300); rd("FC3 40003", 3, 40003, 1)
