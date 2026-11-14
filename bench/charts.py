#!/usr/bin/env python3
"""Committed raw results -> docs/img/charts/*.svg (+ one CSV of the plotted numbers per chart) and docs/results.md.

    uv run --with matplotlib python bench/charts.py        (also: make charts)

Every plotted number is read from docs/bench/results/ (raw bench runs, live-range captures, OVS counters, burger.csv); the prose of
docs/results.md is bench/results.md.in with `{{table:NAME}}` placeholders filled with the same numbers. Look: light surface baked
into every SVG (readable on GitHub light and dark), one y axis per chart, thin marks, categorical colour follows the entity.
Palette: dataviz skill reference palette, checked with validate_palette.js (see bench/README.md)."""
import textwrap, csv, glob, json, os, pathlib, re, statistics as st, sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import dg_report as dg  # noqa: E402  (stdlib only)
import report  # noqa: E402  (matplotlib only inside its own chart functions)
import control_report as cr  # noqa: E402  (stdlib only)
import fdmicro_report as fm  # noqa: E402  (stdlib only)

ROOT, RES = dg.ROOT, dg.RES
OUT = ROOT / "docs/img/charts"
LIVE = RES / "live"
OVS = LIVE
# --- look ---------------------------------------------------------------------------------------------------------
SURFACE, INK, INK2, MUTED, GRID, AXIS = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7"
# colour follows the entity, in every chart
C = dict(before="#898781", after="#2a78d6", mcast="#2a78d6", groups="#1baf7a", pull="#eb6834", zmq="#6250d6", nng="#e34948",
         stock="#898781", fixed="#2a78d6", off="#898781", on="#2a78d6", pass_="#0ca30c", fail="#d03b3b")
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2,
                     "ytick.color": INK2, "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE,
                     "axes.spines.top": False, "axes.spines.right": False, "axes.grid": True, "axes.axisbelow": True, "grid.color": GRID,
                     "grid.linewidth": 0.8, "grid.linestyle": "-", "svg.hashsalt": "ferrofire", "legend.frameon": False, "legend.fontsize": 8.5})
TABLES = {}


def fig_ax(w=7.4, h=3.5, ncols=1, sharey=True):
    fig, axs = plt.subplots(1, ncols, figsize=(w, h), sharey=sharey, squeeze=False)
    for a in axs[0]:
        a.grid(axis="x", visible=False)
    return fig, list(axs[0])


def finish(fig, name, rows, head, title, ylabel, ax0, handles=None, note=None, left=0.08):
    """Title (what vs what) top left, legend under it, y label, SVG + CSV + table."""
    fig.text(0.012, 0.985, title, ha="left", va="top", fontsize=10, fontweight="bold", color=INK)
    if handles:
        fig.legend(handles=handles, loc="upper left", bbox_to_anchor=(0.005, 0.935), ncol=len(handles), handlelength=1.1, columnspacing=1.6)
    if ylabel:
        fig.text(0.012, 0.845 if handles else 0.915, ylabel, ha="left", va="top", fontsize=8.5, color=INK2)
    if note:
        fig.text(0.012, 0.012, textwrap.fill(note, 118), ha="left", va="bottom", fontsize=7.5, color=MUTED)
    fig.subplots_adjust(top=0.74 if handles else 0.82, bottom=0.27 if note else 0.14, left=left, right=0.985, wspace=0.06)
    save(fig, name)
    with open(OUT / f"{name}.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(head)
        w.writerows(rows)
    TABLES[name] = "\n".join(["| " + " | ".join(head) + " |", "|" + "---|" * len(head)] + ["| " + " | ".join(str(c) for c in r) + " |" for r in rows])


def save(fig, name):
    OUT.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT / f"{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/charts.py"})
    if os.environ.get("FF_PREVIEW"):  # PNG copies for looking at the charts while editing; not committed
        fig.savefig(pathlib.Path(os.environ["FF_PREVIEW"]) / f"{name}.png", dpi=110)
    plt.close(fig)


def swatch(label, color):
    return plt.Rectangle((0, 0), 1, 1, fc=color, ec="none", label=label)


def gbars(ax, cats, series, fmt="{:.1f}", w=0.17, ylim_pad=1.18, labels=True):
    """Grouped thin bars. series = [(label, colour, [values])]; value at the tip of each bar (<= 8 bars per axis)."""
    n = len(series)
    top = 0
    for i, (lab, col, vals) in enumerate(series):
        xs = [x + (i - (n - 1) / 2) * (w + 0.03) for x in range(len(cats))]
        ax.bar(xs, vals, width=w, color=col, ec=SURFACE, lw=1)
        for x, v in zip(xs, vals if labels else []):
            ax.text(x, v, fmt.format(v), ha="center", va="bottom", fontsize=7.5, color=INK2)
        top = max(top, max(vals))
    ax.set_xticks(range(len(cats)), cats)
    ax.set_ylim(0, top * ylim_pad if top else 1)
    ax.tick_params(length=0)


def mean_over(js, f):
    return st.mean(f(j) for j in js)


# --- 1. age -------------------------------------------------------------------------------------------------------
def chart_age():
    rows, vals = [], {}
    for p, hz in [("0.1", 10), ("0.01", 100)]:
        bj = [json.load(open(f))["publish"]["idle"]["age_above_floor"] for f in sorted(glob.glob(f"{RES}/raw/zmq_n800_p{p}_r*.client.json"))]
        aj = [dg.stream(j, "whole")["age_above_floor"] for j in dg.load(f"A_age_n800_p{p}_")[f"A_age_n800_p{p}"]]
        for q in ("p50", "p99"):
            b, a = st.mean(x[f"{q}_us"] for x in bj) / 1e3, st.mean(x[f"{q}_us"] for x in aj) / 1e3
            vals[hz, q] = (b, a)
            rows.append((f"{hz} Hz", q, f"{b:.1f}", f"{a:.1f}", len(bj), len(aj)))
    fig, (ax,) = fig_ax()
    cats = [f"{hz} Hz\n{q}" for hz in (10, 100) for q in ("p50", "p99")]
    gbars(ax, cats, [("before", C["before"], [vals[k][0] for k in vals]), ("after", C["after"], [vals[k][1] for k in vals])], w=0.17)
    finish(fig, "01-age", rows, ["rate", "percentile", "before ms", "after ms", "reps before", "reps after"],
           "Value age at 800 tags: publish on a timer vs right after the engine tick", "age, ms (Docker bench)", ax,
           [swatch("before: publisher thread on its own timer", C["before"]), swatch("after: publish the snapshot just taken", C["after"])])


# --- 2. complete cycles vs loss ---------------------------------------------------------------------------------
CELLS = [(100, "0.1", "100 tags, 10 Hz"), (100, "0.01", "100 tags, 100 Hz"), (800, "0.1", "800 tags, 10 Hz")]
LOSS = ["0", "0.1", "1", "5"]


def netem(raw_dir, n, p, loss):
    out = []
    for f in sorted(glob.glob(f"{raw_dir}/C_netem_n{n}_p{p}_loss{loss}_r*.client.json")):
        j = json.load(open(f))
        j["_file"] = f[: -len(".client.json")]
        out.append(j)
    return out


def complete(js, s, p, key=lambda s: s["delivered_per_s"]):
    return mean_over(js, lambda j: 100 * key(dg.stream(j, s)) * float(p))


def chart_loss():
    d4a = str(RES / "raw-datagram-v1-d4a")
    rows = []
    fig, axs = fig_ax(ncols=3)
    for ax, (n, p, lab) in zip(axs, CELLS):
        mc = [complete(netem(d4a, n, p, l), "whole", p) for l in LOSS]
        pl = [complete(netem(d4a, n, p, l), "pull0", p) for l in LOSS]
        for l, a, b in zip(LOSS, mc, pl):
            rows.append((lab, f"{l} %", f"{a:.1f}", f"{b:.1f}", len(netem(d4a, n, p, l))))
        for ys, col in ((mc, C["mcast"]), (pl, C["pull"])):
            ax.plot(range(4), ys, color=col, lw=2, solid_capstyle="round", marker="o", ms=5, mec=SURFACE, mew=1.5)
            ax.text(3.08, ys[-1], f"{ys[-1]:.0f}", color=INK2, fontsize=8, va="center")
        ax.set_xticks(range(4), [f"{l}" for l in LOSS])
        ax.set_xlim(-0.2, 3.5)
        ax.set_title(lab, fontsize=8.5, color=INK2, loc="left")
        ax.set_xlabel("injected loss, %")
        ax.tick_params(length=0)
    axs[0].set_ylim(0, 105)
    finish(fig, "02-complete-vs-loss", rows, ["cell", "loss", "multicast complete %", "pull-bridge complete %", "reps"],
           "Complete cycles vs injected loss: datagram v1 multicast vs pull-bridge", "complete cycles, % of published", axs[0],
           [swatch("datagram v1 multicast", C["mcast"]), swatch("pull-bridge (ZeroMQ REQ/REP)", C["pull"])],
           note="Docker bench, tc netem delay 2ms 1ms + loss on the provider egress, 30 s x 3 reps per point, receiver after ww-d4a.")


