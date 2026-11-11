"""PyVISA (with the pure-Python pyvisa-py backend), an SCPI client independent of ferrofire's own: opens the raw socket resource,
reads the identity and the tags the provider publishes, sets two, provokes errors and reads the status registers. Prints one PASS line
per check and exits 1 on the first FAIL. argv[1]: the stand-in provider's log (a write that reached it is a line `WRITE=point:value,`)."""
import sys, time
import pyvisa

PROVIDER_LOG = sys.argv[1]

def check(what, ok, got=None):
    if ok:
        print("PASS pyvisa: " + what)
    else:
        print(f"FAIL pyvisa: {what} (got {got!r})"); sys.exit(1)

def provider_has(s):
    try: return s in open(PROVIDER_LOG).read()
    except FileNotFoundError: return False

rm = pyvisa.ResourceManager("@py")
dev = rm.open_resource("TCPIP::127.0.0.1::5025::SOCKET", read_termination="\n", write_termination="\n", timeout=5000)
dev.write("*CLS")
idn = dev.query("*IDN?")
check("*IDN? names ferrofire rtu-1 CONF-0001", idn.startswith("ferrofire,rtu-1,CONF-0001,"), idn)
for q, want in [("MEAS:SCAL:LEVEL?", 250.5), ("MEASure:SCALar:FLOW?", -12.25), ("meas:scal:level?", 250.5)]:
    got = dev.query_ascii_values(q)[0]
    check(f"{q} = {want}", got == want, got)
check("MEAS:BIN:BRKR? = 1", dev.query("MEAS:BIN:BRKR?") == "1")
check("compound query joins with ;", dev.query("MEAS:SCAL:LEVEL?;:MEAS:SCAL:FLOW?;*OPC?") == "250.5;-12.25;1")
check("SYST:ERR? is empty so far", dev.query("SYST:ERR?") == '0,"No error"')
dev.write("SOUR:SETPOINT 420.5")
dev.write("SOUR:BREAKER_CMD ON")
for _ in range(40):
    if provider_has("load-mw-setpoint:420.500000") and provider_has("load-breaker-toggle:true"): break
    time.sleep(0.25)
check("SOUR:SETPOINT 420.5 reached the provider write queue", provider_has("load-mw-setpoint:420.500000"))
check("SOUR:BREAKER_CMD ON reached the provider write queue", provider_has("load-breaker-toggle:true"))
check("SYST:ERR? still empty after two good writes", dev.query("SYST:ERR?") == '0,"No error"')
check("SOUR:SETPOINT? MAX = 1000", dev.query("SOUR:SETPOINT? MAX") == "1000")
dev.write("*ESE 16")
dev.write("SOUR:SETPOINT 99999")
stb = int(dev.query("*STB?"))
check("*STB? after the error: EAV (4) and ESB (32)", stb == 36, stb)
esr = int(dev.query("*ESR?"))
check("*ESR? after the error: execution error bit (16)", esr == 16, esr)
check("*ESR? is cleared by the read", int(dev.query("*ESR?")) == 0)
stb = int(dev.query("*STB?"))
check("*STB? now: EAV only (4)", stb == 4, stb)
err = dev.query("SYST:ERR?")
check("SYST:ERR? is -222 for the out-of-range write", err.startswith("-222,"), err)
check("*STB? once the queue is drained", int(dev.query("*STB?")) == 0)
time.sleep(1)
check("the out-of-range write never reached the provider", not provider_has("99999"))
dev.write("SOUR:NOPE 1")
check("an unknown header is -113", dev.query("SYST:ERR?").startswith("-113,"))
dev.write("*ESE 0")
dev.close()
print("PYVISA DONE")
