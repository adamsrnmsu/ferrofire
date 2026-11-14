#!/usr/bin/env python3
"""Findings, theme "reliability-fixes": is a command ever lost, wrong, out of order or flickering, and what did the ACK hold (ww-1df.32) change?

Reads docs/bench/results/control/{ferrofire-before-ww-1df.32,ferrofire,stock} and range/ of the same (cells.csv, cmds.csv), recomputes every
number, prints them, and writes docs/img/findings/reliability-fixes-{1-flaps,2-order,3-latency}.svg. Stdlib only, no clock, no randomness:
reruns are byte-identical. Run: python3 bench/findings/reliability-fixes.py
"""
import csv, glob, math, pathlib, statistics as st

ROOT = pathlib.Path(__file__).resolve().parents[2]
CTL = ROOT / "docs/bench/results/control"
OUT = ROOT / "docs/img/findings"
STACKS = {"before": "ferrofire-before-ww-1df.32", "ferrofire": "ferrofire", "stock": "stock"}
PUBLISH_S = {"before": 0.05, "ferrofire": 0.05, "stock": 1.0}  # ferrofire grid dt 0.05 s, stock bennu publishes once a second
FAMS = ["gen:seq", "gen:sim", "breaker:sim", "breaker:seq"]
FAM_NAME = {"gen:seq": "generator, one at a time", "gen:sim": "generator, all at once", "breaker:sim": "breaker, all at once", "breaker:seq": "breaker, one at a time"}
COUNTS = "issued ok lost wrong_final wrong_provider wrong_collateral order_violations flaps write_errors".split()


def files(env, stack, suffix):
    d = CTL / ("range" if env == "range" else "") / STACKS[stack]
    return [f for f in sorted(glob.glob(f"{d}/R*_rep*.{suffix}")) if ".part" not in f]


def inverted(rows, period_s):
    """Adjacent commands (issue order) of one RTU in one phase that became visible in the opposite order: [(kind, stream_gap_ms, visible_inversion_ms)].
    kind 'reorder' = the later command reached the stream >= half a publish period before the earlier one (or a stamp is missing), else 'skew'
    (both in one publish, polled between its datagrams). Same rule as bench/control_report.py order_split."""
    v = [r for r in sorted(rows, key=lambda r: int(r["seq"])) if int(r["t_visible"])]
    out = []
    for a, b in zip(v, v[1:]):
        if int(b["t_visible"]) < int(a["t_visible"]):
            sa, sb = int(a["t_stream"]), int(b["t_stream"])
            real = not (sa and sb) or sa - sb >= period_s * 1e9 / 2
            out.append(("reorder" if real else "skew", abs(sa - sb) / 1e6 if sa and sb else None, (int(a["t_visible"]) - int(b["t_visible"])) / 1e6))
    return out


def pct(v, q):  # nearest-rank percentile
    v = sorted(v)
    return v[min(len(v) - 1, max(0, math.ceil(q * len(v)) - 1))]


def load(env, stack):
    fam = {}
    for f in files(env, stack, "cells.csv"):
        for r in csv.DictReader(open(f)):
            t = fam.setdefault(f"{r['kind']}:{r['mode']}", dict.fromkeys(COUNTS, 0))
            for c in COUNTS:
                t[c] += int(float(r[c]))
    inv, seq, sim, run95, flapped = [], {}, [], [], 0
    for f in files(env, stack, "cmds.csv"):
        g, mine = {}, []
        for r in csv.DictReader(open(f)):
            g.setdefault((r["cell"], r["dir"], r["rtu"]), []).append(r)
            flapped += int(r["flaps"]) > 0
            if r["cell"].startswith("breaker:seq") and r["done"] == "1":
                mine.append((int(r["t_visible"]) - int(r["t_cmd"])) / 1e6)
            if r["kind"] == "breaker" and r["done"] == "1":
                ms = (int(r["t_visible"]) - int(r["t_cmd"])) / 1e6
                seq.setdefault(int(r["R"]), []).append(ms) if r["cell"].split(":")[1] == "seq" else sim.append(ms)
        run95.append(pct(mine, .95))
        for rows in g.values():
            inv += inverted(rows, PUBLISH_S[stack])
    tot = {c: sum(t[c] for t in fam.values()) for c in COUNTS}
    assert tot["issued"] == tot["ok"] + tot["lost"], (env, stack)
    assert flapped == tot["flaps"], (env, stack)  # a command flaps at most once, so flaps = commands that flapped
    assert len(inv) == tot["order_violations"], (env, stack, len(inv), tot["order_violations"])
    return dict(fam=fam, tot=tot, inv=inv, seq=seq, sim=sim, run95=run95, runs=len(files(env, stack, "cells.csv")))


