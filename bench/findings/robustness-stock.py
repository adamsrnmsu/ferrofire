#!/usr/bin/env python3
"""Findings, theme robustness-stock: which faults break a stock bennu field device, and do the fixes hold?

Reads the burger board reports (docs/bench/results/burger/*-report.md, fdleak-fixed-burger-churn.json),
recomputes every number, cross-checks burger.csv / docs/img/charts/08*.csv / prose, and draws three SVGs
into docs/img/findings/robustness-stock-*.svg. Stdlib only; output is byte-identical on rerun.

    python3 bench/findings/robustness-stock.py

Scope: stock bennu vs bennu with upstream PRs #79 / #81 / #82. Not ferrofire, so the ferrofire blue
(#2a78d6) is not used here: stock = grey (as everywhere), "bennu + fixes" = violet.
The stock modbus-churn run has no raw file: its numbers exist only in burger.csv (quoted from a PR comment).
"""
import csv, json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RAW = ROOT / "docs/bench/results/burger"
OUT = ROOT / "docs/img/findings"
FD_SOFT_LIMIT = 1024  # quoted from docs (results.md 8a, datagram-v1-bench.md G6); no raw file records it

# palette: dataviz reference palette, validated grey<->violet in both modes (validate_palette.js)
CSS = """
:root{--surface:#fcfcfb;--ink:#0b0b0b;--ink2:#52514e;--muted:#898781;--grid:#e1e0d9;--stock:#898781;--fixed:#6250d6}
@media (prefers-color-scheme:dark){:root{--surface:#1a1a19;--ink:#f0efec;--ink2:#c3c2b7;--muted:#898781;--grid:#383835;--stock:#898781;--fixed:#9085e9}}
text{font-family:-apple-system,'Segoe UI',Helvetica,Arial,sans-serif;font-size:12px;fill:var(--ink)}
.t{font-size:15px;font-weight:700}.s{fill:var(--ink2)}.m{fill:var(--muted);font-size:11px}.b{font-weight:600}
.grid{stroke:var(--grid);stroke-width:1}.stock{fill:var(--stock)}.fixed{fill:var(--fixed)}
.ring{stroke:var(--surface);stroke-width:2}.bad{fill:#d03b3b}.good{fill:#0ca30c}
.ic{stroke:#fff;stroke-width:2;fill:none;stroke-linecap:round;stroke-linejoin:round}
"""


def n(v):  # 1006 -> "1 006"
    return f"{v:,}".replace(",", " ")


# ---------- read raw ----------
def report(name):
    """One burger board report -> bennu build line, overall verdict, {item: (verdict, reason)}, {host: (bytes, repeats)}."""
    txt = (RAW / name).read_text()
    checks, logs = {}, {}
    for line in txt.splitlines():
        c = [x.strip() for x in line.strip().strip("|").split("|")]
        if len(c) >= 4 and c[1] == "python":
            checks[c[0]] = ("fail" if c[2] == "FAIL" else "pass", c[3])
        m = re.search(r"watcher-stderr\.log ([\d,]+) \|.*?(?:repeated (\d+)x|\| no \|)", line)
        if m and c[0].startswith("rtu-"):
            logs[c[0]] = (int(m.group(1).replace(",", "")), int(m.group(2) or 0))
    return dict(bennu=re.search(r"^- bennu: (.*)$", txt, re.M).group(1), overall=re.findall(r"^\*\*(PASS|FAIL)\*\*", txt, re.M)[-1],
                checks=checks, logs=logs)


R = {k: report(f"{k}-report.md") for k in ("robust-stock", "robust-fixed", "watcher-stock", "watcher-fixed", "fdleak-fixed")}
churn = json.loads((RAW / "fdleak-fixed-burger-churn.json").read_text())
index = {(r["item"], r["build"], r["metric"]): r for r in csv.DictReader((RAW / "burger.csv").open())}
quoted = lambda m: int(index[("modbus-churn", "stock", m)]["value"])  # no raw: PR-comment numbers via burger.csv

ITEMS = ["modbus-badframes", "iec104-masters", "dnp3-accept", "modbus-churn", "watcher-quiet"]
verdict = {"stock": {}, "fixed": {}}
for b in verdict:
    for it in ITEMS[:3]:
        verdict[b][it] = R[f"robust-{b}"]["checks"][it][0]
    verdict[b]["watcher-quiet"] = R[f"watcher-{b}"]["checks"]["watcher-quiet"][0]
