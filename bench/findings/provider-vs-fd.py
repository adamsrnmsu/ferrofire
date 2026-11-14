#!/usr/bin/env python3
"""Provider or field device: which one is the bottleneck (2x2 cross bench, ww-1df.40).

    uv run --with matplotlib python bench/findings/provider-vs-fd.py

Reads docs/bench/results/control/cross-*/R*_rep*.{cmds,cells}.csv + env.txt (never *.part*), recomputes every number (bennu v2026.11.03 = the current stock, dirs `-v2026.11.03`; bennu 6.0.0 = the older runs, ww-1df.47),
prints them, and writes docs/img/findings/provider-vs-fd-*.svg (byte-identical on rerun over the same raw files).
"""
import collections, csv, os, pathlib, re, statistics as st
import matplotlib
matplotlib.use("svg")
import matplotlib.pyplot as plt

ROOT = pathlib.Path(__file__).resolve().parents[2]
RAW, OUT = ROOT / "docs/bench/results/control", ROOT / "docs/img/findings"
# look: same tokens as bench/charts.py (ferrofire blue, stock grey, everywhere)
SURFACE, INK, INK2, MUTED, GRID, AXIS, FF, STK, STK2 = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7", "#2a78d6", "#898781", "#c3c2b7"
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2, "ytick.color": INK,
                     "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE, "axes.spines.top": False,
                     "axes.spines.right": False, "axes.grid": True, "axes.axisbelow": True, "grid.color": GRID, "grid.linewidth": 0.8,
                     "svg.hashsalt": "ferrofire", "hatch.linewidth": 1.6, "hatch.color": SURFACE})
# pairing: directory, row label, fill, hatch (fill = provider colour; hatch = "the two halves differ")
PAIRS = [("cross-ferrofire-ferrofire", "ferrofire provider + ferrofire FD", FF, None),
         ("cross-ferrofire-stock-v2026.11.03", "ferrofire provider + bennu v2026.11.03 FD", FF, "///"),
         ("cross-stock-stock-v2026.11.03-0.1s", "bennu v2026.11.03 provider (0.1 s) + its FD", STK2, "///"),
         ("cross-stock-stock-v2026.11.03", "bennu v2026.11.03 provider (1 s) + its FD", STK, "///"),
         ("cross-stock-ferrofire", "bennu provider (1 s) + ferrofire FD", STK, "xxx"),
         ("cross-ferrofire-stock", "ferrofire provider + bennu 6.0.0 FD", FF, "\\\\\\"),
         ("cross-stock-stock-0.1s", "bennu 6.0.0 provider (0.1 s) + its FD", STK2, "\\\\\\"),
         ("cross-stock-stock", "bennu 6.0.0 provider (1 s) + its FD", STK, "\\\\\\")]
RS = (1, 2, 5, 10)
RUN = re.compile(r"R(\d+)_rep(\d+)\.(cmds|cells)\.csv$")   # excludes R5_rep3.part0.* (a run in flight)


def load():
    """lat[dir][R, cell] = [ms write -> visible on read-back, completed commands]; tot[dir][R, cell] = summed cells.csv columns + rep set; snapshot time."""
    lat, tot, ends = collections.defaultdict(lambda: collections.defaultdict(list)), collections.defaultdict(dict), []
    for d, *_ in PAIRS:
        for f in sorted((RAW / d).glob("R*_rep*.c*.csv")):
            m = RUN.search(f.name)
            if not m:
                continue
            rows = list(csv.DictReader(open(f)))
            if m[3] == "cmds":
                for r in rows:
                    if r["done"] == "1" and int(r["t_visible"]):
                        lat[d][int(r["R"]), r["cell"]].append((int(r["t_visible"]) - int(r["t_cmd"])) / 1e6)
                continue
            ends += re.findall(r"(?m)^start \S+ end (\S+)", (f.parent / f.name.replace(".cells.csv", ".env.txt")).read_text())
            for r in rows:
                c = tot[d].setdefault((int(r["R"]), r["cell"]), collections.Counter(reps=set()))
                c["reps"].add(r["rep"])
                for k in ("issued", "ok", "lost", "flaps", "wrong_final", "wrong_provider", "wrong_collateral", "write_errors"):
                    c[k] += int(r[k])
    return lat, tot, max(ends)


