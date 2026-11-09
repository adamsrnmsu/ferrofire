#!/usr/bin/env python3
"""Raw runs -> docs/bench/results/{runs,aggregate}.csv, docs/bench/img/*.png, docs/bench/transport-comparison.md.

    uv run --with matplotlib python bench/report.py        (also: make bench-report)

Every number in the report comes from docs/bench/results/raw/*.client.json and *.mon.csv; the narrative is
bench/report.md.in with `{{table:NAME}}` placeholders filled from the aggregate."""
import csv, glob, json, math, pathlib, re, statistics as st

ROOT = pathlib.Path(__file__).resolve().parent.parent
RES, IMG = ROOT / "docs/bench/results", ROOT / "docs/bench/img"
TR, COLOR = ["zmq", "nng"], {"zmq": "#0072B2", "nng": "#D55E00"}
HZ = {"0.1": 10, "0.01": 100}


def interp(rows, col, t):
    """Cumulative counter `col` at epoch-ms `t`, linear between bracketing monitor rows."""
    prev = rows[0]
    for r in rows:
        if r["t_ms"] >= t:
            if r["t_ms"] == prev["t_ms"]:
                return r[col]
            f = (t - prev["t_ms"]) / (r["t_ms"] - prev["t_ms"])
            return prev[col] + f * (r[col] - prev[col])
        prev = r
    return rows[-1][col]


def run_metrics(cj, mon):
    m, ph = {}, cj["phases"]
    for op, key in [("read", "read"), ("query", "query"), ("wr", "write.write_rtt"), ("rb", "write.readback_rtt"), ("vis", "write.write_to_visible")]:
        d = cj
        for k in key.split("."):
            d = d[k]
        for s in ("p50", "p95", "p99", "max", "mean"):
            m[f"{op}_{s}_us"] = d[f"{s}_us"]
        m[f"{op}_per_s"] = d["rate_per_s"]
        m[f"{op}_n"] = d["n"]
    m["errors"] = cj["read"]["errors"] + cj["query"]["errors"] + cj["write"]["errors"]
    m["query_reply_bytes"] = cj["query"]["reply_bytes"]
    p = cj["publish"]["idle"]
    m.update(pub_cycles_per_s=p["cycles_per_s"], pub_chunks_per_cycle=p["chunks_per_cycle"], pub_loss_pct=p["loss_pct"],
             pub_payload_Bps=p["payload_bytes_per_s"], pub_tags_per_s=p["tags_per_s"])
    for k, s in [("spread", "cycle_spread"), ("ia", "interarrival"), ("age", "age_above_floor")]:
        for q in ("p50", "p99", "max", "stdev"):
            m[f"pub_{k}_{q}_us"] = p[s][f"{q}_us"]
    # wire traffic and CPU per phase from the monitor
    def win(name):
        w = ph[name]
        t0, t1 = w["start_ns"] / 1e6, w["end_ns"] / 1e6
        d = lambda c: interp(mon, c, t1) - interp(mon, c, t0)
        return d, (t1 - t0) / 1e3
    d, secs = win("publish")
    cyc = p["cycles"]
    m.update(net_pub_Bps=d("pub_bytes") / secs, net_pub_pps=d("pub_pkts") / secs, net_pub_B_cycle=d("pub_bytes") / cyc,
             net_pub_pkts_cycle=d("pub_pkts") / cyc, net_other_Bps=d("oth_bytes") / secs,
             # the client sends no requests in this phase; req bytes seen here are interpolation leakage from the next phase
             net_total_idle_Bps=(d("pub_bytes") + d("oth_bytes")) / secs)
    for name, nreq in [("read", cj["read"]["n"]), ("query", cj["query"]["n"]), ("write", cj["write"]["write_rtt"]["n"] + cj["write"]["readback_rtt"]["n"])]:
        d, secs = win(name)
        m[f"net_{name}_req_Bps"], m[f"net_{name}_req_pps"] = d("req_bytes") / secs, d("req_pkts") / secs
        m[f"net_{name}_B_per_req"], m[f"net_{name}_pkts_per_req"] = d("req_bytes") / nreq, d("req_pkts") / nreq
        m[f"net_{name}_total_Bps"] = (d("req_bytes") + d("pub_bytes") + d("oth_bytes")) / secs
    for name in ("publish", "read", "query", "write"):
        d, secs = win(name)
        m[f"cpu_{name}_pct"] = d("cpu_ticks") / 100 / secs * 100  # CLK_TCK = 100; percent of one core
    w = ph["publish"]
    rss = [r["rss_kb"] for r in mon if w["start_ns"] / 1e6 <= r["t_ms"] <= w["end_ns"] / 1e6]
    m["rss_mb_mean"], m["rss_mb_max"] = st.mean(rss) / 1024, max(rss) / 1024
    m["max_frame"], m["mon_drops"] = mon[-1]["max_frame"], mon[-1]["drops"]
    return m


