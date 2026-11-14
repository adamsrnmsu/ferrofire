#!/usr/bin/env python3
"""Findings, theme "CPU: where it goes and which optimizations pay" (ww-1df.34 profiling pass).

    uv run --with matplotlib python bench/findings/cpu-profiling.py

Raw data lives OUTSIDE the repo, in /var/tmp/ff-prof (volatile). When that directory is there, the five files this script
needs are copied verbatim into bench/findings/cpu-profiling-raw/; the numbers are always computed from those copies, so
the script still runs after /var/tmp is cleaned. Writes docs/img/findings/cpu-profiling-{1-rate,2-prototypes,3-hotspots}.svg
and prints every recomputed number as JSON.
"""
import collections, json, os, pathlib, re, shutil, statistics
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = pathlib.Path(__file__).resolve().parents[2]
SRC, RAW, OUT = pathlib.Path("/var/tmp/ff-prof"), ROOT / "bench/findings/cpu-profiling-raw", ROOT / "docs/img/findings"
FILES = {"w6-sweep.log": "w6-sweep.log", "m/campaign.log": "campaign.log", "w1a-grid-idle/prov.dem.folded": "w1a-grid-idle.folded",
         "w1b2-grid-storm-fast/prov.dem.folded": "w1b2-grid-storm.folded", "w2b-big8112-10hz/prov.dem.folded": "w2b-big8112.folded"}
RAW.mkdir(parents=True, exist_ok=True)
for src, dst in FILES.items():
    if (SRC / src).exists() and not (RAW / dst).exists():
        shutil.copyfile(SRC / src, RAW / dst)

# Same surface, inks and series colours as bench/charts.py: ferrofire blue everywhere, stock bennu grey (not drawn here).
SURFACE, INK, INK2, MUTED, GRID, AXIS = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7"
FF, FF_LIGHT, REST = "#2a78d6", "#9ec5f4", "#e1e0d9"
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2,
                     "ytick.color": INK2, "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE,
                     "axes.spines.top": False, "axes.spines.right": False, "axes.grid": True, "axes.axisbelow": True, "grid.color": GRID,
                     "grid.linewidth": 0.8, "svg.hashsalt": "ferrofire"})


def save(fig, name, title, note):
    fig.text(0.012, 0.975, title, ha="left", va="top", fontsize=10.5, fontweight="bold", color=INK)
    fig.text(0.012, 0.015, note, ha="left", va="bottom", fontsize=7.5, color=MUTED)
    OUT.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT / f"cpu-profiling-{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/findings/cpu-profiling.py"})
    if os.environ.get("FF_PREVIEW"):  # PNG copies for looking at the charts while editing; not committed
        fig.savefig(pathlib.Path(os.environ["FF_PREVIEW"]) / f"cpu-profiling-{name}.png", dpi=110)
    plt.close(fig)


NUM = {}

# --- 1. rate sweep ---------------------------------------------------------------------------------------------------------
# w6-sweep.log has one unlabelled line per run; the order is the loop order of bench/profile/w6.sh.
cpu = [float(x) for x in re.findall(r"cpu_pct_of_one_core=([\d.]+)", (RAW / "w6-sweep.log").read_text())]
assert len(cpu) == 16, cpu
DT = [0.05, 0.02, 0.01, 0.005]
sweep = {"idle": cpu[0:8:2], "storm": cpu[1:8:2], "big8112": cpu[8:12], "m800": cpu[12:16]}


def fit(ys):  # least squares CPU % = a + b * steps/s; dt where it reaches 100 % of a core
    xs = [1 / d for d in DT]
    b, a = statistics.linear_regression(xs, ys)[0], statistics.linear_regression(xs, ys)[1]
    return a, b, 1 / ((100 - a) / b)


NUM["sweep"] = {k: v for k, v in sweep.items()}
NUM["sweep_fit"] = {k: dict(zip(("intercept_pct", "pct_per_step_per_s", "dt_at_one_core_s"), (round(x, 4) for x in fit(sweep[k])))) for k in ("idle", "storm")}

