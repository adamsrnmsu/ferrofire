#!/usr/bin/env python3
"""Headline findings: how fast and how reliably a SCADA command takes effect, ferrofire vs stock bennu as shipped.

    python3 bench/findings/headline.py      (stdlib only; rerun is byte-identical)

Reads docs/bench/results/control/{ferrofire,stock,range/ferrofire,range/stock}/R*_rep*.{cmds,cells}.csv,
recomputes every number, writes docs/img/findings/headline-{1-latency,2-loss,3-burst}.svg and prints the numbers as JSON.
'lost' in cells.csv = not done within 10 s (12 s for a generator), per docs/bench/control-latency.md.
"""
import csv, glob, json, math, os, statistics, textwrap
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
RAW, OUT = f"{ROOT}/docs/bench/results/control", f"{ROOT}/docs/img/findings"
STACKS = {"ferrofire": "ferrofire", "stock": "stock bennu"}


def pct(v, p):
    """Same rule as bench/control_report.py: sorted(v)[int(p*len)]."""
    v = sorted(v)
    return v[min(len(v) - 1, int(p * len(v)))]


def load(env, stack):
    """-> (lat[cell, dir, R] = [ms to visible], sp[...] = [ms to setpoint], cells[cell, R] = dict(issued, lost, per_s=[...]))."""
    d = f"{RAW}/{'range/' if env == 'range' else ''}{stack}"
    lat, sp, cells = defaultdict(list), defaultdict(list), defaultdict(lambda: dict(issued=0, lost=0, per_s=[]))
    for f in sorted(glob.glob(f"{d}/R*_rep*.cmds.csv")):
        for r in csv.DictReader(open(f)):
            k, t = (r["cell"], r["dir"], int(r["R"])), int(r["t_cmd"])
            if int(r["t_visible"]):
                lat[k].append((int(r["t_visible"]) - t) / 1e6)
            if int(r["t_setpoint"]):
                sp[k].append((int(r["t_setpoint"]) - t) / 1e6)
    for f in sorted(glob.glob(f"{d}/R*_rep*.cells.csv")):
        for r in csv.DictReader(open(f)):
            c = cells[r["cell"], int(r["R"])]
            c["issued"] += int(r["issued"]); c["lost"] += int(r["lost"]); c["per_s"].append(float(r["per_s"]))
    assert cells, f"no data under {d}"
    return lat, sp, cells


def pool(m, pred):
    return [x for k, v in m.items() if pred(k) for x in v]


def compute():
    n = {}
    for env, Rs in (("docker", (1, 2, 5, 10)), ("range", (1, 2, 5))):
        for s in STACKS:
            lat, sp, cells = load(env, s)
            e = n.setdefault(env, {}).setdefault(s, {})
            e["seq100"] = {R: dict(n=len(v), p50=round(pct(v, .5)), p95=round(pct(v, .95)))
                           for R in Rs for v in [pool(lat, lambda k: k[0] == "breaker:seq:100" and k[2] == R)]}
            e["sim100"] = {R: dict(issued=c["issued"], lost=c["lost"], pct=round(100 * c["lost"] / c["issued"], 1),
                                   per_s=statistics.median(c["per_s"])) for R in Rs for c in [cells["breaker:sim:100", R]]}
            e["total"] = dict(issued=sum(c["issued"] for c in cells.values()), lost=sum(c["lost"] for c in cells.values()))
            sim = [c for (cell, _), c in cells.items() if cell.startswith("breaker:sim")]
            e["sim_breaker"] = dict(issued=sum(c["issued"] for c in sim), lost=sum(c["lost"] for c in sim))
            seq = [c for (cell, _), c in cells.items() if ":seq:" in cell]
            e["seq_all"] = dict(issued=sum(c["issued"] for c in seq), lost=sum(c["lost"] for c in seq))
            if env == "docker":
                e["sim1000"] = {R: dict(issued=c["issued"], lost=c["lost"], reps=len(c["per_s"]), per_s=statistics.median(c["per_s"]))
                                for R in Rs for c in [cells["breaker:sim:1000", R]]}
                g = lambda m, d: pool(m, lambda k: k[0].startswith("gen:seq") and k[1] == d)
                up, pg, dn = g(lat, "up"), g(sp, "up"), g(lat, "down")
                e["gen"] = dict(status_on=dict(n=len(up), p50=round(pct(up, .5))), status_off=dict(n=len(dn), p50=round(pct(dn, .5))),
                                pg_at_setpoint=dict(n=len(pg), p50=round(pct(pg, .5)) if pg else None))
    return n