def load():
    runs = []
    for f in sorted(glob.glob(str(RES / "raw/*.client.json"))):
        name = pathlib.Path(f).name.removesuffix(".client.json")
        tr, n, p, rep = re.fullmatch(r"(\w+)_n(\d+)_p([\d.]+)_r(\d+)", name).groups()
        cj = json.load(open(f))
        mon = [{k: float(v) for k, v in r.items()} for r in csv.DictReader(open(f"{RES}/raw/{name}.mon.csv"))]
        runs.append(dict(transport=tr, n=int(n), hz=HZ[p], rep=int(rep), **run_metrics(cj, mon)))
    return runs


def aggregate(runs):
    agg = {}
    keys = [k for k in runs[0] if k not in ("transport", "n", "hz", "rep")]
    for tr in TR:
        for hz in HZ.values():
            for n in sorted({r["n"] for r in runs}):
                rs = [r for r in runs if (r["transport"], r["hz"], r["n"]) == (tr, hz, n)]
                if rs:
                    agg[tr, hz, n] = {k: (st.mean(r[k] for r in rs), st.stdev(r[k] for r in rs) if len(rs) > 1 else 0.0,
                                          min(r[k] for r in rs), max(r[k] for r in rs)) for k in keys} | {"reps": len(rs)}
    return agg