def chart_reorder():
    d4a, old = str(RES / "raw-datagram-v1-d4a"), str(RES / "raw-datagram-v1")
    rows, bef, aft = [], [], []
    for n, p, lab in CELLS:
        b, a = complete(netem(old, n, p, "0"), "whole", p), complete(netem(d4a, n, p, "0"), "whole", p)
        bef.append(b), aft.append(a)
        rows.append((lab, f"{b:.1f}", f"{a:.1f}"))
    fig, (ax,) = fig_ax()
    gbars(ax, [c[2] for c in CELLS], [("before", C["before"], bef), ("after", C["after"], aft)], w=0.17, ylim_pad=1.12)
    finish(fig, "02b-reorder-fix", rows, ["cell (0 % loss, delay 2ms 1ms)", "complete % before ww-d4a", "complete % after ww-d4a"],
           "Complete cycles at zero loss under reordering: receiver before vs after the fix", "complete cycles, %", ax,
           [swatch("receiver as first shipped (drops a late chunk as stale)", C["before"]), swatch("order-tolerant receiver (ww-d4a)", C["after"])])


# --- 3. per-tag staleness -----------------------------------------------------------------------------------------
def chart_stale():
    d4a, rows = str(RES / "raw-datagram-v1-d4a"), []
    fig, axs = fig_ax(ncols=3)
    for ax, (n, p, lab) in zip(axs, CELLS):
        ys = {}
        for s in ("whole", "pull0"):
            ys[s] = [mean_over(netem(d4a, n, p, l), lambda j: dg.stream(j, s)["probe_gap"]["p99_us"]) / 1e6 / float(p) for l in LOSS]
        for i, l in enumerate(LOSS):
            rows.append((lab, f"{l} %", f"{ys['whole'][i] * float(p) * 1e3:.0f}", f"{ys['whole'][i]:.1f}", f"{ys['pull0'][i] * float(p) * 1e3:.0f}", f"{ys['pull0'][i]:.1f}"))
        for s, col in (("whole", C["mcast"]), ("pull0", C["pull"])):
            ax.plot(range(4), ys[s], color=col, lw=2, solid_capstyle="round", marker="o", ms=5, mec=SURFACE, mew=1.5)
            ax.text(3.08, ys[s][-1], f"{ys[s][-1]:.1f}", color=INK2, fontsize=8, va="center")
        ax.set_xticks(range(4), LOSS)
        ax.set_xlim(-0.2, 3.5)
        ax.set_title(lab, fontsize=8.5, color=INK2, loc="left")
        ax.set_xlabel("injected loss, %")
        ax.tick_params(length=0)
    axs[0].set_ylim(bottom=0)
    finish(fig, "03-tag-staleness", rows, ["cell", "loss", "multicast p99 ms", "multicast p99 periods", "pull p99 ms", "pull p99 periods"],
           "Per-tag staleness p99 vs injected loss: datagram v1 multicast vs pull-bridge", "gap between updates of one tag, p99, publish periods", axs[0],
           [swatch("datagram v1 multicast", C["mcast"]), swatch("pull-bridge (ZeroMQ REQ/REP)", C["pull"])],
           note="Gap between arrivals of 8 probe tags spread over the cycle's datagrams; 1 period = 100 ms at 10 Hz, 10 ms at 100 Hz. Same runs as chart 2.")


# --- 4. fan-out -----------------------------------------------------------------------------------------------------
def chart_fanout():
    d, rows, data = dg.load("D_"), [], {}
    spec = [("one 800-tag stream", "mcast", "D_whole_n800_p{p}"), ("per-RTU groups", "groups", "D_slices_r32_k100_p{p}"), ("pull-bridge", "pull", "D_pull_r32_k100_p{p}")]
    for p, hz in [("0.1", 10), ("0.01", 100)]:
        for lab, col, key in spec:
            r = [dg.mon_rates(j) for j in d[key.format(p=p)]]
            data[hz, lab] = (st.mean(x["bps"] for x in r) / 1e6, st.mean(x["cpu"] for x in r), len(r))
            rows.append((f"{hz} Hz", lab, f"{data[hz, lab][0]:.2f}", f"{data[hz, lab][1]:.1f}", len(r)))
    for metric, name, ylabel, fmt, i in [("provider wire bytes", "04a-fanout-wire", "provider wire traffic, MB/s", "{:.2f}", 0), ("provider CPU", "04b-fanout-cpu", "provider CPU, % of one core", "{:.1f}", 1)]:
        fig, (ax,) = fig_ax()
        gbars(ax, ["10 Hz", "100 Hz"], [(lab, C[col], [data[hz, lab][i] for hz in (10, 100)]) for lab, col, _ in spec], fmt=fmt, w=0.17)
        head = ["rate", "mode", "provider wire MB/s", "provider CPU % of a core", "reps"]
        finish(fig, name, rows, head, f"Fan-out, 32 RTUs x 100 tags (800-tag model): {metric}", ylabel, ax, [swatch(lab, C[col]) for lab, col, _ in spec],
               note="Docker bench, 30 s x 3 reps; wire = request + publish frames on the provider's eth0 (both directions). One stream is received whole by every RTU.")


# --- 5. historical ZeroMQ vs NNG ------------------------------------------------------------------------------------
def chart_nng():
    runs, rows = report.load("raw"), []
    hs = [swatch("ZeroMQ", C["zmq"]), swatch("NNG", C["nng"])]
    # 5a loss at 800 tags: bar = mean of 3 reps, dots = each rep
    fig, (ax,) = fig_ax()
    rep = {(t, hz): [r["pub_loss_pct"] for r in runs if (r["transport"], r["hz"], r["n"]) == (t, hz, 800)] for t in ("zmq", "nng") for hz in (10, 100)}
    gbars(ax, ["10 Hz", "100 Hz"], [("ZeroMQ", C["zmq"], [st.mean(rep["zmq", hz]) for hz in (10, 100)]), ("NNG", C["nng"], [st.mean(rep["nng", hz]) for hz in (10, 100)])], fmt="{:.1f}", w=0.17, labels=False)
    for i, t in enumerate(("zmq", "nng")):
        for k, hz in enumerate((10, 100)):
            x = k + (i - 0.5) * 0.20
            ax.scatter([x] * 3, rep[t, hz], s=14, color=INK, zorder=3, lw=0)
    ax.set_ylim(0, max(max(v) for v in rep.values()) * 1.12)
    for k in (0, 1):
        ax.text(k - 0.10, 0.6, "0 in\nall 3", ha="center", fontsize=7.5, color=INK2)
    for t in ("zmq", "nng"):
        for hz in (10, 100):
            rows.append(("publish cycles lost, 800 tags", t, f"{hz} Hz", ", ".join(f"{v:.1f}" for v in rep[t, hz]), f"{st.mean(rep[t, hz]):.1f}"))
    finish(fig, "05a-nng-loss", rows[:4], ["metric", "transport", "rate", "each rep, %", "mean, %"],
           "Historical: publish cycles lost at 800 tags, ZeroMQ vs NNG", "cycles not delivered complete, %", ax, hs,
           note="Docker bench, 3 reps each (black dots), bars = mean. Docs: transport-comparison.md. NNG was dropped on 2026-11-09.")
    # 5b request latency, every run (shows NNG's two modes)
    fig, (ax,) = fig_ax(h=3.8)
    rows = []
    for i, t in enumerate(("zmq", "nng")):
        ys = sorted((r["read_p50_us"], r["n"], r["hz"], r["rep"]) for r in runs if r["transport"] == t)
        for k, (y, n, hz, rp) in enumerate(ys):
            ax.scatter([i + (k % 6 - 2.5) * 0.045], [y], s=34, color=C[t], ec=SURFACE, lw=1.2, zorder=3)
            rows.append((t, n, hz, rp, f"{y:.1f}"))
    ax.text(1.28, 100, "fast mode\n88-105 µs (2 runs\nbetween: 115, 128)", fontsize=8, color=INK2, va="center")
    ax.text(1.28, 153, "slow mode\n144-162 µs", fontsize=8, color=INK2, va="center")
    ax.set_xticks([0, 1], ["ZeroMQ", "NNG"])
    ax.set_xlim(-0.5, 1.75)
    ax.set_ylim(0, 180)
    ax.tick_params(length=0)
    finish(fig, "05b-nng-latency", rows, ["transport", "tags", "Hz", "rep", "READ p50 µs"],
           "Historical: request (READ) latency, one dot per run, ZeroMQ vs NNG", "READ round trip p50, µs", ax, hs,
           note="Docker bench, 24 runs each (4 tag counts x 2 rates x 3 reps). NNG falls into two modes, ZeroMQ into one; cause of the NNG split unknown.")
    # 5c packets per publish cycle
    fig, (ax,) = fig_ax()
    ns, rows = [10, 50, 100, 800], []
    sers = []
    for t in ("zmq", "nng"):
        v = [st.mean(r["net_pub_pkts_cycle"] for r in runs if r["transport"] == t and r["n"] == n) for n in ns]
        sers.append((t, C[t], v))
    for k, n in enumerate(ns):
        rows.append((n, f"{sers[0][2][k]:.2f}", f"{sers[1][2][k]:.2f}"))
    gbars(ax, [f"{n} tags" for n in ns], [("ZeroMQ", sers[0][1], sers[0][2]), ("NNG", sers[1][1], sers[1][2])], fmt="{:.1f}", w=0.17)
    finish(fig, "05c-nng-packets", rows, ["tags", "ZeroMQ packets/cycle", "NNG packets/cycle"],
           "Historical: packets on the wire per publish cycle, ZeroMQ vs NNG", "packets per publish cycle", ax, hs,
           note="Docker bench, mean of 6 runs (2 rates x 3 reps) per tag count. NNG acknowledges each TCP message; the count varies with ACK coalescing.")


