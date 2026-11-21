import os
os.environ["NACK"] = "1"   # the stand-in provider answers every WRITE with ERR= (probe_nopv.py: no provider at all)
from dnp3probe import *
# ww-a7r: a control whose provider write is rejected. Run against diff-prov-server.xml: an output and an input on one tag, in class 1.
# Stock applies the rejected write all the same: an input on the tag reads it, and the output status never goes back.
# The sweep that makes the input's event runs once a second whatever the probe does, so every control is followed by a wait of more than
# a second and a read of the events: the output event (made when the control arrives) and the input event come in a fixed order.
STATUS = hdr(1, 0, 6) + hdr(10, 0, 6) + hdr(30, 0, 6) + hdr(40, 0, 6)
print("== start")
req("drain", 1, CLS); confirm("confirm drain")
print("== a binary control")
req("crob on", 5, CROB(0, 3))
for w in (0.2, 1.0, 2.0, 4.0, 8.0):
    nap(w)
    req("after %.1f s: g1 g10 g30 g40" % w, 1, STATUS, wait=0.05)
req("class1", 1, bytes([60, 2, 6])); confirm("confirm")
print("== an analog control")
req("aof 9", 5, AOF(0, 9))
nap(1.6)
req("g1 g10 g30 g40", 1, STATUS)
req("class1", 1, bytes([60, 2, 6])); confirm("confirm")
print("== a second control of each on the same tag")
req("crob off", 5, CROB(0, 4))
nap(1.6)
req("g1 g10 g30 g40", 1, STATUS)
req("class1", 1, bytes([60, 2, 6])); confirm("confirm")
req("aof 3", 5, AOF(0, 3))
nap(1.6)
req("g1 g10 g30 g40", 1, STATUS)
req("class1", 1, bytes([60, 2, 6])); confirm("confirm")
