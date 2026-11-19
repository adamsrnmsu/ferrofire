from dnp3probe import *
# ww-a7r: the event buffer of the output types (g11, g42) when it overflows, without <event-buffer-size>: diff-server.xml, binary output 5
# and analog output 8 in class 1 and 2. Controls make the events; each is read and confirmed through every fragment of the response.
import dnp3probe as P
def fragments(label, fc, objs):
    req(label, fc, objs, wait=0.8)
    for n in range(12):
        ctl = P.lastfrag
        if ctl is None or not ctl & 0x20:
            break
        confirm("  confirm %s #%d" % (label, n), wait=0.8)
        if ctl & 0x40:
            break

print("== assign classes, drain")
req("assign g10 4-5 class1", 22, bytes([60, 2, 6, 10, 0, 0, 4, 5]))
req("assign g40 3-8 class2", 22, bytes([60, 3, 6, 40, 0, 0, 3, 8]))
fragments("drain", 1, CLS)
print("== exactly 100 events of each output type: no overflow")
for i in range(100):
    req("crob %d" % i, 5, CROB(5, 3 + i % 2), wait=0.02)
    req("aof %d" % i, 5, AOF(8, i + 0.5), wait=0.02)
req("g10", 1, hdr(10, 0, 6))
fragments("class1: 100 events", 1, bytes([60, 2, 6]))
fragments("class2: 100 events", 1, bytes([60, 3, 6]))
print("== 105 events of each: the oldest 5 are gone and IIN2.3 says so")
for i in range(105):
    req("crob %d" % i, 5, CROB(5, 3 + i % 2), wait=0.02)
    req("aof %d" % i, 5, AOF(8, 1000 + i + 0.5), wait=0.02)
req("g10", 1, hdr(10, 0, 6))
fragments("g11v0 events", 1, hdr(11, 0, 6))
req("g10 after the first fragments", 1, hdr(10, 0, 6))
fragments("class2", 1, bytes([60, 3, 6]))
fragments("class1", 1, bytes([60, 2, 6]))
req("class1 empty", 1, bytes([60, 2, 6]))
print("== after the overflow is read out, the next events are kept again")
req("crob", 5, CROB(5, 3)); req("aof", 5, AOF(8, 3.5))
fragments("g42v2 events", 1, hdr(42, 2, 6))
fragments("class1", 1, bytes([60, 2, 6]))
