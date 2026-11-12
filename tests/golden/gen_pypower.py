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

def dump(name, ppc, opt=opt, run=runpf, extra={}):
    # case9's gen array is all integer literals -> an int64 array, and runpf then truncates Pg/Qg to integers.
    # Compare against the float result (the maths), not that dtype accident.
    for k in ("bus", "gen", "branch"):
        ppc[k] = ppc[k].astype(float)
    r, ok = run(ppc, opt)
    json.dump({
        **extra,
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

# ENFORCE_Q_LIMS=1. PYPOWER 5.1.19's Q-limit branch cannot run as shipped: it indexes arrays with float bus numbers (IndexError on the
# first violation), writes `bus[pv, BUS_TYPE] = pv` (a bus index as a bus type) when the slack bus is converted, and parses
# `a == PV | b == REF` as a chained comparison. The reference is runpf with exactly those lines repaired (index casts, parentheses,
# PV for pv); the algorithm (violation test with OPF_VIOLATION, PQ conversion, load adjust, restart from the same V0, slack
# promotion, angle shift) is PYPOWER's own.
def runpf_qlim():
    import pypower.runpf as m
    src = inspect.getsource(m)
    for old, new in [
        ("(bus[gen[:, GEN_BUS], BUS_TYPE] == PV | ", "((bus[gen[:, GEN_BUS].astype(int), BUS_TYPE] == PV) | "),
        ("bus[gen[:, GEN_BUS], BUS_TYPE] == REF))", "(bus[gen[:, GEN_BUS].astype(int), BUS_TYPE] == REF)))"),
        ("if len(infeas) == len(remaining) or all(infeas == remaining):", "if len(infeas) == len(remaining):"),
        ("any(bus[gen[mx, GEN_BUS], BUS_TYPE] == REF)", "any(bus[gen[mx, GEN_BUS].astype(int), BUS_TYPE] == REF)"),
        ("bus[pv, BUS_TYPE] = pv", "bus[pv, BUS_TYPE] = PV"),
        ("bi = gen[limited[i], GEN_BUS]  ", "bi = gen[limited[i], GEN_BUS].astype(int)  "),
    ]:
        assert old in src, old
        src = src.replace(old, new)
    ns = dict(m.__dict__)
    exec(compile(src, "runpf_qlim", "exec"), ns)
    return ns["runpf"]

QMAX, QMIN = 3, 4
# name -> (case, {gen row: (QMAX, QMIN)}); the limits go into the golden so the Rust test applies the very same ones.
QLIM = {
    "case9_qlim": ("case9", {1: (10.0, None), 2: (None, -1.0)}),           # two PV gens bind, one at each limit
    "case9_qlim_slack": ("case9", {0: (10.0, None), 1: (10.0, None)}),     # the slack gen binds: first PV bus becomes slack
    "case14_qlim": ("case14", {}),                                          # the stock limits already bind
    "case30_qlim": ("case30", {0: (None, 5.0), 1: (20.0, None), 3: (None, -2.0)}),
}
qrun, qopt = runpf_qlim(), ppoption(VERBOSE=0, OUT_ALL=0, ENFORCE_Q_LIMS=1)
for name, (cname, lim) in QLIM.items():
    ppc = getattr(importlib.import_module("pypower." + cname), cname)()
    ppc["gen"] = ppc["gen"].astype(float)
    for row, (mx, mn) in lim.items():
        if mx is not None: ppc["gen"][row, QMAX] = mx
        if mn is not None: ppc["gen"][row, QMIN] = mn
    dump(name, ppc, qopt, qrun, {"case": cname, "QMAX": ppc["gen"][:, QMAX].tolist(), "QMIN": ppc["gen"][:, QMIN].tolist()})
print("golden written to", out)
