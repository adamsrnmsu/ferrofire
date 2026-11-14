#!/usr/bin/env python3
"""Field device against field device, no provider difference (fdmicro, ww-1df.41).

    uv run --with matplotlib python bench/findings/field-device.py

Reads docs/bench/results/fdmicro/*.metrics.csv through bench/fdmicro_report.py (the loader of docs/bench/field-device.md, so the
numbers are derived the same way), recomputes every number, prints them, and writes docs/img/findings/field-device-*.svg
(byte-identical on rerun over the same raw files).
"""
import os, pathlib, statistics as st, sys
import matplotlib
matplotlib.use("svg")
import matplotlib.pyplot as plt
from matplotlib.patches import Patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "bench"))
import fdmicro_report as fm  # noqa: E402  (stdlib only)

OUT = ROOT / "docs/img/findings"
# look: same tokens as bench/charts.py (ferrofire blue, stock grey, everywhere)
SURFACE, INK, INK2, MUTED, GRID, AXIS, FF, STK, STK_OLD = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7", "#2a78d6", "#898781", "#c9c8c0"
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2, "ytick.color": INK,
                     "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE, "axes.spines.top": False,
                     "axes.spines.right": False, "axes.grid": True, "axes.axisbelow": True, "grid.color": GRID, "grid.linewidth": 0.8,
                     "svg.hashsalt": "ferrofire"})
N, O = fm.NEW, fm.OLD   # bennu v2026.11.03 (current stock, has upstream 91fd137) and bennu 6.0.0 (historical); ww-1df.47
SK = (("ferrofire", "ferrofire", FF), (N, "bennu v2026.11.03", STK), (O, "bennu 6.0.0 (pre-91fd137)", STK_OLD))
W = 0.28   # spacing of the three series in one group
NOTE = "Docker bench, one VM, one scripted provider feeding all devices the same bytes, 3 reps. v2026.11.03 = current bennu; 6.0.0 = before upstream fix 91fd137.\n"
num = lambda v: f"{v:,.0f}".replace(",", " ")


def frame(fig, name, title, sub, note, **adj):
    fig.text(0.012, 0.975, title, ha="left", va="top", fontsize=11, fontweight="bold")
    fig.text(0.012, 0.9, sub, ha="left", va="top", fontsize=8.5, color=INK2)
    fig.text(0.012, 0.012, note, ha="left", va="bottom", fontsize=7.5, color=MUTED)
    fig.legend(handles=[Patch(color=c, label=l) for _, l, c in SK], loc="upper right", bbox_to_anchor=(0.99, 0.86), ncols=3, frameon=False, fontsize=8.5)
    fig.subplots_adjust(**adj)
    OUT.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT / f"{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/findings/field-device.py"})
    if os.environ.get("FF_PREVIEW"):  # PNG copy for looking at the chart while editing; not committed (as bench/charts.py)
        fig.savefig(pathlib.Path(os.environ["FF_PREVIEW"]) / f"{name}.png", dpi=110)
    plt.close(fig)


