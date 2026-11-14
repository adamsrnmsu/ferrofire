#!/usr/bin/env python3
"""Loader for the control-latency raw results (docs/bench/results/control/{ferrofire,stock,range/*}): one dict per command with its
latencies in ms from t_cmd, and one per phase (cells.csv). Used by bench/charts.py for docs/bench/control-latency.md. Stdlib only (ww-1df.23.6)."""
import csv, glob, gzip, os, pathlib, re, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import control_join as J  # noqa: E402

CTL = HERE.parent / "docs/bench/results/control"
DIRS = {("docker", "ferrofire"): CTL / "ferrofire", ("docker", "stock"): CTL / "stock", ("docker", "stock-v2026.11.03"): CTL / "stock-v2026.11.03",
        ("range", "ferrofire"): CTL / "range/ferrofire", ("range", "stock"): CTL / "range/stock",
        ("docker", "ferrofire-before"): CTL / "ferrofire-before-ww-1df.32", ("range", "ferrofire-before"): CTL / "range/ferrofire-before-ww-1df.32"}
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
                            done=r["done"] == "1", ack=ms("t_ack"), applied=ms("t_apply_ns"), solved=ms("t_solved_ns"), stream=ms("t_stream"), visible=ms("t_visible"), setpoint=ms("t_setpoint")))
    return out


def phases(env, stack):
    """cells.csv rows, one per (R, rep, cell, dir) phase, numbers as float."""
    out = []
    for f in sorted(glob.glob(f"{DIRS[env, stack]}/R*_rep*.cells.csv")):
        for r in csv.DictReader(open(f)):
            out.append({k: (float(v) if k in ("n", "issued", "ok", "lost", "wrong_final", "wrong_provider", "wrong_collateral", "order_violations", "flaps", "write_errors", "span_s", "wall_s", "per_s", "poll_errors", "R", "rep") else v) for k, v in r.items()})
    return out


# Publish period of the stack's provider: ferrofire runs the grid at dt 0.05 s (bench/gen_grid.py), stock bennu publishes once a second.
PUBLISH_S = {"ferrofire": 0.05, "ferrofire-before": 0.05, "stock": 1.0, "stock-v2026.11.03": 1.0}
# Which stock bennu a stack is (ww-1df.47). Directories without a version are bennu 6.0.0 (the phenix overlay image, before upstream 91fd137); `-v2026.11.03` is the
# release that has it. NEW is the current one, the main comparison; OLD is kept as the historical one.
NEW, OLD = "stock-v2026.11.03", "stock"
LABEL = {"ferrofire": "ferrofire", NEW: "bennu v2026.11.03", OLD: "bennu 6.0.0 (pre-91fd137)"}
def parse_dir(name):
    """(kind, provider, fd, version, interval s) of a results directory name: `stock[-v<ver>][-<s>s]` (a stock provider + its own field device) or
    `cross-<provider>-<fd>[-v<ver>][-<s>s]`; version "" = bennu 6.0.0, interval None = not tuned (1 s for stock, 0.05 s for ferrofire). None if the name is neither."""
    m = re.fullmatch(r"(?:stock()|cross-(ferrofire|stock)-(ferrofire|stock))(-v[\d.]+)?(?:-([\d.]+)s)?", name)
    if not m:
        return None
    ver, iv = (m[4] or "-")[1:], float(m[5]) if m[5] else None
    return ("stock", "stock", "stock", ver, iv) if m[2] is None else ("cross", m[2], m[3], ver, iv)


# Stock at faster publish intervals (ww-1df.35): results/control/stock[-v<version>]-<interval>s/, Docker only, stack name = the directory name.
_FAST = [(parse_dir(d.name), d.name) for d in CTL.glob("stock-*s") if d.is_dir()]
STOCK_FAST = sorted((k[4], n) for k, n in _FAST if k and k[4] and not k[3])[::-1]    # bennu 6.0.0
NEW_FAST = sorted((k[4], n) for k, n in _FAST if k and k[4] and k[3] == "v2026.11.03")[::-1]
for _iv, _n in STOCK_FAST + NEW_FAST:
    DIRS["docker", _n] = CTL / _n
    PUBLISH_S[_n] = _iv
    LABEL[_n] = f"{LABEL[NEW if _n.startswith(NEW) else OLD]}, {_iv:g} s"

# The 2x2 provider x field-device cross matrix (ww-1df.40): results/control/cross-<provider>-<fd>[-v<version>][-<interval>s]/, Docker only, stack name = the directory name.
CROSS = sorted(d.name for d in CTL.glob("cross-*") if d.is_dir())
for _n in CROSS:
    _k = parse_dir(_n)
    assert _k and _k[0] == "cross", _n
    DIRS["docker", _n] = CTL / _n
    PUBLISH_S[_n] = 0.05 if _k[1] == "ferrofire" else _k[4] or 1.0