def panels(name, title, sub, xlabel, value, text, xmax, note):
    """One row of bars per pairing, one panel per RTU count; value(d, R) -> number or None, text(d, R) -> bar-end label."""
    fig, axs = plt.subplots(1, len(RS), figsize=(10.4, 4.9), sharey=True)
    ys = range(len(PAIRS))
    for ax, R in zip(axs, RS):
        for y, (d, _, col, hatch) in zip(ys, PAIRS):
            v = value(d, R)
            if v is None:
                ax.text(xmax * 0.03, y, "cannot run", va="center", fontsize=8, color=MUTED, style="italic")
                continue
            ax.barh(y, v, height=0.62, color=col, hatch=hatch, edgecolor=SURFACE, linewidth=0)
            ax.text(v + xmax * 0.03, y, text(d, R), va="center", fontsize=8.5, color=INK)
        ax.set_title(f"{R} RTU{'s' if R > 1 else ''}", fontsize=9, color=INK2, loc="left")
        ax.set_xlim(0, xmax * 1.42)
        ax.set_ylim(len(PAIRS) - 0.5, -0.5)
        ax.grid(axis="y", visible=False)
        ax.tick_params(length=0)
    axs[0].set_yticks(list(ys), [p[1] for p in PAIRS])
    fig.text(0.012, 0.975, title, ha="left", va="top", fontsize=11, fontweight="bold")
    fig.text(0.012, 0.92, sub, ha="left", va="top", fontsize=8.5, color=INK2)
    fig.text(0.62, 0.16, xlabel, ha="center", va="top", fontsize=8.5, color=INK2)
    fig.text(0.012, 0.012, note, ha="left", va="bottom", fontsize=7.5, color=MUTED)
    fig.subplots_adjust(top=0.82, bottom=0.25, left=0.28, right=0.99, wspace=0.08)
    OUT.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT / f"{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/findings/provider-vs-fd.py"})
    if os.environ.get("FF_PREVIEW"):  # PNG copy for looking at the chart while editing; not committed (as bench/charts.py)
        fig.savefig(pathlib.Path(os.environ["FF_PREVIEW"]) / f"{name}.png", dpi=110)
    plt.close(fig)


