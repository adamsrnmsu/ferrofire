"""pymodbus (an independent client) against the server on 127.0.0.1:502 with conformance/modbus/server.xml: reads agree with what the
peer published, writes are acknowledged (nothing lands without a provider), exceptions are the ones SPEC 4.1 lists. Exit 1 on a mismatch."""
import sys
from pymodbus.client import ModbusTcpClient

fails = 0
def check(what, got, want):
    global fails
    if got == want: print("ok:  ", what, got)
    else: fails += 1; print("FAIL:", what, "got", got, "want", want, file=sys.stderr)

c = ModbusTcpClient(sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1", port=502)
assert c.connect(), "no connection"
check("input register 30000 (250.5 over [0,1000])", c.read_input_registers(30000, count=1, slave=1).registers, [0x4020])
check("holding register 40000 (the same point)", c.read_holding_registers(40000, count=1, slave=1).registers, [0x4020])
check("discrete inputs 10000..10001", c.read_discrete_inputs(10000, count=2, slave=1).bits[:2], [False, True])
check("coils 0..1 (coil 1 is the generator point, published true)", c.read_coils(0, count=2, slave=1).bits[:2], [False, True])
check("write coil 0", c.write_coil(0, True, slave=1).isError(), False)
check("write coils 0..1", c.write_coils(0, [True, False], slave=1).isError(), False)
check("write register 40000", c.write_register(40000, 1234, slave=1).isError(), False)
check("write registers 40000", c.write_registers(40000, [4321], slave=1).isError(), False)
check("write register, unit id 7", c.write_register(40000, 1, slave=7).isError(), False)
for what, r, code in [
    ("read input register 29999", c.read_input_registers(29999, count=1, slave=1), 2),
    ("read holding registers 40000 x2 (40001 unmapped)", c.read_holding_registers(40000, count=2, slave=1), 2),
    ("read coil 5 (unmapped coil: 3, SPEC Q3)", c.read_coils(5, count=1, slave=1), 3),
    ("write coil 9", c.write_coil(9, True, slave=1), 2),
]:
    check(what, (r.isError(), getattr(r, "exception_code", None)), (True, code))
c.close()
sys.exit(1 if fails else 0)
