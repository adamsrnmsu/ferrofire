#!/usr/bin/env python3
"""Theme "stream-fidelity": raw results -> numbers (stdout, JSON) + docs/img/findings/stream-fidelity-*.svg.

    uv run --no-project --with matplotlib python bench/findings/stream-fidelity.py

Reads only docs/bench/results/{raw,raw-datagram-v1,raw-datagram-v1-d4a,live}; writes only the SVGs. Reruns are byte-identical.
Colour follows the entity (same hexes as bench/charts.py): ferrofire multicast blue, pull-bridge orange, per-RTU groups aqua;
grey stays reserved for stock bennu."""
import csv, glob, json, os, pathlib, re, statistics as st, sys

sys.dont_write_bytecode = True
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = pathlib.Path(__file__).resolve().parents[2]
RES = ROOT / "docs/bench/results"
OUT = ROOT / "docs/img/findings"
SURFACE, INK, INK2, MUTED, GRID, AXIS = "#fcfcfb", "#0b0b0b", "#52514e", "#898781", "#e1e0d9", "#c3c2b7"
MCAST, PULL, GROUPS = "#2a78d6", "#eb6834", "#1baf7a"
plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 9, "text.color": INK, "axes.labelcolor": INK2, "xtick.color": INK2,
                     "ytick.color": INK2, "axes.edgecolor": AXIS, "axes.linewidth": 0.8, "axes.facecolor": SURFACE, "figure.facecolor": SURFACE,
                     "axes.spines.top": False, "axes.spines.right": False, "axes.grid": True, "axes.axisbelow": True, "grid.color": GRID,
                     "grid.linewidth": 0.8, "svg.hashsalt": "ferrofire"})
mean = st.mean
th = lambda n: f"{n:,}".replace(",", " ")  # 35 993


def runs(d, cell):
    """Client JSONs of every rep of a cell, in rep order, each with its file stem."""
    out = []
    for f in sorted(glob.glob(f"{RES}/{d}/{cell}_r[0-9].client.json")):
        j = json.load(open(f))
        j["_file"] = f[: -len(".client.json")]
        out.append(j)
    assert out, (d, cell)
    return out


def stream(j, name):
    return next(s for s in j["streams"] if s["name"] == name)


def complete(js, name, key=lambda s: s["delivered_per_s"]):
    """Complete cycles delivered, % of the cycles published, mean over reps."""
    return mean(100 * key(stream(j, name)) * j["period_s"] for j in js)


def interp(rows, col, t):
    prev = rows[0]
    for r in rows:
        if r["t_ms"] >= t:
            if r["t_ms"] == prev["t_ms"]:
                return r[col]
            return prev[col] + (t - prev["t_ms"]) / (r["t_ms"] - prev["t_ms"]) * (r[col] - prev[col])
        prev = r
    return rows[-1][col]


def provider_load(j):
    """Provider wire MB/s (request + publish bytes) and CPU % of one core over the client's window, from the monitor CSV."""
    rows = [{k: float(v) for k, v in r.items()} for r in csv.DictReader(open(j["_file"] + ".mon.csv"))]
    t0, t1 = (j["phases"]["publish"][k] / 1e6 for k in ("start_ns", "end_ns"))
    d = lambda c: interp(rows, c, t1) - interp(rows, c, t0)
    secs = (t1 - t0) / 1e3
    return (d("req_bytes") + d("pub_bytes")) / secs / 1e6, d("cpu_ticks") / secs


# ---- numbers ---------------------------------------------------------------------------------------------------------

def live_hour():
    out = {}
    for host in ("waterway-provider", "rtu-1", "rtu-2"):
        t = (RES / f"live/{host}/cap-dg1h-{host}.txt").read_text()
        m = re.search(r"received (\d+), seq (\d+)\.\.(\d+), missing (\d+) .*?dup/reordered (\d+).*?\n\s*inter-arrival ms: p50 ([\d.]+) p99 ([\d.]+) max ([\d.]+)", t)
        g = [float(x) for x in m.groups()]
        out[host] = dict(datagrams=int(g[0]), seq_span=int(g[2] - g[1] + 1), missing=int(g[3]), dup_reordered=int(g[4]), p50_ms=g[5], p99_ms=g[6], max_ms=g[7],
                         dur_s=float(re.search(r"dur ([\d.]+)s", t)[1]))
    return out


