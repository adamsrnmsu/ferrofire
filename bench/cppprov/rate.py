#!/usr/bin/env python3
"""rate.py GROUP PORT SECONDS PERIOD : join a multicast group, count datagrams for SECONDS, print one JSON line: datagrams, datagrams per s, publishes per s (a gap over PERIOD/2
starts a new publish: a publish is one or more chunks), size mean, publish-to-publish interval mean / p50 / p99 / max (ms). The publish rate the provider achieved (ww-1df.42). Stdlib only, runs in the bench containers."""
import json, socket, struct, sys, time
g, port, secs, period = sys.argv[1], int(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("", port))
s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, struct.pack("4s4s", socket.inet_aton(g), socket.inet_aton("0.0.0.0")))
s.settimeout(1)
ts, sz, end = [], [], time.monotonic() + secs
while time.monotonic() < end:
    try:
        b = s.recv(65535)
    except socket.timeout:
        continue
    ts.append(time.monotonic()); sz.append(len(b))
starts = [t for i, t in enumerate(ts) if i == 0 or t - ts[i - 1] > period / 2]   # first chunk of each publish
gaps = sorted((b - a) * 1e3 for a, b in zip(starts, starts[1:]))
p = lambda q: round(gaps[min(len(gaps) - 1, int(q * len(gaps)))], 2) if gaps else None
print(json.dumps({"datagrams": len(ts), "datagrams_per_s": round(len(ts) / secs, 3), "publishes": len(starts), "publishes_per_s": round(len(starts) / secs, 3), "bytes_mean": round(sum(sz) / len(sz)) if sz else 0,
                  "gap_ms_mean": round(sum(gaps) / len(gaps), 2) if gaps else None, "gap_ms_p50": p(.5), "gap_ms_p99": p(.99), "gap_ms_max": round(gaps[-1], 2) if gaps else None}))