verdict["fixed"]["modbus-churn"] = R["fdleak-fixed"]["checks"]["modbus-churn"][0]
verdict["stock"]["modbus-churn"] = index[("modbus-churn", "stock", "verdict")]["value"]  # quoted, no raw
assert all(len(v) == 5 for v in verdict.values())
fails = {b: sum(v == "fail" for v in verdict[b].values()) for b in verdict}
raw_backed_stock_fails = sum(verdict["stock"][it] == "fail" for it in ITEMS if it != "modbus-churn")

bpm = {b: {h: int(x) for h, x in re.findall(r"(rtu-\d) (\d+) B/min", R[f"watcher-{b}"]["checks"]["watcher-quiet"][1].split("(")[-1])} for b in verdict}
threshold = int(re.search(r"over (\d+)", R["watcher-stock"]["checks"]["watcher-quiet"][1]).group(1))
line_bytes = {h: bts / rep for h, (bts, rep) in R["watcher-stock"]["logs"].items()}
lines_per_s = {h: bpm["stock"][h] / line_bytes[h] / 60 for h in line_bytes}
badframe_cases = int(re.search(r"\((\d+) cases", R["robust-stock"]["checks"]["modbus-badframes"][1]).group(1))
pid_after = re.search(r"pid (\d+) -> (\w+)", R["robust-stock"]["checks"]["modbus-badframes"][1]).groups()
fx = re.search(r"\((\d+)/(\d+) connections; socket fds (\d+) -> (\d+); log grew (\d+) B\)", R["fdleak-fixed"]["checks"]["modbus-churn"][1])
stock = dict(conns=quoted("conns_at_stop"), before=quoted("sockets_before"), after=quoted("sockets_after"))
stock["mb"] = int(re.search(r"\+(\d+) MB", index[("modbus-churn", "stock", "verdict")]["source"]).group(1))
leak_per_conn = (stock["after"] - stock["before"]) / stock["conns"]
builds = sorted({R[k]["bennu"].split(" deb")[0] for k in R if k.endswith("fixed")})

# ---------- mismatches: index, plotted CSVs, prose ----------
mism = []
def check(what, got, want):
    if str(got) != str(want):
        mism.append(f"{what}: raw says {got}, doc says {want}")

for b in verdict:
    for it in ITEMS:
        check(f"burger.csv {it}/{b} verdict", verdict[b][it], index[(it, b, "verdict")]["value"])
    for h, v in bpm[b].items():
        check(f"burger.csv watcher {b} {h} B/min", v, index[("watcher-quiet", b, f"{h}_Bpm")]["value"])
for k, j in (("conns_done", "done"), ("sockets_before", "sockets_before"), ("sockets_after", "sockets_after")):
    check(f"burger.csv modbus-churn/fixed {k}", churn[j], index[("modbus-churn", "fixed", k)]["value"])
check("fdleak-fixed-report.md vs churn json", tuple(map(int, fx.groups())), (churn["done"], churn["conns"], churn["sockets_before"], churn["sockets_after"], churn["log_grew"]))
check("fixed still answering", churn["answers"] and churn["error"] is None, True)
try:  # plotted CSVs of the existing charts (derived, cross-check only)
    for r in csv.DictReader((ROOT / "docs/img/charts/08c-robust-matrix.csv").open()):
        for b in verdict:
            check(f"08c-robust-matrix.csv {r['check']}/{b}", verdict[b][r["check"]], r[b].lower())
    for r in csv.DictReader((ROOT / "docs/img/charts/08b-watcher-log.csv").open()):
        check(f"08b-watcher-log.csv {r['RTU']} stock", bpm["stock"][r["RTU"]], r["stock watcher-stderr.log B/min"].replace(",", ""))
        check(f"08b-watcher-log.csv {r['RTU']} fixed", bpm["fixed"][r["RTU"]], r["fixed B/min"])
    g6 = re.search(r"\*\*([\d.]+ GB) in about three minutes\*\*", (ROOT / "docs/bench/datagram-v1-bench.md").read_text())
    if g6:
        mism.append(f"stock fd-leak stderr volume: results.md 8a / burger.csv say {stock['mb']} MB, datagram-v1-bench.md G6 says {g6.group(1)} "
                    "in about three minutes (neither has a raw file; possibly different occurrences)")
except FileNotFoundError as e:
    print("cross-check skipped:", e.filename, file=sys.stderr)