def loss():
    """Complete cycles % under netem loss: multicast with the order-tolerant receiver (ww-d4a run) vs the pull-bridge in the same run,
    plus the first receiver (raw-datagram-v1) and the per-tag update gap (probe_gap)."""
    out = {}
    for n, p in [(100, "0.1"), (100, "0.01"), (800, "0.1")]:
        for l in ["0", "0.1", "1", "5"]:
            cell = f"C_netem_n{n}_p{p}_loss{l}"
            new, old = runs("raw-datagram-v1-d4a", cell), runs("raw-datagram-v1", cell)
            period_ms = float(p) * 1000
            gap = lambda s: mean(stream(j, s)["probe_gap"]["p99_us"] for j in new) / 1e3 / period_ms  # one tag: p99 gap between its updates, in periods
            out[f"n{n}_{round(1 / float(p))}hz_loss{l}"] = dict(
                mcast=complete(new, "whole"), pull=complete(new, "pull0"), mcast_first_receiver=complete(old, "whole"), pull_first_run=complete(old, "pull0"),
                mcast_first_run_tolerant_count=complete(old, "whole", lambda s: s["tolerant"]["delivered_per_s"]),
                datagrams_per_cycle=mean((stream(j, "whole")["datagrams"]["seen"] + stream(j, "whole")["datagrams"]["lost"]) / ((j["secs"] + 3.3) / float(p)) for j in new),
                tag_gap_p99_periods_mcast=gap("whole"), tag_gap_p99_periods_pull=gap("pull0"), reps=len(new), secs=new[0]["secs"])
    return out


def fanout():
    out = {}
    for cell in ["D_whole_n800_p0.1", "D_slices_r32_k50_p0.1", "D_slices_r32_k100_p0.1", "D_pull_r32_k50_p0.1", "D_pull_r32_k100_p0.1",
                 "D_slices_r8_k100_p0.1", "D_pull_r8_k100_p0.1"]:
        js = runs("raw-datagram-v1", cell)
        v = [provider_load(j) for j in js]
        out[cell[2:]] = dict(wire_mb_s=mean(x[0] for x in v), cpu_pct=mean(x[1] for x in v), streams=len(js[0]["streams"]), secs=js[0]["secs"], provider_tags=js[0]["provider_tags"],
                             complete_pct=mean(mean(100 * s["delivered_per_s"] * j["period_s"] for s in j["streams"]) for j in js), reps=len(js))
    return out


def age():
    out = {}
    for p, hz in [("0.1", 10), ("0.01", 100)]:
        after = [stream(j, "whole")["age_above_floor"] for j in runs("raw-datagram-v1", f"A_age_n800_p{p}")]
        out[f"{hz}hz_after"] = dict(p50_ms=mean(a["p50_us"] for a in after) / 1e3, p99_ms=mean(a["p99_us"] for a in after) / 1e3, reps=len(after))
        for d in ("raw", "raw-after-nng-fix"):
            b = [j["publish"]["idle"]["age_above_floor"] for j in runs(d, f"zmq_n800_p{p}")]
            out[f"{hz}hz_before_{d}"] = dict(p50_ms=mean(a["p50_us"] for a in b) / 1e3, p99_ms=mean(a["p99_us"] for a in b) / 1e3, reps=len(b))
    return out


def faults():
    out = {}
    for s in ("whole", "pull0"):
        v = []
        for j in runs("raw-datagram-v1", "F_kill_n100_p0.1"):
            r = json.load(open(j["_file"] + ".events.json"))["restart"][0] / 1e6
            v.append(min(x for x in stream(j, s)["complete_ms"] if x > r) - r)
        out[f"kill_restart_to_fresh_ms_{s}"] = v
    js = runs("raw-datagram-v1", "F_inject1_n100_p0.1")
    out["garbage_sent"] = sum(json.load(open(j["_file"] + ".inject.json"))["sent"] for j in js)
    out["garbage_counted"] = sum(sum(stream(j, "whole")["rejects"][k] for k in ("garbage", "wrong_group", "truncated", "stale")) for j in js)
    out["complete_pct_while_injecting"] = complete(js, "whole")
    return out


# ---- charts ----------------------------------------------------------------------------------------------------------

def frame(w, h, title, sub, note, top=0.78, bottom=0.2, left=0.09, right=0.97):
    fig, ax = plt.subplots(figsize=(w, h))
    ax.grid(axis="x", visible=False)
    fig.text(0.015, 0.975, title, ha="left", va="top", fontsize=11, fontweight="bold")
    fig.text(0.015, 0.895, sub, ha="left", va="top", fontsize=8.5, color=INK2)
    fig.text(0.015, 0.015, note, ha="left", va="bottom", fontsize=7.5, color=MUTED)
    fig.subplots_adjust(top=top, bottom=bottom, left=left, right=right)
    return fig, ax


