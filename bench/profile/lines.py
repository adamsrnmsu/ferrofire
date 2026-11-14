#!/usr/bin/env python3
"""lines.py PERF.DATA BINARY SYMSUBSTR [N] : self-sample source lines (inlined frames expanded, innermost first) inside functions whose
raw symbol contains SYMSUBSTR (run in ffprof-perf:22.04: perf script + addr2line)."""
import sys, subprocess, collections, re
data, binp, sub, rawsym, N = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5]) if len(sys.argv) > 5 else 12
nm = {}
for l in subprocess.run(["nm", "--defined-only", binp], capture_output=True, text=True).stdout.splitlines():
    p = l.split(" ", 2)
    if len(p) == 3 and p[1] in "tTwW": nm.setdefault(p[2], int(p[0], 16))
ips = collections.Counter(); tot = 0; base = None
out = subprocess.run(["perf", "script", "-i", data, "-G", "-F", "ip,sym,symoff"], capture_output=True, text=True).stdout
for l in out.splitlines():
    m = re.match(r"\s*([0-9a-f]+)\s+(\S+)\+0x([0-9a-f]+)", l)
    if not m: continue
    tot += 1
    if sub in m.group(2):
        if base is None: base = int(m.group(1), 16) - int(m.group(3), 16) - nm[rawsym]
        ips[int(m.group(1), 16) - base] += 1
loc = collections.Counter(); outer = collections.Counter()
for a, c in ips.items():
    r = subprocess.run(["addr2line", "-e", binp, "-i", "-f", hex(a)], capture_output=True, text=True).stdout.strip().split("\n")
    pairs = list(zip(r[0::2], r[1::2]))
    short = lambda s: re.sub(r"^.*?/(src/|library/|\.cargo/registry/src/[^/]+/)", r"\1", s)
    loc[short(pairs[0][1])] += c
    outer[short(pairs[-1][1])] += c
print(f"samples in matching functions: {sum(ips.values())} of {tot} ({100*sum(ips.values())/tot:.1f}%)")
print("innermost line:")
for k, c in loc.most_common(N): print(f"  {100*c/tot:5.1f}%  {k}")
print("outermost (non-inlined) line:")
for k, c in outer.most_common(5): print(f"  {100*c/tot:5.1f}%  {k}")
