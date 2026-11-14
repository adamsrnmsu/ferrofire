#!/usr/bin/env python3
"""Loader for the field-device microbench raw results (docs/bench/results/fdmicro/, ww-1df.41): one metrics.csv per (stack, profile, protocol, rep) plus the per-sample
stream-to-register and write-forward delays. Used by bench/charts.py for docs/bench/field-device.md. Stdlib only."""
import csv, glob, os, pathlib, statistics as st

HERE = pathlib.Path(__file__).resolve().parent
FD = HERE.parent / "docs/bench/results/fdmicro"
STACKS = ("ferrofire", "stock")
PROFILES = {"S": 15, "M": 800, "L": 8112}   # tags per profile
MAIN_HZ = {"S": "10", "M": "100", "L": "10"}    # the profile's main publish rate


def _key(f):
    stack, prof, proto, rep = os.path.basename(f).split(".")[0].split("_")
    return stack, prof, proto, int(rep[3:])


def load():
    """{(stack, profile, proto): {rep: {(phase, variant, metric): value}}}"""
    out = {}
    for f in sorted(glob.glob(f"{FD}/*.metrics.csv")):
        stack, prof, proto, rep = _key(f)
        out.setdefault((stack, prof, proto), {})[rep] = {(r["phase"], r["variant"], r["metric"]): float(r["value"]) for r in csv.DictReader(open(f))}
    return out


DATA = load()


def reps(stack, prof, proto):
    return sorted(DATA.get((stack, prof, proto), {}))


def vals(stack, prof, proto, phase, variant, metric):
    return [m[phase, variant, metric] for m in DATA.get((stack, prof, proto), {}).values() if (phase, variant, metric) in m]


def med(stack, prof, proto, phase, variant, metric):
    v = vals(stack, prof, proto, phase, variant, metric)
    return st.median(v) if v else None


def total(stack, prof, proto, phase, variant, metric):
    v = vals(stack, prof, proto, phase, variant, metric)
    return sum(v) if v else None


def pooled(stack, prof, proto, kind, variant):
    """Every delay (ms) of one (stack, profile, protocol, kind = s2r|wf, variant) over all reps, sorted, and the number of samples that never showed (-1)."""
    d, miss = [], 0
    for f in sorted(glob.glob(f"{FD}/{stack}_{prof}_{proto}_rep*.{kind}.csv")):
        for r in csv.DictReader(open(f)):
            if r["variant"] != variant:
                continue
            x = int(r["delay_ns"])
            if x < 0:
                miss += 1
            else:
                d.append(x / 1e6)
    return sorted(d), miss


def pct(v, p):
    """Nearest rank, as the harness: v[ceil(p*n)-1], v sorted."""
    return v[max(0, min(len(v) - 1, -(-int(p * 1e6) * len(v) // 1000000) - 1))] if v else None