def save(fig, name):
    OUT.mkdir(parents=True, exist_ok=True)
    fig.savefig(OUT / f"stream-fidelity-{name}.svg", format="svg", metadata={"Date": None, "Creator": "bench/findings/stream-fidelity.py"})
    if os.environ.get("FF_PREVIEW"):  # PNG copies for looking at the charts while editing; never written into the repo
        fig.savefig(pathlib.Path(os.environ["FF_PREVIEW"]) / f"stream-fidelity-{name}.png", dpi=110)
    plt.close(fig)


def chart_live(L):
    p, r1, r2 = L["waterway-provider"], L["rtu-1"], L["rtu-2"]
    assert p["missing"] == r1["missing"] == r2["missing"] == 0 and r1["datagrams"] == r1["seq_span"] and r2["datagrams"] == r2["seq_span"]
    fig, ax = frame(7.4, 3.4, f"Live range, one hour at 10 Hz: {th(p['datagrams'])} datagrams sent, {r1['missing'] + r2['missing']} missing at either RTU",
                    "Gap between consecutive datagrams, ms (one is sent every 100 ms; axis starts at 95)",
                    f"tcpdump on each host for {p['dur_s'] / 60:.0f} min, checked by sequence number: rtu-1 received {th(r1['datagrams'])}, rtu-2 {th(r2['datagrams'])}, "
                    f"duplicated or reordered {r1['dup_reordered'] + r2['dup_reordered']}.\nferrofire provider, two stock bennu RTUs, one OVS bridge, one run.")
    hosts, qs = [("sent by the provider", p), ("received at rtu-1", r1), ("received at rtu-2", r2)], [("median", "p50_ms", "o"), ("p99", "p99_ms", "D"), ("longest", "max_ms", "s")]
    ax.axhline(100, color=INK2, lw=0.8, ls=(0, (4, 3)))
    for i, (_, d) in enumerate(hosts):
        ax.plot([i, i], [d["p50_ms"], d["max_ms"]], color=MCAST, lw=2, alpha=0.35, solid_capstyle="round")
        for ql, q, mk in qs:
            ax.plot(i, d[q], mk, color=MCAST, ms=8, mec=SURFACE, mew=1.5)
            ax.text(i + 0.07, d[q] + {"p50_ms": -1.3, "p99_ms": 0.9, "max_ms": 0}[q], f"{ql} {d[q]:.1f}", ha="left", va="center", fontsize=8.5)
    ax.set_xticks(range(3), [h for h, _ in hosts])
    ax.set_ylim(95, 115)
    ax.set_yticks(range(95, 116, 5))
    ax.set_xlim(-0.4, 2.7)
    ax.tick_params(axis="x", length=0)
    save(fig, "1-live-hour")


