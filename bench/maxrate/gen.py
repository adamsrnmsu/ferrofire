#!/usr/bin/env python3
"""gen.py MODEL R DT_MS PUB_MS OUTDIR: the provider model and the R field-device configs of one maximum-rate cell (ww-1df.43). Stdlib only, deterministic.

MODEL is S (15 tags), M (800), L (8112) or G (the control grid of bench/gen_grid.py: 250 buses, 1250 breakers, 10 generators).
Writes OUTDIR/model.yaml (ferrofire run; engine tick DT_MS, publish period PUB_MS, one publish slice = one multicast group 239.0.1.<n>:40000 per RTU) and OUTDIR/rtu-<n>.xml (modbus-server).
Every model carries a tick counter: state bench.k (+1 per engine tick) and the output bench.cnt = bench.k mod 65536, published to RTU 1 and mapped to its input register 39000
(range 0..65535, so the register IS the integer the provider sent). The observer (ffmaxrate) times the register against the datagram that carried the value.
S/M/L: bench.tNNNN = (bench.k * 0.1 + NNNN * 0.37) % 1000, so every value changes every tick (the worst case for the wire and the device); tags are dealt round-robin over the slices.
G: the grid and its RTU configs as gen_grid writes them, plus the counter."""
import os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import gen_grid

SIZES = {"S": 15, "M": 800, "L": 8112}
CNT_IR = 39000


def counter(dt):
    return f"  bench.k: {{ode: \"{1 / dt:g}\", init: 0}}\n", "  bench.cnt: {type: analog, expr: \"floor(bench.k + 0.5) % 65536\"}\n"


def synthetic(n, r, dt, pub):
    st, cnt = counter(dt)
    names = ["bench.cnt"] + [f"bench.in{i}" for i in range(4)] + [f"bench.t{k:04d}" for k in range(n - 5)]
    ins = "".join(f"  bench.in{i}: {{type: analog, default: 0}}\n" for i in range(4))
    outs = cnt + "".join(f"  bench.in{i}: {{type: analog}}\n" for i in range(4)) + "".join(
        f"  bench.t{k:04d}: {{type: analog, expr: \"(bench.k * 0.1 + {k} * 0.37) % 1000\"}}\n" for k in range(n - 5))
    slices = [names[i::r] for i in range(r)]
    sl = "".join(f"  - endpoint: 'udp://*;239.0.1.{i + 1}:40000'\n    tags: [{', '.join(t)}]\n" for i, t in enumerate(slices))
    return f"version: 1\ndt: {dt}\npublish_rate: {pub}\ninputs:\n{ins}states:\n{st}outputs:\n{outs}publish_slices:\n{sl}", slices


def fd_xml(i, slice_):
    tags, ins, irs = [], [], []
    for j, name in enumerate(slice_):
        cnt = name == "bench.cnt"
        tags.append(f"<external-tag><name>t{j}</name><io>I{j}</io><type>analog</type></external-tag>")
        ins.append(f"<analog><id>I{j}</id><name>{name}</name></analog>")
        irs.append(f"<input-register><address>{CNT_IR if cnt else 30000 + j}</address><max-value>{65535 if cnt else 1000}</max-value><min-value>0</min-value><tag>t{j}</tag></input-register>")
    tags.append("<external-tag><name>sp0</name><io>O0</io><type>analog</type></external-tag>")
    hr = "<holding-register><address>40000</address><max-value>1000</max-value><min-value>0</min-value><tag>sp0</tag></holding-register>"
    j = "\n            "
    return (f"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<SCEPTRE>\n    <field-device>\n        <name>rtu-{i}</name>\n        <cycle-time>50</cycle-time>\n"
            f"        <tags>\n            {j.join(tags)}\n        </tags>\n        <comms>\n            <modbus-server>\n                <endpoint>tcp://0.0.0.0:502</endpoint>\n"
            f"                {(chr(10) + '                ').join(irs + [hr])}\n            </modbus-server>\n        </comms>\n"
            f"        <input>\n            <endpoint>udp://*;239.0.1.{i}:40000</endpoint>\n            {j.join(ins)}\n        </input>\n"
            "        <output>\n            <endpoint>tcp://provider:5555</endpoint>\n            <analog><id>O0</id><name>bench.in0</name></analog>\n        </output>\n"
            "    </field-device>\n</SCEPTRE>\n")


def grid(r, dt, pub, out):
    """gen_grid's model and configs for r RTUs, with the counter added (to the model, RTU 1's slice and RTU 1's config)."""
    tmp = os.path.join(out, "grid")
    gen_grid.emit(gen_grid.build(25, 10, 1), tmp, [r], rate=pub, dt=dt)
    st, cnt = counter(dt)
    m = open(f"{tmp}/R{r}/model.yaml").read().replace("../case.py", "grid/case.py")
    m = m.replace("publish_slices:", f"states:\n{st}outputs:\n{cnt}publish_slices:").replace("tags: [", "tags: [bench.cnt, ", 1)
    x = open(f"{tmp}/R{r}/ferrofire/rtu-1.xml").read()
    for end, add in (("</tags>", "<external-tag><name>benchcnt</name><io>I9999</io><type>analog</type></external-tag>"),
                     ("</modbus-server>", f"<input-register><address>{CNT_IR}</address><max-value>65535</max-value><min-value>0</min-value><tag>benchcnt</tag></input-register>"),
                     ("</input>", "<analog><id>I9999</id><name>bench.cnt</name></analog>")):
        assert x.count(end) == 1
        x = x.replace(end, f"    {add}\n        {end}")
    open(f"{out}/rtu-1.xml", "w").write(x)
    for i in range(2, r + 1):
        os.replace(f"{tmp}/R{r}/ferrofire/rtu-{i}.xml", f"{out}/rtu-{i}.xml")
    return m


def main(model, r, dt_ms, pub_ms, out):
    r, dt, pub = int(r), float(dt_ms) / 1000, float(pub_ms) / 1000
    os.makedirs(out, exist_ok=True)
    if model == "G":
        m = grid(r, dt, pub, out)
    else:
        m, slices = synthetic(SIZES[model], r, dt, pub)
        for i, s in enumerate(slices, 1):
            open(f"{out}/rtu-{i}.xml", "w").write(fd_xml(i, s))
    open(f"{out}/model.yaml", "w").write(m)


if __name__ == "__main__":
    main(*sys.argv[1:6])
