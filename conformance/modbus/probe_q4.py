"""SPEC Q4: values outside a register's range, against diff-edges-server.xml with no provider. Stock converts the scaled value to a 32-bit
integer (saturating) and keeps the low 16 bits, so 1000.5 over [0,1000] reads 31; ours clamps to 0 or 65535. NOT a byte-equal set
(make diff-modbus skips it): tests/modbus_golden.rs asserts that every word that differs is an out-of-range value and that ours is the clamp."""
from modbusprobe import *
N = 12
vals = ["0", "1", "-1", "0.5", "250.5", "1000", "1000.5", "65535", "65536", "100000", "-100000", "1e10", "-1e10", "1e-9", "10.25", "100", "-50", "50", "nan", "inf", "-inf", "123456789012", "0.0005"]
for v in vals:
    publish(",".join("ia%d:%s" % (a, v) for a in list(range(30000, 30000 + N)) + [0]))
    rd("FC4 x12 of %s" % v, 4, 30000, N); rd("FC4 @0 of %s" % v, 4, 0, 1)
