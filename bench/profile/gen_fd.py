#!/usr/bin/env python3
"""gen_fd.py N PROVIDER_HOST > rtu.xml : field device fed by the whole N-tag stream of bench/gen_model.py (sim.t, bench.in0..3, bench.tNNNN),
Modbus server (4 writable holding regs = bench.in0..3, 200 input regs, 4 coils-less), DNP3 outstation (200 analog inputs, 4 analog outputs)."""
import sys
n, host = int(sys.argv[1]), sys.argv[2]
names = ["sim.t"] + [f"bench.in{i}" for i in range(4)] + [f"bench.t{k:04d}" for k in range(n - 5)]
o = ['<?xml version="1.0" encoding="utf-8"?><SCEPTRE><field-device><name>rtu</name><cycle-time>50</cycle-time><tags>']
for i, nm in enumerate(names):
    o.append(f"<external-tag><name>p{i}</name><io>I{i}</io><type>analog</type></external-tag>")
for i in range(4):
    o.append(f"<external-tag><name>w{i}</name><io>O{i}</io><type>analog</type></external-tag>")
o.append("</tags><comms><modbus-server><endpoint>tcp://0.0.0.0:502</endpoint>")
for i in range(4):
    o.append(f"<holding-register><address>{i}</address><tag>w{i}</tag><max-value>1000</max-value><min-value>0</min-value></holding-register>")
for i in range(200):
    o.append(f"<input-register><address>{i}</address><tag>p{5+i}</tag><max-value>100000</max-value><min-value>0</min-value></input-register>")
o.append("</modbus-server><dnp3-server><endpoint>tcp://0.0.0.0:20000</endpoint><address>10</address><event-logging>false</event-logging>")
for i in range(200):
    o.append(f"<analog-input><address>{i}</address><tag>p{5+i}</tag></analog-input>")
for i in range(4):
    o.append(f"<analog-output><address>{i}</address><tag>w{i}</tag></analog-output>")
o.append("</dnp3-server></comms><input><endpoint>udp://*;239.0.0.1:40000</endpoint>")
for i, nm in enumerate(names):
    o.append(f"<analog><id>I{i}</id><name>{nm}</name></analog>")
o.append(f"</input><output><endpoint>tcp://{host}:5555</endpoint>")
for i in range(4):
    o.append(f"<analog><id>O{i}</id><name>bench.in{i}</name></analog>")
o.append("</output></field-device></SCEPTRE>")
print("\n".join(o))