# --- 6. live hour ---------------------------------------------------------------------------------------------------------
def live_cap(vm):
    t = open(LIVE / ("waterway-provider" if vm == "provider" else vm) / f"cap-dg1h-{'waterway-provider' if vm == 'provider' else vm}.txt").read()
    m = re.search(r"received (\d+), seq (\d+)\.\.(\d+), missing (\d+).*", t)
    ia = re.search(r"inter-arrival ms: p50 ([\d.]+) p99 ([\d.]+) max ([\d.]+)", t)
    return dict(n=int(m[1]), lo=int(m[2]), hi=int(m[3]), missing=int(m[4]), p50=float(ia[1]), p99=float(ia[2]), max=float(ia[3]))


def chart_live():
    pts = ["provider", "rtu-1", "rtu-2"]
    d = {p: live_cap(p) for p in pts}
    rows = [(p, d[p]["n"], f"{d[p]['lo']}..{d[p]['hi']}", d[p]["missing"], d[p]["p50"], d[p]["p99"], d[p]["max"]) for p in pts]
    head = ["capture point", "datagrams", "_ff.seq range", "missing", "inter-arrival p50 ms", "p99 ms", "max ms"]
    fig, (ax,) = fig_ax(h=2.9)
    ys = list(range(3))[::-1]
    ax.barh(ys, [d[p]["n"] for p in pts], height=0.3, color=C["mcast"], ec=SURFACE)
    for y, p in zip(ys, pts):
        ax.text(d[p]["n"] + 500, y, f"{d[p]['n']:,}  ({d[p]['missing']} missing)", va="center", fontsize=8.5, color=INK2)
    ax.set_yticks(ys, ["provider (sent)", "rtu-1 (received)", "rtu-2 (received)"])
    ax.set_xlim(0, 52000)
    ax.grid(axis="x", visible=True)
    ax.grid(axis="y", visible=False)
    ax.tick_params(length=0)
    ax.set_xlabel("datagrams in 3 600 s at 10 Hz")
    finish(fig, "06a-live-datagrams", rows, head, "Live hour: datagrams sent by the provider vs received by each stock bennu RTU", "", ax,
           note="waterway-arm64 range, one OVS bridge, 10 Hz, 1 datagram per cycle; counted from tcpdump captures at each NIC (bench/live/seqgap.py).", left=0.2)
    fig, (ax,) = fig_ax(h=3.3)
    cats = ["provider", "rtu-1", "rtu-2"]
    gbars(ax, cats, [("p50", "#9ec5f4", [d[p]["p50"] for p in pts]), ("p99", C["mcast"], [d[p]["p99"] for p in pts])], fmt="{:.1f}", w=0.17, ylim_pad=1.25)
    finish(fig, "06b-live-interarrival", rows, head, "Live hour: time between consecutive datagrams at each capture point", "inter-arrival, ms (period 100 ms)", ax,
           [swatch("p50", "#9ec5f4"), swatch("p99", C["mcast"])], note="Same captures as above; the up-to-1.4 ms excess at p99 is VM scheduling at the capture point, not loss.")


# --- 7. OVS snooping ------------------------------------------------------------------------------------------------------
def ovs_bps(label, secs):
    """tx B/s per VM segment NIC (the /1 tap of each VM; router/2) between the before/after snapshots, nominal window `secs`."""
    def load(f):
        t = open(f).read()
        ports = t.partition("-- ports")[2].partition("-- snooping")[0]
        vm = {}
        for l in t.partition("-- vms")[2].partition("-- end")[0].splitlines():
            m = re.match(r".*\|\s*(\S+)\s*\|\s*\[(.*)\]", l)
            if m:
                for i, tp in enumerate(m[2].split(", ")):
                    vm[tp.strip()] = f"{m[1]}/{i}"
        names = {m[2]: vm.get(m[1]) for m in re.finditer(r"name\s*:\s*(\S+)\s+ofport\s*:\s*(\d+)", ports)}
        tx, cur = {}, None
        for l in t.partition("-- ports")[0].splitlines():
            m = re.match(r"\s+port\s+(\S+?):\s+rx", l)
            if m:
                cur = m[1]
            m = re.match(r"\s+tx pkts=\d+, bytes=(\d+)", l)
            if m and cur:
                tx[names.get(cur)] = int(m[1])
        return tx
    b, a = load(OVS / f"ovs-{label}-before.txt"), load(OVS / f"ovs-{label}-after.txt")
    return {k: (a[k] - b[k]) / secs for k in a if k and k in b}


def chart_ovs():
    runs = [("whole stream", "dg1h", 3600, "dgsn5m", 300), ("per-RTU groups", "pr10m", 600, "prsn10m", 600)]
    by = ["opc/1", "historian/1", "hmi/1", "scada-server/1", "engineer-workstation/1"]
    rows, fig_data = [], []
    for lab, off, so, on, sn in runs:
        o, n = ovs_bps(off, so), ovs_bps(on, sn)
        cats = {"bystander NIC (mean of 5)": (st.mean(o[k] for k in by), st.mean(n[k] for k in by)), "rtu-1": (o["rtu-1/1"], n["rtu-1/1"]), "rtu-2": (o["rtu-2/1"], n["rtu-2/1"])}
        fig_data.append((lab, cats))
        for c, (x, y) in cats.items():
            rows.append((lab, c, f"{x:,.0f}", f"{y:,.0f}", f"{so} s / {sn} s"))
    fig, axs = fig_ax(7.4, 3.6, ncols=2)
    for ax, (lab, cats) in zip(axs, fig_data):
        gbars(ax, ["bystander\n(each of 5)", "rtu-1", "rtu-2"], [("off", C["off"], [v[0] for v in cats.values()]), ("on", C["on"], [v[1] for v in cats.values()])], fmt="{:,.0f}", w=0.17, ylim_pad=1.12)
        ax.set_title(lab, fontsize=8.5, color=INK2, loc="left")
    finish(fig, "07-ovs-snooping", rows, ["stream", "NIC", "snooping off B/s", "snooping on B/s", "window off / on"],
           "OVS multicast snooping off vs on: bytes sent to each NIC", "bytes/s sent by the bridge to the NIC", axs[0],
           [swatch("snooping off (default)", C["off"]), swatch("snooping on", C["on"])],
           note="waterway-arm64 range; OVS per-port tx counters before/after (docs/bench/results/live/ovs-*.txt) over the nominal window; includes ARP and sampler traffic.")


# --- 8. stock vs fixed bennu ----------------------------------------------------------------------------------------------
def burger():
    return list(csv.DictReader(open(RES / "burger/burger.csv")))


