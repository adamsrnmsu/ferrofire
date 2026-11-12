#!/usr/bin/env python3
"""gen_grid.py OUTDIR [--h 25 --w 10 --seed 1 --rtus 1,2,5,10] -> the synthetic transmission grid of the control-latency bench
(epic ww-1df.23). Stdlib only, deterministic for a given (h, w, seed).

Grid: an h x w torus of buses, five branch (breaker) families per bus: (+1,0) (0,+1) (+1,+1) (+1,-1) (0,+2), so 5*h*w breakers
(default 1250 on 250 buses). Bus 1 is the slack (generator row 0, never commanded). 10 commandable PV generators sit on evenly spaced
buses (generator j = gen row j+1). Every breaker is a branch whose `.active` tag is the command and the status.

Islanding: a spanning tree of the torus (BFS) is kept always closed; `flip_order` is the shuffled list of the other
5*h*w - (h*w - 1) breakers (1001 at the default). Opening ANY prefix of flip_order, up to all of it, leaves the grid connected, so the
slack never islands and every solve is a plain AC solve. Flip n breakers = the first n entries of flip_order (n <= len(flip_order)).
Envelope: all 1001 safe flips open, or all 10 generators off, solve; both at once collapse the voltage (bus-alert) and are out of range.
A driver that opens something else owns the consequence (ferrofire publishes VM = 0 for the dead buses, `islands: deenergize`).

Partition over R RTUs: breaker k -> RTU k % R, generator j -> RTU j % R (round-robin). Output per R in OUTDIR/R<R>/: model.yaml
(ferrofire provider, per-RTU publish slices), config.ini (stock pybennu-power-solver), ferrofire/rtu-N.xml and bennu/rtu-N.xml
(field-device configs; they differ only in the input endpoint, a per-RTU group for ferrofire's slices, one shared group for stock),
scada.json (register map). OUTDIR/case.py (PYPOWER format, shared by both providers) and OUTDIR/grid.json (topology, flip_order)."""
import argparse, json, os, random

FAMILIES = [(1, 0), (0, 1), (1, 1), (1, -1), (0, 2)]
PD, QD = 2.0, 0.5            # MW / MVAr per bus
GEN_PG, GEN_PMAX = 40.0, 100.0   # commandable generators: initial setpoint and limit (MW)
N_GENS = 10


