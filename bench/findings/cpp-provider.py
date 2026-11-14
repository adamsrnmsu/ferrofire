#!/usr/bin/env python3
"""ferrofire against Sandia's C++ bennu-simulink-provider with a stub model: plumbing only, no solver (cppprov, ww-1df.42).

    uv run --with matplotlib python bench/findings/cpp-provider.py

Reads docs/bench/results/cppprov/<provider>-<fd>/N*_R10_P*_rep*.<cell>.cmds.csv, recomputes every number the way
bench/cppprov/report.py does (latency = t_visible - t_cmd of the commands done; seq pools seq:10 and seq:50), prints them,
and writes docs/img/findings/cpp-provider-*.svg (byte-identical on rerun over the same raw files).
"""
import collections, csv, os, pathlib, re
import matplotlib
matplotlib.use("svg")
import matplotlib.pyplot as plt

ROOT = pathlib.Path(__file__).resolve().parents[2]
RAW, OUT = ROOT / "docs/bench/results/cppprov", ROOT / "docs/img/findings"
# look: same tokens as bench/charts.py (ferrofire blue, stock grey, everywhere)
SURFACE, INK, INK2, MUTED, GRID, AXIS, FF, STK = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7", "#2a78d6", "#898781"
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2, "ytick.color": INK,
                     "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE, "axes.spines.top": False,
                     "axes.spines.right": False, "axes.grid": True, "axes.axisbelow": True, "grid.color": GRID, "grid.linewidth": 0.8,
                     "svg.hashsalt": "ferrofire", "hatch.linewidth": 1.6, "hatch.color": SURFACE})
# pairing: directory, row label, fill, hatch (fill = provider colour; hatch = "the FD is the other kind"), as provider-vs-fd.py
PAIRS = [("cpp-stock", "C++ provider + stock FD", STK, None),
         ("cpp-ferrofire", "C++ provider + ferrofire FD", STK, "///"),
         ("ferrofire-stock", "ferrofire provider + stock FD", FF, "///"),
         ("ferrofire-ferrofire", "ferrofire provider + ferrofire FD", FF, None)]
RUN = re.compile(r"N(\d+)_R10_P([\d.]+)_rep(\d+)\.breaker-(seq|sim)-(\d+)\.cmds\.csv$")
pct = lambda v, p: sorted(v)[min(len(v) - 1, int(p * len(v)))]   # as bench/cppprov/report.py
NOTE = ("Docker bench, one VM, 10 RTUs, 3 reps. Stub model with a 0.05 s step on the C++ side, no solver on either: plumbing only.\n"
        "Stock FD = bennu 6.0.0 (before upstream fix 91fd137; rerun pending). Colour = provider (blue ferrofire, grey C++); hatched = the FD is the other kind.")


def load():
    """lat[dir, N, publish, kind][rep] = [ms write -> visible, done commands]; cnt[dir, N, publish, kind] = [issued, done]. kind: seq | sim<n>."""
    lat, cnt = collections.defaultdict(lambda: collections.defaultdict(list)), collections.defaultdict(lambda: [0, 0])
    for d, *_ in PAIRS:
        for f in sorted((RAW / d).glob("N*.cmds.csv")):
            m = RUN.search(f.name)
            k = (d, int(m[1]), m[2], "seq" if m[4] == "seq" else f"sim{m[5]}")
            for r in csv.DictReader(open(f)):
                cnt[k][0] += 1
                if r["done"] == "1":
                    cnt[k][1] += 1
                    lat[k][int(m[3])].append((int(r["t_visible"]) - int(r["t_cmd"])) / 1e6)
    return lat, cnt


def panels(name, title, sub, xlabel, cols, draw, xmax):
    """One row per pairing, one panel per column of `cols` (key, panel title); draw(ax, y, dir, key, colour, hatch)."""
    fig, axs = plt.subplots(1, len(cols), figsize=(10.4, 3.5), sharey=True)
    for ax, (key, t) in zip(axs, cols):
        for y, (d, _, col, hatch) in enumerate(PAIRS):
            draw(ax, y, d, key, col, hatch)
        ax.set_title(t, fontsize=9, color=INK2, loc="left")
        ax.set_xlim(0, xmax)
        ax.set_ylim(len(PAIRS) - 0.5, -0.5)
        ax.grid(axis="y", visible=False)
        ax.tick_params(length=0)
    axs[0].set_yticks(range(len(PAIRS)), [p[1] for p in PAIRS])
    fig.text(0.012, 0.975, title, ha="left", va="top", fontsize=11, fontweight="bold")
    fig.text(0.012, 0.9, sub, ha="left", va="top", fontsize=8.5, color=INK2)
    fig.text(0.62, 0.2, xlabel, ha="center", va="top", fontsize=8.5, color=INK2)
    fig.text(0.012, 0.012, NOTE, ha="left", va="bottom", fontsize=7.5, color=MUTED)
    fig.subplots_adjust(top=0.76, bottom=0.28, left=0.235, right=0.99, wspace=0.08)
    OUT.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT / f"{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/findings/cpp-provider.py"})
    if os.environ.get("FF_PREVIEW"):  # PNG copy for looking at the chart while editing; not committed (as bench/charts.py)
        fig.savefig(pathlib.Path(os.environ["FF_PREVIEW"]) / f"{name}.png", dpi=110)
    plt.close(fig)


