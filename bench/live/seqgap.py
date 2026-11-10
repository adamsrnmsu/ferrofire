#!/usr/bin/env python3
"""seqgap.py PCAP [PORT]: ferrofire datagram v1 stats from a tcpdump pcap: per group, datagrams, _ff.seq gaps, dups, restarts, cadence."""
import re, struct, sys
f = open(sys.argv[1], 'rb').read(); port = int(sys.argv[2]) if len(sys.argv) > 2 else 40000
magic = struct.unpack('<I', f[:4])[0]; e = '<' if magic in (0xa1b2c3d4, 0xa1b23c4d) else '>'; ns = magic in (0xa1b23c4d, 0x4d3cb2a1)
off = 24; g = {}; n = bad = noseq = 0; src = set()
sq = re.compile(rb'_ff\.seq:(\d+),'); cy = re.compile(rb'_ff\.cyc:(\d+),')
while off + 16 <= len(f):
    s, u, cl, ol = struct.unpack(e + 'IIII', f[off:off + 16]); off += 16; d = f[off:off + cl]; off += cl
    if len(d) < 42 or d[12:14] != b'\x08\x00' or d[23] != 17: continue
    ihl = (d[14] & 15) * 4; sp, dp = struct.unpack('>HH', d[14 + ihl:18 + ihl])
    if dp != port: continue
    p = d[14 + ihl + 8:]; n += 1; src.add('.'.join(map(str, d[26:30])))
    if not p or len(p) < 1 + p[0] or p[0] == 0 or p[-1:] != b'\0': bad += 1; continue
    grp = p[1:1 + p[0]].decode('latin1'); body = p[1 + p[0]:]; m = sq.search(body); c = cy.search(body)
    if not m: noseq += 1; continue
    t = s + u / (1e9 if ns else 1e6)
    g.setdefault(grp, []).append((t, int(m.group(1)), int(c.group(1)) if c else -1))
print('udp/%d datagrams %d, sources %s, malformed/truncated %d, no _ff.seq %d' % (port, n, ','.join(sorted(src)), bad, noseq))
for grp, v in sorted(g.items()):
    miss = dup = restarts = 0; prev = v[0][1]; gaps = []; worst = 0
    for i in range(1, len(v)):
        d = v[i][1] - prev
        if d > 1: miss += d - 1; worst = max(worst, d - 1)
        elif d == 0: dup += 1
        elif d < 0:
            if v[i][1] < 5: restarts += 1   # provider restart: counter back near 0
            else: dup += 1                  # reordered
        prev = v[i][1]; gaps.append(v[i][0] - v[i - 1][0])
    gaps.sort(); q = lambda x: gaps[min(len(gaps) - 1, int(x * len(gaps)))] * 1000 if gaps else 0
    print('group %s: received %d, seq %d..%d, missing %d (%.4f%%), longest run lost %d, dup/reordered %d, restarts %d, cycles %d..%d' % (
        grp, len(v), v[0][1], v[-1][1], miss, 100.0 * miss / max(1, len(v) + miss), worst, dup, restarts, v[0][2], v[-1][2]))
    print('  inter-arrival ms: p50 %.1f p99 %.1f max %.1f' % (q(.5), q(.99), gaps[-1] * 1000 if gaps else 0))