fig, ax = plt.subplots(figsize=(7.4, 3.9))
xs = [1 / d for d in DT]
for k, lab, ls, mfc in (("storm", "1000-breaker storm", (0, (4, 2)), SURFACE), ("idle", "idle grid", "-", FF)):
    a, b, dsat = fit(sweep[k])
    ax.plot([xs[-1], 1 / dsat], [a + b * xs[-1], 100], color=FF, lw=1, ls=(0, (1, 3)))
    ax.plot(xs, sweep[k], color=FF, lw=2, ls=ls, marker="o", ms=6, mfc=mfc, mec=FF, mew=1.5, zorder=3)
    for x, y in zip(xs, sweep[k]):
        ax.annotate(f"{y:.0f}", (x, y), textcoords="offset points", xytext=(0, 8 if k == "storm" else -14), ha="center", fontsize=8, color=INK2)
    ax.annotate(lab, (xs[-1], sweep[k][-1]), textcoords="offset points", xytext=(10, -2 if k == "storm" else -10), fontsize=8.5, color=INK, va="center")
ax.axhline(100, color=AXIS, lw=1)
ax.text(12, 101.5, "one core", fontsize=8, color=MUTED, va="bottom")
i, s = fit(sweep["idle"])[2], fit(sweep["storm"])[2]
ax.text(505, 30, f"straight-line fit reaches one core\nat dt {s:.4f} s (storm) to {i:.4f} s (idle)", fontsize=8, color=INK2, ha="right", va="center")
ax.set_xticks(xs + [300, 400, 500], [f"{x:.0f}\n{d:g}" for x, d in zip(xs, DT)] + ["300", "400", "500"])
ax.text(215, -17.5, "\u2190 dt, s", fontsize=9, color=INK2, va="center")
ax.set_xlim(0, 520)
ax.set_ylim(0, 112)
ax.set_xlabel("simulation steps per second (1 / dt)")
ax.set_ylabel("provider CPU, % of one core")
ax.grid(axis="x", visible=False)
fig.subplots_adjust(top=0.86, bottom=0.27, left=0.085, right=0.985)
save(fig, "1-rate", f"ferrofire provider: {sweep['idle'][0]:.0f} % of a core at dt 0.05 s, {sweep['idle'][-1]:.0f} % at dt 0.005 s",
     "1250-branch grid, 10 field devices, one 25 s window per point, Docker Desktop VM (absolute CPU noisy about 2x).\n"
     "Publish interval shrinks with dt (0.1, 0.05, 0.02, 0.01 s). Dotted: extrapolation, not measured. Raw: /var/tmp/ff-prof/w6-sweep.log.")

# --- 2. prototypes ---------------------------------------------------------------------------------------------------------
blocks = collections.defaultdict(list)  # "workload idea" -> CPU readings, alternating base, variant
key = None
for l in (RAW / "campaign.log").read_text().splitlines():
    if l.startswith("####"):
        key = l[5:]
    elif (m := re.search(r"^\s*(base|E)? ?(\S+)\s+cpu=([\d.]+)", l)) and (key != "query E" or m[2] == "query"):
        blocks[key].append(float(m[3]))
E_extra = [(float(a), float(b)) for a, b in re.findall(r"query cpu=[\d.]+ rate=(\d+)/s p50=([\d.]+)us", (RAW / "campaign.log").read_text())]
IDEAS = [  # (block, idea, what, workload, verdict from docs/profiling.md)
    ("query E", "E", "cache the QUERY reply", "QUERY load, 800 tags", "build"),
    ("grid-storm B", "B", "one power sync per write batch", "1000-flip storm, dt 0.05 s", "build"),
    ("gridfast-idle C", "C", "per-target index lists", "idle grid, dt 0.005 s", "build"),
    ("gridfast-idle D", "D", "FNV hash in the top store", "idle grid, dt 0.005 s", "skip"),
    ("big A", "A", "skip redundant evals", "8112 tags", "marginal"),
    ("grid-storm F", "F", "bounded LU row update", "1000-flip storm, dt 0.05 s", "skip"),
]
rows = []
for blk, idea, what, wl, verdict in IDEAS:
    base, var = blocks[blk][0::2], blocks[blk][1::2]
    assert len(base) == len(var) == 3, (blk, blocks[blk])
    rows.append(dict(idea=idea, what=what, workload=wl, verdict=verdict, base=base, variant=var, base_med=statistics.median(base), var_med=statistics.median(var),
                     pairs_pct=[round(100 * (v / b - 1), 1) for b, v in zip(base, var)], change_pct=round(100 * (statistics.median(var) / statistics.median(base) - 1), 1)))
