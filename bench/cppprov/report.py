#!/usr/bin/env python3
"""report.py DIR [STOCK_LABEL] : markdown tables of the stub-model comparison (ww-1df.42) from docs/bench/results/cppprov/<provider>-<fd>/N<n>_R<r>_P<pub>_rep<k>.*. Stdlib only, deterministic.
Latency = t_visible - t_cmd (ms) over the commands done in time; seq pools the seq:10 and seq:50 cells, burst is the largest simultaneous cell the point count allows
(sim:100 at 100 points, sim:1000 at 1250); applied = the provider-side stamp (the stub's, or ferrofire's `Applied:` line) minus t_cmd.
STOCK_LABEL (default "stock") names the stock bennu in the rows, e.g. "bennu v2026.11.03" (ww-1df.47: the first run was bennu 6.0.0, the rerun v2026.11.03)."""
import csv, glob, gzip, json, os, re, sys
from collections import defaultdict
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import control_join as J

pct = lambda v, p: v[min(len(v) - 1, int(p * len(v)))] if v else None
f0 = lambda x: "" if x is None else f"{x:.0f}"
f1 = lambda x: "" if x is None else f"{x:.1f}"
ORDER = ["cpp:stock", "ferrofire:stock", "cpp:ferrofire", "ferrofire:ferrofire"]
NAMES = {"cpp:stock": "C++ provider + stock FD", "ferrofire:stock": "ferrofire + stock FD", "cpp:ferrofire": "C++ provider + ferrofire FD", "ferrofire:ferrofire": "ferrofire + ferrofire FD"}


def env_stats(path):
    """idle rate json, idle cpu % per role (provider = all its processes, fd = all field devices), peak rss kB per role."""
    t = open(path).read()
    rate = json.loads(re.search(r"^idle_rate (\{.*\})$", t, re.M).group(1)) if re.search(r"^idle_rate \{", t, re.M) else {}
    win = float(re.search(r"idle_window_s (\S+)", t).group(1))
    ticks = lambda lab: [int(m[1]) for m in re.finditer(rf"^{lab} \S+ ticks=(\d+)", t, re.M)]
    cpu = {}
    for role in ("prov", "fd"):
        a, b = ticks(f"idle_{role}_start"), ticks(f"idle_{role}_end")
        cpu[role] = (sum(b) - sum(a)) / win if a and len(a) == len(b) else None   # ticks are 1/100 s: ticks per second = % of one core
    hwm = defaultdict(int)
    for m in re.finditer(r"^(cell_prov|cell_fd|idle_prov_end|idle_fd_end) (?:\S+ )?(\S+) ticks=\d+ hwm_kb=(\d+)", t, re.M):
        hwm["prov" if "prov" in m[1] else "fd"] = max(hwm["prov" if "prov" in m[1] else "fd"], int(m[3]))
    return rate, cpu, hwm