def build(h, w, seed):
    """Topology and case rows. Bus ids 1..h*w; branches in family order; gens[0] is the slack."""
    assert h >= 3 and w >= 5, "torus too small: parallel branches"
    rng = random.Random(seed)
    nb = h * w
    bid = lambda x, y: (x % h) * w + (y % w) + 1
    branches, seen = [], set()
    for fx, fy in FAMILIES:
        for x in range(h):
            for y in range(w):
                f, t = bid(x, y), bid(x + fx, y + fy)
                assert f != t and frozenset((f, t)) not in seen
                seen.add(frozenset((f, t)))
                branches.append((f, t, 0.01, round(0.08 + 0.04 * rng.random(), 5), 0.002))
    # BFS spanning tree stays closed; the remaining branches may open in any combination
    adj = {i: [] for i in range(1, nb + 1)}
    for k, (f, t, *_) in enumerate(branches):
        adj[f].append((t, k)); adj[t].append((f, k))
    tree, reached, queue = set(), {1}, [1]
    for u in queue:
        for v, k in adj[u]:
            if v not in reached:
                reached.add(v); tree.add(k); queue.append(v)
    assert len(tree) == nb - 1
    flip_order = [k for k in range(len(branches)) if k not in tree]
    rng.shuffle(flip_order)
    gen_buses = [1 + (j + 1) * nb // (N_GENS + 1) for j in range(N_GENS)]
    assert len(set(gen_buses)) == N_GENS and 1 not in gen_buses
    return {"h": h, "w": w, "seed": seed, "nbus": nb, "branches": branches, "flip_order": flip_order,
            "gen_buses": gen_buses}


def case_py(g):
    """PYPOWER-format case (13 bus columns, 10 gen, 13 branch). Gen rows: slack first, then the commandable ones."""
    gb = set(g["gen_buses"])
    rows = lambda rs: ",\n        ".join("[" + ", ".join(repr(float(v)) for v in r) + "]" for r in rs)
    bus = [[i, 3 if i == 1 else (2 if i in gb else 1), PD, QD, 0, 0, 1, 1.0, 0.0, 345, 1, 1.1, 0.9]
           for i in range(1, g["nbus"] + 1)]
    gen = [[1, 0, 0, 500, -500, 1.0, 100, 1, 1000, 0]] + [[b, GEN_PG, 0, 50, -50, 1.0, 100, 1, GEN_PMAX, 0] for b in g["gen_buses"]]
    br = [[f, t, r, x, b, 9900, 9900, 9900, 0, 0, 1, -360, 360] for f, t, r, x, b in g["branches"]]
    return ("from numpy import array\n\n\ndef case_control():\n    ppc = {\"version\": '2'}\n    ppc[\"baseMVA\"] = 100.0\n"
            f"    ppc[\"bus\"] = array([\n        {rows(bus)}\n    ])\n"
            f"    ppc[\"gen\"] = array([\n        {rows(gen)}\n    ])\n"
            f"    ppc[\"branch\"] = array([\n        {rows(br)}\n    ])\n    return ppc\n")


def bname(g, k):
    f, t = g["branches"][k][:2]
    return f"branch-1_{f}-{t}"


def gname(g, j):
    return f"generator-1_bus-{g['gen_buses'][j]}"


def partition(g, nr):
    """Per RTU: its breaker indices, generator indices and 5 monitor buses (ends of its first breakers)."""
    out = []
    for r in range(nr):
        brk = list(range(r, len(g["branches"]), nr))
        gens = list(range(r, N_GENS, nr))
        mon = []
        for k in brk:
            for b in g["branches"][k][:2]:
                if b not in mon and len(mon) < 5:
                    mon.append(b)
        out.append({"breakers": brk, "gens": gens, "mon": mon})
    return out


def rtu_map(g, r, part):
    """Register map of one RTU. Coils/discrete inputs: breakers then gen status; 3xxxx input regs, 4xxxx holding regs."""
    nb = len(part["breakers"])
    m = {"name": f"rtu-{r + 1}", "breakers": [], "gens": [], "aux": []}
    for i, k in enumerate(part["breakers"]):
        m["breakers"].append({"idx": k, "point": bname(g, k) + ".active", "coil": i, "di": i})
    for i, j in enumerate(part["gens"]):
        b = g["gen_buses"][j]
        m["gens"].append({"idx": j, "bus": b, "status_point": gname(g, j) + ".active", "coil": nb + i, "di": nb + i,
                          "setpoint_point": gname(g, j) + ".mw", "hr": 40000 + i, "pg_point": f"bus-{b}.gen_mw", "ir": 30000 + i,
                          "range": [0, GEN_PMAX]})
    for i, b in enumerate(part["mon"]):
        m["aux"].append({"point": f"bus-{b}.voltage", "ir": 30100 + 2 * i, "range": [0.5, 1.5]})
        m["aux"].append({"point": f"bus-{b}.angle", "ir": 30101 + 2 * i, "range": [-90, 90]})
    return m


def fd_xml(m, input_ep, provider_ep, modbus_ep):
    """A sceptre-style config.xml: tags are aliases onto points; input points come from the provider, output points go to it."""
    tags, coils, dis, hrs, irs, ins, outs = [], [], [], [], [], [], []
    n = {"I": 0, "O": 0}

    def pt(io, kind, name, tag):
        pid = f"{io}{n[io]}"; n[io] += 1
        (ins if io == "I" else outs).append(f"<{kind}><id>{pid}</id><name>{name}</name></{kind}>")
        tags.append(f"<external-tag><name>{tag}</name><io>{pid}</io><type>{kind}</type></external-tag>")

    reg = lambda el, a, tag, lo, hi: f"<{el}><address>{a}</address><max-value>{hi}</max-value><min-value>{lo}</min-value><tag>{tag}</tag></{el}>"
    for i, b in enumerate(m["breakers"]):
        pt("O", "binary", b["point"], f"brk{i}_cmd"); coils.append(f"<coil><address>{b['coil']}</address><tag>brk{i}_cmd</tag></coil>")
        pt("I", "binary", b["point"], f"brk{i}_st"); dis.append(f"<discrete-input><address>{b['di']}</address><tag>brk{i}_st</tag></discrete-input>")
    for i, e in enumerate(m["gens"]):
        pt("O", "binary", e["status_point"], f"gen{i}_cmd"); coils.append(f"<coil><address>{e['coil']}</address><tag>gen{i}_cmd</tag></coil>")
        pt("I", "binary", e["status_point"], f"gen{i}_st"); dis.append(f"<discrete-input><address>{e['di']}</address><tag>gen{i}_st</tag></discrete-input>")
        pt("O", "analog", e["setpoint_point"], f"gen{i}_sp"); hrs.append(reg("holding-register", e["hr"], f"gen{i}_sp", *e["range"]))
        pt("I", "analog", e["pg_point"], f"gen{i}_pg"); irs.append(reg("input-register", e["ir"], f"gen{i}_pg", *e["range"]))
    for i, a in enumerate(m["aux"]):
        pt("I", "analog", a["point"], f"aux{i}"); irs.append(reg("input-register", a["ir"], f"aux{i}", *a["range"]))
    j = "\n            "
    return (f"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<SCEPTRE>\n    <field-device>\n        <name>{m['name']}</name>\n"
            f"        <cycle-time>50</cycle-time>\n        <tags>\n            {j.join(tags)}\n        </tags>\n"
            f"        <comms>\n            <modbus-server>\n                <endpoint>{modbus_ep}</endpoint>\n"
            f"                {(chr(10) + '                ').join(coils + dis + hrs + irs)}\n            </modbus-server>\n        </comms>\n"
            f"        <input>\n            <endpoint>{input_ep}</endpoint>\n            {j.join(ins)}\n        </input>\n"
            f"        <output>\n            <endpoint>{provider_ep}</endpoint>\n            {j.join(outs)}\n        </output>\n"
            "    </field-device>\n</SCEPTRE>\n")


def model_yaml(slices, rate):
    s = "".join(f"  - endpoint: 'udp://*;239.0.1.{r + 1}:40000'\n    tags: [{', '.join(t)}]\n" for r, t in enumerate(slices))
    return ("# generated by bench/gen_grid.py: do not edit\nversion: 1\ndt: 0.05\n"
            f"publish_rate: {rate}\npower:\n  case: ../case.py\n  islands: deenergize\npublish_slices:\n{s}")


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def emit(g, out, rtus, rate=0.1, provider_host="provider", rtu_host="rtu-{n}"):
    write(f"{out}/case.py", case_py(g))
    write(f"{out}/grid.json", json.dumps({k: g[k] for k in ("h", "w", "seed", "nbus", "gen_buses", "flip_order")}
                                         | {"n_breakers": len(g["branches"]), "breakers": [bname(g, k) + ".active" for k in range(len(g["branches"]))]}, indent=1))
    for nr in rtus:
        d, parts = f"{out}/R{nr}", partition(g, nr)
        maps = [rtu_map(g, r, p) for r, p in enumerate(parts)]
        slices = []
        for r, m in enumerate(maps):
            ep = "tcp://0.0.0.0:502"
            pep = f"tcp://{provider_host}:5555"
            write(f"{d}/ferrofire/{m['name']}.xml", fd_xml(m, f"udp://*;239.0.1.{r + 1}:40000", pep, ep))
            write(f"{d}/bennu/{m['name']}.xml", fd_xml(m, "udp://*;239.0.0.1:40000", pep, ep))
            slices.append(sorted({p for b in m["breakers"] for p in (b["point"],)} | {x for e in m["gens"] for x in (e["status_point"], e["setpoint_point"], e["pg_point"])}
                                 | {a["point"] for a in m["aux"]}))
        write(f"{d}/model.yaml", model_yaml(slices, rate))
        write(f"{d}/config.ini", "[power-solver-service]\nsolver-type = PyPower\ndebug = false\nserver-endpoint = tcp://0.0.0.0:5555\n"
                                 "publish-endpoint = udp://*;239.0.0.1:40000\ncase-file = /etc/sceptre/case.py\n")
        write(f"{d}/scada.json", json.dumps({"R": nr, "seed": g["seed"], "port": 502, "rtus": [m | {"host": rtu_host.format(n=r + 1)} for r, m in enumerate(maps)],
                                              "flip_order": g["flip_order"], "gen_order": list(range(N_GENS)),
                                              "gen_setpoint": GEN_PG}, indent=1))


def main():
    a = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    a.add_argument("out")
    a.add_argument("--h", type=int, default=25)
    a.add_argument("--w", type=int, default=10)
    a.add_argument("--seed", type=int, default=1)
    a.add_argument("--rtus", default="1,2,5,10")
    a.add_argument("--rate", type=float, default=0.1, help="ferrofire publish period (s)")
    a.add_argument("--provider-host", default="provider")
    a = a.parse_args()
    g = build(a.h, a.w, a.seed)
    emit(g, a.out, [int(x) for x in a.rtus.split(",")], a.rate, a.provider_host)
    print(f"{len(g['branches'])} breakers, {g['nbus']} buses, {N_GENS} generators, {len(g['flip_order'])} safe flips -> {a.out}")


if __name__ == "__main__":
    main()