# ---- SVG (hand-written so the palette can follow prefers-color-scheme) ----
# ferrofire = blue #2a78d6, stock bennu = grey #898781 (same pair as bench/charts.py and the other bench/findings scripts); "before the fix" = ferrofire tint.
CSS = """
text{font-family:-apple-system,'Segoe UI',Helvetica,Arial,sans-serif;font-size:12px;fill:#52514e}
.bg{fill:#fcfcfb}.t{font-size:15px;font-weight:700;fill:#0b0b0b}.ink{fill:#0b0b0b}.mu{fill:#898781;font-size:11px}
.grid{stroke:#e1e0d9;stroke-width:1}.axis{stroke:#c3c2b7;stroke-width:1}
.ff{fill:#2a78d6}.ffb{fill:#9ec5f4}.stk{fill:#898781}.b{font-weight:700}
@media (prefers-color-scheme:dark){text{fill:#c3c2b7}.bg{fill:#1a1a19}.t,.ink{fill:#f0efec}.mu{fill:#898781}
.grid{stroke:#333330}.axis{stroke:#52514e}.ff{fill:#3987e5}.ffb{fill:#1f4d85}.stk{fill:#898781}}
"""


def n(x):  # 4552 -> "4 552" (thin space would vary by font; plain space)
    return f"{x:,}".replace(",", " ")


def text(x, y, s, cls="", anchor="start"):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    return f'<text x="{x:.1f}" y="{y:.1f}" class="{cls}" text-anchor="{anchor}">{s}</text>'


def bar(x, y, w, h, cls):  # 2 px rounded data end
    return f'<rect x="{x:.1f}" y="{y:.1f}" width="{max(w, 0):.1f}" height="{h}" rx="2" class="{cls}"/>'


def save(name, w, h, title, sub, body, note):
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" aria-label="{title}">'
           f'<style>{CSS}</style><rect class="bg" width="{w}" height="{h}"/>' + text(14, 26, title, "t") + text(14, 46, sub) + "".join(body)
           + "".join(text(14, h - 12 - 14 * (len(note) - 1 - i), s, "mu") for i, s in enumerate(note)) + "</svg>\n")
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / name).write_text(svg)
    print("wrote", (OUT / name).relative_to(ROOT))


def chart_flaps(D):
    b, a, rb, ra = D["docker", "before"], D["docker", "ferrofire"], D["range", "before"], D["range", "ferrofire"]
    x0, wmax, body, y = 250, 300, [], 78
    body.append(text(x0, 66, "flaps per 100 commands", "mu"))
    for f in FAMS:
        fb, fa = b["fam"][f], a["fam"][f]
        rate = 100 * fb["flaps"] / fb["issued"]
        body += [text(14, y + 22, FAM_NAME[f], "ink"), text(x0 - 8, y + 12, "before", "mu", "end"), text(x0 - 8, y + 32, "after", "mu", "end"),
                 bar(x0, y, max(wmax * rate / 50, 1.5), 16, "ffb"), bar(x0, y + 20, 1.5, 16, "ff"),
                 text(x0 + max(wmax * rate / 50, 1.5) + 8, y + 12, f"{rate:.0f}" if rate >= 1 else f"{rate:.3f}", "ink b"),
                 text(x0 + max(wmax * rate / 50, 1.5) + (30 if rate >= 1 else 48), y + 12, f"{n(fb['flaps'])} of {n(fb['issued'])}", "mu"),
                 text(x0 + 10, y + 32, "0", "ink b"), text(x0 + 24, y + 32, f"0 of {n(fa['issued'])}", "mu")]
        y += 52
    body.append(f'<line class="axis" x1="{x0}" x2="{x0}" y1="72" y2="{y - 8}"/>')
    save("reliability-fixes-1-flaps.svg", 760, y + 44, f"The ACK hold removed every flap: {n(b['tot']['flaps'])} before, {a['tot']['flaps']} after",
         f"ferrofire, Docker bench, {n(b['tot']['issued'])} commands per build. A flap: read-back shows new, old, new again.", body,
         [f"Live range: {rb['tot']['flaps']} flaps before, {ra['tot']['flaps']} after, over {n(rb['tot']['issued'])} commands. Stock bennu: {D['docker', 'stock']['tot']['flaps']} flaps.",
          "Fix (ww-1df.32): the field device ignores a differing stream value for up to 2 s after a provider ACK."])


