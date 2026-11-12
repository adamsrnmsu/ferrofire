#!/usr/bin/env python3
"""control_check.py RESULTS_DIR [PREFIX...] : sanity checks of control-bench runs (ww-1df.23.3), exit 1 on any problem. Per run (R<R>_rep<k>):
every phase issued exactly n commands; each done command has t_cmd <= t_ack and t_cmd <= t_apply <= t_solved and t_stream <= t_visible
(the stages that exist), and a provider stamp; the cell and command rows agree on the number of lost. Prints one line per run. Stdlib only."""
import csv, glob, gzip, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import control_join as J


def check(base):
    bad = []
    cells = list(csv.DictReader(open(base + ".cells.csv")))
    rows = list(csv.DictReader(open(base + ".cmds.csv")))
    J.join(rows, J.stamps(gzip.open(base + ".prov.log.gz", "rt")))
    for c in cells:
        if int(c["issued"]) != int(c["n"]):
            bad.append(f"{c['cell']} {c['dir']}: issued {c['issued']} != n {c['n']}")
    for r in rows:
        if r["done"] != "1":
            continue
        t = {k: int(r[k] or 0) for k in ("t_cmd", "t_ack", "t_echo", "t_stream", "t_visible", "t_setpoint", "t_apply_ns", "t_solved_ns")}
        tag = f"{r['cell']} {r['dir']} #{r['seq']}"
        if not (0 < t["t_cmd"] <= t["t_ack"] and t["t_cmd"] <= t["t_echo"]):
            bad.append(f"{tag}: cmd/ack/echo out of order {t}")
        elif not (t["t_cmd"] <= t["t_apply_ns"] <= t["t_solved_ns"]):
            bad.append(f"{tag}: no provider stamp or out of order {t}")
        elif not (t["t_stream"] and t["t_stream"] <= t["t_visible"]):
            bad.append(f"{tag}: stream/visible out of order {t}")
    lost = sum(int(c["lost"]) for c in cells)
    if lost != sum(r["done"] != "1" for r in rows):
        bad.append("cells.csv and cmds.csv disagree on lost")
    print(f"{os.path.basename(base)}: {len(cells)} phases, {len(rows)} commands, lost {lost}, {len(bad)} problems")
    for b in bad[:10]:
        print("   ", b)
    return not bad


if __name__ == "__main__":
    d, pre = sys.argv[1], sys.argv[2:] or [""]
    runs = sorted({p[: -len(".cells.csv")] for x in pre for p in glob.glob(f"{d}/{x}*.cells.csv") if ".part." not in p})
    sys.exit(0 if all([check(r) for r in runs]) else 1)