def main(d, stock="stock"):
    NAME = {k: v.replace("stock FD", f"{stock} FD") for k, v in NAMES.items()}
    lat, app, cnt, rates, cpus, rss = defaultdict(list), defaultdict(list), defaultdict(lambda: [0, 0]), defaultdict(list), defaultdict(lambda: defaultdict(list)), defaultdict(lambda: defaultdict(int))
    for f in sorted(glob.glob(f"{d}/*/N*_rep*.cmds.csv")):
        pair, base = os.path.basename(os.path.dirname(f)).replace("-", ":"), os.path.basename(f)
        m = re.match(r"N(\d+)_R(\d+)_P([\d.]+)_rep(\d+)\.(.+)\.cmds\.csv", base)
        n, r, pub, rep, cell = int(m[1]), int(m[2]), m[3], m[4], m[5].replace("-", ":")
        rows = list(csv.DictReader(open(f)))
        lg = f[:-len(".cmds.csv")] + ".prov.log.gz"
        if os.path.exists(lg):
            J.join(rows, J.stamps(gzip.open(lg, "rt", errors="replace")))
        kind = "seq" if ":seq:" in cell else ("burst" if cell.endswith(f":{1000 if n >= 1000 else 100}") else "sim" + cell.rsplit(":", 1)[1])
        k = (pair, n, pub, kind)
        for row in rows:
            c = cnt[k]
            c[0] += 1
            t = int(row["t_cmd"])
            if row["done"] == "1":
                c[1] += 1
                lat[k].append((int(row["t_visible"]) - t) / 1e6)
            if row.get("t_apply_ns"):
                app[k].append((int(row["t_apply_ns"]) - t) / 1e6)
    flaps = defaultdict(lambda: [0, 0])   # (pair, n, pub) -> [flaps, commands]: the read-back showed the new state, then the old, then the new again
    for f in sorted(glob.glob(f"{d}/*/N*_rep*.cells.csv")):
        m = re.match(r"N(\d+)_R(\d+)_P([\d.]+)_rep", os.path.basename(f))
        for row in csv.DictReader(open(f)):
            c = flaps[os.path.basename(os.path.dirname(f)).replace("-", ":"), int(m[1]), m[3]]
            c[0] += int(row["flaps"])
            c[1] += int(row["issued"])
    for f in sorted(glob.glob(f"{d}/*/N*_rep*.env.txt")):
        pair = os.path.basename(os.path.dirname(f)).replace("-", ":")
        m = re.match(r"N(\d+)_R(\d+)_P([\d.]+)_rep(\d+)\.env\.txt", os.path.basename(f))
        k = (pair, int(m[1]), m[3])
        rate, cpu, hwm = env_stats(f)
        rates[k].append(rate)
        for role in ("prov", "fd"):
            cpus[k][role].append(cpu[role])
            rss[k][role] = max(rss[k][role], hwm[role])
    keys = sorted({k[:3] for k in cnt}, key=lambda k: (k[1], float(k[2]) * -1, ORDER.index(k[0]) if k[0] in ORDER else 9))
    print("### Write to visible, ms (p50 / p95 / max over all reps)\n")
    print("| points | publish s | stack | seq p50 | seq p95 | seq max | sim:10 p50 | sim:10 p95 | burst p50 | burst p95 | burst max | seq applied p50 | burst applied p50 |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for k in keys:
        g = lambda kind: sorted(lat[k + (kind,)])
        a = lambda kind: sorted(app[k + (kind,)])
        s, m, b = g("seq"), g("sim10"), g("burst")
        print(f"| {k[1]} | {k[2]} | {NAME.get(k[0], k[0])} | {f0(pct(s, .5))} | {f0(pct(s, .95))} | {f0(s[-1] if s else None)} | {f0(pct(m, .5))} | {f0(pct(m, .95))} | {f0(pct(b, .5))} | {f0(pct(b, .95))} | {f0(b[-1] if b else None)} | {f0(pct(a('seq'), .5))} | {f0(pct(a('burst'), .5))} |")
    print("\n### Burst: simultaneous writes, one Modbus client per RTU (10 RTUs), done within 10 s\n")
    print("| points | publish s | stack | burst size | issued | done | lost | lost % |\n|---|---|---|---|---|---|---|---|")
    for k in keys:
        c = cnt[k + ("burst",)]
        print(f"| {k[1]} | {k[2]} | {NAME.get(k[0], k[0])} | {1000 if k[1] >= 1000 else 100} | {c[0]} | {c[1]} | {c[0] - c[1]} | {100 * (c[0] - c[1]) / c[0] if c[0] else 0:.1f} |")
    print("\n### Flaps: the read-back showed the new state, then the old, then the new again (all cells pooled)\n")
    print("| points | publish s | stack | commands | flaps | flaps % |\n|---|---|---|---|---|---|")
    for k in keys:
        c = flaps[k]
        print(f"| {k[1]} | {k[2]} | {NAME.get(k[0], k[0])} | {c[1]} | {c[0]} | {100 * c[0] / c[1] if c[1] else 0:.1f} |")
    print("\n### Publish rate and cost of the provider, idle (nothing driving it)\n")
    print("| points | publish s | stack | publishes/s | interval mean ms | p99 ms | datagrams per publish | bytes per datagram | provider cpu % of a core | provider peak RSS MB | 10 field devices cpu % | field device peak RSS MB (largest of the 10) |\n|---|---|---|---|---|---|---|---|---|---|---|---|")
    mean = lambda v: sum(v) / len(v) if v else None
    for k in keys:
        rs = [r for r in rates[k] if r]
        dp = mean([r["datagrams"] / r["publishes"] for r in rs])
        print(f"| {k[1]} | {k[2]} | {NAME.get(k[0], k[0])} | {mean([r['publishes_per_s'] for r in rs]) or 0:.2f} | {f1(mean([r['gap_ms_mean'] for r in rs]))} | {f1(mean([r['gap_ms_p99'] for r in rs]))} | {f1(dp)} | {f0(mean([r['bytes_mean'] for r in rs]))} | "
              f"{f1(mean([c for c in cpus[k]['prov'] if c is not None]))} | {rss[k]['prov'] / 1024:.1f} | {f1(mean([c for c in cpus[k]['fd'] if c is not None]))} | {rss[k]['fd'] / 1024:.1f} |")


if __name__ == "__main__":
    main(*sys.argv[1:3])