def chart_bennu():
    b = burger()
    get = lambda item, build, metric: next(float(r["value"]) for r in b if (r["item"], r["build"], r["metric"]) == (item, build, metric))
    hs = [swatch("stock bennu", C["stock"]), swatch("bennu with the fix", C["fixed"])]
    # 8a fd leak: open sockets at the start and at the end of the churn
    fig, (ax,) = fig_ax()
    ends = {k: get("modbus-churn", k, "conns_at_stop" if k == "stock" else "conns_done") for k in ("stock", "fixed")}
    sers = {k: [get("modbus-churn", k, "sockets_before"), get("modbus-churn", k, "sockets_after")] for k in ends}
    rows = [(k, v, int(ends[k]), sers[k][0], sers[k][1], "stopped answering" if k == "stock" else "still answering") for k, v in (("stock", "stock bennu"), ("fixed", "bennu with the fix"))]
    gbars(ax, ["before the churn", "end of the churn\n(stock: connection 1 006;\nfixed: connection 1 500)"], [("stock", C["stock"], [sers["stock"][0], sers["stock"][1]]), ("fixed", C["fixed"], [sers["fixed"][0], sers["fixed"][1]])], fmt="{:.0f}", w=0.17)
    ax.axhline(1024, color=AXIS, lw=1)
    ax.text(-0.25, 1040, "soft limit of open files: 1 024", fontsize=8, color=MUTED, va="bottom", ha="left")
    ax.set_ylim(0, 1250)
    finish(fig, "08a-fd-leak", rows, ["build", "label", "connections done", "sockets before", "sockets at the end", "state at the end"], "Modbus server fd leak: stock bennu vs bennu with the fix (#79)",
           "open sockets of the field device", ax, hs, note="Phenix range, rtu-1, one connection opened and closed per read. Stock numbers are from the PR comment (burger.csv).")
    # 8b watcher log growth
    fig, (ax,) = fig_ax(h=3.4)
    rows = []
    for k in ("rtu-1", "rtu-2"):
        rows.append((k, f"{get('watcher-quiet', 'stock', 'rtu-1_Bpm' if k == 'rtu-1' else 'rtu-2_Bpm'):,.0f}", f"{get('watcher-quiet', 'fixed', 'rtu-1_Bpm' if k == 'rtu-1' else 'rtu-2_Bpm'):,.0f}"))
    gbars(ax, ["rtu-1", "rtu-2"], [("stock", C["stock"], [float(r[1].replace(",", "")) / 1000 for r in rows]), ("fixed", C["fixed"], [float(r[2].replace(",", "")) / 1000 for r in rows])], fmt="{:.1f}", w=0.17)
    finish(fig, "08b-watcher-log", rows, ["RTU", "stock watcher-stderr.log B/min", "fixed B/min"], "bennu-watcherd stderr log growth: stock vs fixed (#82)",
           "log growth, KB/min", ax, hs, note="Phenix range, burger `watcher-quiet` check over an idle minute-scale window; the stock line is `pid = N`, repeated.")
    # 8c robust matrix
    items = [("modbus-churn", "1 500 connections, fds stay flat (#79)"), ("modbus-badframes", "bad Modbus frames dropped, device survives (#81)"),
             ("iec104-masters", "IEC-104 second master does not starve the first (#81)"), ("dnp3-accept", "DNP3 outstation accepts again after accept() error (#81)"),
             ("watcher-quiet", "watcher stderr log quiet (#82)")]
    fig, ax = plt.subplots(figsize=(7.4, 3.3))
    ax.set_xlim(0, 10)
    ax.set_ylim(0, len(items) + 1)
    ax.axis("off")
    ax.text(5.9, len(items) + 0.5, "stock bennu", ha="center", va="center", fontsize=9, fontweight="bold", color=INK2)
    ax.text(8.6, len(items) + 0.5, "with the fix", ha="center", va="center", fontsize=9, fontweight="bold", color=INK2)
    rows = []
    for i, (item, desc) in enumerate(items):
        y = len(items) - i - 0.5
        ax.plot([0, 10], [y + 0.5] * 2, color=GRID, lw=0.8)
        ax.text(0.1, y, desc, va="center", fontsize=8.5, color=INK)
        res = []
        for build, x in (("stock", 5.9), ("fixed", 8.6)):
            ok = next(r["value"] for r in b if (r["item"], r["build"], r["metric"]) == (item, build, "verdict")) == "pass"
            res.append("pass" if ok else "FAIL")
            ax.add_patch(plt.Rectangle((x - 0.9, y - 0.17), 1.8, 0.34, fc=C["pass_"] if ok else C["fail"], ec="none"))
            ax.text(x, y, ("✓ pass" if ok else "✗ FAIL"), ha="center", va="center", fontsize=8.5, color="#ffffff", fontweight="bold")
        rows.append((item, desc, res[0], res[1]))
    fig.text(0.012, 0.985, "Robustness checks: stock bennu vs bennu with the fixes (#79, #81, #82)", ha="left", va="top", fontsize=10, fontweight="bold", color=INK)
    fig.subplots_adjust(top=0.9, bottom=0.03, left=0.01, right=0.99)
    save(fig, "08c-robust-matrix")
    with open(OUT / "08c-robust-matrix.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["check", "what it requires", "stock", "fixed"])
        w.writerows(rows)
    TABLES["08c-robust-matrix"] = "\n".join(["| check | what it requires | stock | fixed |", "|---|---|---|---|"] + [f"| {' | '.join(map(str, r))} |" for r in rows])


# --- 9. control latency: breaker flips and generator spin up/down, ferrofire vs stock bennu, Docker vs range ---------------------
FF, STK = C["fixed"], C["stock"]   # ferrofire blue, stock grey (as in section 8); range = same colour, hatched
HATCH = {"docker": "", "range": "////"}
SERIES = [(s, e) for e in ("docker", "range") for s in ("ferrofire", "stock")]
SNAME = {"ferrofire": "ferrofire", "stock": "stock bennu"}
_CMD, _PH = {}, {}


def cmds(env, stack):
    if (env, stack) not in _CMD:
        _CMD[env, stack] = cr.commands(env, stack)
    return _CMD[env, stack]


def phs(env, stack):
    if (env, stack) not in _PH:
        _PH[env, stack] = cr.phases(env, stack)
    return _PH[env, stack]


def lat(env, stack, field, kind, mode, n=None, R=None, dir=None):
    """Sorted `field` (ms from t_cmd) of the done commands of a family (and n, R, dir if given)."""
    return sorted(c[field] for c in cmds(env, stack) if c["done"] and c[field] is not None and c["kind"] == kind and c["mode"] == mode
                  and n in (None, c["n"]) and R in (None, c["R"]) and dir in (None, c["dir"]))


def sw(label, color, hatch=""):
    return plt.Rectangle((0, 0), 1, 1, fc=color, ec=SURFACE if hatch else "none", hatch=hatch, label=label)


def handles(envs):
    return [sw(f"{SNAME[s]}, {e}" if len(envs) > 1 else SNAME[s], FF if s == "ferrofire" else STK, HATCH[e]) for s, e in SERIES if e in envs]


def cbars(ax, cats, series, fmt="{:.0f}", log=False):
    """series = [(colour, hatch, [value or None])]; None = the stack could not produce that number ('n/a' at the foot)."""
    n, w = len(series), 0.8 / len(series)
    top = max(v for _, _, vs in series for v in vs if v is not None)
    for i, (col, h, vals) in enumerate(series):
        xs = [x + (i - (n - 1) / 2) * w for x in range(len(cats))]
        ax.bar([x for x, v in zip(xs, vals) if v is not None], [v for v in vals if v is not None], width=w * 0.9, color=col, ec=SURFACE, lw=1, hatch=h or None)
        for x, v in zip(xs, vals):
            ax.text(x, v if v is not None else (1.2 if log else 0), fmt.format(v) if v is not None else "n/a", ha="center", va="bottom", fontsize=7, color=INK2, rotation=0 if n <= 2 else 90)
    ax.set_xticks(range(len(cats)), cats)
    if log:
        ax.set_yscale("log")
        ax.set_ylim(1, top * 4)
    else:
        ax.set_ylim(0, top * (1.12 if n <= 2 else 1.3))
    ax.tick_params(length=0)


def p(v, q):
    x = cr.pct(v, q)
    return "" if x is None else f"{x:.0f}"


def chart_control():
    f0 = lambda x: f"{x:.0f}"
    # 9a sequential breaker flips, visible at the SCADA, by R
    Rs, rows, bars = [1, 2, 5, 10], [], []
    for s in ("ferrofire", "stock"):
        v = {R: lat("docker", s, "visible", "breaker", "seq", 100, R) for R in Rs}
        bars.append((FF if s == "ferrofire" else STK, "", [cr.pct(v[R], .5) for R in Rs]))
        rows += [(SNAME[s], R, len(v[R]), p(v[R], .5), p(v[R], .95), p(v[R], .99), f0(v[R][-1])) for R in Rs]
    fig, (ax,) = fig_ax()
    cbars(ax, [f"{R} RTU" + ("s" if R > 1 else "") for R in Rs], bars)
    finish(fig, "09a-breaker-seq", rows, ["stack", "RTUs", "flips done", "p50 ms", "p95 ms", "p99 ms", "max ms"],
           "Sequential breaker flips: ferrofire vs stock bennu, Docker", "SCADA write sent to read-back showing the new state, p50, ms", ax, handles(["docker"]),
           note="Each flip waits for the previous one to be visible. 5 reps per (stack, R); the rows are the 500 flips of each (stack, R).")
    # 9b where the time goes: ack, applied, stream, visible; sequential 100 flips, 5 RTUs
    stages, rows, bars = ["ack", "applied", "stream", "visible"], [], []
    for s in ("ferrofire", "stock"):
        v = {k: lat("docker", s, k, "breaker", "seq", 100, 5) for k in stages}
        bars.append((FF if s == "ferrofire" else STK, "", [cr.pct(v[k], .5) for k in stages]))
        rows += [(SNAME[s], k, len(v[k]), p(v[k], .5), p(v[k], .95), p(v[k], .99), f0(v[k][-1])) for k in stages]
    fig, (ax,) = fig_ax()
    cbars(ax, ["write answered\n(SCADA)", "provider applied\nthe write", "first datagram at\nthe RTU", "read-back at\nthe SCADA"], bars)
    finish(fig, "09b-stages", rows, ["stack", "stage", "commands", "p50 ms", "p95 ms", "p99 ms", "max ms"],
           "Where a sequential breaker flip spends its time: ferrofire vs stock bennu, Docker", "time since the write was sent, p50, ms", ax, handles(["docker"]),
           note="5 reps, 500 flips per stack. Provider-applied is the provider's own stamp (ferrofire: --stamp-writes; stock: bench/stock_provider.py), on the SCADA driver's host clock.")
    # 9c simultaneous breaker flips: how many were done in time, by n, R = 1 and 10
    ns, rows = [1, 5, 10, 50, 100, 1000], []
    fig, axs = fig_ax(ncols=2)
    for ax, R in zip(axs, (1, 10)):
        bars = []
        for s in ("ferrofire", "stock"):
            ph = [q for q in phs("docker", s) if q["R"] == R and q["cell"].startswith("breaker:sim:")]
            by = {n: [q for q in ph if q["n"] == n] for n in ns}
            bars.append((FF if s == "ferrofire" else STK, "", [100 * sum(q["ok"] for q in by[n]) / sum(q["issued"] for q in by[n]) for n in ns]))
            rows += [(SNAME[s], R, n, int(sum(q["issued"] for q in by[n])), int(sum(q["ok"] for q in by[n])), int(sum(q["lost"] for q in by[n])), len(by[n])) for n in ns]
        cbars(ax, [str(n) for n in ns], bars, fmt="{:.0f}")
        ax.set_title(f"{R} RTU" + ("s" if R > 1 else ""), fontsize=8.5, color=INK2, loc="left")
        ax.set_xlabel("flips issued at once", color=INK2)
    finish(fig, "09c-breaker-sim-done", rows, ["stack", "RTUs", "flips issued at once", "commands issued", "done within 10 s", "lost", "phases"],
           "Simultaneous breaker flips done in time: ferrofire vs stock bennu, Docker", "share of the flips visible at the SCADA within 10 s, %", axs[0], handles(["docker"]),
           note="All flips of a phase sent at once, one Modbus client per RTU. 5 reps; the n = 1000 phases of stock bennu ran once (1 rep).")
    # 9d simultaneous throughput
    rows, fig = [], None
    fig, axs = fig_ax(ncols=2)
    for ax, R in zip(axs, (1, 10)):
        bars = []
        for s in ("ferrofire", "stock"):
            ph = [q for q in phs("docker", s) if q["R"] == R and q["cell"].startswith("breaker:sim:")]
            by = {n: [q["per_s"] for q in ph if q["n"] == n] for n in ns}
            bars.append((FF if s == "ferrofire" else STK, "", [st.median(by[n]) for n in ns]))
            rows += [(SNAME[s], R, n, len(by[n]), f"{st.median(by[n]):.1f}") for n in ns]
        cbars(ax, [str(n) for n in ns], bars, fmt="{:,.0f}", log=True)
        ax.set_title(f"{R} RTU" + ("s" if R > 1 else ""), fontsize=8.5, color=INK2, loc="left")
        ax.set_xlabel("flips issued at once", color=INK2)
    finish(fig, "09d-breaker-sim-throughput", rows, ["stack", "RTUs", "flips issued at once", "phases", "median flips per s"],
           "Simultaneous breaker flips per second: ferrofire vs stock bennu, 1 and 10 RTUs, Docker", "flips done per second of the phase, median over reps (log scale)", axs[0], handles(["docker"]),
           note="Done within 10 s over the time from the first write to the last read-back; lost flips are not counted, so a stock phase that lost most of its flips shows what it did serve.")
    # 9e generators: spin-up and spin-down, sequential, all n and R pooled
    cats = ["spin-up: status on\nat the SCADA", "spin-up: PG within 1%\nof the setpoint", "spin-down: status off\nat the SCADA"]
    rows, bars = [], []
    for s in ("ferrofire", "stock"):
        g = [("status on", lat("docker", s, "visible", "gen", "seq", dir="up")), ("PG at setpoint", lat("docker", s, "setpoint", "gen", "seq", dir="up")), ("status off", lat("docker", s, "visible", "gen", "seq", dir="down"))]
        bars.append((FF if s == "ferrofire" else STK, "", [cr.pct(v, .5) for _, v in g]))
        rows += [(SNAME[s], k, len(v), p(v, .5), p(v, .95), p(v, .99), f0(v[-1]) if v else "") for k, v in g]
    fig, (ax,) = fig_ax()
    cbars(ax, cats, bars)
    finish(fig, "09e-gen", rows, ["stack", "event", "generators done", "p50 ms", "p95 ms", "p99 ms", "max ms"],
           "Generator spin-up and spin-down: ferrofire (ramp) vs stock bennu (no ramp), Docker", "SCADA write sent to the event, p50, ms", ax, handles(["docker"]),
           note="Generators 1, 2, 5, 10 on 1, 2, 5, 10 RTUs pooled, 5 reps. Ferrofire spin-down: PG ramps 40 MW to 0 first, status goes off at the end. Stock has no ramp and no PG readback to wait on (n/a).")
    # 9f Docker vs range
    Rr, rows = [1, 2, 5], []
    fig, axs = fig_ax(ncols=2)
    for ax, mode in zip(axs, ("seq", "sim")):
        bars = []
        for s, e in SERIES:
            v = {R: lat(e, s, "visible", "breaker", mode, 100, R) for R in Rr}
            bars.append((FF if s == "ferrofire" else STK, HATCH[e], [cr.pct(v[R], .5) for R in Rr]))
            rows += [(SNAME[s], e, mode, R, len(v[R]), p(v[R], .5), p(v[R], .95), p(v[R], .99), f0(v[R][-1])) for R in Rr]
        cbars(ax, [f"{R} RTU" + ("s" if R > 1 else "") for R in Rr], bars, log=True)
        ax.set_title({"seq": "sequential", "sim": "simultaneous (completed flips only)"}[mode], fontsize=8.5, color=INK2, loc="left")
    finish(fig, "09f-docker-vs-range", rows, ["stack", "where", "mode", "RTUs", "flips done", "p50 ms", "p95 ms", "p99 ms", "max ms"],
           "100 breaker flips: Docker vs live range, ferrofire vs stock bennu", "SCADA write sent to read-back showing the new state, p50, ms (log scale)", axs[0], handles(["docker", "range"]),
           note="Docker: 5 reps, 18 vCPU VM. Range: 3 reps, arm64 KVM VMs (provider and SCADA driver on one VM). Stock simultaneous flips that were lost are not in the p50.")
    # 9g lost and wrong, all phases
    rows = []
    OC = {(e, s): cr.order_counts(e, s) for e in ("docker", "range") for s in ("ferrofire-before", "ferrofire", "stock")}
    for e in ("docker", "range"):
        for s in ("ferrofire-before", "ferrofire", "stock"):
            for kind, mode in (("breaker", "seq"), ("breaker", "sim"), ("gen", "seq"), ("gen", "sim")):
                ph = [q for q in phs(e, s) if q["cell"].startswith(f"{kind}:{mode}:")]
                t = lambda k: int(sum(q[k] for q in ph))
                oc = [sum(v[i] for (R, k), v in OC[e, s].items() if k == f"{kind}:{mode}") for i in (0, 1)]
                rows.append((e, SNAME.get(s, "ferrofire before ww-1df.32"), f"{kind} {mode}", t("issued"), t("lost"), t("wrong_final"), t("wrong_provider"), t("wrong_collateral"), oc[0], oc[1], t("flaps"), t("write_errors"), len(ph)))
    TABLES["09g-lost-wrong"] = "\n".join(["| where | stack | family | commands | lost | wrong final | wrong at provider | wrong collateral | reordered | chunk skew | flaps | write errors | phases |", "|" + "---|" * 13] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])
    # 9h per-cell numbers (CSV only): every (where, stack, cell, direction, R)
    with open(OUT / "09h-control-all-cells.csv", "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["where", "stack", "kind", "mode", "n", "dir", "RTUs", "commands", "done", "lost", "visible p50 ms", "p95", "p99", "max", "applied p50 ms", "setpoint p50 ms"])
        for s, e in SERIES:
            g = {}
            for c in cmds(e, s):
                g.setdefault((c["kind"], c["mode"], c["n"], c["dir"], c["R"]), []).append(c)
            for k in sorted(g, key=lambda k: (k[0], k[1], k[2], k[3], k[4])):
                cs = g[k]
                v, a, sp = (sorted(c[f] for c in cs if c["done"] and c[f] is not None) for f in ("visible", "applied", "setpoint"))
                w.writerow([e, s, *k[:4], k[4], len(cs), len(v), len(cs) - len(v), p(v, .5), p(v, .95), p(v, .99), f0(v[-1]) if v else "", p(a, .5), p(sp, .5)])
    sv = dict(l.split(" ", 1) for l in (cr.CTL / "solve-time.txt").read_text().splitlines() if l.startswith(("per_solve_ms", "host_load", "docker_vm")))
    TABLES["09i-solve"] = sv["per_solve_ms"].split(" (")[0]


def idle_cpu():
    """{interval s: provider CPU %, idle, no commands} from the stock-capacity.txt probe (bench/stock_capacity.sh)."""
    out = {}
    f = cr.CTL / "stock-capacity.txt"
    for l in f.read_text().splitlines() if f.exists() else []:
        m = re.match(r"interval ([\d.]+) .* cpu_pct ([\d.]+)", l)
        if m:
            out[float(m[1])] = float(m[2])
    return out


def solve(stack):
    """[p50, p95] ms of the stock write handler (provider-side `Applied:` stamps: entry to the ACK, i.e. the re-solve) over every stamp of the stack."""
    v = sorted((c["solved"] - c["applied"]) for c in cmds("docker", stack) if c["applied"] is not None and c["solved"] is not None)
    return [f"{cr.pct(v, .5):.0f}", f"{cr.pct(v, .95):.0f}"]


def chart_stock_intervals():
    """9j: the stock provider at faster publish intervals (ww-1df.35), Docker, ferrofire at 50 ms as the reference."""
    ivs = [(1.0, "stock")] + list(cr.STOCK_FAST)
    idle, rows, S = idle_cpu(), [], []
    ff = lat("docker", "ferrofire", "visible", "breaker", "seq", 100, 5)
    ffg = lat("docker", "ferrofire", "visible", "gen", "seq", dir="up")
    for iv, name in ivs:
        seq, seq1 = (lat("docker", name, "visible", "breaker", "seq", 100, R) for R in (5, 1))
        sim = {R: [q for q in phs("docker", name) if q["R"] == R and q["cell"] == "breaker:sim:100"] for R in (1, 10)}
        lost = {R: 100 * sum(q["lost"] for q in sim[R]) / sum(q["issued"] for q in sim[R]) for R in sim}
        oc = [sum(v[i] for (R, k), v in cr.order_counts("docker", name).items() if k == "breaker:sim") for i in (0, 1)]
        gen = lat("docker", name, "visible", "gen", "seq", dir="up")
        cpu, per = cr.provider_stats(name) if name != "stock" else ([], [])
        reps = len({q["rep"] for q in phs("docker", name)})
        d = dict(iv=iv, seq50=cr.pct(seq, .5), seq95=cr.pct(seq, .95), r1=cr.pct(seq1, .5), lost1=lost[1], lost10=lost[10], gen=cr.pct(gen, .5), cpu=cr.pct(cpu, .5) if cpu else None, idle=idle.get(iv),
                 per=cr.pct(per, .5) if per else None)
        S.append(d)
        rows.append((f"{iv:g}" + (" (as shipped)" if name == "stock" else ""), reps, f"{d['per']:.0f}" if d["per"] else ("1000 (nominal)" if name == "stock" else ""), p(seq, .5), p(seq, .95), p(seq1, .5), p(seq1, .95), f"{lost[1]:.0f}", f"{lost[10]:.0f}",
                     oc[0], oc[1], p(gen, .5), f"{d['idle']:.0f}" if d["idle"] is not None else "", f"{d['cpu']:.0f}" if d["cpu"] else ""))
    f50, g50 = cr.pct(ff, .5), cr.pct(ffg, .5)
    pc = cr.provider_stats(ivs[-1][1])
    TABLES["09j-period"], TABLES["09j-peak"] = f"{cr.pct(pc[1], .5):.0f}", f"{pc[0][-1]:.0f}"
    cap = [dict(re.findall(r"(\w+) ([\d.]+)", l)) for l in (cr.CTL / "stock-capacity.txt").read_text().splitlines() if l.startswith("interval ")]
    TABLES["09k-stock-capacity"] = "\n".join(["| interval s | pack ms | send ms | one publish ms | effective period ms | publishes per s | CPU % of a core (idle) | solve p50 ms | solve p95 ms |", "|" + "---|" * 9] +
        ["| " + " | ".join([c["interval"], c["pack_ms_mean"], c["send_ms_mean"], c["work_ms"], c["effective_period_ms"], c["publishes_per_s"], c["cpu_pct"], *solve(n)]) + " |" for c, (_, n) in zip(cap, ivs)])
    ff1 = lat("docker", "ferrofire", "visible", "breaker", "seq", 100, 1)
    ffo = [sum(v[i] for (R, k), v in cr.order_counts("docker", "ferrofire").items() if k == "breaker:sim") for i in (0, 1)]
    rows.append(("ferrofire, 0.05", 5, "50", p(ff, .5), p(ff, .95), p(ff1, .5), p(ff1, .95), "0", "0", ffo[0], ffo[1], p(ffg, .5), "not measured", "not measured"))
    xs, labels = range(len(S)), [f"{d['iv']:g} s" for d in S]
    fig, axs = plt.subplots(2, 2, figsize=(7.4, 5.6))
    for a in axs.flat:
        a.grid(axis="x", visible=False)
        a.set_xticks(xs, labels)
        a.tick_params(length=0)
    ref = lambda a, v, txt: (a.axhline(v, color=FF, lw=1.2, ls="--"), a.text(-0.1, v, txt, color=FF, fontsize=7.5, ha="left", va="bottom"))
    a = axs[0][0]
    a.plot(xs, [d["seq50"] for d in S], color=STK, marker="o", lw=1.6, label="p50")
    a.plot(xs, [d["seq95"] for d in S], color=STK, marker="o", mfc=SURFACE, lw=1.2, ls=":", label="p95")
    a.plot(xs, [d["r1"] for d in S], color=STK, marker="s", lw=1.0, ls="--", label="p50, 1 RTU")
    ref(a, f50, "ferrofire 50 ms: p50 %.0f ms" % f50)
    a.set_title("sequential breaker flip visible, 5 RTUs, ms", fontsize=8.5, color=INK2, loc="left")
    a.legend(loc="upper right")
    a = axs[0][1]
    a.plot(xs, [d["lost1"] for d in S], color=STK, marker="o", lw=1.6, label="1 RTU")
    a.plot(xs, [d["lost10"] for d in S], color=STK, marker="o", mfc=SURFACE, lw=1.2, ls=":", label="10 RTUs")
    ref(a, 0, "ferrofire: 0")
    a.set_title("100 simultaneous flips lost, %", fontsize=8.5, color=INK2, loc="left")
    a.legend(loc="center right")
    a = axs[1][0]
    a.plot(xs, [d["gen"] for d in S], color=STK, marker="o", lw=1.6)
    ref(a, g50, "ferrofire: %.0f ms" % g50)
    a.set_title("generator spin-up, status on at the SCADA, p50, ms", fontsize=8.5, color=INK2, loc="left")
    a = axs[1][1]
    a.plot(xs, [d["idle"] for d in S], color=STK, marker="o", mfc=SURFACE, lw=1.2, ls=":", label="idle (no commands)")
    a.plot([i for i, d in enumerate(S) if d["cpu"]], [d["cpu"] for d in S if d["cpu"]], color=STK, marker="o", lw=1.6, label="during the bench cells")
    a.set_title("stock provider CPU, % of one core", fontsize=8.5, color=INK2, loc="left")
    a.legend(loc="upper left")
    for a in axs.flat:
        a.set_ylim(bottom=0)
        a.set_xlabel("stock publish interval (as shipped: 1 s)", color=INK2, fontsize=8)
    fig.subplots_adjust(hspace=0.55)
    finish(fig, "09j-stock-intervals", rows, ["publish interval s", "reps", "effective period ms", "seq p50 ms (5 RTUs)", "seq p95 ms (5 RTUs)", "seq p50 ms (1 RTU)", "seq p95 ms (1 RTU)", "100 sim lost % (1 RTU)", "100 sim lost % (10 RTUs)",
                                              "reordered", "chunk skew", "gen status-on p50 ms", "idle CPU %", "bench CPU %"],
           "Stock bennu at faster publish intervals vs ferrofire at 50 ms, Docker", None, axs[0][0],
           note="Stock grey, ferrofire blue dashed. Intervals are the sleep of stock's publish loop (bench/stock_provider.py); sequential = 100 flips on 5 RTUs (and 1 RTU); generator spin-up = all R pooled; ferrofire reference = its 5-rep runs.")


CROSS_NAMES = {"cross-ferrofire-ferrofire": "ferrofire + ferrofire FD", "cross-ferrofire-stock": "ferrofire + stock FD",
               "cross-stock-stock-0.1s": "stock 0.1 s + stock FD", "cross-stock-stock": "stock 1 s + stock FD"}


def chart_cross():
    """9l/9m: the 2x2 provider x field-device cross matrix (ww-1df.40), Docker. Colour = provider (ferrofire blue, stock grey; lighter grey = stock tuned to 0.1 s), hatch = stock field device."""
    names = [n for n in CROSS_NAMES if n in cr.CROSS and phs("docker", n)]
    col = {"cross-ferrofire-ferrofire": (FF, ""), "cross-ferrofire-stock": (FF, "////"), "cross-stock-stock-0.1s": ("#b9b8b0", "////"), "cross-stock-stock": (STK, "////")}
    Rs, reps = (1, 2, 5, 10), {n: len({q["rep"] for q in phs("docker", n)}) for n in names}
    f0 = lambda x: "" if x is None else f"{x:.0f}"
    seq = {(n, R): {f: lat("docker", n, f, "breaker", "seq", 100, R) for f in ("applied", "stream", "visible")} for n in names for R in Rs}
    sim = lambda n, c, R: [q for q in phs("docker", n) if q["R"] == R and q["cell"] == c]
    lost = lambda n, c, R: 100 * sum(q["lost"] for q in sim(n, c, R)) / max(1, sum(q["issued"] for q in sim(n, c, R)))
    rows = [(CROSS_NAMES[n], reps[n], R, f0(cr.pct(seq[n, R]["visible"], .5)), f0(cr.pct(seq[n, R]["visible"], .95)), f0(cr.pct(seq[n, R]["applied"], .5)), f0(cr.pct(seq[n, R]["stream"], .5)))
            for n in names for R in Rs]
    fig, axs = plt.subplots(2, 2, figsize=(7.4, 6.0))
    for a in axs.flat:
        a.grid(axis="x", visible=False)
        a.tick_params(length=0)
    xs = lambda i, k: [x + (i - (len(names) - 1) / 2) * 0.8 / len(names) for x in range(k)]
    bw = 0.8 / len(names) * 0.9
    a = axs[0][0]
    for i, n in enumerate(names):
        v = [cr.pct(seq[n, R]["visible"], .5) for R in Rs]
        a.bar(xs(i, 4), v, width=bw, color=col[n][0], hatch=col[n][1] or None, ec=SURFACE, lw=1)
        a.plot(xs(i, 4), [cr.pct(seq[n, R]["visible"], .95) for R in Rs], ls="", marker="_", color=INK, ms=6)
    a.set_yscale("log")
    a.set_ylim(10, 2000)
    a.set_xticks(range(4), [f"{R} RTU" + ("s" if R > 1 else "") for R in Rs])
    a.set_title("100 sequential flips visible: p50 bar, p95 tick, ms (log)", fontsize=8.5, color=INK2, loc="left")
    a = axs[0][1]
    for i, n in enumerate(names):
        for j, f in enumerate(("applied", "stream", "visible")):
            v = cr.pct(seq[n, 5][f], .5)
            a.bar(j + (i - (len(names) - 1) / 2) * 0.8 / len(names), v, width=bw, color=col[n][0], hatch=col[n][1] or None, ec=SURFACE, lw=1)
    a.set_yscale("log")
    a.set_ylim(10, 2000)
    a.set_xticks(range(3), ["provider\napplied", "first datagram\nwith the state", "visible at\nthe SCADA"])
    a.set_title("stage times, 5 RTUs, p50, ms (log)", fontsize=8.5, color=INK2, loc="left")
    a = axs[1][0]
    for i, n in enumerate(names):
        a.bar(xs(i, 4), [lost(n, "breaker:sim:100", R) for R in Rs], width=bw, color=col[n][0], hatch=col[n][1] or None, ec=SURFACE, lw=1)
    a.set_xticks(range(4), [f"{R} RTU" + ("s" if R > 1 else "") for R in Rs])
    a.set_title("100 simultaneous flips lost (not visible in 10 s), %", fontsize=8.5, color=INK2, loc="left")
    a.set_ylim(0, 100)
    oc = {n: [sum(v[i] for (R, k), v in cr.order_counts("docker", n).items() if k == "breaker:sim") for i in (0, 1)] for n in names}
    fl = {n: int(sum(q["flaps"] for q in phs("docker", n))) for n in names}
    a = axs[1][1]
    for i, n in enumerate(names):
        a.bar(xs(i, 2), [oc[n][0], fl[n]], width=bw, color=col[n][0], hatch=col[n][1] or None, ec=SURFACE, lw=1)
        for x, v in zip(xs(i, 2), [oc[n][0], fl[n]]):
            a.text(x, v, f"{v}", ha="center", va="bottom", fontsize=7, color=INK2, rotation=90)
    a.set_xticks(range(2), ["reordered pairs\n(simultaneous flips)", "flaps\n(all phases)"])
    a.set_ylim(0, max(max(v[0] for v in oc.values()), max(fl.values()), 1) * 1.25)
    a.set_title("reordered visible pairs and flaps, all R and reps", fontsize=8.5, color=INK2, loc="left")
    fig.subplots_adjust(hspace=0.5, top=0.8, bottom=0.12, left=0.08, right=0.985, wspace=0.18)
    fig.text(0.012, 0.985, "Provider vs field device: the 2x2 cross matrix, Docker", ha="left", va="top", fontsize=10, fontweight="bold", color=INK)
    fig.legend(handles=[sw(CROSS_NAMES[n], *col[n]) for n in names], loc="upper left", bbox_to_anchor=(0.005, 0.95), ncol=2, handlelength=1.1, columnspacing=1.6)
    save(fig, "09l-cross")
    head = ["provider + field device", "reps", "RTUs", "seq p50 ms", "seq p95 ms", "applied p50 ms", "stream p50 ms"]
    with open(OUT / "09l-cross.csv", "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(head)
        w.writerows(rows)
    TABLES["09l-cross-seq"] = "\n".join(["| " + " | ".join(head) + " |", "|" + "---|" * len(head)] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])
    gen = {n: lat("docker", n, "visible", "gen", "seq", dir="up") for n in names}
    rows = [(CROSS_NAMES[n], reps[n], *[f"{lost(n, 'breaker:sim:100', R):.0f}" for R in Rs], *[f"{lost(n, 'breaker:sim:1000', R):.0f}" for R in (1, 10)], oc[n][0], oc[n][1], fl[n], f0(cr.pct(gen[n], .5))) for n in names]
    head = ["provider + field device", "reps", "100 sim lost % 1 RTU", "2 RTUs", "5 RTUs", "10 RTUs", "1000 sim lost % 1 RTU", "10 RTUs", "reordered", "chunk skew", "flaps", "gen status-on p50 ms"]
    TABLES["09m-cross-burst"] = "\n".join(["| " + " | ".join(head) + " |", "|" + "---|" * len(head)] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])
    with open(OUT / "09m-cross-burst.csv", "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(head)
        w.writerows(rows)


def chart_fdmicro():
    """12a-12e: the field-device microbench (ww-1df.41): ferrofire field-device vs stock bennu-field-deviced, one scripted provider, Docker. Median over reps unless a table says pooled."""
    if not fm.DATA:
        return
    SK = ("ferrofire", "stock")
    col = {"ferrofire": FF, "stock": STK}
    n = lambda s, pr, proto="mb": len(fm.reps(s, pr, proto))
    f1 = lambda x: "" if x is None else f"{x:.1f}"
    f3 = lambda x: "" if x is None else f"{x:.3f}"
    f0 = lambda x: "" if x is None else f"{x:,.0f}".replace(",", " ")
    leg = [sw(SNAME[s], col[s]) for s in SK]

    def bars(ax, cats, vals, ticks=None, fmt="{:.2f}", log=False):
        """vals[stack] = list per cat (None = no number); ticks[stack] = a second marker (p99)."""
        w = 0.8 / len(SK)
        for i, s in enumerate(SK):
            xs = [x + (i - (len(SK) - 1) / 2) * w for x in range(len(cats))]
            ok = [(x, v) for x, v in zip(xs, vals[s]) if v is not None]
            ax.bar([x for x, _ in ok], [v for _, v in ok], width=w * 0.9, color=col[s], ec=SURFACE, lw=1)
            if ticks:
                ax.plot([x for x, t in zip(xs, ticks[s]) if t is not None], [t for t in ticks[s] if t is not None], ls="", marker="_", color=INK, ms=7)
            for x, v in ok:
                ax.text(x, v, fmt.format(v), ha="center", va="bottom", fontsize=6.5, color=INK2, rotation=90)
        ax.set_xticks(range(len(cats)), cats)
        ax.tick_params(length=0)
        top = max(v for vs in [*vals.values(), *(ticks or {}).values()] for v in vs if v is not None)
        ax.set_ylim(*( (min(v for vs in vals.values() for v in vs if v) / 3, top * 8) if log else (0, top * 1.35)))
        if log:
            ax.set_yscale("log")

    def title(ax, t):
        ax.set_title(t, fontsize=8.5, color=INK2, loc="left")

    # 12a stream to register -------------------------------------------------------------------------------------------------------
    cases = [("S", "1", "S 15 tags, 1 Hz"), ("S", "10", "S 15 tags, 10 Hz"), ("S", "100", "S 15 tags, 100 Hz"), ("M", "100", "M 800 tags, 100 Hz"), ("L", "10", "L 8112 tags, 10 Hz")]
    cases = [c for c in cases if any(fm.pooled(s, c[0], "mb", "s2r", f"{c[1]}hz")[0] for s in SK)]
    q = {(s, c[0], c[1]): fm.pooled(s, c[0], "mb", "s2r", f"{c[1]}hz") for s in SK for c in cases}
    rows = [(c[2], SNAME[s], n(s, c[0]), len(q[s, c[0], c[1]][0]) + q[s, c[0], c[1]][1], q[s, c[0], c[1]][1], *[f3(fm.pct(q[s, c[0], c[1]][0], x)) for x in (.5, .95, .99)], f3(max(q[s, c[0], c[1]][0] or [None]))) for c in cases for s in SK]
    fig, (ax,) = fig_ax()
    bars(ax, [c[2].replace(", ", "\n") for c in cases], {s: [fm.pct(q[s, c[0], c[1]][0], .5) for c in cases] for s in SK}, {s: [fm.pct(q[s, c[0], c[1]][0], .99) for c in cases] for s in SK}, "{:.3f}")
    finish(fig, "12a-fd-stream-to-register", rows, ["case", "device", "reps", "flips", "missed", "p50 ms", "p95 ms", "p99 ms", "max ms"],
           "Stream to Modbus register: ferrofire field device vs stock, scripted provider", "milliseconds from the datagram send to the first read showing it; bar p50, tick p99; tight poll on one connection", ax, leg,
           note="Delay = read completed minus datagram sent: an upper bound, one read round trip (about 30 us) wide. Pooled over the reps, about 100 flips per rep and case.", left=0.09)

    # 12b modbus reads -------------------------------------------------------------------------------------------------------------
    rows = []
    for pr in fm.PROFILES:
        big = min(100, {"S": 10, "M": 719, "L": 6050}[pr])   # n_ir of the layouts; the request size of the large read
        for nreg in (1, big):
            for c in (1, 4, 16):
                for s in SK:
                    g = lambda m: fm.med(s, pr, "mb", "read", f"c{c}_n{nreg}", m)
                    if g("req_per_s") is not None:
                        rows.append((pr, f"FC4 x {nreg}", c, SNAME[s], n(s, pr), f0(g("req_per_s")), f1(g("us_p50")), f1(g("us_p95")), f1(g("us_p99")), f0(g("errors")), f0(g("clients_served")), f1(g("cpu_pct")), f0(g("req_per_s") / g("cpu_pct") * 100)))
    fig, axs = fig_ax(7.4, 3.5, 3, sharey=False)
    cl = (1, 4, 16)
    g = lambda s, c, k: fm.med(s, "M", "mb", "read", f"c{c}_n1", k)
    bars(axs[0], ["1", "4", "16"], {s: [g(s, c, "req_per_s") for c in cl] for s in SK}, fmt="{:.0f}")
    title(axs[0], "requests per second")
    bars(axs[1], ["1", "4", "16"], {s: [g(s, c, "us_p99") for c in cl] for s in SK}, fmt="{:.0f}")
    title(axs[1], "p99 latency, us")
    bars(axs[2], ["1", "4", "16"], {s: [None if not g(s, c, "cpu_pct") else g(s, c, "req_per_s") / g(s, c, "cpu_pct") * 100 for c in cl] for s in SK}, fmt="{:.0f}")
    title(axs[2], "requests per device-cpu-second")
    for a in axs:
        a.set_xlabel("clients", fontsize=8, color=INK2)
    finish(fig, "12b-fd-modbus-reads", rows, ["profile", "request", "clients", "device", "reps", "req/s", "p50 us", "p95 us", "p99 us", "errors", "clients served", "device cpu %", "req per device-cpu-s"],
           "Modbus read latency and throughput, 1, 4 and 16 clients", "", axs[0], leg, note="Profile M (800 tags, stream at 100 Hz), FC4 of 1 register; every profile and the 100-register read are in the table. Each client is a tight request loop on its own connection; the load generator shares the VM with the device.", left=0.1)

    # 12c write forward ------------------------------------------------------------------------------------------------------------
    rows, pr3 = [], list(fm.PROFILES)
    for pr in pr3:
        for s in SK:
            g = lambda v, m: fm.med(s, pr, "mb", "write", v, m)
            if g("spaced", "forward_ms_p50") is not None:
                rows.append((pr, SNAME[s], n(s, pr), f1(g("spaced", "forward_ms_p50")), f1(g("spaced", "forward_ms_p95")), f1(g("spaced", "forward_ms_p99")), f0(g("spaced", "lost")), f0(g("rapid", "lost")), f1(g("rapid", "forward_ms_p50")), f1(g("spaced", "ack_us_p50"))))
    fig, axs = fig_ax(7.4, 3.5, 2, sharey=False)
    bars(axs[0], pr3, {s: [fm.med(s, pr, "mb", "write", "spaced", "forward_ms_p50") for pr in pr3] for s in SK}, {s: [fm.med(s, pr, "mb", "write", "spaced", "forward_ms_p99") for pr in pr3] for s in SK}, "{:.1f}")
    title(axs[0], "write to WRITE= at provider, ms")
    bars(axs[1], pr3, {s: [None if fm.med(s, pr, "mb", "write", "rapid", "lost") is None else 100 - fm.med(s, pr, "mb", "write", "rapid", "lost") for pr in pr3] for s in SK}, fmt="{:.0f}")
    title(axs[1], "rapid writes forwarded, %")
    finish(fig, "12c-fd-write-forward", rows, ["profile", "device", "reps", "spaced p50 ms", "p95 ms", "p99 ms", "spaced lost of 60", "rapid lost of 100", "rapid p50 ms", "write ack p50 us"],
           "Write-forward: Modbus FC5 to the WRITE= request at the provider", "", axs[0], leg, note="Same 50 ms cycle-time in both configs; p50 bar, p99 tick; rapid = one write every 60-150 ms. A write counts as lost if no WRITE= naming its value arrived within 1 s (spaced) or 250 ms (rapid).")

    # 12d datagram delivery, cpu, rss ---------------------------------------------------------------------------------------------------
    load = [("M", "100", "M 800 tags, 100 Hz"), ("L", "10", "L 8112 tags, 10 Hz")]
    rows = []
    for pr, hz, lab in load:
        for s in SK:
            g = lambda m: fm.total(s, pr, "mb", "drops", f"{hz}hz", m)
            if g("udp_sent"):
                rows.append((lab, SNAME[s], n(s, pr), f0(g("udp_sent")), f0(g("udp_in")), f1(100 * (1 - g("udp_in") / g("udp_sent"))), f0(g("rcvbuf_errors")), f0(g("sender_dropped")), f1(100 * g("counter_steps_skipped") / max(1, g("counter_changes") + g("counter_steps_skipped"))), f1(100 * g("end_state_bad_regs") / max(1, g("end_state_checked")))))
    fig, axs = plt.subplots(1, 3, figsize=(7.4, 3.5))
    for a in axs:
        a.grid(axis="x", visible=False)
    cats = [l[2].replace(", ", "\n") for l in load]
    dv = lambda s: [None if not fm.total(s, pr, "mb", "drops", f"{hz}hz", "udp_sent") else 100 * fm.total(s, pr, "mb", "drops", f"{hz}hz", "udp_in") / fm.total(s, pr, "mb", "drops", f"{hz}hz", "udp_sent") for pr, hz, _ in load]
    bars(axs[0], cats, {s: dv(s) for s in SK}, fmt="{:.1f}")
    axs[0].set_ylim(0, 125)
    title(axs[0], "datagrams reaching device, %")
    cp = [(pr, "stream", f"{fm.MAIN_HZ[pr]}hz") for pr in pr3]
    bars(axs[1], pr3, {s: [fm.med(s, pr, "mb", "stream", f"{fm.MAIN_HZ[pr]}hz", "cpu_pct") for pr in pr3] for s in SK}, fmt="{:.1f}")
    title(axs[1], "device cpu, stream only, %")
    bars(axs[2], pr3, {s: [fm.med(s, pr, "mb", "stream", f"{fm.MAIN_HZ[pr]}hz", "rss_mb") for pr in pr3] for s in SK}, fmt="{:.1f}")
    title(axs[2], "device RSS, MB")
    finish(fig, "12d-fd-load", rows, ["case", "device", "reps", "datagrams sent", "reached the device", "lost %", "RcvbufErrors", "sender drops", "cycles never visible %", "registers wrong at the end %"],
           "Full-cycle streams: delivery, cpu and memory of the field device", "", axs[0], leg, note="Left: kernel UDP InDatagrams of the device's network namespace over 30 s of stream. A lost datagram is a whole chunk of the cycle.", left=0.06)
    rows = []
    for pr in pr3:
        for s in SK:
            g = lambda ph, v, m: fm.med(s, pr, "mb", ph, v, m)
            hz = f"{fm.MAIN_HZ[pr]}hz"
            if g("idle", "-", "cpu_pct") is not None:
                rows.append((pr, SNAME[s], n(s, pr), f1(g("idle", "-", "rss_mb")), f1(g("stream", hz, "rss_mb")), f1(g("drops", hz, "rss_hwm_mb")), f1(g("idle", "-", "cpu_pct")), f1(g("stream", hz, "cpu_pct")),
                             f1(g("s2r", hz, "cpu_pct")), f1(g("read", "c4_n1", "cpu_pct")), f1(g("read", "c16_n1", "cpu_pct"))))
    TABLES["12d-cpu-rss"] = "\n".join(["| " + " | ".join(h) + " |" for h in [["profile", "device", "reps", "idle RSS MB", "stream RSS MB", "peak RSS MB", "idle cpu %", "stream cpu %", "stream + 1 tight reader cpu %", "4 readers cpu %", "16 readers cpu %"]]] + ["|" + "---|" * 11] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])
    with open(OUT / "12d-cpu-rss.csv", "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["profile", "device", "reps", "idle_rss_mb", "stream_rss_mb", "peak_rss_mb", "idle_cpu_pct", "stream_cpu_pct", "stream_1reader_cpu_pct", "4readers_cpu_pct", "16readers_cpu_pct"])
        w.writerows(rows)

    # 12e dnp3 -------------------------------------------------------------------------------------------------------------------------
    rows = []
    for pr in pr3:
        for s in SK:
            g = lambda ph, m: fm.med(s, pr, "dnp3", ph, f"{fm.MAIN_HZ[pr]}hz", m)
            d, miss = fm.pooled(s, pr, "dnp3", "s2r", f"dnp3-{fm.MAIN_HZ[pr]}hz")
            if g("dnp3read", "ms_p50") is not None:
                rows.append((pr, SNAME[s], n(s, pr, "dnp3"), f0(g("dnp3read", "response_bytes")), f3(g("dnp3read", "ms_p50")), f3(g("dnp3read", "ms_p95")), f3(g("dnp3read", "ms_p99")), f0(g("dnp3tight", "polls_per_s")),
                             f1(g("dnp3tight", "cpu_pct")), f0(fm.med(s, pr, "dnp3", "idle", "-", "rss_mb")), f0(g("stream", "rss_mb")), f1(fm.med(s, pr, "dnp3", "idle", "-", "cpu_pct")), f1(g("stream", "cpu_pct")),
                             f0(fm.pct(d, .5)), f0(fm.pct(d, .95)), f0(max(d or [None])), miss))
    fig, axs = fig_ax(7.4, 3.5, 3, sharey=False)
    bars(axs[0], pr3, {s: [fm.med(s, pr, "dnp3", "dnp3read", f"{fm.MAIN_HZ[pr]}hz", "ms_p50") for pr in pr3] for s in SK}, {s: [fm.med(s, pr, "dnp3", "dnp3read", f"{fm.MAIN_HZ[pr]}hz", "ms_p99") for pr in pr3] for s in SK}, "{:.2f}", log=True)
    title(axs[0], "poll time, ms (log)")
    bars(axs[1], pr3, {s: [fm.med(s, pr, "dnp3", "dnp3tight", f"{fm.MAIN_HZ[pr]}hz", "polls_per_s") for pr in pr3] for s in SK}, fmt="{:.0f}", log=True)
    title(axs[1], "polls per second (log)")
    dd = {s: [fm.pct(fm.pooled(s, pr, "dnp3", "s2r", f"dnp3-{fm.MAIN_HZ[pr]}hz")[0], .5) for pr in pr3] for s in SK}
    bars(axs[2], pr3, dd, fmt="{:.0f}")
    title(axs[2], "stream to visible, ms")
    for a in axs[1:]:
        a.set_ylabel("")
    finish(fig, "12e-fd-dnp3", rows, ["profile", "device", "reps", "response bytes", "poll p50 ms", "p95 ms", "p99 ms", "back-to-back polls/s", "cpu % while polling", "idle RSS MB", "stream RSS MB", "idle cpu %", "stream cpu %", "stream-to-visible p50 ms", "p95 ms", "max ms", "flips never seen"],
           "DNP3 class-0 integrity poll: latency, throughput and stream-to-visible", "", axs[0], leg, note="One master, READ g60v1 (class 0), every fragment that asks for it confirmed. Poll time: p50 bar, p99 tick, 300 polls 20 ms apart; polls per second: back to back; stream to visible: p50, tight poll, probe flipped 1.5-3 s apart, pooled.", left=0.06)


def main():
    for f in (chart_age, chart_loss, chart_reorder, chart_stale, chart_fanout, chart_nng, chart_live, chart_ovs, chart_bennu, chart_control, chart_stock_intervals, chart_cross, chart_fdmicro):
        f()
    for tin, tout in (("results.md.in", ROOT / "docs/results.md"), ("control_latency.md.in", ROOT / "docs/bench/control-latency.md"), ("field_device.md.in", ROOT / "docs/bench/field-device.md")):
        text = (HERE / tin).read_text()
        for k, v in TABLES.items():
            text = text.replace("{{table:" + k + "}}", v)
        left = re.findall(r"\{\{[^}]+\}\}", text)
        assert not left, left
        tout.write_text(text)
    print("wrote", len(list(OUT.glob("*.svg"))), "charts and docs/results.md")


if __name__ == "__main__":
    main()