def main():
    lat, tot, snap = load()
    have = [d for d, *_ in PAIRS if tot[d]]
    reps = lambda d, cell: "/".join(str(len(tot[d][R, cell]["reps"])) if (R, cell) in tot[d] else "0" for R in RS)
    p50 = lambda d, R, cell="breaker:seq:100": st.median(lat[d][R, cell]) if lat[d][R, cell] else None
    lossp = lambda d, R, cell: 100 * tot[d][R, cell]["lost"] / tot[d][R, cell]["issued"] if (R, cell) in tot[d] else None
    frac = lambda d, R, cell: f"{tot[d][R, cell]['lost']}/{tot[d][R, cell]['issued']}"
    S, M, F = "cross-stock-stock-v2026.11.03", "cross-ferrofire-stock-v2026.11.03", "cross-ferrofire-ferrofire"   # bennu v2026.11.03, the current stock
    S6, M6 = "cross-stock-stock", "cross-ferrofire-stock"                                                       # bennu 6.0.0 (pre-91fd137), the historical one
    share = {R: 100 * (p50(S, R) - p50(M, R)) / (p50(S, R) - p50(F, R)) for R in RS}   # part of the full gain that the provider swap alone gives
    share6 = {R: 100 * (p50(S6, R) - p50(M6, R)) / (p50(S6, R) - p50(F, R)) for R in RS}

    print(f"snapshot {snap}  pairings present {len(have)}/{len(PAIRS)}: {', '.join(have)}")
    for d, label, *_ in PAIRS:
        if d not in have:
            print(f"\n{label}: NO DATA ({d}/ has no cells.csv: the ferrofire FD rejects stock's datagrams, control-latency.md section 9)")
            continue
        print(f"\n{label}   [{d}]")
        for cell in ("breaker:seq:100", "breaker:sim:10", "breaker:sim:50", "breaker:sim:100", "breaker:sim:1000", "gen:seq:5", "gen:sim:5"):
            cs = [tot[d].get((R, cell)) for R in RS]
            print(f"  {cell:17} reps {reps(d, cell):8} p50 ms " + "/".join(f"{p50(d, R, cell):.0f}" if p50(d, R, cell) else "-" for R in RS)
                  + "   lost " + " ".join(frac(d, R, cell) if c else "-" for R, c in zip(RS, cs))
                  + f"   flaps {sum(c['flaps'] for c in cs if c)}   wrong/write-err {sum(c['wrong_final'] + c['wrong_provider'] + c['wrong_collateral'] + c['write_errors'] for c in cs if c)}")
    print("\nshare of the full sequential gain from the provider swap alone, % by RTUs: v2026.11.03 " + "/".join(f"{share[R]:.0f}" for R in RS) + "; 6.0.0 " + "/".join(f"{share6[R]:.0f}" for R in RS))
    print("flips completed per RTU, 1000 at once: " + "; ".join(f"{d} " + "/".join(f"{tot[d][R, 'breaker:sim:1000']['ok'] / len(tot[d][R, 'breaker:sim:1000']['reps']) / R:.0f}" for R in RS) for d in have))
    gen = collections.Counter()
    for d in have:
        for (R, cell), c in tot[d].items():
            gen[d, cell.split(":")[0]] += c["flaps"]
            gen[d, cell.split(":")[0], "n"] += c["issued"]
    print("flaps: " + "; ".join(f"{d} breaker {gen[d, 'breaker']}/{gen[d, 'breaker', 'n']} gen {gen[d, 'gen']}/{gen[d, 'gen', 'n']}" for d in have))

    foot = lambda cell: (f"Raw data to {snap}. v2026.11.03 = current bennu; 6.0.0 (backslash hatch) = before upstream fix 91fd137.\nBennu provider + ferrofire FD cannot run: its datagrams lack the NUL the FD requires (ww-y87). Docker bench, one VM. FD = field device.\nColour = provider (blue ferrofire, grey bennu); hatched = the FD is the other kind. 3 reps per cell (1 for 1 000 flips behind a bennu provider)."
                         )
    sh = {M: share, M6: share6}
    ms = lambda d, R: f"{p50(d, R):.0f}" + (f"  ({sh[d][R]:.0f} % of gain)" if d in sh else "")
    panels("provider-vs-fd-1-latency",
           f"Swapping only the provider cuts a flip from {p50(S, 1) / 1e3:.1f} s to {p50(M, 1) / 1e3:.2f} s on 1 RTU (6.0.0 field device: {p50(M6, 1) / 1e3:.2f} s)",
           "Median time from SCADA write to read-back, 100 sequential breaker flips (ms). In brackets: share of the full ferrofire gain.",
           "write to read-back, median (ms)", p50, ms, max(p50(S, R) for R in RS), foot("breaker:seq:100"))
    cell = "breaker:sim:100"
    panels("provider-vs-fd-2-burst-loss",
           f"Burst loss was the 6.0.0 field device: {lossp(M6, 1, cell):.0f} % of 100 flips missed, none with v2026.11.03 ({lossp(M, 1, cell):.0f} %)",
           "Share of 100 simultaneous breaker flips never seen on read-back (%), all reps pooled.",
           "flips missed (%)", lambda d, R: lossp(d, R, cell), lambda d, R: f"{lossp(d, R, cell):.0f} %", 100, foot(cell))

    # the one check: the claims the chart titles make hold in the raw data
    assert all(c["lost"] == 0 for c in tot[F].values()), "ferrofire+ferrofire lost a command"
    assert abs(p50(M, 1) - p50(F, 1)) < 5 and p50(F, 1) < p50(S, 1) and abs(p50(M, 10) - p50(F, 10)) < 10   # v2026.11.03 FD: the 1-RTU floor is gone
    assert p50(M6, 1) > 5 * p50(F, 1) and abs(lossp(M6, 1, cell) - lossp(S6, 1, cell)) < 5            # 6.0.0: floor and burst loss follow the field device
    assert all(lossp(d, R, cell) == 0 for d in (M, S, "cross-stock-stock-v2026.11.03-0.1s") for R in RS)


if __name__ == "__main__":
    main()
