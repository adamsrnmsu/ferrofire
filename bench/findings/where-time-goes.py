#!/usr/bin/env python3
"""Where the time goes, and why stock is slow: raw control-bench results -> docs/img/findings/where-time-goes-*.svg + the numbers on stdout.

    uv run --with matplotlib python bench/findings/where-time-goes.py

Reads docs/bench/results/control/{ferrofire,stock-v2026.11.03[-0.1s],stock,stock-*s}/ (cmds.csv, cells.csv, prov.log.gz), stock-capacity.txt, solve-time.txt.
Docker bench only; 5 reps (ferrofire, bennu 6.0.0), 3 (bennu v2026.11.03, current: 1 s and 0.1 s only). Percentile rule = bench/control_report.py: sorted v[int(p*len)]. Reruns are byte-identical."""
import csv, glob, gzip, pathlib, re, statistics, sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "bench"))
import control_join as J  # noqa: E402  (stdlib only: joins the provider's Applied: stamps onto the commands)

CTL, OUT = ROOT / "docs/bench/results/control", ROOT / "docs/img/findings"
# Look: as bench/charts.py and the other bench/findings scripts: ferrofire blue, stock bennu grey, in every chart; every series is labelled
# directly (the grey has no hue, so colour alone never carries identity); light surface baked in so it reads on light and dark pages.
FF, STK, STK_OLD = "#2a78d6", "#898781", "#c9c8c0"   # ferrofire blue; bennu v2026.11.03 (current) dark grey; bennu 6.0.0 (pre-91fd137, the historical one) light grey
SURFACE, INK, INK2, MUTED, GRID, AXIS = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7"
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2, "ytick.color": INK2,
                     "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE, "axes.spines.top": False,
                     "axes.spines.right": False, "axes.axisbelow": True, "grid.color": GRID, "grid.linewidth": 0.8, "svg.hashsalt": "ferrofire"})
INTERVALS = [("stock", 1.0), ("stock-0.5s", 0.5), ("stock-0.2s", 0.2), ("stock-0.1s", 0.1), ("stock-0.05s", 0.05)]   # bennu 6.0.0
NEW_INTERVALS = [("stock-v2026.11.03", 1.0), ("stock-v2026.11.03-0.1s", 0.1)]                                         # bennu v2026.11.03 (ww-1df.47)
XT = ["1 s\n(as shipped)", "0.5 s", "0.2 s", "0.1 s", "0.05 s"]


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(p * len(v)))]


def files(d, ext):
    return [f for f in sorted(glob.glob(f"{CTL}/{d}/R*_rep*.{ext}")) if ".part" not in f]


def commands(d):
    """Done commands of a results directory: cell, R and ms from t_cmd to each stage (None = no stamp)."""
    out = []
    for f in files(d, "cmds.csv"):
        rows = list(csv.DictReader(open(f)))
        J.join(rows, J.stamps(gzip.open(f[:-len("cmds.csv")] + "prov.log.gz", "rt", errors="replace")))
        for r in rows:
            if r["done"] == "1":
                ms = lambda k: (int(r[k]) - int(r["t_cmd"])) / 1e6 if r[k] not in ("", "0") else None
                out.append(dict(cell=r["cell"], R=int(r["R"]), ack=ms("t_ack"), applied=ms("t_apply_ns"), stream=ms("t_stream"), visible=ms("t_visible")))
    return out


def seq(cmds, R, stage, p=.5):
    return pct([c[stage] for c in cmds if c["cell"] == "breaker:seq:100" and c["R"] == R and c[stage] is not None], p)


def burst(d, R, col="lost"):
    """(sum of `col`, commands issued) over the reps of 100 simultaneous breaker flips on R RTUs."""
    rows = [r for f in files(d, "cells.csv") for r in csv.DictReader(open(f)) if r["cell"] == "breaker:sim:100" and r["R"] == str(R)]
    return sum(int(r[col]) for r in rows), sum(int(r["issued"]) for r in rows)


