#!/usr/bin/env python3
"""Maximum publish rate, ferrofire provider to ferrofire field device (ww-1df.43).

    uv run --with matplotlib python bench/findings/max-rate.py

Reads docs/bench/results/maxrate (through bench/maxrate_report.py, the loader of docs/bench/max-rate.md), recomputes every number, prints them, and writes
docs/img/findings/max-rate-*.svg (byte-identical on rerun over the same raw files).
"""
import os, pathlib, sys
import matplotlib
matplotlib.use("svg")
import matplotlib.pyplot as plt
from matplotlib.patches import Patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "bench"))
import maxrate_report as mr  # noqa: E402  (stdlib only)

OUT = ROOT / "docs/img/findings"
SURFACE, INK, INK2, MUTED, GRID, AXIS, FF, STK = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7", "#2a78d6", "#898781"
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2, "ytick.color": INK,
                     "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE, "axes.spines.top": False,
                     "axes.spines.right": False, "axes.grid": True, "axes.axisbelow": True, "grid.color": GRID, "grid.linewidth": 0.8, "svg.hashsalt": "ferrofire"})
MC = {"S": "#6250d6", "M": "#1baf7a", "L": FF, "G": "#eb6834"}   # as bench/charts.py (13a-13f)
ML = {"S": "15 tags", "M": "800 tags", "L": "8 112 tags", "G": "control grid"}


def frame(fig, name, title, sub, note, handles, **adj):
    fig.text(0.012, 0.975, title, ha="left", va="top", fontsize=11, fontweight="bold")
    fig.text(0.012, 0.9, sub, ha="left", va="top", fontsize=8.5, color=INK2)
    fig.text(0.012, 0.012, note, ha="left", va="bottom", fontsize=7.5, color=MUTED)
    fig.legend(handles=handles, loc="upper right", bbox_to_anchor=(0.99, 0.86), ncols=len(handles), frameon=False, fontsize=8.5)
    fig.subplots_adjust(**adj)
    OUT.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT / f"{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/findings/max-rate.py"})
    if os.environ.get("FF_PREVIEW"):
        fig.savefig(pathlib.Path(os.environ["FF_PREVIEW"]) / f"{name}.png", dpi=110)
    plt.close(fig)


def main():
    dts = mr.DTS
    # 1. achieved rate against the target, per size ---------------------------------------------------------------------------------------------------------------
    fig, axs = plt.subplots(1, 2, figsize=(10.4, 3.6), sharey=True)
    for ax, r in zip(axs, (1, 10)):
        ax.set_title(f"{r} field device" + ("s" if r > 1 else ""), fontsize=8.5, color=INK2, loc="left")
        ax.plot([mr.hz(d) for d in dts], [mr.hz(d) for d in dts], color=MUTED, lw=0.8, ls="--")
        for m in "SMLG":
            ys = [mr.med(m, r, d, "pub_rate_hz") for d in dts]
            ax.plot([mr.hz(d) for d in dts], ys, color=MC[m], lw=1.5, marker="o", ms=3.5)
            top = max(ys)
            knee = [d for d in dts if mr.med(m, r, d, "pub_ratio") >= 99 and mr.med(m, r, d, "late_pct") < 1]
            print(f"R={r} {ML[m]}: ceiling {top:.0f} Hz, fastest target met {mr.hz(min(knee)):g} Hz at provider cpu {mr.med(m, r, min(knee), 'prov.cpu_pct'):.0f} % (target 1 kHz: {mr.med(m, r, 1, 'pub_rate_hz'):.0f} Hz, provider {mr.med(m, r, 1, 'prov.cpu_pct'):.0f} %)")
        ax.set_xscale("log"); ax.set_yscale("log")
        ax.set_xticks([mr.hz(d) for d in dts], [f"{mr.hz(d):g}" for d in dts]); ax.minorticks_off(); ax.tick_params(length=0)
        ax.set_xlabel("target publish rate, Hz", fontsize=8)
    axs[0].set_ylabel("achieved publish rate, Hz (dashed: the target)", fontsize=8)
    lost = sum(mr.total(m, r, d, "rcvbuf_errors_total") or 0 for m in "SMLG" for r in (1, 10) for d in dts)
    print(f"RcvbufErrors over every cell and rep: {lost:.0f}")
    frame(fig, "max-rate-1-achieved", "Publish rate achieved: the provider's core is the ceiling",
          "Every size follows the target until the provider thread is at 100 % of a core, then flattens; no datagram was lost at any rate.",
          "Docker bench, one VM, real provider and real field devices, engine tick = publish period 50 to 1 ms, 3 reps, median. Every value changes every tick (worst case).",
          [Patch(color=MC[m], label=ML[m]) for m in "SMLG"], left=0.07, right=0.985, top=0.7, bottom=0.2, wspace=0.06)

    # 2. flip latency against the engine tick, as shipped and with the device scan = dt -----------------------------------------------------------------------------
    fig, ax = plt.subplots(figsize=(10.4, 3.4))
    for fd, label, col in ((False, "scan 50 ms (as shipped)", STK), (True, "scan = engine dt", FF)):
        pts = []
        for d in dts:
            lat, issued, lost = mr.control_latency(d, 1, "breaker:seq:100", fd)
            if lat:
                pts.append((d, mr.pct(lat, .5), mr.pct(lat, .95)))
                print(f"flip, 1 device, dt {d} ms, {label}: p50 {pts[-1][1]:.1f} p95 {pts[-1][2]:.1f} ms; {lost} of {issued} not seen")
        ax.plot([d for d, *_ in pts], [a for _, a, _ in pts], color=col, lw=1.5, marker="o", ms=4)
        ax.plot([d for d, *_ in pts], [b for _, _, b in pts], color=col, lw=1.0, ls="--", marker="o", ms=3)
        for d, a, _ in pts:
            ax.text(d, a, f" {a:.0f}", fontsize=7.5, color=INK2, va="bottom", ha="left")
    sim_lost = sum(float(x["breaker:sim:100"]["lost"]) for d in dts for fd in (False, True) for r in (1, 10) for _, x in mr.control(d, r, fd))
    print(f"simultaneous flips lost over every cell: {sim_lost:.0f}")
    ax.set_xscale("log"); ax.set_yscale("log"); ax.invert_xaxis()
    ax.set_xticks(list(dts), [str(d) for d in dts]); ax.set_yticks([10, 20, 50, 100], ["10", "20", "50", "100"]); ax.minorticks_off(); ax.tick_params(length=0)
    ax.set_xlabel("engine tick = publish period, ms", fontsize=8); ax.set_ylabel("flip to visible, ms: p50 solid, p95 dashed", fontsize=8)
    frame(fig, "max-rate-2-flip", "Breaker flip to visible: the device's scan cycle, then the grid solve",
          "With the 50 ms scan the flip stays near 50 ms at any engine tick; with scan = dt it falls to 13 ms, where one 9 ms AC re-solve of the grid is the floor.",
          "Docker bench, 250-bus control grid, 1 field device, 100 sequential flips x 3 reps per point. 0 of 300 flips unseen in every cell.",
          [Patch(color=STK, label="scan 50 ms (as shipped)"), Patch(color=FF, label="scan = engine dt")], left=0.07, right=0.985, top=0.72, bottom=0.2)


if __name__ == "__main__":
    main()
