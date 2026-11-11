"""The waterway rtu-1 register map (diff-waterway-server.xml, every range [0,1000]) with provider values like the real simulation's,
provider down. Reads happen within seconds of the writes: stock applies an unacknowledged write only after ~15 s."""
from modbusprobe import *
def pub(**kv): publish(",".join("%s:%s" % (k.replace("_", "."), v) for k, v in kv.items()))
print("== idle lock")
pub(lowerwater_height=3.5, lock_height=3.5, lowergate_open=0, lowervalve_open=0, lowersensor_active=0, lowersensor_direction=0,
    locksensor_active=0, locksensor_direction=0, boat_location=0, boat_direction=0)
rd("FC4 30000 x10", 4, 30000, 10); rd("FC3 40000 x2", 3, 40000, 2)
print("== a boat arrives at the lower sensor heading up, the gate logic fires")
pub(lowersensor_active=1, lowersensor_direction=1, boat_location=12.25, boat_direction=1, lock_height=3.55, lowerwater_height=3.5)
rd("FC4 30000 x10", 4, 30000, 10)
for a in range(30000, 30010): rd("FC4 %d" % a, 4, a, 1)
rd("FC3 40000 x2", 3, 40000, 2); rd("FC3 40000", 3, 40000, 1); rd("FC3 40001", 3, 40001, 1)
print("== direction -1 is outside [0,1000], and so is a negative height: both read 0 (a value above the range is SPEC Q4: probe_q4.py)")
pub(lowersensor_direction=-1, locksensor_direction=-1, lowerwater_height=-0.5, lock_height=1000, boat_location=1000, boat_direction=-1)
rd("FC4 30000 x10", 4, 30000, 10)
print("== writes (provider down: they queue, the registers do not change)")
wr("FC6 40000 = 65535 (gate open)", 6, 40000, 65535)
wr("FC6 40001 = 32768", 6, 40001, 32768)
wm("FC16 40000 x2", 16, 40000, 2, struct.pack(">HH", 0, 65535))
rd("FC3 40000 x2", 3, 40000, 2)
print("== the other tables are not mapped on this RTU")
rd("FC1 coil 0", 1, 0, 1); rd("FC2 10000", 2, 10000, 1); rd("FC3 30000", 3, 30000, 1); rd("FC4 40000", 4, 40000, 1)
rd("FC3 40002", 3, 40002, 1); rd("FC4 30010", 4, 30010, 1); rd("FC4 29999 x2", 4, 29999, 2)