def main():
    lat, cnt = load()
    cells = [(n, p) for n in (100, 1250) for p in ("0.1", "0.05")]
    pool = lambda d, n, p, kind="seq": [v for rep in lat[d, n, p, kind].values() for v in rep]
    p50 = lambda d, n, p: pct(pool(d, n, p), .5)
    reps = lambda d, n, p: [pct(v, .5) for _, v in sorted(lat[d, n, p, "seq"].items())]
    lossp = lambda d, p: 100 * (1 - cnt[d, 1250, p, "sim1000"][1] / cnt[d, 1250, p, "sim1000"][0])
    for d, label, *_ in PAIRS:
        print(label)
        for n, p in cells:
            b = "sim1000" if n == 1250 else "sim100"
            print(f"  N {n:4} publish {p:4} s: seq p50 {p50(d, n, p):.0f} ms, p95 {pct(pool(d, n, p), .95):.0f}, per rep " + "/".join(f"{v:.0f}" for v in reps(d, n, p))
                  + f"; burst {b[3:]}: done {cnt[d, n, p, b][1]}/{cnt[d, n, p, b][0]}")
    CF, FFF = "cpp-ferrofire", "ferrofire-ferrofire"
    ratio = {(n, p): p50(CF, n, p) / p50(FFF, n, p) for n, p in cells}
    print("C++ / ferrofire seq p50, both behind the ferrofire FD: " + "; ".join(f"N {n} publish {p} s x{v:.1f}" for (n, p), v in ratio.items()))
    lo, hi = sorted(ratio[n, "0.05"] for n in (100, 1250))

    def lat_draw(ax, y, d, key, col, hatch):
        v = p50(d, *key)
        ax.barh(y, v, height=0.62, color=col, hatch=hatch, edgecolor=SURFACE, linewidth=0)
        ax.plot(reps(d, *key), [y] * 3, ls="", marker="o", ms=5, color=INK, mec=SURFACE, mew=1)
        ax.text(222, y, f"{v:.0f}", va="center", fontsize=8.5)   # fixed x, right of every rep dot (max 201), so the label reads as the bar's value
    panels("cpp-provider-1-latency",
           f"At a 0.05 s publish ferrofire shows a flip in {p50(FFF, 100, '0.05'):.0f} ms, the C++ provider chain in {p50(CF, 1250, '0.05'):.0f}-{p50(CF, 100, '0.05'):.0f} ms ({lo:.1f}-{hi:.1f}x, indicative only)",
           "Median time from SCADA write to read-back, sequential breaker flips (ms). Bar: 3 reps pooled; dots: each rep's median (the C++ chain is bimodal).",
           "write to read-back, median (ms)", [((n, p), f"{n} points, publish {p} s") for n, p in cells], lat_draw, 285)

    def loss_draw(ax, y, d, key, col, hatch):
        v = lossp(d, key)
        ax.barh(y, v, height=0.62, color=col, hatch=hatch, edgecolor=SURFACE, linewidth=0)
        ax.text(v + 2, y, f"{v:.0f} %  ({cnt[d, 1250, key, 'sim1000'][0] - cnt[d, 1250, key, 'sim1000'][1]} of {cnt[d, 1250, key, 'sim1000'][0]})", va="center", fontsize=8.5)
    panels("cpp-provider-2-burst-loss",
           f"1 000 flips at once: ferrofire loses none; the C++ chain loses {lossp(CF, '0.05'):.0f}-{lossp(CF, '0.1'):.0f} % behind a ferrofire FD",
           "Share of 1 000 simultaneous flips not seen on read-back within 10 s (%), 1 250 points, reps pooled. Stock FD = bennu 6.0.0. 100 at once: none lost.",
           "flips missed (%)", [(p, f"publish {p} s") for p in ("0.1", "0.05")], loss_draw, 135)

    # the one check: the claims the chart titles make hold in the raw data
    assert all(cnt[FFF, n, p, k][0] == cnt[FFF, n, p, k][1] for n, p in cells for k in ("seq", "sim10", "sim100")) and lossp(FFF, "0.1") == lossp(FFF, "0.05") == 0
    assert all(cnt[d, 100, p, "sim100"][0] == cnt[d, 100, p, "sim100"][1] == 300 for d, *_ in PAIRS for p in ("0.1", "0.05"))
    assert 2 < lo <= hi < 3.5 and ratio[1250, "0.1"] < 1.1   # the factor holds at 0.05 s only


if __name__ == "__main__":
    main()
