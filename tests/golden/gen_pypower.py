"""Golden power-flow results from the installed PYPOWER 5.1.19 (never committed; generated at test time).

  uv run --python 3.11 --with PYPOWER==5.1.19 --with 'numpy<2' --with 'scipy<1.14' python -I -W ignore tests/golden/gen_pypower.py target/golden

Needs numpy<2 (PYPOWER 5.1.19 imports numpy.in1d, removed in numpy 2.x) and so Python <= 3.13.
Writes <outdir>/<case>.json (VM VA PG QG PF QF PT QT success, external order) and a copy of the case's .py file,
so the Rust tests (which only see the repo directory inside the container) can parse the same input.
"""
import importlib, inspect, json, os, shutil, sys
from numpy import array
from pypower.api import ppoption, runpf

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
opt = ppoption(VERBOSE=0, OUT_ALL=0)
VM, VA, PG, QG, PF, QF, PT, QT, BR_STATUS = 7, 8, 1, 2, 13, 14, 15, 16, 10

def dump(name, ppc, opt=opt):
    # case9's gen array is all integer literals -> an int64 array, and runpf then truncates Pg/Qg to integers.
    # Compare against the float result (the maths), not that dtype accident.
    for k in ("bus", "gen", "branch"):
        ppc[k] = ppc[k].astype(float)
    r, ok = runpf(ppc, opt)
    json.dump({
        "success": int(ok),
        "VM": r["bus"][:, VM].tolist(), "VA": r["bus"][:, VA].tolist(),
        "PG": r["gen"][:, PG].tolist(), "QG": r["gen"][:, QG].tolist(),
        "PF": r["branch"][:, PF].tolist(), "QF": r["branch"][:, QF].tolist(),
        "PT": r["branch"][:, PT].tolist(), "QT": r["branch"][:, QT].tolist(),
    }, open(os.path.join(out, name + ".json"), "w"))

for name in ["case9", "case14", "case30", "case57", "case118", "case300"]:
    mod = importlib.import_module("pypower." + name)
    shutil.copy(inspect.getfile(mod), os.path.join(out, name + ".py"))
    dump(name, getattr(mod, name)())

# branch 4-5 out of service (spec 6.2); the Rust side writes branch-1_4-5.active=false
from pypower.case9 import case9
ppc = case9()
row = [i for i, b in enumerate(ppc["branch"]) if int(b[0]) == 4 and int(b[1]) == 5][0]
ppc["branch"][row, BR_STATUS] = 0
dump("case9_br45off", ppc)
# DC power flow (makeBdc/dcpf)
for name in ["case9", "case30"]:
    dump(name + "_dc", getattr(importlib.import_module("pypower." + name), name)(), ppoption(VERBOSE=0, OUT_ALL=0, PF_DC=1))
print("golden written to", out)
