#!/usr/bin/env python3
"""check.py opendss|helics|helics-opendss : drive one field device over Modbus against a bennu physics provider and judge the round trip (ww-82n).

Prints one `state:` line per step (registers as read, so a ferrofire run and a stock run can be diffed) and `ok:`/`FAIL:` lines.
Exit 1 on any FAIL. Runs inside ff-physics:22.04 (pymodbus 3.9 from pybennu), next to the device on 127.0.0.1:502.
"""
import sys
import time

from pymodbus.client import ModbusTcpClient

c = ModbusTcpClient("127.0.0.1", port=502)
fails = 0


def check(ok, what):
    global fails
    fails += not ok
    print(("ok:   " if ok else "FAIL: ") + what)


def state(n_ir, n_di, label):
    ir = c.read_input_registers(30000, count=n_ir).registers
    di = c.read_discrete_inputs(10000, count=n_di).bits[:n_di]
    print(f"state: {label}: ir={ir} di={di}")
    return ir, di


def near(a, b, counts=2):
    """Registers equal within `counts` (65535 = full scale): the solver's last digit moves between solves."""
    return len(a) == len(b) and all(abs(x - y) <= counts for x, y in zip(a, b))


def until(pred, n_ir, n_di, label, timeout=15):
    """Poll until pred(ir, di) holds (the provider publishes every 1 to 2 s); the last reading either way."""
    end = time.monotonic() + timeout
    while True:
        ir = c.read_input_registers(30000, count=n_ir).registers
        di = c.read_discrete_inputs(10000, count=n_di).bits[:n_di]
        if pred(ir, di) or time.monotonic() > end:
            return state(n_ir, n_di, label)
        time.sleep(0.2)


def opendss():
    # IR 30000 Line.1 amps (0-1000), 30001 Line.1 far-end volts (0-20000), 30002 Breaker5 amps; coil 0 commands Breaker4
    ir0, _ = until(lambda ir, di: ir[0] > 0, 3, 1, "closed")
    check(ir0[0] > 0 and ir0[1] > 0, f"OpenDSS feeds Line.1 through Breaker4 (amps reg {ir0[0]}, volts reg {ir0[1]})")
    c.write_coil(0, False)
    ir1, _ = until(lambda ir, di: ir[0] == 0, 3, 1, "Breaker4 opened")
    check(ir1[0] == 0 and ir1[1] == 0, "opening Breaker4 over Modbus de-energises Line.1 in the OpenDSS solve")
    check(ir1[2] != ir0[2], f"the parallel feeder's current moves ({ir0[2]} -> {ir1[2]}): a new power flow, not a copied value")
    c.write_coil(0, True)
    ir2, _ = until(lambda ir, di: near(ir, ir0), 3, 1, "Breaker4 closed again")
    check(near(ir2, ir0), f"closing Breaker4 restores the first solve ({ir0} -> {ir2})")


def helics():
    # IR 30000 bus-1.voltage (0-2) from the federate; DI 10000 gen1.active = the federate's logic of load1.active, DI 10001 load1.active;
    # coil 0 writes load1.active, which the provider sends to the federate as a HELICS message
    ir0, _ = until(lambda ir, di: ir[0] > 0, 1, 2, "start")
    check(ir0[0] > 0, f"bus-1.voltage arrives from the HELICS federate (reg {ir0[0]})")
    for want in (True, False):   # load1 starts false: true first, so each step is a change
        c.write_coil(0, want)
        _, di = until(lambda ir, d: d == [want, want], 1, 2, f"load1.active written {str(want).lower()}")
        check(di[0] == want, f"gen1.active follows the write only through the federate's logic ({str(want).lower()})")


def helics_opendss():
    # the same registers as opendss(), but the solve is in dss_federate.py and every value and the breaker command go through HELICS
    ir0, _ = until(lambda ir, di: ir[0] > 0 and di[0], 3, 1, "closed")
    check(ir0[0] > 0 and ir0[1] > 0, f"the federate's OpenDSS solve arrives through HELICS (amps reg {ir0[0]}, volts reg {ir0[1]})")
    c.write_coil(0, False)
    ir1, di1 = until(lambda ir, di: ir[0] == 0 and not di[0], 3, 1, "Breaker4 opened")
    check(ir1[0] == 0 and ir1[1] == 0 and not di1[0], "opening Breaker4 over Modbus zeroes Line.1 in a solve that came back through HELICS")
    check(ir1[2] != ir0[2], f"Breaker5's current moves ({ir0[2]} -> {ir1[2]}): a new power flow, not a copied value")
    c.write_coil(0, True)
    ir2, _ = until(lambda ir, di: near(ir, ir0) and di[0], 3, 1, "Breaker4 closed again")
    check(near(ir2, ir0), f"closing Breaker4 restores the first solve ({ir0} -> {ir2})")


if __name__ == "__main__":
    for _ in range(50):   # the device opens 502 a moment after it starts
        if c.connect():
            break
        time.sleep(0.2)
    {"opendss": opendss, "helics": helics, "helics-opendss": helics_opendss}[sys.argv[1]]()
    sys.exit(1 if fails else 0)
