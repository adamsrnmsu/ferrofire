#!/usr/bin/env python3
"""compare.py A.txt B.txt : the `state:` lines of two check.py runs match: same steps and bits, registers within 2 counts
(the OpenDSS solve's last digit moves between solves, check.py near()). Prints both on a mismatch; exit 1."""
import ast, re, sys

LINE = re.compile(r"^state: (.*): ir=(\[.*?\]) di=(\[.*?\])$")


def states(path):
    return [(m[1], ast.literal_eval(m[2]), ast.literal_eval(m[3])) for m in map(LINE.match, open(path)) if m]


a, b = states(sys.argv[1]), states(sys.argv[2])
same = bool(a) and len(a) == len(b) and all(
    la == lb and da == db and len(ia) == len(ib) and all(abs(x - y) <= 2 for x, y in zip(ia, ib))
    for (la, ia, da), (lb, ib, db) in zip(a, b))
if not same:
    for name, s in (sys.argv[1], a), (sys.argv[2], b):
        print(name, *s, sep="\n  ")
sys.exit(not same)