# ---------- SVG helpers ----------
def svg(name, w, h, title, sub, body, desc):
    out = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" aria-label="{title}">\n'
           f"<title>{title}</title><desc>{desc}</desc>\n<style>{CSS}</style>\n"
           f'<rect width="{w}" height="{h}" fill="var(--surface)"/>\n'
           f'<text class="t" x="24" y="32">{title}</text>\n<text class="s" x="24" y="52">{sub}</text>\n' + "\n".join(body) + "\n</svg>\n")
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / name).write_text(out)
    return f"docs/img/findings/{name}"


def T(x, y, s, cls="", anchor="start"):
    return f'<text x="{x:.1f}" y="{y:.1f}"' + (f' class="{cls}"' if cls else "") + (f' text-anchor="{anchor}"' if anchor != "start" else "") + f">{s}</text>"


def status(x, y, ok, word, detail):  # icon + word + detail: never colour alone
    path = "M-4,0 L-1.2,3 L4,-3" if ok else "M-3,-3 L3,3 M3,-3 L-3,3"
    return (f'<g><title>{word}: {detail}</title><circle cx="{x}" cy="{y}" r="9" class="{"good" if ok else "bad"}"/>'
            f'<path class="ic" transform="translate({x},{y})" d="{path}"/></g>' + T(x + 16, y - 2, word, "b") + T(x + 16, y + 13, detail, "m"))


FOOT_ONE_RUN = "One board run per build, live waterway-arm64 range, 2026-11-10."

# ---------- chart 1: scoreboard ----------
rows = [  # (item, fault in plain words, PR, stock outcome, fixed outcome); outcomes worded from the report 'reason' column
    ("modbus-badframes", "Truncated Modbus frame", "MBAP length 0 · PR #81", f"device dies (pid {pid_after[0]} → {pid_after[1].lower()})", "frame dropped, same pid"),
    ("iec104-masters", "Second IEC-104 master connects", "PR #81", "first master stops receiving", "both masters keep receiving"),
    ("dnp3-accept", "DNP3 accept() error", "fd limit hit, then restored · PR #81", "never accepts again", "accepts again, answers"),
    ("modbus-churn", f"{n(churn['conns'])} Modbus connect/close", "one socket each · PR #79", f"stops answering at {n(stock['conns'])} *", f"{n(churn['done'])} done, still answers"),
    ("watcher-quiet", "Watcher polling an idle RTU", "stderr log growth · PR #82", f"{bpm['stock']['rtu-1'] / 1000:.0f} KB/min of log", f"{bpm['fixed']['rtu-1']} B/min"),
]
assert "truncated request, MBAP length 0" in R["robust-stock"]["checks"]["modbus-badframes"][1]
assert "stopped receiving after a second client" in R["robust-stock"]["checks"]["iec104-masters"][1]
assert "did not resume accepting" in R["robust-stock"]["checks"]["dnp3-accept"][1]
assert "same pid" in R["robust-fixed"]["checks"]["modbus-badframes"][1] and "resumed accepting" in R["robust-fixed"]["checks"]["dnp3-accept"][1]
X1, X2, Y0, RH = 300, 530, 112, 46
b = [f'<rect x="{X1 - 9}" y="77" width="18" height="6" rx="3" class="stock"/>', T(X1 + 16, 84, "stock bennu", "b"),
     f'<rect x="{X2 - 9}" y="77" width="18" height="6" rx="3" class="fixed"/>', T(X2 + 16, 84, "bennu + fixes", "b"), T(24, 84, "fault", "s")]
for i, (it, label, how, s_out, f_out) in enumerate(rows):
    y = Y0 + i * RH
    b += [f'<line class="grid" x1="24" x2="736" y1="{y - 20}" y2="{y - 20}"/>', T(24, y - 2, label, "b"), T(24, y + 13, how, "m"),
          status(X1, y, verdict["stock"][it] == "pass", "fails" if verdict["stock"][it] == "fail" else "passes", s_out),
          status(X2, y, verdict["fixed"][it] == "pass", "fails" if verdict["fixed"][it] == "fail" else "passes", f_out)]
yb = Y0 + 5 * RH
b += [f'<line class="grid" x1="24" x2="736" y1="{yb - 20}" y2="{yb - 20}"/>',
      T(24, yb, "* quoted from a PR #79 comment; no raw file kept. The other nine cells are parsed from the board reports.", "m"),
      T(24, yb + 16, f"Each fix was tested on its own build ({len(builds)} builds, one per PR), not one combined build. " + FOOT_ONE_RUN, "m")]
