"""Writes diff-edges-server.xml: a logic-free Modbus server with every table, address-space corners and a spread of scaling ranges.
No XML comment in the output: the stock parser exits silently on one before the root element.  usage: python3 gen_edges.py > diff-edges-server.xml"""
# (min, max) per register; None = key not given. Input registers 30000+k and holding registers 40000+k use the same list.
RANGES = [(0, 1000), (-1000, 2000), (0, 1), (-100, 100), (0, 65535), (None, 500), (-10, None), (50, -50), (100, 100), (0, 0.001), (10, 10.5), (-32768, 32767)]
COILS = [0, 1, 2, 3, 7] + list(range(10, 20)) + [65535]
DISCRETES = [0] + list(range(10000, 10012)) + [65535]
INREG = [(30000 + k, r) for k, r in enumerate(RANGES)] + [(0, (0, 1000)), (65535, (0, 1000))]
HOLD = [(40000 + k, r) for k, r in enumerate(RANGES)] + [(0, (0, 1000)), (65535, (0, 1000))]

tags, mappings, ins, outs = [], [], [], []
def tag(name, io, kind): tags.append(f"<external-tag><name>{name}</name><io>{io}</io><type>{kind}</type></external-tag>")
def rng(r):
    lo, hi = r
    return "".join(f"<{k}-value>{v}</{k}-value>" for k, v in (("max", hi), ("min", lo)) if v is not None)
for a, r in INREG:
    tag(f"ir{a}", f"ia{a}", "analog"); ins.append(f"<analog><id>ia{a}</id><name>ia{a}</name></analog>")
    mappings.append(f"<input-register><address>{a}</address>{rng(r)}<tag>ir{a}</tag></input-register>")
for a in DISCRETES:
    tag(f"di{a}", f"ib{a}", "binary"); ins.append(f"<binary><id>ib{a}</id><name>ib{a}</name></binary>")
    mappings.append(f"<discrete-input><address>{a}</address><tag>di{a}</tag></discrete-input>")
for a in COILS:
    tag(f"co{a}", f"ob{a}", "binary"); outs.append(f"<binary><id>ob{a}</id><name>ob{a}</name></binary>")
    mappings.append(f"<coil><address>{a}</address><tag>co{a}</tag></coil>")
for a, r in HOLD:
    tag(f"hr{a}", f"oa{a}", "analog"); outs.append(f"<analog><id>oa{a}</id><name>oa{a}</name></analog>")
    mappings.append(f"<holding-register><address>{a}</address>{rng(r)}<tag>hr{a}</tag></holding-register>")
def main():
    nl = "\n        "
    print(f'''<?xml version="1.0" encoding="utf-8"?>
<SCEPTRE>
    <field-device>
        <name>edges</name>
        <cycle-time>200</cycle-time>
        <tags>
        {nl.join(tags)}
        </tags>
        <comms>
            <modbus-server>
                <endpoint>tcp://127.0.0.1:5502</endpoint>
                <event-logging>edges-modbus-server.log</event-logging>
                {(nl + "    ").join(mappings)}
            </modbus-server>
        </comms>
        <input>
            <endpoint>udp://239.0.0.1:40000</endpoint>
            {(nl + "    ").join(ins)}
        </input>
        <output>
            <endpoint>tcp://127.0.0.1:5555</endpoint>
            {(nl + "    ").join(outs)}
        </output>
    </field-device>
</SCEPTRE>''')


if __name__ == "__main__":
    main()
