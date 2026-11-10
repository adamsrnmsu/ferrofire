#!/usr/bin/env python3
"""docs/bench/results/raw-datagram-v1/* -> docs/bench/datagram-v1-bench.md (stdlib only).

    python3 bench/dg_report.py        (also: make bench-dg-report)

Every number in the tables is computed from the raw files; the prose is bench/datagram_report.md.in, with `{{NAME}}`
placeholders filled from the tables. The earlier report (transport-comparison.md) and its raw data are only read (section A "before")."""
import csv, glob, json, pathlib, re, statistics as st
from collections import defaultdict

ROOT = pathlib.Path(__file__).resolve().parent.parent
RES = ROOT / "docs/bench/results"
RAW = RES / "raw-datagram-v1"


def load(prefix):
    """{cell name without _rN: [client json, ...]} for cells starting with prefix, reps in order."""
    out = defaultdict(list)
    for f in sorted(glob.glob(f"{RAW}/{prefix}*.client.json")):
        name = re.sub(r"_r\d+\.client\.json$", "", pathlib.Path(f).name)
        j = json.load(open(f))
        j["_file"] = f[: -len(".client.json")]
        out[name].append(j)
    return out


def stream(j, name):
    return next(s for s in j["streams"] if s["name"] == name)


def ms(us):
    return f"{us / 1000:.1f}" if us < 1e5 else f"{us / 1000:.0f}"


def mean_sd(xs, f=lambda x: f"{x:.1f}"):
    xs = list(xs)
    return f(st.mean(xs)) if len(xs) < 2 else f"{f(st.mean(xs))} ± {f(st.stdev(xs))}"


def stat(js, name, path, q, scale=ms):
    """mean over reps of stream[name][path][q] (a latency summary), formatted by scale."""
    v = []
    for j in js:
        d = stream(j, name)
        for k in path.split("."):
            d = d[k]
        v.append(d[q])
    return scale(st.mean(v))


def worst(js, name, path, q):
    return ms(max(stream(j, name)[path][q] for j in js))


def table(head, rows):
    return "\n".join(["| " + " | ".join(head) + " |", "|" + "---|" * len(head)] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])


def interp(rows, col, t):
    prev = rows[0]
    for r in rows:
        if r["t_ms"] >= t:
            if r["t_ms"] == prev["t_ms"]:
                return r[col]
            return prev[col] + (t - prev["t_ms"]) / (r["t_ms"] - prev["t_ms"]) * (r[col] - prev[col])
        prev = r
    return rows[-1][col]


def mon_rates(j):
    """Provider-netns wire B/s and packets/s (request + publish classes), CPU % of one core and RSS MB over the client's window."""
    rows = [{k: float(v) for k, v in r.items()} for r in csv.DictReader(open(j["_file"] + ".mon.csv"))]
    w = j["phases"]["publish"]
    t0, t1 = w["start_ns"] / 1e6, w["end_ns"] / 1e6
    d = lambda c: interp(rows, c, t1) - interp(rows, c, t0)
    secs = (t1 - t0) / 1e3
    return dict(bps=(d("req_bytes") + d("pub_bytes")) / secs, pps=(d("req_pkts") + d("pub_pkts")) / secs, pub_bps=d("pub_bytes") / secs,
                req_bps=d("req_bytes") / secs, cpu=d("cpu_ticks") / 100 / secs * 100, rss=max(r["rss_kb"] for r in rows) / 1024)


# ---- A: age -----------------------------------------------------------------------------------------------------

def section_a():
    rows = []
    for p, hz in [("0.1", 10), ("0.01", 100)]:
        for label, d in [("before (raw/, original build)", "raw"), ("before (raw-after-nng-fix/)", "raw-after-nng-fix")]:
            js = [json.load(open(f)) for f in sorted(glob.glob(f"{RES}/{d}/zmq_n800_p{p}_r*.client.json"))]
            if js:
                a = lambda k, q: mean_sd((j["publish"]["idle"][k][q] for j in js), ms)
                rows.append((hz, label, len(js), a("age_above_floor", "p50_us"), a("age_above_floor", "p99_us"), a("age_above_floor", "max_us"),
                             a("cycle_spread", "p50_us"), "-", "-"))
        js = load("A_age_n800_p" + p + "_")[f"A_age_n800_p{p}"]
        a = lambda k, q: mean_sd((stream(j, "whole")[k][q] for j in js), ms)
        rows.append((hz, "**after (sync publish, this run)**", len(js), a("age_above_floor", "p50_us"), a("age_above_floor", "p99_us"),
                     a("age_above_floor", "max_us"), a("cycle_spread", "p50_us"), a("transit", "p50_us"), a("transit", "p99_us")))
    return table(["Hz", "build", "reps", "age p50 ms", "age p99 ms", "age max ms", "cycle spread p50 ms", "transit p50 ms", "transit p99 ms"], rows)