# ---------- SVG (hand-written: stdlib only, no timestamps; fixed light surface like the sibling charts, so it reads on light and dark pages) ----------
W = 720
CSS = """
text{font-family:'DejaVu Sans',Verdana,sans-serif}
.bg{fill:#fcfcfb}.t{fill:#0b0b0b;font-weight:bold;font-size:15px}.s{fill:#52514e;font-size:12px}
.v{fill:#0b0b0b;font-size:12px}.m{fill:#898781;font-size:11px}
.g{stroke:#e1e0d9;stroke-width:1}.a{stroke:#c3c2b7;stroke-width:1}.ff{fill:#2a78d6}.st{fill:#898781}
.rg{fill:#fcfcfb;stroke:#0b0b0b;stroke-width:1.5}.ln{stroke:#c3c2b7;stroke-width:2}.ring{stroke:#fcfcfb;stroke-width:2}
""".strip()
CLS = {"ferrofire": "ff", "stock": "st"}


def num(x):
    return f"{x:,.0f}".replace(",", " ")


def text(x, y, s, cls="s", anchor="start"):
    return f'<text x="{x:.1f}" y="{y:.1f}" class="{cls}" text-anchor="{anchor}">{s}</text>'


def diamond(x, y, tip, r=5):
    return f'<path d="M{x:.1f} {y - r:.1f}l{r} {r}l-{r} {r}l-{r} -{r}z" class="rg"><title>{tip}</title></path>'


def frame(h, title, sub, note, body, legend=("ferrofire", "stock", "range")):
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {h}" width="{W}" height="{h}" role="img" aria-label="{title}">',
         f"<style>{CSS}</style>", f'<rect width="{W}" height="{h}" class="bg"/>', text(16, 26, title, "t"), text(16, 46, sub)]
    x = 16
    for k in legend:
        if k == "range":
            o += [diamond(x + 6, 64, "live range"), text(x + 18, 68, "same test on the live range")]
        else:
            o += [f'<rect x="{x}" y="58" width="12" height="12" rx="2" class="{CLS[k]}"/>', text(x + 18, 68, STACKS[k])]
            x += 40 + 7.2 * len(STACKS[k])
    lines = textwrap.wrap(note, 108)
    assert len(lines) <= 2 and len(title) <= 72, (title, lines)   # what fits in 720 px
    notes = [text(16, h - 12 - 15 * (len(lines) - 1 - i), l, "m") for i, l in enumerate(lines)]
    return "\n".join(o + body + notes + ["</svg>"]) + "\n"


def bars(n, title, sub, note, ymax, step, unit, val, label, tip, rng):
    """Grouped bars per RTU count (Docker); diamonds = live range."""
    h, x0, x1, y0, y1 = 392, 56, W - 16, 320, 96
    Y = lambda v: y0 - (y0 - y1) * v / ymax
    b = []
    for t in range(0, ymax + 1, step):
        b += [f'<line x1="{x0}" x2="{x1}" y1="{Y(t):.1f}" y2="{Y(t):.1f}" class="{"a" if t == 0 else "g"}"/>', text(x0 - 8, Y(t) + 4, num(t), "m", "end")]
    b.append(text(x0 - 40, y1 - 12, unit, "m"))
    Rs, gw, bw = (1, 2, 5, 10), (x1 - x0) / 4, 56
    for i, R in enumerate(Rs):
        cx = x0 + gw * (i + .5)
        b.append(text(cx, y0 + 20, f"{R} RTU{'s' if R > 1 else ''}", "s", "middle"))
        for j, s in enumerate(STACKS):
            v, bx = val(n["docker"][s], R), cx + (j - 1) * (bw + 2) + 1
            top, hh = Y(v), y0 - Y(v)
            if hh > 0:
                r = min(4, hh)
                b.append(f'<path d="M{bx:.1f} {y0}v-{hh - r:.1f}q0 -{r} {r} -{r}h{bw - 2 * r}q{r} 0 {r} {r}v{hh - r:.1f}z" class="{CLS[s]}"><title>{tip(s, R, "Docker")}</title></path>')
            b.append(text(bx + bw / 2, top - 8, label(n["docker"][s], R), "v", "middle"))
            if R in rng:
                b.append(diamond(bx + bw / 2, Y(val(n["range"][s], R)), tip(s, R, "range")))
    return frame(h, title, sub, note, b)


def chart_latency(n):
    d, r = n["docker"], n["range"]
    return bars(n, "A breaker flip shows up in 0.1 s on ferrofire, 1 s on stock bennu",
                "Median time from SCADA write to the new state read back at the RTU; flips sent one at a time",
                f"Docker: {d['ferrofire']['seq100'][1]['n']} flips per bar (5 reps). Live range: {r['ferrofire']['seq100'][1]['n']} per point (3 reps, 1/2/5 RTUs). Stock bennu as shipped publishes once a second.",
                1200, 200, "ms", lambda e, R: e["seq100"][R]["p50"], lambda e, R: num(e["seq100"][R]["p50"]),
                lambda s, R, env: f"{STACKS[s]}, {R} RTU, {env}: p50 {n[env.lower()][s]['seq100'][R]['p50']} ms, p95 {n[env.lower()][s]['seq100'][R]['p95']} ms", (1, 2, 5))


