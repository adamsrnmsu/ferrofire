#!/usr/bin/env python3
"""top.py FOLDED BINARY [N] : self/total % per function from a stackcollapse file; file:line of each function's entry via nm+addr2line."""
import sys, re, subprocess, collections
f, binp, N = sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 15
self_, tot, att, T = collections.Counter(), collections.Counter(), collections.Counter(), 0
HASH = re.compile(r"\[[0-9a-f]{16}\]")
def dem(names):
    names = sorted(names)
    out = subprocess.run(["c++filt"], input="\n".join(names), capture_output=True, text=True).stdout.split("\n")
    return {a: HASH.sub("", b) for a, b in zip(names, out)}
raw = [l.rstrip() for l in open(f)]
syms = {x for l in raw for x in l.rpartition(" ")[0].split(";")}
D = dem(syms)
open(f.replace(".folded", ".dem.folded"), "w").write("\n".join(";".join(D[x] for x in l.rpartition(" ")[0].split(";")) + " " + l.rpartition(" ")[2] for l in raw) + "\n")
for ln in (";".join(D[x] for x in l.rpartition(" ")[0].split(";")) + " " + l.rpartition(" ")[2] for l in raw):
    st, _, c = ln.rstrip().rpartition(" "); c = int(c); fr = st.split(";")[1:] or st.split(";")  # drop comm
    T += c; self_[fr[-1]] += c
    nf = next((x for x in reversed(fr) if 'ferrofire::' in x or 'ffcontrol::' in x or 'ffbench::' in x), fr[-1]); att[nf] += c
    for x in set(fr): tot[x] += c
nm = {}
for l in subprocess.run(["nm", "-C", "--defined-only", binp], capture_output=True, text=True).stdout.splitlines():
    p = l.split(" ", 2)
    if len(p) == 3 and p[1] in "tTwW": nm.setdefault(HASH.sub("", re.sub(r"::h[0-9a-f]{16}$", "", p[2])), p[0])
def loc(sym):
    a = nm.get(sym)
    if not a: return "?"
    o = subprocess.run(["addr2line", "-e", binp, "0x" + a], capture_output=True, text=True).stdout.strip()
    return re.sub(r"^.*/(\.cargo/registry/src/[^/]+/|src/)", lambda m: m.group(1) if m.group(1) == "src/" else "", o)
print(f"samples {T}")
print("| self% | total% | symbol | entry file:line |\n|---|---|---|---|")
for s, c in self_.most_common(N):
    print(f"| {100*c/T:.1f} | {100*tot[s]/T:.1f} | `{s[:90]}` | {loc(s)} |")
print("\ntop by total (non-leaf, excluding leaves above):")
print("| total% | self% | symbol | entry file:line |\n|---|---|---|---|")
for s, c in tot.most_common(N + 8):
    if s in dict(self_.most_common(N)): continue
    print(f"| {100*c/T:.1f} | {100*self_[s]/T:.1f} | `{s[:90]}` | {loc(s)} |")

print("\nself attributed to the nearest ferrofire:: frame (kernel/libc/alloc callees folded into their caller):")
print("| attributed% | symbol | entry file:line |\n|---|---|---|")
for s, c in att.most_common(N):
    print(f"| {100*c/T:.1f} | `{s[:90]}` | {loc(s)} |")