# ---- B/C: seq-gap loss and netem --------------------------------------------------------------------------------

def netem_dropped(js):
    """Packets the netem qdisc dropped, mean over reps (from `tc -s qdisc` after the run)."""
    v = []
    for j in js:
        m = re.search(r"netem .*?\n? ?Sent \d+ bytes \d+ pkt \(dropped (\d+)", open(j["_file"] + ".tc.txt").read(), re.S)
        v.append(int(m.group(1)) if m else float("nan"))
    return st.mean(v)


def section_c():
    c = load("C_netem_")
    out = {}
    for n, p in [(100, "0.1"), (100, "0.01"), (800, "0.1")]:
        rows = []
        for loss in ["0", "0.1", "1", "5"]:
            js = c.get(f"C_netem_n{n}_p{p}_loss{loss}", [])
            if not js:
                continue
            per_s = 1 / float(p)
            m, pl = (lambda k: mean_sd((stream(j, "whole")[k] for j in js), lambda x: f"{x:.2f}")), "pull0"
            # datagrams per cycle, estimated: everything seen or missed by the seq range over the cycles of the whole run (3 s warm-up + window + 0.3 s)
            chunks = st.mean((stream(j, "whole")["datagrams"]["seen"] + stream(j, "whole")["datagrams"]["lost"]) / ((j["secs"] + 3.3) / float(p)) for j in js)
            exp = 100 * (1 - float(loss) / 100) ** chunks
            dg = mean_sd((stream(j, "whole")["datagrams"]["loss_pct"] for j in js), lambda x: f"{x:.2f}")
            dl = lambda s: mean_sd((100 * stream(j, s)["delivered_per_s"] / per_s for j in js), lambda x: f"{x:.1f}")
            tl = lambda s, q: ms(st.mean(stream(j, s)["tolerant"]["staleness"][q] for j in js))
            tdl = mean_sd((100 * stream(j, "whole")["tolerant"]["delivered_per_s"] / per_s for j in js), lambda x: f"{x:.1f}")
            rows.append((f"{loss}%", dg, f"{chunks:.1f}", f"{exp:.1f}", dl("whole"), stat(js, "whole", "staleness", "p99_us"), worst(js, "whole", "staleness", "max_us"),
                         tdl, tl("whole", "p50_us"), tl("whole", "p99_us"), ms(max(stream(j, "whole")["tolerant"]["staleness"]["max_us"] for j in js)),
                         "·", dl(pl), stat(js, pl, "staleness", "p50_us"), stat(js, pl, "staleness", "p99_us"), worst(js, pl, "staleness", "max_us"),
                         mean_sd((stream(j, pl)["bridge_log"]["warn_lines"] for j in js), lambda x: f"{x:.0f}"), len(js)))
        head = ["netem loss", "datagram loss % (seq gaps)", "datagrams/cycle (est.)", "expected complete % (1-p)^chunks", "mcast complete % (receiver as shipped)", "stale p99 ms", "stale max ms (worst rep)",
                "mcast complete % (order-tolerant)", "stale p50 ms", "p99", "max", "·", "pull complete %", "stale p50 ms", "p99", "max (worst rep)", "bridge warn lines/rep", "reps"]
        out[f"C_{n}_{p}"] = table(head, rows)
    return out


