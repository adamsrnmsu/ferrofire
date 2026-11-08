"""Compare rustbelt's pybennu-power-solver drop-in with the STOCK pybennu PyPower provider class (run offline, no sockets:
its Provider base class is stubbed because the image's swig _Endpoint.so is unusable). Usage:
  pypower_cmp.py CASE.py tcp://127.0.0.1:5555 [--write tag:value,...]
Initial state must match (tag list, order and values); after --write the two are compared and the differences are
reported, not failed (documented deviation: stock turns every active non-slack bus into PQ on any write)."""
import sys, types, zmq
stub = types.ModuleType("pybennu.distributed.provider")
class Provider:
    def __init__(self, *a, **k): pass
stub.Provider = Provider
import pybennu.distributed
sys.modules["pybennu.distributed.provider"] = stub
import pybennu.providers.power.solvers.python_power as pp
import numpy as np
raw_loadcase = pp.loadcase
# Stock PyPower on a case whose gen matrix is all int literals (PYPOWER's own case9) keeps int64 arrays and TRUNCATES the solved
# gen PG/QG (71.95 -> 71). We compare against the float-cast case (what the golden tests use) and print the raw diff as info.
def float_loadcase(f, *a, **k):
    c = raw_loadcase(f, *a, **k)
    return {k2: (v.astype(float) if isinstance(v, np.ndarray) else v) for k2, v in c.items()}
PyPower = pp.PyPower

case, ep = sys.argv[1], sys.argv[2]
write = sys.argv[4] if len(sys.argv) > 4 and sys.argv[3] == "--write" else None
ref_raw = PyPower(None, None, case)
pp.loadcase = float_loadcase
ref = PyPower(None, None, case)
sock = zmq.Context().socket(zmq.REQ); sock.setsockopt(zmq.RCVTIMEO, 10000); sock.connect(ep)
def ask(m): sock.send(m.encode()); return sock.recv().rstrip(b"\0").decode()

def num(s):
    return {"true": 1.0, "false": 0.0}[s] if s in ("true", "false") else float(s)

def compare(label, tol_abs, tol_rel, report_worst=0):
    rq = ref.query()[4:].split(","); oq = ask("QUERY=")[4:].split(",")
    assert rq == oq, f"{label}: tag list/order differs: stock {len(rq)} vs ours {len(oq)}; first diff {next((a, b) for a, b in zip(rq, oq) if a != b)}"
    bad, worst = [], []
    for t in rq:
        if not t: continue
        a = ref.read(t); b = ask("READ=" + t)
        assert a.startswith("ACK=") and b.startswith("ACK="), (t, a, b)
        assert a.split(":")[0] == b.split(":")[0]
        x, y = num(a.split(":", 1)[1]), num(b.split(":", 1)[1])
        d = abs(x - y)
        worst.append((d, t, x, y))
        if d > tol_abs + tol_rel * abs(x): bad.append((t, x, y))
    worst.sort(reverse=True)
    print(f"{label}: {len([t for t in rq if t])} tags, tag list+order identical, {len(bad)} outside tol (abs {tol_abs}, rel {tol_rel}); max diff {worst[0][0]:.3g} at {worst[0][1]}")
    for d, t, x, y in worst[:report_worst]: print(f"    {t}: stock {x} ours {y}")
    return bad

t0 = ref.query()[4:].split(",")[1]
print("read form  stock:", ref.read(t0), "| ours:", ask("READ=" + t0))
ints = [t for t in ref_raw.query()[4:].split(",")[:-1] if ref_raw.read(t) != ref.read(t)]
print(f"info: stock with int-dtype gen matrix differs from float-cast stock on {len(ints)} tags (truncation), e.g. {ints[:2]}")
bad = compare("initial", 1e-6, 1e-6, report_worst=3)
assert not bad, bad[:10]
if write:
    print("write reply stock:", ref.write(dict(p.split(":") for p in write.split(","))), "| ours:", ask("WRITE=" + write))
    compare("after write", 1e-6, 1e-6, report_worst=5)
print("OK")
