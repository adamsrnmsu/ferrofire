from dnp3probe import *
# 130 binary + 130 analog class-1 inputs (diff-big-server.xml): the start-up events already overflow the 100-event buffers.
# Multi-fragment responses: every non-final fragment (CON set) is confirmed before asking again.
import dnp3probe as P
def fragments(label, fc, objs):
    """Read, then confirm every fragment that asks for it, following the multi-fragment response to its end."""
    req(label, fc, objs, wait=0.8)
    for n in range(12):
        ctl = P.lastfrag
        if ctl is None or not ctl & 0x20:
            break
        confirm("  confirm %s #%d" % (label, n), wait=0.8)
        if ctl & 0x40:
            break

print("== overflow at start-up")
fragments("read class0 (iin shows overflow + class bits)", 1, bytes([60, 1, 6]))
print("== integrity, confirming each fragment")
fragments("integrity", 1, bytes([60, 2, 6, 60, 3, 6, 60, 4, 6, 60, 1, 6]))
req("class1 after", 1, bytes([60, 2, 6]))
print("== second round after a publish")
body = ",".join("pb%d:true" % i for i in range(130)) + "," + ",".join("pa%d:%d.5" % (i, i) for i in range(130)) + ",pb300:true,pb301:false,pabig:7.5"
publish(body)
req("g2v0 count 5", 1, bytes([2, 0, 7, 5]))
confirm("  confirm count 5")
req("g32v0 count 3", 1, bytes([32, 0, 7, 3]))
confirm("  confirm count 3")
fragments("class0 only (class bits in iin)", 1, bytes([60, 1, 6]))
fragments("class1", 1, bytes([60, 2, 6]))
req("class1 empty", 1, bytes([60, 2, 6]))
print("== unconfirmed: a new request while waiting for confirm; the events come again")
publish(",".join("pb%d:false" % i for i in range(10)))
req("g2v0 first", 1, bytes([2, 0, 6]))
req("g2v0 second (no confirm between)", 1, bytes([2, 0, 6]))
confirm("  confirm second")
req("g2v0 third", 1, bytes([2, 0, 6]))
print("== confirm timeout (6 s) returns events to unselected")
publish(",".join("pb%d:true" % i for i in range(3)))
req("g2v0 sent", 1, bytes([2, 0, 6]))
nap(6)
req("g2v0 after timeout", 1, bytes([2, 0, 6]))
confirm("  confirm last")
print("== static-only multi-fragment (450 analog outputs, no events pending)")
publish("pb0:true")
fragments("drain", 1, bytes([60, 2, 6, 60, 3, 6, 60, 4, 6]))
fragments("g30v0 static only", 1, hdr(30, 0, 6))
req("g30v0 again, no confirm", 1, hdr(30, 0, 6), wait=1.0)
req("g1v2 ranges across 255", 1, hdr(1, 2, 1, 120, 0, 44, 1), wait=0.8)
req("g1v2 all", 1, hdr(1, 2, 6), wait=0.8)
req("g30v6 index 65535 list", 1, bytes([30, 6, 0x08, 1, 0, 0xFF, 0xFF]), wait=0.8)
fragments("g30v6 all (65535 and 1000..1449)", 1, hdr(30, 6, 6))
print("== done")
