#!/usr/bin/env python3
"""Loader for the maximum-publish-rate raw results (docs/bench/results/maxrate/, ww-1df.43): one metrics.csv per (model, R, dt, publish period, rep), written by
bench/maxrate/run.py (the observer is bench/maxrate/src/main.rs), and the control-latency cells of the same sweep (docs/bench/results/maxrate/control/dt<ms>/).
Used by bench/charts.py (docs/bench/max-rate.md) and bench/findings/max-rate.py. Stdlib only."""
import csv, glob, os, pathlib, re, statistics as st

HERE = pathlib.Path(__file__).resolve().parent
DIR = HERE.parent / "docs/bench/results/maxrate"
MODELS = {"S": "15 tags", "M": "800 tags", "L": "8 112 tags", "G": "control grid (1 300 tags, 1 250 breakers)"}
DTS = (50, 20, 10, 5, 2, 1)   # ms, engine tick = publish period in the main sweep
NAME = re.compile(r"(?P<m>[SMLG])_R(?P<r>\d+)_dt(?P<dt>\d+)_pub(?P<pub>\d+)_rep(?P<rep>\d+)\.metrics\.csv$")


def load(d=DIR):
    """{(model, R, dt_ms, pub_ms): {rep: {metric: float}}}"""
    out = {}
    for f in sorted(glob.glob(f"{d}/*.metrics.csv")):
        m = NAME.search(os.path.basename(f))
        k = (m["m"], int(m["r"]), int(m["dt"]), int(m["pub"]))
        out.setdefault(k, {})[int(m["rep"])] = {r["metric"]: float(r["value"]) for r in csv.DictReader(open(f))}
    return out


DATA = load()


def reps(m, r, dt, pub=None):
    return DATA.get((m, r, dt, dt if pub is None else pub), {})


def derived(x, r):
    """Metrics computed from one run's raw ones: worst device, ratios."""
    d = dict(x)
    fds = [f"rtu{i}" for i in range(1, r + 1)]
    d["fd_cpu_max"] = max(x[f"{f}.cpu_pct"] for f in fds)
    d["fd_cpu_mean"] = st.mean(x[f"{f}.cpu_pct"] for f in fds)
    d["fd_rss_max"] = max(x[f"{f}.rss_mb"] for f in fds)
    d["fd_loss_max_pct"] = max(100 * x[f"{f}.dg_lost"] / x[f"{f}.dg_sent"] if x[f"{f}.dg_sent"] else 0 for f in fds)
    d["pub_ratio"] = 100 * x["pub_rate_hz"] / x["pub_rate_target_hz"] if "pub_rate_hz" in x else float("nan")
    return d


def vals(m, r, dt, metric, pub=None):
    return [derived(x, r).get(metric, float("nan")) for _, x in sorted(reps(m, r, dt, pub).items())]


def med(m, r, dt, metric, pub=None):
    v = [x for x in vals(m, r, dt, metric, pub) if x == x]
    return st.median(v) if v else None


def total(m, r, dt, metric, pub=None):
    v = [x for x in vals(m, r, dt, metric, pub) if x == x]
    return sum(v) if v else None


def hz(dt_ms):
    return 1000 / dt_ms


# ---- control cells (ffcontrol, run_control.py with DT_MS) ---------------------------------------------------------------------------------------
CTL = DIR / "control"


def cdir(dt, fd=False):
    """Results directory of one engine dt; fd=True: the variant whose field-device scan cycle-time is also dt ms (default 50)."""
    return f"{CTL}/dt{dt}" + (f"-fd{dt}" if fd else "")


def control(dt, r, fd=False):
    """[(rep, rows)] of the cells.csv of one (dt, R): rows = {cell: row dict}."""
    out = []
    for f in sorted(glob.glob(f"{cdir(dt, fd)}/R{r}_rep*.cells.csv")):
        rep = int(re.search(r"rep(\d+)", f)[1])
        out.append((rep, {row["cell"]: row for row in csv.DictReader(open(f))}))
    return out


def control_latency(dt, r, cell, fd=False):
    """(sorted visible latencies in ms from t_cmd of the done commands, issued, not visible) of one cell over the reps; the 'visible' stage as bench/control_report.py."""
    lat, issued, lost = [], 0, 0
    for f in sorted(glob.glob(f"{cdir(dt, fd)}/R{r}_rep*.cmds.csv")):
        for row in csv.DictReader(open(f)):
            if row["cell"] != cell:
                continue
            issued += 1
            if row["done"] == "1" and row["t_visible"] not in ("", "0"):
                lat.append((int(row["t_visible"]) - int(row["t_cmd"])) / 1e6)
            else:
                lost += 1
    return sorted(lat), issued, lost


def pct(v, p):
    """Nearest rank, as the harness: v[ceil(p*n)-1], v sorted."""
    return v[max(0, min(len(v) - 1, -(-int(p * 1e6) * len(v) // 1000000) - 1))] if v else None


if __name__ == "__main__":  # self-check: python3 bench/maxrate_report.py
    assert NAME.search("L_R10_dt5_pub50_rep2.metrics.csv")["pub"] == "50"
    assert pct([1, 2, 3, 4], 0.5) == 2 and pct([], 0.5) is None
    print("ok", len(DATA), "cells")