NUM["prototypes"] = rows
NUM["other_blocks"] = {k: dict(base=v[0::2], variant=v[1::2]) for k, v in blocks.items() if k not in [r[0] for r in IDEAS]}
NUM["E_query"] = dict(base_rate_per_s=statistics.median(r for r, _ in E_extra[0::2]), E_rate_per_s=statistics.median(r for r, _ in E_extra[1::2]),
                      base_p50_us=statistics.median(p for _, p in E_extra[0::2]), E_p50_us=statistics.median(p for _, p in E_extra[1::2]))

fig, ax = plt.subplots(figsize=(7.4, 3.9))
for y, r in enumerate(rows):
    ax.barh(y, r["change_pct"], height=0.5, color=FF if r["verdict"] == "build" else FF_LIGHT, ec=SURFACE)
    ax.scatter(r["pairs_pct"], [y] * 3, s=16, color=INK, zorder=3, lw=0)
    ax.text(-97, y - 0.13, f"{r['idea']}  {r['what']}", fontsize=9, color=INK, va="center", fontweight="bold" if r["verdict"] == "build" else "normal")
    ax.text(-97, y + 0.22, r["workload"], fontsize=7.5, color=MUTED, va="center")
    ax.text(4, y, f"{r['change_pct']:+.0f} %".replace("-", "−") + f"   {r['base_med']:.1f} → {r['var_med']:.1f} % of a core   {r['verdict']}", fontsize=8.5, color=INK2, va="center")
ax.axvline(0, color=AXIS, lw=1)
ax.set_ylim(len(rows) - 0.4, -0.6)
ax.set_xlim(-98, 62)
ax.set_xticks([-40, -30, -20, -10, 0], ["−40", "−30", "−20", "−10", "0"])
ax.set_yticks([])
ax.spines["left"].set_visible(False)
ax.grid(axis="y", visible=False)
ax.set_xlabel("change in provider CPU with the prototype, %", x=0.475)
fig.subplots_adjust(top=0.88, bottom=0.25, left=0.012, right=0.988)
b3 = [r for r in rows if r["verdict"] == "build"]
save(fig, "2-prototypes", "Three of six prototypes pay: " + ", ".join(f"{r['idea']} {r['change_pct']:+.0f} %".replace("-", "−") for r in b3) + " provider CPU",
     "Bar: median of 3 runs with the prototype against the median of 3 runs without, alternated in one block. Dots: each pair.\n"
     "Docker Desktop VM; only pairs inside one block are comparable. Verdicts from docs/profiling.md. Raw: /var/tmp/ff-prof/m/campaign.log.")

# --- 3. hotspots -----------------------------------------------------------------------------------------------------------
BUCKETS = [  # first match on the stack wins, so the shares of one workload add up to 100 %
    ("power-flow solve (dense LU)", "ferrofire::power::pf::"),
    ("Power::get, a full sync per write", "<ferrofire::power::Power>::get"),
    ("expression eval (Node::eval)", "<ferrofire::expr::Node>::eval"),
    ("publish bookkeeping (publish_all)", "ferrofire::bennu::publish_all"),
]
WL = [("w1a-grid-idle", "idle grid\n1250 branches"), ("w1b2-grid-storm", "1000-breaker\nstorm"), ("w2b-big8112", "8112-tag model\n")]
SHORT = {BUCKETS[0][0]: "power-flow solve\n(dense LU)", BUCKETS[1][0]: "Power::get\n(a sync per write)", BUCKETS[2][0]: "expression eval\n(Node::eval)", BUCKETS[3][0]: "publish bookkeeping\n(publish_all)"}
hot = {}
for w, _ in WL:
    share, self_, tot, T = collections.Counter(), collections.Counter(), collections.Counter(), 0
    for l in (RAW / f"{w}.folded").read_text().splitlines():
        st, _, c = l.rpartition(" ")
        c, fr = int(c), st.split(";")[1:]
        T += c
        self_[fr[-1]] += c
        for x in set(fr):
            tot[x] += c
        share[next((b for b, pat in BUCKETS if any(pat in x for x in fr)), "everything else")] += c
    pick = lambda cnt, pat: round(100 * sum(c for s, c in cnt.items() if s.startswith(pat)) / T, 1)
    hot[w] = dict(buckets={b: round(100 * c / T, 1) for b, c in share.items()},
                  self_pct={"SipHash write": pick(self_, "<core::hash::sip::Hasher"), "Power::get": pick(self_, "<ferrofire::power::Power>::get"),
                            "lu_solve": pick(self_, "ferrofire::power::pf::lu_solve"), "Node::eval": pick(self_, "<ferrofire::expr::Node>::eval"),
                            "top::publish": pick(self_, "ferrofire::top::publish")},
                  total_pct={"publish_all": pick(tot, "ferrofire::bennu::publish_all"), "top::publish": pick(tot, "ferrofire::top::publish")})