def chart_loss(C):
    xs = ["0", "0.1", "1", "5"]
    a, b, c = C["n100_10hz_loss5"], C["n800_10hz_loss5"], C["n100_100hz_loss5"]
    fig, axs = plt.subplots(1, 3, figsize=(7.4, 3.7), sharey=True)
    fig.text(0.015, 0.975, f"At 5 % loss only {b['mcast']:.0f} % of 800-tag cycles arrive whole; pull-bridge {b['pull']:.0f} %", ha="left", va="top", fontsize=11, fontweight="bold")
    fig.text(0.015, 0.895, "Complete cycles delivered, % of cycles published, vs injected packet loss", ha="left", va="top", fontsize=8.5, color=INK2)
    fig.text(0.015, 0.015, f"Docker bench, netem loss on the provider's interface (independent, plus 1 ms jitter), {a['reps']} reps x {a['secs']:.0f} s, order-tolerant receiver (ww-d4a).\n"
             f"One tag at 5 % loss: p99 gap between updates {a['tag_gap_p99_periods_mcast']:.1f}-{max(x['tag_gap_p99_periods_mcast'] for x in (a, b, c)):.1f} periods on multicast, "
             f"{min(x['tag_gap_p99_periods_pull'] for x in (a, b, c)):.1f}-{max(x['tag_gap_p99_periods_pull'] for x in (a, b, c)):.1f} on the pull-bridge.", ha="left", va="bottom", fontsize=7.5, color=MUTED)
    for ax, (key, head) in zip(axs, [("n100_10hz", f"100 tags, 10 Hz\n{a['datagrams_per_cycle']:.0f} datagrams per cycle"), ("n800_10hz", f"800 tags, 10 Hz\n{b['datagrams_per_cycle']:.0f} datagrams per cycle"),
                                     ("n100_100hz", f"100 tags, 100 Hz\n{c['datagrams_per_cycle']:.0f} datagrams per cycle")]):
        ax.grid(axis="x", visible=False)
        m, p = [C[f"{key}_loss{l}"]["mcast"] for l in xs], [C[f"{key}_loss{l}"]["pull"] for l in xs]
        ax.plot(range(4), p, color=PULL, lw=2, marker="s", ms=6, mec=SURFACE, mew=1.2)
        ax.plot(range(4), m, color=MCAST, lw=2, marker="o", ms=6.5, mec=SURFACE, mew=1.2)
        hi = m[3] > p[3]
        for v, top in ((m[3], hi), (p[3], not hi)):
            ax.text(3.18, v + (3 if top else -3), f"{v:.0f} %", ha="left", va="bottom" if top else "top", fontsize=8.5)
        ax.set_title(head, fontsize=8.5, color=INK2, loc="left", pad=6)
        ax.set_xticks(range(4), [f"{l} %" for l in xs])
        ax.set_xlim(-0.3, 3.95)
        ax.set_ylim(0, 108)
    axs[1].set_xlabel("injected packet loss")
    for y, col, mk, label in ((34, MCAST, "o", "multicast stream"), (18, PULL, "s", "pull-bridge (polls REQ/REP)")):
        axs[0].plot([0.2, 0.7], [y, y], color=col, lw=2, marker=mk, ms=6.5, mec=SURFACE, mew=1.2, markevery=[1])
        axs[0].text(0.9, y, label, va="center", fontsize=8.5)
    fig.subplots_adjust(top=0.72, bottom=0.27, left=0.07, right=0.985, wspace=0.08)
    save(fig, "2-loss")


def chart_fanout(D):
    rows = [("one stream, all RTUs listen\n(all 800 tags)", D["whole_n800_p0.1"], MCAST), ("one multicast group per RTU\n(32 x 100 tags)", D["slices_r32_k100_p0.1"], GROUPS),
            ("one pull-bridge per RTU\n(32 x 100 tags)", D["pull_r32_k100_p0.1"], PULL)]
    w, g, p = (r[1] for r in rows)
    fig, ax = frame(7.4, 3.2, f"32 RTUs at 10 Hz: one shared stream {w['wire_mb_s']:.2f} MB/s, per-RTU groups {g['wire_mb_s']:.2f}, polling {p['wire_mb_s']:.2f}",
                    "Provider network traffic, MB/s (requests + publishes)",
                    f"Docker bench, one VM, {w['reps']} reps x {w['secs']:.0f} s, the same 800-tag model in all three; {min(w['complete_pct'], g['complete_pct'], p['complete_pct']):.0f} % of cycles delivered. "
                    f"\nShared stream measured with {w['streams']} listener; per-RTU rows send each RTU 100 tags. With 8 RTUs x 100 tags: groups {D['slices_r8_k100_p0.1']['wire_mb_s']:.2f}, polling {D['pull_r8_k100_p0.1']['wire_mb_s']:.2f} MB/s.", left=0.3, bottom=0.24, top=0.76)
    for i, (label, d, col) in enumerate(rows):
        ax.barh(i, d["wire_mb_s"], 0.5, color=col)
        ax.text(d["wire_mb_s"] + 0.02, i, f"{d['wire_mb_s']:.2f} MB/s  ·  {d['cpu_pct']:.1f} % of a core", va="center", fontsize=8.5)
    ax.set_yticks(range(3), [r[0] for r in rows])
    ax.invert_yaxis()
    ax.grid(axis="y", visible=False)
    ax.grid(axis="x", visible=True)
    ax.set_xlim(0, max(d["wire_mb_s"] for _, d, _ in rows) * 1.55)
    ax.set_xlabel("MB/s")
    ax.tick_params(axis="y", length=0)
    save(fig, "3-fanout")


def main():
    N = dict(live_hour=live_hour(), loss=loss(), fanout=fanout(), age=age(), faults=faults())
    chart_live(N["live_hour"])
    chart_loss(N["loss"])
    chart_fanout(N["fanout"])
    print(json.dumps(N, indent=1, sort_keys=True, default=lambda x: round(x, 3)))


if __name__ == "__main__":
    main()