def chart_order(D):
    lo, hi, x0, x1 = -3, 5, 150, 735  # log10 ms
    X = lambda ms: x0 + (x1 - x0) * (math.log10(min(max(ms, 10 ** lo), 10 ** hi)) - lo) / (hi - lo)
    body = []
    for e in range(lo, hi + 1):
        lab = {-3: "1 µs", -2: "10 µs", -1: "0.1 ms", 0: "1 ms", 1: "10 ms", 2: "100 ms", 3: "1 s", 4: "10 s", 5: "100 s"}[e]
        body += [f'<line class="grid" x1="{X(10 ** e):.1f}" x2="{X(10 ** e):.1f}" y1="70" y2="250"/>', text(X(10 ** e), 266, lab, "mu", "middle")]
    body.append(text((x0 + x1) / 2, 284, "gap between the two commands' stream stamps, per inverted pair (log scale)", "", "middle"))
    for y, s, cls, name in ((115, "ferrofire", "ff", "ferrofire"), (205, "stock", "stk", "stock bennu")):
        inv = D["docker", s]["inv"]
        body.append(text(14, y + 4, name, "ink b"))
        for i, (_, gap, _) in enumerate(sorted(inv, key=lambda t: t[1])):
            body.append(f'<circle cx="{X(gap):.1f}" cy="{y + (i * 37 % 41) - 20:.0f}" r="2.5" class="{cls}" fill-opacity="{0.4 if s == "ferrofire" else 0.75}"/>')
        for kind, word in (("skew", "in one publish (chunk skew)"), ("reorder", "real reorders, 1 s+ apart")):
            g = [t[1] for t in inv if t[0] == kind]
            lab = f"{n(len(g))} {word}" if g else "0 real reorders"
            body.append(text(X(3000) if kind == "reorder" else X(max(g)), y - 28, lab, "ink", "middle" if kind == "reorder" else "end"))
    fs, ss = D["docker", "ferrofire"], D["docker", "stock"]
    fg, sg = [t[1] for t in fs["inv"]], [t[1] for t in ss["inv"] if t[0] == "reorder"]
    save("reliability-fixes-2-order.svg", 760, 340, f"ferrofire never reorders commands; stock bennu did {n(len(sg))} times",
         "Pairs of commands to one RTU that became visible in the opposite order, simultaneous breaker flips, Docker bench.", body,
         [f"ferrofire: all {len(fg)} pairs left in one publish, stamps at most {max(fg):.1f} ms apart. Stock: gap {min(sg) / 1e3:.2f} to {max(sg) / 1e3:.1f} s, median {st.median(sg) / 1e3:.1f} s.",
          f"Pairs are counted among completed commands: ferrofire {n(fs['fam']['breaker:sim']['ok'])}, stock {n(ss['fam']['breaker:sim']['ok'])} (it lost {n(ss['fam']['breaker:sim']['lost'])})."])