def chart_loss(n):
    d = n["docker"]
    return bars(n, "100 flips at once: ferrofire lands all, stock bennu misses up to 81 %",
                "Share of 100 simultaneous breaker flips not visible at the RTU within 10 s",
                f"Docker: {d['stock']['sim100'][1]['issued']} flips per bar (5 x 100). Live range: 300 per point. Stock bennu as shipped (1 s publish); it loses fewer as RTUs are added.",
                100, 20, "% missed", lambda e, R: e["sim100"][R]["pct"], lambda e, R: f"{e['sim100'][R]['pct']:.0f} %",
                lambda s, R, env: f"{STACKS[s]}, {R} RTU, {env}: lost {n[env.lower()][s]['sim100'][R]['lost']} of {n[env.lower()][s]['sim100'][R]['issued']}", (1, 2, 5))


def chart_burst(n):
    """Dot plot on a log axis: flips completed per second in a burst of 1000."""
    d, h, x0, x1, y1 = n["docker"], 336, 80, W - 70, 100
    X = lambda v: x0 + (x1 - x0) * math.log10(v) / 4
    b = []
    for e in range(5):
        b += [f'<line x1="{X(10 ** e):.1f}" x2="{X(10 ** e):.1f}" y1="{y1 - 10}" y2="{y1 + 150}" class="g"/>', text(X(10 ** e), y1 + 168, num(10 ** e), "m", "middle")]
    b.append(text((x0 + x1) / 2, y1 + 186, "flips completed per second (log scale)", "s", "middle"))
    for i, R in enumerate((1, 2, 5, 10)):
        y, f, s = y1 + 12 + 40 * i, d["ferrofire"]["sim1000"][R], d["stock"]["sim1000"][R]
        b += [text(x0 - 12, y + 4, f"{R} RTU{'s' if R > 1 else ''}", "s", "end"),
              f'<line x1="{X(s["per_s"]):.1f}" x2="{X(f["per_s"]):.1f}" y1="{y}" y2="{y}" class="ln"/>',
              f'<circle cx="{X(s["per_s"]):.1f}" cy="{y}" r="6" class="st ring"><title>stock bennu, {R} RTU: {s["per_s"]} flips/s, lost {s["lost"]} of {s["issued"]}</title></circle>',
              f'<circle cx="{X(f["per_s"]):.1f}" cy="{y}" r="6" class="ff ring"><title>ferrofire, {R} RTU: {f["per_s"]} flips/s, lost {f["lost"]} of {f["issued"]}</title></circle>',
              text(X(s["per_s"]), y - 12, f"{s['per_s']:.1f}", "v", "middle"), text(X(f["per_s"]) + 12, y + 4, num(f["per_s"]), "v")]
    lo, hi = d["stock"]["sim1000"], d["ferrofire"]["sim1000"]
    return frame(h, "Burst of 1 000 flips: ferrofire 7 000+ per second, stock bennu 2 to 18",
                 "Breaker flips completed per second when 1 000 are issued at once (Docker)",
                 f"ferrofire: median of 5 reps, 0 of {num(hi[1]['issued'])} missed per row. Stock bennu: 1 rep, and {lo[1]['lost']}, {lo[2]['lost']}, {lo[5]['lost']}, {lo[10]['lost']} of its 1 000 missed the 10 s deadline; rate counts those done.",
                 b, legend=("ferrofire", "stock"))


def check(n):
    """The titles and findings are only true if these hold."""
    d, r = n["docker"], n["range"]
    for env in (d, r):
        assert env["ferrofire"]["total"]["lost"] == 0
        assert all(95 <= v["p50"] <= 105 for v in env["ferrofire"]["seq100"].values())
        assert all(1000 <= v["p50"] <= 1050 for v in env["stock"]["seq100"].values())
        assert env["stock"]["seq_all"]["lost"] == 0
    assert round(d["stock"]["sim100"][1]["pct"]) == 81 and d["stock"]["sim100"][10]["lost"] == 0
    assert all(v["per_s"] > 7000 for v in d["ferrofire"]["sim1000"].values())
    assert all(1.5 < v["per_s"] < 18.5 for v in d["stock"]["sim1000"].values())
    assert d["stock"]["gen"]["pg_at_setpoint"]["n"] == 0


if __name__ == "__main__":
    n = compute()
    check(n)
    os.makedirs(OUT, exist_ok=True)
    for name, svg in (("1-latency", chart_latency(n)), ("2-loss", chart_loss(n)), ("3-burst", chart_burst(n))):
        open(f"{OUT}/headline-{name}.svg", "w").write(svg)
    print(json.dumps(n, sort_keys=True))
