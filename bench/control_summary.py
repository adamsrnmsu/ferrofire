#!/usr/bin/env python3
"""control_summary.py DIR : per (cell, R) over every rep of a stack's raw dir (docs/bench/results/control/<stack>/): commands issued / done in time /
lost, and p50 / p95 / max of t_visible - t_cmd (ms) over the done commands, plus the provider-applied lag (t_apply - t_cmd) from the `Applied:` stamps
in prov.log.gz. Markdown table on stdout. Stdlib only (ww-1df.23.4)."""
import csv, glob, gzip, os, sys
from collections import defaultdict
sys.path.insert(0, os.path.dirname(__file__))
import control_join as J

def pct(v, p):
    return v[min(len(v) - 1, int(p * len(v)))] if v else None

def main(d):
    vis, app, cnt = defaultdict(list), defaultdict(list), defaultdict(lambda: [0, 0, 0])
    for f in sorted(glob.glob(f"{d}/R*_rep*.cmds.csv")):
        base = f[:-len(".cmds.csv")]
        rows = list(csv.DictReader(open(f)))
        if os.path.exists(base + ".prov.log.gz"):
            J.join(rows, J.stamps(gzip.open(base + ".prov.log.gz", "rt", errors="replace")))
        for r in rows:
            k = (r["cell"], r["dir"], int(r["R"]))
            t = int(r["t_cmd"])
            c = cnt[k]
            c[0] += 1
            if r["done"] == "1":
                c[1] += 1
                vis[k].append((int(r["t_visible"]) - t) / 1e6)
            else:
                c[2] += 1
            if r.get("t_apply_ns"):
                app[k].append((int(r["t_apply_ns"]) - t) / 1e6)
    print("| cell | dir | R | commands | done | lost | visible p50 ms | p95 | max | applied p50 ms |\n|---|---|---|---|---|---|---|---|---|---|")
    f = lambda x: "" if x is None else f"{x:.0f}"
    for k in sorted(cnt, key=lambda k: (k[0].split(":")[0], k[0].split(":")[1], int(k[0].split(":")[2]), k[1], k[2])):
        v, a = sorted(vis[k]), sorted(app[k])
        print(f"| {k[0]} | {k[1]} | {k[2]} | {cnt[k][0]} | {cnt[k][1]} | {cnt[k][2]} | {f(pct(v, .5))} | {f(pct(v, .95))} | {f(v[-1] if v else None)} | {f(pct(a, .5))} |")

if __name__ == "__main__":
    main(sys.argv[1])
