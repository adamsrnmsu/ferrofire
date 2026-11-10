#!/usr/bin/env python3
"""matrix_table.py : markdown table of the live matrix (ww-2s0) from docs/bench/results/live/<host>/cap-<cfg>-<n>-<host>.txt"""
import re, statistics as st, sys
D = "docs/bench/results/live"
def parse(f):
    t = open(f).read(); dur = float(re.search(r'dur ([\d.]+)s', t).group(1))
    u = re.search(r'udp:40000 pkts (\d+) bytes (\d+)', t); k = re.search(r'tcp:5555 pkts (\d+) bytes (\d+)', t)
    cp = re.search(r'cpu ticks before: ([\d ]+)after: ([\d ]+)\(', t)
    cpu = (sum(map(int, cp.group(2).split())) - sum(map(int, cp.group(1).split()))) / dur   # ticks/s at CLK_TCK 100 = % of one core
    return dict(udp=int(u.group(2)) / dur if u else 0, tcp=int(k.group(2)) / dur if k else 0, tpk=int(k.group(1)) / dur if k else 0, cpu=cpu)
def cell(vals, fmt):
    return (fmt % st.mean(vals)) + (" (%s-%s)" % (fmt % min(vals), fmt % max(vals)))
CFG = [("pushstock", "push, stock `bennu-simulink-provider` + solver (10 Hz multicast)"), ("pushff", "push, `ferrofire run` datagram v1 (10 Hz multicast)"),
       ("pull100", "pull-bridge, `--rate 100` ms"), ("pull1000", "pull-bridge, `--rate 1000` ms")]
print("| 30 s window, 3 reps (mean, min-max) | Provider NIC udp+tcp B/s | Provider tcp pkts/s | rtu-1 NIC B/s | rtu-2 NIC B/s | Provider CPU (% core) | RTU CPU: rtu-1 / rtu-2 (% core, device + watcher) |")
print("|---|---|---|---|---|---|---|")
for c, name in CFG:
    r = {h: [parse(f"{D}/{h}/cap-{c}-{n}-{h}.txt") for n in (1, 2, 3)] for h in ("waterway-provider", "rtu-1", "rtu-2")}
    p = r["waterway-provider"]
    print("| %s | %s | %s | %s | %s | %s | %s / %s |" % (name, cell([x["udp"] + x["tcp"] for x in p], "%.0f"), cell([x["tpk"] for x in p], "%.0f"),
          cell([x["udp"] + x["tcp"] for x in r["rtu-1"]], "%.0f"), cell([x["udp"] + x["tcp"] for x in r["rtu-2"]], "%.0f"),
          cell([x["cpu"] for x in p], "%.1f"), cell([x["cpu"] for x in r["rtu-1"]], "%.1f"), cell([x["cpu"] for x in r["rtu-2"]], "%.1f")))