def chart_latency(D):
    B, A = D["docker", "before"], D["docker", "ferrofire"]
    b, a = B["seq"], A["seq"]
    x0, y0, H, top = 60, 250, 170, 120  # axis 0..120 ms
    Y = lambda ms: y0 - H * ms / top
    body = [text(14, 68, "ms, command to visible at the RTU (p50)", "mu")]
    for v in (0, 40, 80, 120):
        body += [f'<line class="{"axis" if v == 0 else "grid"}" x1="{x0}" x2="735" y1="{Y(v):.1f}" y2="{Y(v):.1f}"/>', text(x0 - 8, Y(v) + 4, str(v), "mu", "end")]
    for i, R in enumerate(sorted(a)):
        cx = x0 + 90 + i * 165
        for dx, d, cls in ((-34, b, "ffb"), (2, a, "ff")):
            p = st.median(d[R])
            body += [f'<rect x="{cx + dx}" y="{Y(p):.1f}" width="32" height="{y0 - Y(p):.1f}" rx="2" class="{cls}"/>', text(cx + dx + 16, Y(p) - 6, f"{p:.1f}", "ink", "middle")]
        body += [text(cx - 18, y0 + 15, "before", "mu", "middle"), text(cx + 18, y0 + 15, "after", "mu", "middle"),
                 text(cx, y0 + 33, f"{R} RTU" + ("s" if R > 1 else ""), "ink", "middle")]
    allb, alla = sum(b.values(), []), sum(a.values(), [])
    save("reliability-fixes-3-latency.svg", 760, 330, f"The fix cost no latency: a breaker flip still shows in {st.median(alla):.1f} ms",
         f"ferrofire, sequential breaker flips, Docker bench, {n(len(alla))} commands per build.", body,
         [f"p95 {pct(allb, .95):.1f} ms before, {pct(alla, .95):.1f} ms after (per run {min(B['run95']):.0f}-{max(B['run95']):.0f} and {min(A['run95']):.0f}-{max(A['run95']):.0f} ms: overlapping); simultaneous p95 {pct(D['docker', 'before']['sim'], .95):.1f} and {pct(D['docker', 'ferrofire']['sim'], .95):.1f} ms."])


def main():
    mk = lambda seq, vis, s: {"seq": str(seq), "t_visible": str(vis), "t_stream": str(s)}
    assert [k for k, *_ in inverted([mk(0, 10, 100), mk(1, 5, 98_000_000)], 0.05)] == ["skew"]
    assert [k for k, *_ in inverted([mk(0, 10, 1_000_000_000), mk(1, 5, 5)], 1.0)] == ["reorder"]
    assert inverted([mk(0, 5, 100), mk(1, 10, 1), mk(2, 0, 1)], 0.05) == []
    D = {(e, s): load(e, s) for e in ("docker", "range") for s in STACKS}
    for (e, s), d in D.items():
        inv, t = d["inv"], d["tot"]
        re, sk = [x for x in inv if x[0] == "reorder"], [x for x in inv if x[0] == "skew"]
        allseq = sum(d["seq"].values(), [])
        print(f"{e:6} {s:9} runs={d['runs']:2} " + " ".join(f"{c}={t[c]}" for c in COUNTS))
        print(f"       flaps by family: " + ", ".join(f"{f}={d['fam'][f]['flaps']}/{d['fam'][f]['issued']}" for f in FAMS)
              + f" | lost breaker:sim={d['fam']['breaker:sim']['lost']}/{d['fam']['breaker:sim']['issued']}")
        print(f"       reordered={len(re)} chunk_skew={len(sk)}"
              + (f" skew: stream gap max {max(x[1] for x in sk):.2f} ms, visible inversion max {max(x[2] for x in sk):.2f} ms" if sk else "")
              + (f" | reorder stream gap min/median/max {min(x[1] for x in re):.0f}/{st.median(x[1] for x in re):.0f}/{max(x[1] for x in re):.0f} ms" if re else ""))
        print(f"       breaker seq visible p50 by R: " + ", ".join(f"R{R}={st.median(v):.1f}" for R, v in sorted(d["seq"].items()))
              + f" | all p50={st.median(allseq):.1f} p95={pct(allseq, .95):.1f} | per-run p95 {min(d['run95']):.1f}-{max(d['run95']):.1f} | sim p95={pct(d['sim'], .95):.1f} ms")
    chart_flaps(D), chart_order(D), chart_latency(D)


if __name__ == "__main__":
    main()
