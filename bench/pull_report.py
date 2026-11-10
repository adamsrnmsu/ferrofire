#!/usr/bin/env python3
"""Pull-bridge prototype results (ww-otj.11): docs/bench/results/raw-pull/*.client.json + *.mon.csv -> pull.csv and a markdown table.

    python3 bench/pull_report.py        (stdlib only)

Per cell (mode, transport, RTUs x tags, Hz), mean over reps: bytes/s on the provider link (all frames in the provider's
namespace, L2-L4 included), provider CPU (% of one core), loss (mean and max over the RTUs' subscribers), staleness
(`age above floor` across all RTUs, see bench/README.md). Reference rows: the same metrics for the existing single-subscriber push
runs (zmq multicast = one copy however many subscribers; nng = one copy per subscriber, so the table also shows nng x R)."""
import csv, glob, json, pathlib, re, statistics as st, sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import report  # noqa: E402  (its interp / load; matplotlib is only imported inside its chart functions)

RES = report.RES
HZ = {"0.1": 10, "0.01": 100}


def cell_metrics(cj, mon):
    w = cj["phases"]["publish"]
    t0, t1 = w["start_ns"] / 1e6, w["end_ns"] / 1e6
    d = lambda c: report.interp(mon, c, t1) - report.interp(mon, c, t0)
    secs = (t1 - t0) / 1e3
    loss = [r["loss_pct"] for r in cj["rtu"]]
    a = cj["merged_age"]
    return dict(link_Bps=(d("req_bytes") + d("pub_bytes") + d("oth_bytes")) / secs, link_pps=(d("req_pkts") + d("pub_pkts") + d("oth_pkts")) / secs,
                cpu_pct=d("cpu_ticks") / secs, loss_mean=st.mean(loss), loss_max=max(loss), age_p50_ms=a["p50_us"] / 1e3, age_p99_ms=a["p99_us"] / 1e3,
                age_max_ms=a["max_us"] / 1e3, rss_mb=mon[-1]["rss_kb"] / 1024)


def load_pull():
    out = {}
    for f in sorted(glob.glob(str(RES / "raw-pull" / "*.client.json"))):
        name = pathlib.Path(f).name.removesuffix(".client.json")
        mode, tr, n, p, rep = re.fullmatch(r"(\w+?)_(\w+?)_n(\d+x\d+)_p([\d.]+)_r(\d+)", name).groups()
        cj = json.load(open(f))
        mon = [{k: float(v) for k, v in r.items()} for r in csv.DictReader(open(f"{RES}/raw-pull/{name}.mon.csv"))]
        out.setdefault((mode, tr, n, HZ[p]), []).append(cell_metrics(cj, mon))
    return out


def mean(rs, k):
    return st.mean(r[k] for r in rs)


def push_reference(rtus):
    """Existing single-subscriber push runs at 800 tags (after the nng fix for nng); zmq is one multicast stream for any subscriber count."""
    ref = []
    for tr, raw in (("zmq", "raw"), ("nng", "raw-after-nng-fix")):
        for hz in (10, 100):
            rs = [r for r in report.load(raw) if (r["transport"], r["n"], r["hz"]) == (tr, 800, hz)]
            if rs:
                m = lambda k: st.mean(r[k] for r in rs)
                scale = rtus if tr == "nng" else 1
                ref.append((f"push {tr} (1 subscriber x{scale})" if tr == "nng" else "push zmq (multicast, any subscribers)", hz, scale * m("net_pub_Bps"),
                            m("cpu_publish_pct"), m("pub_loss_pct"), m("pub_age_p50_us") / 1e3, m("pub_age_p99_us") / 1e3))
    return ref


def main():
    cells = load_pull()
    if not cells:
        sys.exit("no runs in docs/bench/results/raw-pull (MODE=pull python3 bench/run.py)")
    rows = [dict(mode=m, transport=tr, rtus_x_tags=n, hz=hz, reps=len(rs), **{k: mean(rs, k) for k in rs[0]}) for (m, tr, n, hz), rs in sorted(cells.items())]
    with open(RES / "pull.csv", "w", newline="") as f:
        w = csv.DictWriter(f, rows[0].keys())
        w.writeheader()
        w.writerows(rows)
    print("| mode | transport | RTUs x tags | Hz | reps | provider link B/s | provider CPU % | loss % mean (max) | staleness p50 / p99 / max ms |\n|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        print(f"| {r['mode']} | {r['transport']} | {r['rtus_x_tags']} | {r['hz']} | {r['reps']} | {r['link_Bps']:,.0f} | {r['cpu_pct']:.1f} | "
              f"{r['loss_mean']:.3f} ({r['loss_max']:.3f}) | {r['age_p50_ms']:.1f} / {r['age_p99_ms']:.1f} / {r['age_max_ms']:.1f} |")
    print("\nReference, existing push runs (800 tags to ONE subscriber; nng scaled by the RTU count to what R subscribers would cost):\n")
    print("| push reference | Hz | provider publish B/s | provider CPU % | loss % | staleness p50 / p99 ms |\n|---|---|---|---|---|---|")
    for rtus in sorted({int(n.split("x")[0]) for (_, _, n, _) in cells}):
        for lab, hz, bps, cpu, loss, p50, p99 in push_reference(rtus):
            print(f"| {lab} | {hz} | {bps:,.0f} | {cpu:.1f} | {loss:.3f} | {p50:.1f} / {p99:.1f} |")


if __name__ == "__main__":
    main()