def main():
    reps = {fm.reps(s, p, proto).__len__() for s, *_ in SK for p in fm.PROFILES for proto in ("mb", "dnp3")}
    print(f"reps per cell: {sorted(reps)}")

    # 1. delivery of a full-cycle stream (the drop test, 30 s) ---------------------------------------------------------------------
    load = [("M", "100", "M: 800 tags at 100 Hz\n(13 datagrams a cycle)"), ("L", "10", "L: 8 112 tags at 10 Hz\n(128-130 datagrams a cycle)")]
    tot = lambda s, pr, hz, m: fm.total(s, pr, "mb", "drops", f"{hz}hz", m)
    lost = {(s, pr): 100 * (1 - tot(s, pr, hz, "udp_in") / tot(s, pr, hz, "udp_sent")) for s, *_ in SK for pr, hz, _ in load}
    stale = {(s, pr): 100 * tot(s, pr, hz, "end_state_bad_regs") / tot(s, pr, hz, "end_state_checked") for s, *_ in SK for pr, hz, _ in load}
    per_rep = sorted(100 * (1 - i / n) for i, n in zip(fm.vals("ferrofire", "L", "mb", "drops", "10hz", "udp_in"), fm.vals("ferrofire", "L", "mb", "drops", "10hz", "udp_sent")))
    for s, label, _ in SK:
        for pr, hz, _ in load:
            print(f"stream {pr} {label}: sent {num(tot(s, pr, hz, 'udp_sent'))} reached {num(tot(s, pr, hz, 'udp_in'))} lost {lost[s, pr]:.1f} %  RcvbufErrors {num(tot(s, pr, hz, 'rcvbuf_errors'))}  registers wrong at the end {stale[s, pr]:.1f} %")
    print("ferrofire L lost % per rep " + "/".join(f"{v:.1f}" for v in per_rep))
    fig, ax = plt.subplots(figsize=(10.4, 3.3))
    for j, (pr, hz, _) in enumerate(load):
        for i, (s, label, col) in enumerate(SK):
            y = j + (i - 1) * W
            ax.barh(y, lost[s, pr], height=0.25, color=col)
            ax.text(lost[s, pr] + 0.5, y, f"{lost[s, pr]:.1f} %" + (f"   ({num(tot(s, pr, hz, 'udp_sent') - tot(s, pr, hz, 'udp_in'))} of {num(tot(s, pr, hz, 'udp_sent'))} datagrams, {stale[s, pr]:.1f} % of registers stale at the end)" if lost[s, pr] else ""), va="center", fontsize=8.5)
    ax.set_yticks(range(len(load)), [l for *_, l in load])
    ax.set_ylim(len(load) - 0.5, -0.5)
    ax.set_xlim(0, 60)
    ax.set_xlabel("datagrams sent that never reached the device (%)")
    ax.grid(axis="y", visible=False)
    ax.tick_params(length=0)
    frame(fig, "field-device-1-stream-loss",
          f"Stock wins: ferrofire's field device loses {lost['ferrofire', 'L']:.1f} % of an 8 112-tag stream, bennu loses none (either version)",
          "Share of a full-cycle stream the kernel dropped at the device's input socket (30 s, all reps pooled).",
          NOTE + f"Per rep: {per_rep[0]:.1f}-{per_rep[-1]:.1f} %. The receive buffer overflows on a burst of 130 datagrams (ww-c2i, open).", top=0.74, bottom=0.29, left=0.2, right=0.99)

    # 2. Modbus reads, profile M, FC4 of 1 register ----------------------------------------------------------------------------------
    cl = (1, 4, 16)
    g = lambda s, c, m: fm.med(s, "M", "mb", "read", f"c{c}_n1", m)
    rps = {(s, c): g(s, c, "req_per_s") for s, *_ in SK for c in cl}
    eff = {(s, c): g(s, c, "req_per_s") / g(s, c, "cpu_pct") * 100 for s, *_ in SK for c in cl}
    for c in cl:
        print(f"modbus M FC4x1 {c:2} clients: req/s ferrofire {num(rps['ferrofire', c])} stock {num(rps[N, c])}; cpu % {g('ferrofire', c, 'cpu_pct'):.0f} / {g(N, c, 'cpu_pct'):.0f}; "
              f"req per cpu-s {num(eff['ferrofire', c])} / {num(eff[N, c])} (stock x{eff[N, c] / eff['ferrofire', c]:.1f}); p99 us {g('ferrofire', c, 'us_p99'):.0f} / {g(N, c, 'us_p99'):.0f}")
    fig, axs = plt.subplots(1, 2, figsize=(10.4, 3.6))
    for ax, d, t in ((axs[0], rps, "requests per second"), (axs[1], eff, "requests per CPU-second of the device")):
        for i, (s, _, col) in enumerate(SK):
            for x, c in enumerate(cl):
                ax.bar(x + (i - 1) * W, d[s, c] / 1e3, width=0.25, color=col)
                ax.text(x + (i - 1) * W, d[s, c] / 1e3, f"{d[s, c] / 1e3:.0f}k", ha="center", va="bottom", fontsize=8.5)
        ax.set_xticks(range(len(cl)), [f"{c} client{'s' if c > 1 else ''}" for c in cl])
        ax.set_title(t + " (thousands)", fontsize=9, color=INK2, loc="left")
        ax.set_ylim(0, max(d.values()) / 1e3 * 1.15)
        ax.grid(axis="x", visible=False)
        ax.tick_params(length=0)
    frame(fig, "field-device-2-modbus-cpu",
          f"Stock wins per CPU: {eff[N, 16] / eff['ferrofire', 16]:.1f}x the Modbus requests per CPU-second at 16 clients; ferrofire serves {100 * (rps['ferrofire', 16] / rps[N, 16] - 1):.0f} % more",
          "Tight read loops of one input register (FC4), 800-tag device, median of 3 reps.",
          NOTE + f"Stock serves every client from one thread (about 100 % of a core); ferrofire used {g('ferrofire', 16, 'cpu_pct') / 100:.1f} cores at 16 clients.",
          top=0.72, bottom=0.2, left=0.045, right=0.99, wspace=0.12)

    # 3. DNP3 class-0 poll ---------------------------------------------------------------------------------------------------------------
    pr3 = list(fm.PROFILES)
    poll = {(s, pr): sorted(fm.vals(s, pr, "dnp3", "dnp3read", f"{fm.MAIN_HZ[pr]}hz", "ms_p50")) for s, *_ in SK for pr in pr3}
    med = {k: st.median(v) for k, v in poll.items()}
    for pr in pr3:
        print(f"dnp3 poll p50 ms {pr}: ferrofire {med['ferrofire', pr]:.3f} (reps {'/'.join(f'{v:.3f}' for v in poll['ferrofire', pr])})  stock {med[N, pr]:.3f} (reps {'/'.join(f'{v:.3f}' for v in poll[N, pr])})"
              f"  response {fm.med(N, pr, 'dnp3', 'dnp3read', f'{fm.MAIN_HZ[pr]}hz', 'response_bytes'):.0f} B")
    fig, ax = plt.subplots(figsize=(10.4, 3.5))
    for j, pr in enumerate(pr3):
        for i, (s, _, col) in enumerate(SK):
            y = j + (i - 1) * W
            ax.plot(poll[s, pr], [y] * len(poll[s, pr]), ls="", marker="o", ms=5, color=col, alpha=0.45, mec="none")
            ax.plot(med[s, pr], y, ls="", marker="o", ms=9, color=col, mec=SURFACE, mew=2)
            far = max(poll[s, pr]) > 20 * med[s, pr]   # one rep far from the other two: label the median where it is, and name the outlier
            ax.text((med[s, pr] if far else max(poll[s, pr])) * 1.35, y, (f"{med[s, pr]:.2f} ms" if med[s, pr] < 10 else f"{med[s, pr]:.0f} ms") + (f"  (one rep {num(max(poll[s, pr]))} ms)" if far else ""), va="center", fontsize=8.5)
    ax.set_xscale("log")
    ax.set_xlim(0.08, 6000)
    ax.set_xticks([0.1, 1, 10, 100, 1000], ["0.1", "1", "10", "100", "1 000"])
    ax.set_yticks(range(len(pr3)), [f"{pr}: {num(fm.PROFILES[pr])} tags" for pr in pr3])
    ax.set_ylim(len(pr3) - 0.5, -0.5)
    ax.set_xlabel("class-0 poll time, p50 (ms, log scale)")
    ax.grid(axis="y", visible=False)
    ax.tick_params(length=0, which="both")
    frame(fig, "field-device-3-dnp3-poll",
          f"ferrofire wins on DNP3 from 800 tags up: a poll takes {med['ferrofire', 'M']:.1f} ms against {med[N, 'M']:.0f} ms on bennu (both versions)",
          "Time of one class-0 integrity poll (300 polls, 20 ms apart). Large dot: median of 3 reps; small dots: each rep.",
          NOTE + "Bennu's 123 ms on M and its stalled polls on L were observed, not traced; the same in v2026.11.03 as in 6.0.0, so not changed by upstream fix 91fd137.",
          top=0.74, bottom=0.27, left=0.11, right=0.99)

    quick = {s: sorted(100 - v for pr in pr3 for v in fm.vals(s, pr, "mb", "write", "rapid", "lost")) for s, *_ in SK}   # not charted: of 100 writes 60-150 ms apart (6.0.0 forwarded about a third; 91fd137 fixed it)
    print("quick writes forwarded of 100, range over 9 runs: " + "; ".join(f"{l} {quick[s][0]:.0f}-{quick[s][-1]:.0f}" for s, l, _ in SK))

    # the one check: the claims the chart titles make hold in the raw data
    assert lost[N, "L"] == 0 == lost[O, "L"] and lost["ferrofire", "M"] == 0 and 20 < lost["ferrofire", "L"] < 30
    assert eff[N, 16] > 4 * eff["ferrofire", 16] and rps["ferrofire", 16] > rps[N, 16] and eff["ferrofire", 1] > eff[N, 1]
    assert med[O, "S"] < med["ferrofire", "S"] < med[N, "S"] and med[N, "M"] > 100 * med["ferrofire", "M"] and med[O, "M"] > 100 * med["ferrofire", "M"]
    assert quick["ferrofire"][0] == 100 == quick[N][0] and quick[O][-1] < 40   # quick writes: v2026.11.03 forwards all of them, 6.0.0 about a third


if __name__ == "__main__":
    main()