def section_after():
    """The netem cells re-run with the order-tolerant Receiver (ww-d4a): the shipped Receiver's own column, next to the old one."""
    after = defaultdict(list)
    for f in sorted(glob.glob(f"{RES}/raw-datagram-v1-d4a/C_netem_*.client.json")):
        after[re.sub(r"_r\d+\.client\.json$", "", pathlib.Path(f).name)].append(json.load(open(f)))
    before, rows = load("C_netem_"), []
    for n, p in [(100, "0.1"), (100, "0.01"), (800, "0.1")]:
        for loss in ["0", "0.1", "1", "5"]:
            key = f"C_netem_n{n}_p{p}_loss{loss}"
            js, old = after.get(key, []), before.get(key, [])
            if not js:
                continue
            pct = lambda xs: mean_sd((100 * stream(j, "whole")["delivered_per_s"] / (1 / float(p)) for j in xs), lambda x: f"{x:.1f}")
            tol = mean_sd((100 * stream(j, "whole")["tolerant"]["delivered_per_s"] / (1 / float(p)) for j in js), lambda x: f"{x:.1f}")
            rows.append((n, f"{round(1 / float(p))} Hz", f"{loss}%", pct(old), pct(js), tol, stat(js, "whole", "staleness", "p50_us"),
                         stat(js, "whole", "staleness", "p99_us"), worst(js, "whole", "staleness", "max_us"),
                         mean_sd((stream(j, "whole")["rejects"]["stale"] for j in js), lambda x: f"{x:.0f}"), len(js)))
    head = ["tags", "rate", "netem loss", "complete % before (receiver as shipped)", "complete % after ww-d4a", "complete % (separate order-tolerant count, same run)",
            "stale p50 ms", "p99", "max (worst rep)", "datagrams rejected stale / rep", "reps"]
    return {"C_after": table(head, rows)}


# ---- D: per-RTU fan-out -----------------------------------------------------------------------------------------

def section_d():
    d = load("D_")
    rows = []
    def order(name):
        m = re.match(r"D_(\w+?)_(?:r(\d+)_k(\d+)|n800)_p([\d.]+)", name)
        return (-float(m[4]), {"whole": 0, "slices": 1, "pull": 2}[m[1]], int(m[2] or 0), int(m[3] or 0))
    for name in sorted(d, key=order):
        js, kind = d[name], name.split("_")[1]
        r = [mon_rates(j) for j in js]
        names = [s["name"] for s in js[0]["streams"]]
        per = lambda f: mean_sd((st.mean(f(s) for s in j["streams"]) for j in js), lambda x: f"{x:.2f}")
        p = js[0]["period_s"]
        rows.append((kind, len(names), js[0]["k"] if kind != "whole" else 800, int(1 / p),
                     mean_sd((x["bps"] / 1e3 for x in r), lambda x: f"{x:.0f}"), mean_sd((x["bps"] / 1e3 / (1 if kind == "whole" else len(names)) for x in r), lambda x: f"{x:.0f}"), mean_sd((x["pps"] for x in r), lambda x: f"{x:.0f}"),
                     mean_sd((x["cpu"] for x in r), lambda x: f"{x:.1f}"), mean_sd((x["rss"] for x in r), lambda x: f"{x:.1f}"),
                     per(lambda s: 100 * s["delivered_per_s"] * p), per(lambda s: s["datagrams"]["loss_pct"]) if kind != "pull" else "n/a",
                     ms(st.mean(max(s["staleness"]["p99_us"] for s in j["streams"]) for j in js)), ms(max(s["staleness"]["max_us"] for j in js for s in j["streams"])), len(js)))
    return table(["mode", "RTUs", "tags/RTU", "Hz", "provider wire kB/s (req+pub)", "kB/s per RTU", "pkts/s", "provider CPU % of a core", "RSS MB", "complete % per RTU", "datagram loss %",
                  "worst-RTU stale p99 ms", "stale max ms", "reps"], rows)


# ---- E: subscriber restart --------------------------------------------------------------------------------------

def section_e():
    e = load("E_restart_")
    rows = []
    for name, js in sorted(e.items()):
        for s, label in [("whole", "multicast subscriber leaves/rejoins"), ("pull0", "pull-bridge killed/respawned")]:
            v = [stream(j, s)["restart"]["first_complete_ms_after_rejoin"] for j in js]
            gap = [stream(j, s)["staleness"]["max_us"] / 1e3 for j in js]
            rows.append((name.replace("E_restart_", ""), label, ", ".join(f"{x:.1f}" for x in v), f"{st.mean(v):.1f}", f"{max(v):.1f}", f"{st.mean(gap):.0f}"))
    return table(["cell", "who", "ms from rejoin to first complete cycle (each rep)", "mean", "max", "longest gap seen ms (incl. the 2 s away)"], rows)


# ---- F: faults --------------------------------------------------------------------------------------------------