def bench_period_ms(d):
    """Median real publish period during the bench, from consecutive `Publish-stats:` lines of one provider (a `== ` section)."""
    per, pat = [], re.compile(r"Publish-stats: t_ns=(\d+) .*?publishes=(\d+)")
    for f in files(d, "prov.log.gz"):
        prev = None
        for l in gzip.open(f, "rt", errors="replace"):
            m = pat.search(l)
            if l.startswith("== "):
                prev = None
            elif m:
                t, n = int(m[1]), int(m[2])
                if prev and n:
                    per.append((t - prev) / 1e6 / n)
                prev = t
    return statistics.median(per) if per else None


def save(fig, name, title, sub):
    fig.text(0.012, 0.975, title, ha="left", va="top", fontsize=11, fontweight="bold")
    fig.text(0.012, 0.895, sub, ha="left", va="top", fontsize=8.5, color=INK2)
    fig.savefig(OUT / f"where-time-goes-{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/findings/where-time-goes.py"})
    plt.close(fig)


def chart_stages(st):
    """One bar per stack on a shared time axis: pale = until the provider applied the write, full = until the SCADA reads it back."""
    fig, ax = plt.subplots(figsize=(7.4, 3.4))
    rows = [("bennu 6.0.0\n(pre-91fd137)", STK_OLD, st["stock"]), ("bennu\nv2026.11.03", STK, st["new"]), ("ferrofire", FF, st["ferrofire"])]
    for y, (name, col, s) in enumerate(rows):
        ax.barh(y, s["applied"], height=0.5, color=col, alpha=0.4, lw=0)
        ax.barh(y, s["visible"] - s["applied"], left=s["applied"], height=0.5, color=col, ec=SURFACE, lw=1.5)
        ax.text(-12, y, name, ha="right", va="center", fontweight="bold")
        ax.text(s["visible"] + 12, y, f"read back at {s['visible']:.0f} ms", va="center")
    for y, s in ((0, st["stock"]), (1, st["new"])):
        ax.text((s["applied"] + s["stream"]) / 2, y, f"applied at {s['applied']:.0f} ms, then waits {s['stream'] - s['applied']:.0f} ms for the next publish", ha="center", va="center", color="white" if y else INK, fontsize=8.5, fontweight="bold")
    f = st["ferrofire"]
    ax.text(f["visible"] + 12, 2.5, f"applied at {f['applied']:.0f} ms, on the wire at {f['stream']:.0f} ms", va="center", color=INK2, fontsize=8)
    ax.set(xlim=(0, 1400), ylim=(-0.45, 2.9), yticks=[], xticks=range(0, 1201, 200), xlabel="time after the SCADA write, ms (p50)")
    ax.spines["left"].set_visible(False)
    ax.grid(axis="x")
    ax.tick_params(length=0)
    fig.subplots_adjust(top=0.8, bottom=0.18, left=0.13, right=0.985)
    s = st["new"]
    save(fig, "stages", f"Bennu applies a flip in {s['applied']:.0f} ms, then waits for its once-a-second publish",
         "One breaker flip, write to read-back at the SCADA; pale = until the provider applied it. 5 RTUs, Docker, v2026.11.03 and 6.0.0 alike.")


def line(ax, ys, col, label, ls="-", dy=0, fmt="{:.0f}", ends=True):
    ax.plot(range(len(ys)), ys, ls, color=col, lw=2, marker="o", ms=6, mec=SURFACE, mew=1.5, clip_on=False)
    ax.text(len(ys) - 1 + 0.12, ys[-1] + dy, label, va="center", fontsize=8.5)
    if ends:
        ax.text(0, ys[0], fmt.format(ys[0]) + "  ", ha="right", va="center", fontsize=8, color=INK2)


def frame(ax, ylabel, top):
    ax.set(xlim=(-0.45, 5.7), ylim=(0, top), xlabel="publish interval of bennu's provider (v2026.11.03: 1 s and 0.1 s only)")
    ax.set_xticks(range(5), XT)
    ax.set_ylabel(ylabel)
    ax.grid(axis="y")
    ax.tick_params(length=0)