NUM["hotspots"] = hot

fig, ax = plt.subplots(figsize=(7.4, 3.3))
for y, (w, lab) in enumerate(WL):
    x, top = 0, sorted(((v, b) for b, v in hot[w]["buckets"].items() if b != "everything else" and v >= 20), reverse=True)
    for n, (v, b) in enumerate(top):
        ax.barh(y, v - 0.4, left=x + 0.2, height=0.62, color=FF if n == 0 else FF_LIGHT, ec="none")
        ax.text(x + v / 2, y, f"{v:.0f} %  {SHORT[b]}", ha="center", va="center", fontsize=8, color="#ffffff" if n == 0 else INK, linespacing=1.25)
        x += v
    ax.barh(y, 100 - x - 0.2, left=x + 0.2, height=0.62, color=REST, ec="none")
    ax.text(x + (100 - x) / 2, y, f"{100 - x:.0f} %  everything else", ha="center", va="center", fontsize=8, color=INK2, linespacing=1.25)
ax.set_yticks(range(len(WL)), [l for _, l in WL], fontsize=9, color=INK)
ax.tick_params(axis="y", length=0)
ax.set_ylim(len(WL) - 0.5, -0.5)
ax.set_xlim(0, 100)
ax.set_xlabel("share of provider CPU samples, %")
ax.spines["left"].set_visible(False)
ax.grid(visible=False)
fig.subplots_adjust(top=0.86, bottom=0.27, left=0.15, right=0.985)
hi, hs, hb = (hot[w]["buckets"] for w, _ in WL)
save(fig, "3-hotspots", f"A different hotspot per workload: publish {hi[BUCKETS[3][0]]:.0f} %, power {hs[BUCKETS[1][0]] + hs[BUCKETS[0][0]]:.0f} %, eval {hb[BUCKETS[2][0]]:.0f} %",
     "perf cpu-clock samples at 499 Hz, 46 s, one run per workload; a sample counts for the first named frame on its stack.\n"
     "A share overstates the gain: C removed most of publish_all and saved 19 %. Raw: /var/tmp/ff-prof/w*/prov.dem.folded.")

# --- context: stock bennu provider idle CPU (different setup, not drawn) ---------------------------------------------------
cap = [dict(re.findall(r"(\w+) ([\d.]+)", l)) for l in (ROOT / "docs/bench/results/control/stock-capacity.txt").read_text().splitlines() if l.startswith("interval ")]
NUM["stock_idle_cpu_pct_by_interval_s"] = {c["interval"]: float(c["cpu_pct"]) for c in cap}

# self-check against the values docs/profiling.md states (fails if the parsing drifts)
assert sweep["idle"] == [10.9, 19.4, 31.2, 48.7] and sweep["storm"] == [17.4, 23.8, 41.3, 59.6]
assert [r["idea"] for r in rows if r["change_pct"] < -15] == ["E", "B", "C"]
assert hot["w2b-big8112"]["self_pct"]["Node::eval"] == 52.8 and hot["w1b2-grid-storm"]["self_pct"]["lu_solve"] == 26.7
print(json.dumps(NUM, indent=1))