def write_csv(runs, agg):
    with open(RES / "runs.csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(runs[0]))
        w.writeheader()
        w.writerows(runs)
    keys = [k for k in runs[0] if k not in ("transport", "n", "hz", "rep")]
    with open(RES / "aggregate.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["transport", "hz", "n", "reps", "metric", "mean", "stdev", "min", "max"])
        for (tr, hz, n), a in sorted(agg.items()):
            for k in keys:
                w.writerow([tr, hz, n, a["reps"], k, *[f"{x:.6g}" for x in a[k]]])


# ---- tables ----------------------------------------------------------------------------------------------------

def num(x):
    a = abs(x)
    return f"{x:,.0f}" if a >= 100 else f"{x:.1f}" if a >= 10 else f"{x:.2f}" if a >= 1 else f"{x:.3f}"


def cell(a, k, sd=True):
    m, s = a[k][0], a[k][1]
    return f"{num(m)} ± {num(s)}" if sd else num(m)


def table(agg, metrics, sd=True):
    """Rows: rate x N. Columns: each metric as zmq | nng."""
    head = "| Hz | N | " + " | ".join(f"{lab} zmq | {lab} nng" for _, lab in metrics) + " |\n"
    out = head + "|" + "---|" * (2 + 2 * len(metrics)) + "\n"
    for hz in HZ.values():
        for n in sorted({k[2] for k in agg}):
            cells = []
            for k, _ in metrics:
                cells += [cell(agg[t, hz, n], k, sd) if (t, hz, n) in agg else "-" for t in TR]
            out += f"| {hz} | {n} | " + " | ".join(cells) + " |\n"
    return out


def runs_table(runs):
    """Per-run READ p50 (µs) and READ/s, reps 1-3 side by side: shows modes that mean ± sd hides."""
    out = "| Hz | N | zmq p50 r1 / r2 / r3 | nng p50 r1 / r2 / r3 | zmq READ/s r1 / r2 / r3 | nng READ/s r1 / r2 / r3 |\n|---|---|---|---|---|---|\n"
    for hz in HZ.values():
        for n in sorted({r["n"] for r in runs}):
            c = []
            for k, f in (("read_p50_us", num), ("read_per_s", lambda x: f"{x:,.0f}")):
                for t in TR:
                    rs = sorted((r for r in runs if (r["transport"], r["hz"], r["n"]) == (t, hz, n)), key=lambda r: r["rep"])
                    c.append(" / ".join(f(r[k]) for r in rs))
            out += f"| {hz} | {n} | {c[0]} | {c[1]} | {c[2]} | {c[3]} |\n"
    return out


def lat(op):
    return [(f"{op}_{q}_us", f"{q}") for q in ("p50", "p95", "p99", "max")]


TABLES = {
    "lat_read": lat("read"), "lat_query": lat("query"), "lat_wr": lat("wr"), "lat_rb": lat("rb"), "lat_vis": lat("vis"),
    "rps": [("read_per_s", "READ/s"), ("query_per_s", "QUERY/s"), ("wr_per_s", "write+readback loops/s")],
    "pub_delivery": [("pub_cycles_per_s", "cycles/s"), ("pub_chunks_per_cycle", "chunks/cycle"), ("pub_loss_pct", "loss %"),
                     ("pub_spread_p50_us", "spread p50 µs"), ("pub_spread_p99_us", "spread p99 µs")],
    "pub_timing": [("pub_ia_stdev_us", "interarrival sd µs"), ("pub_ia_p99_us", "interarrival p99 µs"),
                   ("pub_age_p50_us", "age p50 µs"), ("pub_age_p99_us", "age p99 µs")],
    "net_pub": [("net_pub_Bps", "B/s"), ("net_pub_pps", "pkts/s"), ("net_pub_B_cycle", "B/cycle"), ("net_pub_pkts_cycle", "pkts/cycle")],
    "net_req": [("net_read_B_per_req", "READ B/req"), ("net_read_pkts_per_req", "READ pkts/req"),
                ("net_query_B_per_req", "QUERY B/req"), ("net_query_pkts_per_req", "QUERY pkts/req"),
                ("net_write_B_per_req", "WRITE+READ B/req"), ("net_write_pkts_per_req", "WRITE+READ pkts/req")],
    "net_total": [("net_total_idle_Bps", "publish-only B/s"), ("net_read_total_Bps", "during READ B/s"), ("net_query_total_Bps", "during QUERY B/s")],
    "cpu": [("cpu_publish_pct", "publish-only %"), ("cpu_read_pct", "READ %"), ("cpu_query_pct", "QUERY %"), ("cpu_write_pct", "WRITE %")],
    "rss": [("rss_mb_mean", "RSS MB mean"), ("rss_mb_max", "RSS MB max")],
}


# ---- charts ----------------------------------------------------------------------------------------------------

def charts(agg, runs):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    IMG.mkdir(parents=True, exist_ok=True)
    ns = sorted({k[2] for k in agg})
    ls = {10: "-", 100: "--"}
    mk = {10: "o", 100: "s"}

    def line(ax, tr, hz, k, label=None, **kw):
        y = [agg[tr, hz, n][k][0] for n in ns]
        lo = [agg[tr, hz, n][k][2] for n in ns]
        hi = [agg[tr, hz, n][k][3] for n in ns]
        ax.plot(ns, y, ls[hz], marker=mk[hz], color=COLOR[tr], label=label, **kw)
        ax.fill_between(ns, lo, hi, color=COLOR[tr], alpha=0.12, lw=0)

    def finish(ax, title, ylabel, logy=True):
        ax.set_xscale("log")
        ax.set_xticks(ns, [str(n) for n in ns])
        if logy:
            ax.set_yscale("log")
        ax.set_xlabel("published tags (N)")
        ax.set_ylabel(ylabel)
        ax.set_title(title, fontsize=10)
        ax.grid(True, which="both", alpha=0.25)

    def legend(fig, ax):
        fig.legend(*ax.get_legend_handles_labels(), loc="lower center", ncol=4, frameon=False)

    # 1. request latency
    fig, axs = plt.subplots(2, 2, figsize=(10, 7.5), sharex=True)
    for r, hz in enumerate((10, 100)):
        for c, (op, name) in enumerate((("read", "READ one tag"), ("query", "QUERY whole tag list"))):
            ax = axs[r][c]
            for tr in TR:
                line(ax, tr, hz, f"{op}_p50_us", f"{tr} p50")
                y = [agg[tr, hz, n][f"{op}_p99_us"][0] for n in ns]
                ax.plot(ns, y, ":", marker=mk[hz], color=COLOR[tr], label=f"{tr} p99")
            finish(ax, f"{name}, publish at {hz} Hz", "round-trip latency (µs, log)")
    fig.suptitle("Request/reply latency vs tag count (one client, closed loop; shaded = min-max over 3 runs)", fontsize=11)
    legend(fig, axs[0][0]); fig.tight_layout(rect=(0, 0.04, 1, 0.97)); fig.savefig(IMG / "latency.png", dpi=130); plt.close(fig)

    # 1b. per-run READ p50: nng is bimodal, so show every run instead of mean ± sd
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.6), sharey=True)
    for ax, hz in zip(axs, (10, 100)):
        for i, tr in enumerate(TR):
            rs = [r for r in runs if r["transport"] == tr and r["hz"] == hz]
            ax.scatter([ns.index(r["n"]) + (i - 0.5) * 0.18 + (r["rep"] - 2) * 0.03 for r in rs], [r["read_p50_us"] for r in rs],
                       color=COLOR[tr], label=tr, s=36, alpha=0.85)
        ax.set_xticks(range(len(ns)), [str(n) for n in ns])
        ax.set_xlabel("published tags (N)")
        ax.set_title(f"READ p50 per run, publish at {hz} Hz (3 runs per dot column)", fontsize=10)
        ax.grid(True, alpha=0.25)
    axs[0].set_ylabel("READ median latency (µs)")
    fig.tight_layout(rect=(0, 0.08, 1, 1)); legend(fig, axs[0]); fig.savefig(IMG / "latency_runs.png", dpi=130); plt.close(fig)

    # 2. wire traffic
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.6))
    for tr in TR:
        for hz in (10, 100):
            line(axs[0], tr, hz, "net_pub_Bps", f"{tr} {hz} Hz")
            line(axs[1], tr, hz, "net_pub_pps", f"{tr} {hz} Hz")
    finish(axs[0], "Publish traffic on the wire (L2 frames, both directions)", "bytes per second (log)")
    finish(axs[1], "Publish packets on the wire", "packets per second (log)")
    fig.tight_layout(rect=(0, 0.08, 1, 1)); legend(fig, axs[0]); fig.savefig(IMG / "traffic_publish.png", dpi=130); plt.close(fig)

    # 3. request wire cost per request
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.6))
    for tr in TR:
        for op, hz, lsx in (("read", 10, "-"), ("query", 10, "--")):
            y = [agg[tr, hz, n][f"net_{op}_B_per_req"][0] for n in ns]
            axs[0].plot(ns, y, lsx, marker="o", color=COLOR[tr], label=f"{tr} {op.upper()}")
            y = [agg[tr, hz, n][f"net_{op}_pkts_per_req"][0] for n in ns]
            axs[1].plot(ns, y, lsx, marker="o", color=COLOR[tr], label=f"{tr} {op.upper()}")
    finish(axs[0], "Wire bytes per request+reply (L2-L4 included)", "bytes per request (log)")
    finish(axs[1], "Wire packets per request+reply", "packets per request (log)")
    fig.tight_layout(rect=(0, 0.08, 1, 1)); legend(fig, axs[0]); fig.savefig(IMG / "traffic_request.png", dpi=130); plt.close(fig)

    # 4. provider CPU and RSS
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.6))
    for tr in TR:
        for hz in (10, 100):
            line(axs[0], tr, hz, "cpu_publish_pct", f"{tr} {hz} Hz")
            line(axs[1], tr, hz, "rss_mb_mean", f"{tr} {hz} Hz")
    finish(axs[0], "Provider CPU, publish only (engine + publisher)", "CPU (% of one core, log)")
    finish(axs[1], "Provider resident memory", "RSS (MB)", logy=False)
    fig.tight_layout(rect=(0, 0.08, 1, 1)); legend(fig, axs[0]); fig.savefig(IMG / "cpu_rss.png", dpi=130); plt.close(fig)

    # 5. delivery: cycle spread
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.6))
    for tr in TR:
        for hz in (10, 100):
            line(axs[0], tr, hz, "pub_spread_p50_us", f"{tr} {hz} Hz")
            line(axs[1], tr, hz, "pub_loss_pct", f"{tr} {hz} Hz")
    finish(axs[0], "Time to receive a whole cycle (first to last chunk, median)", "spread (µs, log)")
    finish(axs[1], "Cycles not fully received", "loss (% of expected cycles)", logy=False)
    fig.tight_layout(rect=(0, 0.08, 1, 1)); legend(fig, axs[0]); fig.savefig(IMG / "publish_delivery.png", dpi=130); plt.close(fig)


if __name__ == "__main__":
    runs = load()
    agg = aggregate(runs)
    write_csv(runs, agg)
    charts(agg, runs)
    tpl = (ROOT / "bench/report.md.in")
    if tpl.exists():
        md = tpl.read_text().replace("{{table:read_runs}}", runs_table(runs))
        md = re.sub(r"\{\{table:(\w+)(:nosd)?\}\}", lambda m: table(agg, TABLES[m[1]], sd=not m[2]), md)
        md = md.replace("{{runs}}", str(len(runs)))
        (ROOT / "docs/bench/transport-comparison.md").write_text(md)
    print(f"{len(runs)} runs, {len(agg)} cells")