STATS = re.compile(r"Publish-stats: t_ns=(\d+) period_s=\S+ publishes=(\d+) .*? cpu_s=([\d.]+)")


def provider_stats(stack, env="docker"):
    """From the `Publish-stats:` lines of bench/stock_provider.py (one per 10 s per provider) in the stack's prov.log.gz, one fresh provider per `== ` section (a cell):
    ([provider process CPU, % of one core, per section of >= 2 lines], [effective publish period in ms, one per 10 s window]). Both from consecutive-line deltas,
    so provider start-up (importing pypower, about 4 CPU s) is left out."""
    cpu, per = [], []

    def close(sec):
        if len(sec) >= 2:
            cpu.append(100 * (sec[-1][2] - sec[0][2]) / ((sec[-1][0] - sec[0][0]) / 1e9))
        per.extend((y[0] - x[0]) / 1e6 / y[1] for x, y in zip(sec, sec[1:]))

    for f in sorted(glob.glob(f"{DIRS[env, stack]}/R*_rep*.prov.log.gz")):
        sec = []
        for l in gzip.open(f, "rt", errors="replace"):
            m = STATS.search(l)
            if m:
                sec.append((int(m[1]), int(m[2]), float(m[3])))
            elif l.startswith("== "):
                close(sec)
                sec = []
        close(sec)
    return sorted(cpu), sorted(per)


def order_split(rows, period_s):
    """(reordered, chunk_skew) for one RTU's commands of one phase. Of the adjacent pairs (issue order) whose t_visible is inverted (the driver's
    order_violations), `reordered` = t_stream inverted by >= half a publish period (the later command's state reached the stream a publish earlier),
    or t_stream missing; `chunk_skew` = the rest (both states in one publish, polled between its datagrams)."""
    v = [r for r in sorted(rows, key=lambda r: int(r["seq"])) if int(r["t_visible"])]
    re = sk = 0
    for a, b in zip(v, v[1:]):
        if int(b["t_visible"]) < int(a["t_visible"]):
            sa, sb = int(a["t_stream"]), int(b["t_stream"])
            if not (sa and sb) or sa - sb >= period_s * 1e9 / 2:
                re += 1
            else:
                sk += 1
    return re, sk


def order_counts(env, stack):
    """{(R, 'kind:mode'): [reordered, chunk_skew]} summed over reps, directions and RTUs of the stack's cmds.csv files."""
    out = {}
    for f in sorted(glob.glob(f"{DIRS[env, stack]}/R*_rep*.cmds.csv")):
        g = {}
        for r in csv.DictReader(open(f)):
            g.setdefault((r["cell"], r["dir"], r["rtu"]), []).append(r)
        for (cell, _, _), rows in g.items():
            kind, mode, _ = cell.split(":")
            o = out.setdefault((int(rows[0]["R"]), f"{kind}:{mode}"), [0, 0])
            for i, x in enumerate(order_split(rows, PUBLISH_S[stack])):
                o[i] += x
    return out


if __name__ == "__main__":  # self-check: python3 bench/control_report.py
    assert parse_dir("stock") == ("stock", "stock", "stock", "", None) and parse_dir("stock-0.1s") == ("stock", "stock", "stock", "", 0.1)
    assert parse_dir("stock-v2026.11.03-0.1s") == ("stock", "stock", "stock", "v2026.11.03", 0.1) and parse_dir("stock-v2026.11.03")[3:] == ("v2026.11.03", None)
    assert parse_dir("cross-ferrofire-stock-v2026.11.03") == ("cross", "ferrofire", "stock", "v2026.11.03", None)
    assert parse_dir("cross-stock-stock-0.1s") == ("cross", "stock", "stock", "", 0.1) and parse_dir("cross-stock-stock-v2026.11.03-0.1s")[3:] == ("v2026.11.03", 0.1)
    assert parse_dir("ferrofire") is None and parse_dir("stock-capacity.txt") is None and parse_dir("cross-x-stock") is None
    mk = lambda seq, vis, st: {"seq": str(seq), "t_visible": str(vis), "t_stream": str(st)}
    # a inverted within 3 ms of stream (skew), b inverted by 1 s of stream (reorder), c no stream stamp (reorder), d not inverted
    assert order_split([mk(0, 10, 100), mk(1, 5, 98_000_000)], 0.05) == (0, 1)
    assert order_split([mk(0, 10, 1_000_000_000), mk(1, 5, 5)], 1.0) == (1, 0)
    assert order_split([mk(0, 10, 0), mk(1, 5, 7)], 0.05) == (1, 0)
    assert order_split([mk(0, 5, 100), mk(1, 10, 1), mk(2, 0, 1)], 0.05) == (0, 0)
    print("ok")