def new_dots(ax, ys, fmt="{:.0f}", dy=0, mk="o"):
    """bennu v2026.11.03: measured at 1 s (x = 0) and 0.1 s (x = 3) only, so markers and no line."""
    for x, y in zip((0, 3), ys):
        ax.plot(x, y, ls="", marker=mk, ms=8, color=STK, mec=SURFACE, mew=1.5, clip_on=False, zorder=4)


def chart_sweep(p5, p1, ff, n5, n1):
    fig, ax = plt.subplots(figsize=(7.4, 3.6))
    ax.plot([-0.45, 4.05], [ff, ff], color=FF, lw=2)
    ax.text(-0.4, ff - 50, f"ferrofire {ff:.0f} ms (any RTU count)", va="center", fontsize=8.5)
    line(ax, p1, STK_OLD, f"6.0.0, 1 RTU\n{p1[-1]:.0f} ms", ls="--", ends=False)
    line(ax, p5, STK_OLD, f"6.0.0, 5 RTUs\n{p5[-1]:.0f} ms", dy=65, ends=False)
    new_dots(ax, n5)
    new_dots(ax, n1, mk="s")
    ax.text(0.1, n5[0] - 90, f"v2026.11.03: {n5[0]:.0f} (5 RTUs), {n1[0]:.0f} (1 RTU)", fontsize=8, va="center", color=INK)
    ax.text(3, n5[1] + 120, f"v2026.11.03:\n{n5[1]:.0f} (5 RTUs)\n{n1[1]:.0f} (1 RTU)", fontsize=8, va="center", ha="center", color=INK)
    frame(ax, "write to read-back, ms (p50)", 1150)
    fig.subplots_adjust(top=0.8, bottom=0.2, left=0.09, right=0.985)
    save(fig, "interval", f"Bennu v2026.11.03 at 0.1 s: {n5[1]:.0f} ms on 5 RTUs, {n1[1]:.0f} on 1 (6.0.0: {p1[3]:.0f} on 1)",
         "100 sequential flips per point, Docker; 5 reps (6.0.0), 3 (v2026.11.03). Dashed = 6.0.0 on one RTU. p50.")


