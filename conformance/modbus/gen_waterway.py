"""Writes diff-waterway-server.xml: the rtu-1 config of the waterway range as sceptre's fd_server.mako renders it (register map
phenix-waterway-range/topologies/waterway-arm64/reg_addrs.json, every range [0,1000], logic from waterway-scenario.yaml), with the
endpoint moved to 127.0.0.1:5502 for the differential. usage: python3 gen_waterway.py > diff-waterway-server.xml"""
REGS = [("input-register", 30000 + i, n) for i, n in enumerate(
    ["lowerwater.height", "lock.height", "lowergate.open", "lowervalve.open", "lowersensor.active", "lowersensor.direction",
     "locksensor.active", "locksensor.direction", "boat.location", "boat.direction"])]
REGS.insert(3, ("holding-register", 40000, "lowergate.open"))
REGS.insert(5, ("holding-register", 40001, "lowervalve.open"))
LOGIC = ("lowergate.open = 1*((lowersensor.direction == 1) && (lowersensor.active == 1) && (lock.height >= lowerwater.height - 0.1) && (lock.height <= lowerwater.height + 0.1)) + 0*((locksensor.direction == 1) && (locksensor.active == 1) && (lock.height >= lowerwater.height - 0.1) && (lock.height <= lowerwater.height + 0.1)) + 1*((locksensor.direction == -1) && (locksensor.active == 1) && (lock.height >= lowerwater.height - 0.1) && (lock.height <= lowerwater.height + 0.1)) + 0*((lowersensor.direction == -1) && (lowersensor.active == 1)); "
         "lowervalve.open = 1*((lowersensor.direction == 1) && (lowersensor.active == 1) && (lock.height > lowerwater.height)) + 0*((locksensor.direction == 1) && (locksensor.active == 1) && (lock.height >= lowerwater.height)) + 1*((locksensor.direction == -1) && (locksensor.active == 1) && (lock.height > lowerwater.height)) + 0*((lowersensor.direction == -1) && (lowersensor.active == 1))")
name, inputs, outputs, tags, comms, ins, outs = "rtu-1", {}, {}, [], [], [], []
for typ, addr, field in REGS:
    isin = typ in ("input-register", "discrete-input")
    io = f"{name}_{'I' if isin else 'O'}{addr}"
    tags.append(f"<external-tag><name>var_{io}</name><io>{io}</io><type>analog</type></external-tag>")
    comms.append(f"<{typ}><address>{addr}</address><max-value>1000</max-value><min-value>0</min-value><tag>var_{io}</tag></{typ}>")
    (inputs if isin else outputs)[field] = f"var_{io}"
    (ins if isin else outs).append(f"<analog><id>{io}</id><name>{field}</name></analog>")
def swap(m, s):
    for k in sorted(m, key=len, reverse=True): s = s.replace(k, m[k])
    return s
lines = []
for line in LOGIC.split(";"):
    if line:
        lhs, rhs = line.split("=", 1)
        lines.append(f"{swap(outputs, lhs).strip()} = {swap(inputs, rhs).strip()}")
j = "\n            "
print(f'''<?xml version="1.0" encoding="utf-8"?>
<SCEPTRE>
    <field-device>
        <name>{name}</name>
        <cycle-time>1000</cycle-time>
        <tags>
            {j.join(tags)}
        </tags>
        <comms>
            <modbus-server>
                <endpoint>tcp://127.0.0.1:5502</endpoint>
                <event-logging>{name}-modbus-tcp-server.log</event-logging>
                {(j + "    ").join(comms)}
            </modbus-server>
        </comms>
        <input>
            <endpoint>udp://*;239.0.0.1:40000</endpoint>
            {j.join(ins)}
        </input>
        <output>
            <endpoint>tcp://127.0.0.1:5555</endpoint>
            {j.join(outs)}
        </output>
        <logic>
            <![CDATA[
            {(j + "").join(lines)}
            ]]>
        </logic>
    </field-device>
</SCEPTRE>''')