c1 = svg("robustness-stock-1-scoreboard.svg", 760, yb + 34,
         f"Stock bennu fails all {fails['stock']} fault checks; with the fixes all {5 - fails['fixed']} pass",
         "Five faults thrown at the field device and watcher on the live range: outcome per build", b,
         "Pass/fail matrix of five everything-burger robustness checks, stock bennu against bennu with PRs 79, 81 and 82.")

# ---------- chart 2: fd leak ----------
W, H, L, Rm, Tp, B = 760, 420, 64, 40, 84, 80
px = lambda v: L + v / 1500 * (W - L - Rm)
py = lambda v: H - B - v / 1100 * (H - Tp - B)
b = []
for v in (0, 250, 500, 750, 1000):
    b += [f'<line class="grid" x1="{L}" x2="{W - Rm}" y1="{py(v):.1f}" y2="{py(v):.1f}"/>', T(L - 8, py(v) + 4, n(v), "s", "end")]
for v in (0, 500, 1000, 1500):
    b.append(T(px(v), H - B + 18, n(v), "s", "middle"))
b += [T(px(750), H - B + 38, "Modbus connections opened and closed", "s", "middle"),
      T(L - 48, Tp - 10, "open sockets of the field device", "s"),
      f'<line x1="{L}" x2="{W - Rm}" y1="{py(FD_SOFT_LIMIT):.1f}" y2="{py(FD_SOFT_LIMIT):.1f}" stroke="#d03b3b" stroke-width="1.5" stroke-dasharray="6 4"/>',
      T(W - Rm, py(FD_SOFT_LIMIT) - 6, f"fd soft limit {n(FD_SOFT_LIMIT)}", "s", "end")]
for cls, x1, y1, y0, tip in (("stock", stock["conns"], stock["after"], stock["before"], f"stock bennu: {stock['before']} to {n(stock['after'])} sockets over {n(stock['conns'])} connections (quoted)"),
                             ("fixed", churn["done"], churn["sockets_after"], churn["sockets_before"], f"bennu + fix: {churn['sockets_before']} to {churn['sockets_after']} sockets over {n(churn['done'])} connections")):
    b += [f'<g><title>{tip}</title><line x1="{px(0):.1f}" y1="{py(y0):.1f}" x2="{px(x1):.1f}" y2="{py(y1):.1f}" stroke="var(--{cls})" stroke-width="2" stroke-dasharray="2 5" stroke-linecap="round"/>',
          f'<circle class="{cls} ring" cx="{px(0):.1f}" cy="{py(y0):.1f}" r="5"/><circle class="{cls} ring" cx="{px(x1):.1f}" cy="{py(y1):.1f}" r="5"/></g>']
sx, sy = px(stock["conns"]), py(stock["after"])
b += [T(sx + 14, sy + 24, "stock bennu", "b"), T(sx + 14, sy + 39, f"{stock['before']} → {n(stock['after'])} sockets, stops answering", "s"),
      T(sx + 14, sy + 54, f"at connection {n(stock['conns'])}; +{stock['mb']} MB of stderr", "s"),
      T(sx + 14, sy + 69, "quoted from PR #79 comment, no raw file", "m"),
      T(px(churn["done"]), py(5) - 42, "bennu + fix (PR #79)", "b", "end"),
      T(px(churn["done"]), py(5) - 27, f"{churn['sockets_before']} → {churn['sockets_after']} sockets, {n(churn['done'])} of {n(churn['conns'])} done,", "s", "end"),
      T(px(churn["done"]), py(5) - 12, "still answering", "s", "end"),
      T(24, H - 24, "Dotted connectors join the two measured endpoints of each run; nothing was sampled in between.", "m"), T(24, H - 8, FOOT_ONE_RUN, "m")]
c2 = svg("robustness-stock-2-fd-leak.svg", W, H,
         f"Stock bennu leaks a socket per Modbus connection and stops at {n(stock['conns'])}; the fix holds at {churn['sockets_after']}",
         f"Open sockets before and after a connect/close storm ({leak_per_conn:.2f} sockets leaked per connection on stock)", b,
         "Open sockets of the Modbus field device against connections opened and closed, stock bennu against the fixed build.")