def chart_burst(l1, l2, n1, n2):
    fig, ax = plt.subplots(figsize=(7.4, 3.4))
    ax.axhline(0, color=FF, lw=2, clip_on=False, zorder=3)
    ax.text(4.12, 5, "ferrofire: 0 missed", va="center", fontsize=8.5)
    line(ax, l1, STK_OLD, f"6.0.0, 1 RTU\n{l1[-1]:.0f} % missed", ls="--", fmt="{:.0f} %")
    line(ax, l2, STK_OLD, f"6.0.0, 2 RTUs\n{l2[-1]:.0f} % missed", fmt="{:.0f} %")
    for x, a, b in zip((0, 3), n1, n2):
        ax.plot(x, a, ls="", marker="o", ms=8, color=STK, mec=SURFACE, mew=1.5, clip_on=False, zorder=4)
        ax.plot(x, b, ls="", marker="s", ms=8, color=STK, mec=SURFACE, mew=1.5, clip_on=False, zorder=4)
    ax.text(2.0, 14, "v2026.11.03 (1 s and 0.1 s): 0 missed\non 1 and on 2 RTUs", fontsize=8.5, color=INK, ha="center")
    frame(ax, "flips not visible within 10 s, %", 100)
    fig.subplots_adjust(top=0.8, bottom=0.21, left=0.09, right=0.985)
    save(fig, "burst", f"Bursts miss {l2[-1]:.0f} to {l1[-1]:.0f} % on bennu 6.0.0 at any interval, none on v2026.11.03",
         "100 simultaneous flips on 1 or 2 RTUs, Docker; 500 flips per 6.0.0 point, 300 per v2026.11.03 point.")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    ff = commands("ferrofire")
    sweep = {iv: commands(d) for d, iv in INTERVALS}
    new = {iv: commands(d) for d, iv in NEW_INTERVALS}
    stages = {n: {s: seq(c, 5, s) for s in ("ack", "applied", "stream", "visible")} for n, c in (("ferrofire", ff), ("new", new[1.0]), ("stock", sweep[1.0]))}
    print("stages, 100 sequential flips, 5 RTUs, p50 ms")
    for n, s in stages.items():
        print(f"  {n:10} " + "  ".join(f"{k} {v:.1f}" for k, v in s.items()))
    print("sequential write to read-back, ms: interval  R  p50  p95  applied-p50")
    for R in (1, 2, 5, 10):
        print(f"  ferrofire  R{R:<2} {seq(ff, R, 'visible'):6.0f} {seq(ff, R, 'visible', .95):6.0f} {seq(ff, R, 'applied'):6.0f}")
        for iv, c in sweep.items():
            print(f"  6.0.0 {iv:<4} R{R:<2} {seq(c, R, 'visible'):6.0f} {seq(c, R, 'visible', .95):6.0f} {seq(c, R, 'applied'):6.0f}")
        for iv, c in new.items():
            print(f"  v2026.11.03 {iv:<4} R{R:<2} {seq(c, R, 'visible'):6.0f} {seq(c, R, 'visible', .95):6.0f} {seq(c, R, 'applied'):6.0f}")
    print("100 simultaneous flips: lost/issued per R, and order_violations over all simultaneous-100 phases")
    lost = {}
    for d, iv in [("ferrofire", "ff")] + INTERVALS + [(d, f"new{iv}") for d, iv in NEW_INTERVALS]:
        lost[iv] = {R: burst(d, R) for R in (1, 2, 5, 10)}
        print(f"  {d:12} " + "  ".join(f"R{R} {a}/{b}" for R, (a, b) in lost[iv].items()) + f"  reordered {sum(burst(d, R, 'order_violations')[0] for R in (1, 2, 5, 10))}")
    print("stock publish cost (stock-capacity.txt, idle) and real period during the bench (Publish-stats in prov.log.gz)")
    for l in open(CTL / "stock-capacity.txt"):
        if l.startswith("interval"):
            f = dict(zip(l.split()[::2], l.split()[1::2]))
            b = bench_period_ms("stock" if f["interval"] == "1" else f"stock-{f['interval']}s")
            print(f"  {f['interval']:>4} s  one publish {f['work_ms']} ms  idle period {f['effective_period_ms']} ms  cpu {f['cpu_pct']} %  bench period {'n/a' if b is None else round(b)} ms")
    print("  ferrofire " + [l.strip() for l in open(CTL / "solve-time.txt") if l.startswith("per_solve_ms")][0])

    chart_stages(stages)
    chart_sweep([seq(sweep[iv], 5, "visible") for _, iv in INTERVALS], [seq(sweep[iv], 1, "visible") for _, iv in INTERVALS], seq(ff, 5, "visible"),
                [seq(new[iv], 5, "visible") for iv in (1.0, 0.1)], [seq(new[iv], 1, "visible") for iv in (1.0, 0.1)])
    chart_burst(*[[100 * lost[iv][R][0] / lost[iv][R][1] for _, iv in INTERVALS] for R in (1, 2)], *[[100 * lost[f"new{iv}"][R][0] / lost[f"new{iv}"][R][1] for iv in (1.0, 0.1)] for R in (1, 2)])
    # the smallest check that fails if the join or the percentile breaks: stages come in order, ferrofire loses nothing
    assert all(s["ack"] < s["applied"] < s["stream"] < s["visible"] for s in stages.values()), stages
    assert all(a == 0 and b == 500 for a, b in lost["ff"].values()), lost["ff"]
    assert all(a == 0 for k, v in lost.items() if str(k).startswith("new") for a, _ in v.values()), "the burst chart claims v2026.11.03 loses none"
    assert all(s["ack"] < s["applied"] < s["stream"] < s["visible"] for s in stages.values())


if __name__ == "__main__":
    main()
