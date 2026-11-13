#!/usr/bin/env python3
"""stock_provider.py ARGS... : the stock pybennu-power-solver (PyPower) for the control bench's stock baseline (ww-1df.23.4), unchanged except that
PyPower.write logs one `Applied: t_apply_ns=N t_solved_ns=N tag=T val=V` line per tag (unix ns; `t_apply_ns` when the write handler is entered,
`t_solved_ns` when the re-solve has finished, just before the ACK), the shape bench/control_join.py joins. ARGS are those of pybennu-power-solver,
e.g. `start -c /etc/sceptre/config.ini`. runpf(OUT_ALL=1) prints the whole power-flow report on every solve (stock behaviour, a few hundred KB at 1250
branches): it is still formatted and written, but to /dev/null, so the container log keeps only the `Applied:` lines and whatever else stock prints to stderr. Needs the bennu image (bench/Dockerfile.stock); stock has no write-stamping of its own."""
import os, sys, time
from pybennu.providers.power.solvers.python_power import PyPower
from pybennu.providers.power.power_daemon import server

_write = PyPower.write
_out = os.fdopen(os.dup(1), "w")
os.dup2(os.open(os.devnull, os.O_WRONLY), 1)  # pypower binds sys.stdout at import: redirect the fd


def write(self, tags):
    t0 = time.time_ns()
    reply = _write(self, tags)
    t1 = time.time_ns()
    if reply.startswith("ACK"):
        for tag, val in tags.items():
            print(f"Applied: t_apply_ns={t0} t_solved_ns={t1} tag={tag.strip(chr(0))} val={val}", file=_out, flush=True)
    return reply


PyPower.write = write
sys.exit(server())