def first_after(c, t, key=min):
    return min((x for x in c if x > t), default=None)


def section_f():
    out = {}
    rows = []
    for name, js in sorted(load("F_kill_").items()):
        for s in ("whole", "pull0"):
            o = []
            for j in js:
                ev = json.load(open(j["_file"] + ".events.json"))
                c, k, r = stream(j, s)["complete_ms"], ev["kill"][0] / 1e6, ev["restart"][0] / 1e6
                before, after = max(x for x in c if x < k), first_after(c, r)
                o.append((r - k, after - before, after - r, c[-1] > r + 5000, stream(j, s)["datagrams"]["provider_restarts"]))
            rows.append((s, len(js), mean_sd((x[0] for x in o), lambda x: f"{x:.0f}"), mean_sd((x[1] for x in o), lambda x: f"{x:.0f}"),
                         ", ".join(f"{x[2]:.0f}" for x in o), "yes" if all(x[3] for x in o) else "NO", ", ".join(str(x[4]) for x in o)))
    out["kill"] = table(["stream", "reps", "provider down ms (kill -> restart cmd)", "client-seen outage ms", "restart cmd -> first complete cycle ms (each rep)", "still delivering 5 s later", "provider restarts detected (seq reset)"], rows)
    rows = []
    for name, js in sorted(load("F_repdrop_").items()):
        for s in ("whole", "pull0"):
            o = []
            for j in js:
                ev = json.load(open(j["_file"] + ".events.json"))
                c, on, off = stream(j, s)["complete_ms"], ev["blackhole_on"][1] / 1e6, ev["blackhole_off"][1] / 1e6
                win = [x for x in c if on - 300 <= x <= off + 3000]
                o.append((max(b - a for a, b in zip(win, win[1:])), first_after(c, off) - off, c[-1] > off + 5000,
                          stream(j, s).get("bridge_log", {}).get("warn_lines", "-")))
            rows.append((s, len(js), (off - on), ", ".join(f"{x[0]:.0f}" for x in o), ", ".join(f"{x[1]:.0f}" for x in o),
                         "yes" if all(x[2] for x in o) else "NO", ", ".join(str(x[3]) for x in o)))
    out["repdrop"] = table(["stream", "reps", "replies dropped for ms", "longest gap ms (each rep)", "blackhole off -> first complete cycle ms", "still delivering 5 s later", "bridge warn lines"], rows)
    rows = []
    for inj in (0, 1):
        js = load(f"F_inject{inj}_").get(f"F_inject{inj}_n100_p0.1", [])
        if not js:
            continue
        rj = lambda k: sum(stream(j, "whole")["rejects"][k] for j in js)
        sent = sum(json.load(open(j["_file"] + ".inject.json"))["sent"] for j in js)
        rows.append(("injecting 100/s" if inj else "control", len(js), sent, rj("garbage"), rj("wrong_group"), rj("truncated"), rj("stale"),
                     mean_sd((stream(j, "whole")["delivered_per_s"] * 10 for j in js), lambda x: f"{x:.1f}"), stat(js, "whole", "staleness", "p99_us"),
                     worst(js, "whole", "staleness", "max_us"), sum(stream(j, "whole")["datagrams"]["lost"] for j in js)))
    out["inject"] = table(["run", "reps", "bad datagrams sent", "garbage", "wrong group", "truncated", "stale", "complete % (multicast)", "stale p99 ms", "stale max ms (worst rep)", "datagrams lost (seq)"], rows)
    f = RAW / "F_misspell.txt"
    out["misspell"] = "```\n" + (f.read_text().strip() if f.exists() else "(not run)") + "\n```"
    return out


def main():
    t = {"A": section_a(), "D": section_d(), "E": section_e(), **{k: v for k, v in section_c().items()}, **section_after(), **{f"F_{k}": v for k, v in section_f().items()}}
    n = len(glob.glob(f"{RAW}/*.client.json"))
    text = (ROOT / "bench/datagram_report.md.in").read_text().replace("{{runs}}", str(n))
    for k, v in t.items():
        text = text.replace("{{" + k + "}}", v)
    left = re.findall(r"\{\{[^}]+\}\}", text)
    assert not left, left
    (ROOT / "docs/bench/datagram-v1-bench.md").write_text(text)
    print("wrote docs/bench/datagram-v1-bench.md from", n, "runs")


if __name__ == "__main__":
    main()
