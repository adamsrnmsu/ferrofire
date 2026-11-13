#!/usr/bin/env python3
"""Loader for the control-latency raw results (docs/bench/results/control/{ferrofire,stock,range/*}): one dict per command with its
latencies in ms from t_cmd, and one per phase (cells.csv). Used by bench/charts.py for docs/bench/control-latency.md. Stdlib only (ww-1df.23.6)."""
import csv, glob, gzip, os, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import control_join as J  # noqa: E402

CTL = HERE.parent / "docs/bench/results/control"
DIRS = {("docker", "ferrofire"): CTL / "ferrofire", ("docker", "stock"): CTL / "stock",
        ("range", "ferrofire"): CTL / "range/ferrofire", ("range", "stock"): CTL / "range/stock"}
STAGES = (("ack", "t_ack"), ("stream", "t_stream"), ("visible", "t_visible"), ("setpoint", "t_setpoint"))


def pct(v, p):
    """Same rule as control_summary.py: v[int(p*len)], v sorted."""
    return v[min(len(v) - 1, int(p * len(v)))] if v else None


def commands(env, stack):
    """[{cell, mode, kind, dir, n, R, rep, done, ack, applied, stream, visible, setpoint}] (ms from t_cmd; None = absent)."""
    out = []
    for f in sorted(glob.glob(f"{DIRS[env, stack]}/R*_rep*.cmds.csv")):
        base = f[:-len(".cmds.csv")]
        rows = list(csv.DictReader(open(f)))
        if os.path.exists(base + ".prov.log.gz"):
            J.join(rows, J.stamps(gzip.open(base + ".prov.log.gz", "rt", errors="replace")))
        for r in rows:
            t, ms = int(r["t_cmd"]), lambda k: (int(r[k]) - t) / 1e6 if r.get(k) not in (None, "", "0") else None
            _, mode, n = r["cell"].split(":")
            out.append(dict(cell=r["cell"], mode=mode, kind=r["kind"], dir=r["dir"], n=int(n), R=int(r["R"]), rep=int(r["rep"]) if "rep" in r else int(base.split("_rep")[1]),
                            done=r["done"] == "1", ack=ms("t_ack"), applied=ms("t_apply_ns"), stream=ms("t_stream"), visible=ms("t_visible"), setpoint=ms("t_setpoint")))
    return out


def phases(env, stack):
    """cells.csv rows, one per (R, rep, cell, dir) phase, numbers as float."""
    out = []
    for f in sorted(glob.glob(f"{DIRS[env, stack]}/R*_rep*.cells.csv")):
        for r in csv.DictReader(open(f)):
            out.append({k: (float(v) if k in ("n", "issued", "ok", "lost", "wrong_final", "wrong_provider", "wrong_collateral", "order_violations", "flaps", "write_errors", "span_s", "wall_s", "per_s", "poll_errors", "R", "rep") else v) for k, v in r.items()})
    return out
