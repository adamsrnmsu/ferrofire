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
    for e in ("docker", "range"):
        for s in ("ferrofire", "stock"):
            for kind, mode in (("breaker", "seq"), ("breaker", "sim"), ("gen", "seq"), ("gen", "sim")):
                ph = [q for q in phs(e, s) if q["cell"].startswith(f"{kind}:{mode}:")]
                t = lambda k: int(sum(q[k] for q in ph))
                rows.append((e, SNAME[s], f"{kind} {mode}", t("issued"), t("lost"), t("wrong_final"), t("wrong_provider"), t("wrong_collateral"), t("order_violations"), t("flaps"), t("write_errors"), len(ph)))
    TABLES["09g-lost-wrong"] = "\n".join(["| where | stack | family | commands | lost | wrong final | wrong at provider | wrong collateral | order violations | flaps | write errors | phases |", "|" + "---|" * 12] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])
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


def main():
    for f in (chart_age, chart_loss, chart_reorder, chart_stale, chart_fanout, chart_nng, chart_live, chart_ovs, chart_bennu, chart_control):
        f()
    for tin, tout in (("results.md.in", ROOT / "docs/results.md"), ("control_latency.md.in", ROOT / "docs/bench/control-latency.md")):
        text = (HERE / tin).read_text()
        for k, v in TABLES.items():
            text = text.replace("{{table:" + k + "}}", v)
        left = re.findall(r"\{\{[^}]+\}\}", text)
        assert not left, left
        tout.write_text(text)
    print("wrote", len(list(OUT.glob("*.svg"))), "charts and docs/results.md")


if __name__ == "__main__":
    main()