# ---------- chart 3: watcher log ----------
W, L, X0, Y0, BH = 760, 24, 150, 96, 16
kmax = 60
bx = lambda v: (v / 1000) / kmax * (W - X0 - 230)
b = []
for v in (0, 20, 40, 60):
    x = X0 + bx(v * 1000)
    b += [f'<line class="grid" x1="{x:.1f}" x2="{x:.1f}" y1="{Y0 - 12}" y2="{Y0 + 124}"/>', T(x, Y0 + 140, str(v), "s", "middle")]
b.append(T(X0 + bx(30000), Y0 + 160, "watcher-stderr.log growth, KB per minute", "s", "middle"))
for i, h in enumerate(sorted(bpm["stock"])):
    y = Y0 + i * 62
    b.append(T(L, y + 24, h, "b"))
    for j, (cls, name) in enumerate((("stock", "stock bennu"), ("fixed", "bennu + fix (PR #82)"))):
        v, yy = bpm[cls][h], y + j * (BH + 2)
        wv = max(bx(v), 2)
        b += [f'<g><title>{h}, {name}: {n(v)} B/min</title><path class="{cls}" d="M{X0},{yy} h{wv - min(4, wv):.1f} a4,4 0 0 1 4,4 v{BH - 8} a4,4 0 0 1 -4,4 h-{wv - min(4, wv):.1f} z"/></g>'
              if wv > 4 else f'<g><title>{h}, {name}: {n(v)} B/min</title><rect class="{cls}" x="{X0}" y="{yy}" width="2" height="{BH}"/></g>',
              T(X0 + (wv if v else bx(threshold)) + 8, yy + 12, f"{name}: {v / 1000:.1f} KB/min" if v else f"{name}: 0 B/min", "s" if v == 0 else "")]
xt = X0 + bx(threshold)
b += [f'<line x1="{xt:.1f}" x2="{xt:.1f}" y1="{Y0 - 12}" y2="{Y0 + 124}" stroke="#d03b3b" stroke-width="1.5" stroke-dasharray="6 4"/>',
      T(xt + 5, Y0 - 16, f"check limit {n(threshold)} B/min", "s"),
      T(24, Y0 + 186, f"Stock line is 'pid = N', {min(line_bytes.values()):.0f} bytes, about {min(lines_per_s.values()):.0f} times a second. " + FOOT_ONE_RUN, "m")]
c3 = svg("robustness-stock-3-watcher-log.svg", W, Y0 + 200,
         f"Stock watcher writes {max(bpm['stock'].values()) / 1000:.0f} KB/min to stderr on an idle RTU; with the fix, {max(bpm['fixed'].values())}",
         "Growth of watcher-stderr.log during the watcher-quiet check, two RTUs", b,
         "Bar chart of watcher stderr log growth per minute on two RTUs, stock bennu against the fixed build.")

# ---------- numbers ----------
print(f"scoreboard: stock {5 - fails['stock']}/5 pass ({raw_backed_stock_fails} fails parsed from reports + 1 quoted), fixed {5 - fails['fixed']}/5 pass")
print(f"report overall lines: " + ", ".join(f"{k}={v['overall']}" for k, v in R.items()))
print(f"fixed builds ({len(builds)}): " + " | ".join(builds))
print(f"modbus-badframes stock: {badframe_cases} cases, pid {pid_after[0]} -> {pid_after[1]}")
print(f"modbus-churn stock (QUOTED, no raw): {stock['before']} -> {stock['after']} sockets, stopped at connection {stock['conns']}, +{stock['mb']} MB stderr, "
      f"{leak_per_conn:.3f} sockets/conn, {FD_SOFT_LIMIT - stock['after']} below the {FD_SOFT_LIMIT} limit")
print(f"modbus-churn fixed (json): {churn['done']}/{churn['conns']} done, sockets {churn['sockets_before']} -> {churn['sockets_after']}, answers={churn['answers']}, log grew {churn['log_grew']} B")
print(f"watcher B/min: stock {bpm['stock']}, fixed {bpm['fixed']}, check limit {threshold}; stock x{min(bpm['stock'].values()) / threshold:.0f} over the limit")
print(f"watcher stock line: {line_bytes} bytes/line, {({h: round(v, 1) for h, v in lines_per_s.items()})} lines/s")
print(f"watcher-stderr.log on the #81 (robust-fixed) build, which lacks #82: {R['robust-fixed']['logs']}")
print("charts:", c1, c2, c3)
print(f"mismatches ({len(mism)}):")
for m in mism:
    print("  -", m)
