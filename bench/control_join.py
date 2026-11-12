#!/usr/bin/env python3
"""control_join.py CMDS.csv PROVIDER.log > JOINED.csv : add the provider's own apply/solve stamps to the driver's per-command rows
(ww-1df.23.3). `ferrofire run --stamp-writes` logs `Applied: t_apply_ns=N t_solved_ns=N tag=T val=V` once per accepted write (unix ns, after
the engine applied the batch / after the step that re-solved it). A command joins the first not-yet-used stamp of its tag whose value is its
target and whose t_apply_ns is not before its t_cmd nor more than 30 s after it; no such stamp = the provider never applied it (columns empty). Another provider (the stock
pybennu baseline, ww-1df.23.4) only has to produce lines in the same shape. Stdlib only."""
import csv, re, sys
from collections import defaultdict

LINE = re.compile(r"Applied: t_apply_ns=(\d+) t_solved_ns=(\d+) tag=(\S+) val=(\S+)")
TRUE, FALSE = {"true", "1"}, {"false", "0"}


def stamps(lines):
    """{tag: [(t_apply, t_solved, value)]} in log order."""
    out = defaultdict(list)
    for l in lines:
        m = LINE.search(l)
        if m:
            out[m[3]].append((int(m[1]), int(m[2]), m[4]))
    return out


CAP_NS = 30 * 10**9   # a stamp later than this after t_cmd is not that command's (it never applied; the next command of the tag did)


def join(rows, st, cap=CAP_NS):
    """rows: dicts of the cmds CSV. Returns them with t_apply_ns and t_solved_ns added ('' = no stamp)."""
    used = set()
    for r in sorted(rows, key=lambda r: int(r["t_cmd"])):
        r["t_apply_ns"] = r["t_solved_ns"] = ""
        t_cmd = int(r["t_cmd"])
        if not t_cmd:
            continue
        want = TRUE if r["to"] == "1" else FALSE
        for i, (a, s, v) in enumerate(st.get(r["tag"], [])):
            if t_cmd <= a <= t_cmd + cap and v in want and (r["tag"], i) not in used:
                used.add((r["tag"], i))
                r["t_apply_ns"], r["t_solved_ns"] = a, s
                break
    return rows


def main(cmds, log):
    rows = list(csv.DictReader(open(cmds)))
    cols = list(rows[0]) + ["t_apply_ns", "t_solved_ns"] if rows else []
    w = csv.DictWriter(sys.stdout, cols, lineterminator="\n")
    w.writeheader()
    w.writerows(join(rows, stamps(open(log))))


if __name__ == "__main__":
    main(*sys.argv[1:3])
