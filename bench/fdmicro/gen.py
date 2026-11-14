#!/usr/bin/env python3
"""gen.py OUTDIR: the field-device configs and tag layouts of the field-device microbench (ww-1df.41). Stdlib only, deterministic.

Per profile (S 15 tags, M 800, L 8112) two configs, the SAME file for ferrofire and stock bennu (one stream endpoint, one provider endpoint):
  <P>-mb.xml    modbus-server only      <P>-dnp3.xml   dnp3-server only (link address 10)
  <P>.json      the layout the harness (fdmicro) reads: tags in publish order, their Modbus/DNP3 addresses, counters, commands.
Input tags: `probe` (binary, DI 0, flipped on a schedule), `cnt0..cntK-1` (analog, IR 30000+k, range 0..65535 so the register IS the integer
the provider sent: one counter is prepended to every datagram, the drop detector), then 1 binary per 4 tags (DI) and analog (IR 30000+Kmax+j,
range 0..1000). Output tags: cmd0..cmd3 (binary, coils 0..3) and sp0/sp1 (analog, holding registers 40000/40001) -> the provider's REP.
"""
import json, os, sys

STREAM, PROVIDER, MODBUS, DNP3, CYCLE_MS = "udp://*;239.0.0.1:40000", "tcp://provider:5555", "tcp://0.0.0.0:502", "tcp://0.0.0.0:20000", 50
PROFILES = {"S": dict(n=15, kmax=1, hz=10), "M": dict(n=800, kmax=24, hz=100), "L": dict(n=8112, kmax=160, hz=10)}   # hz: the profile's main publish rate


def layout(p):
    n, kmax = p["n"], p["kmax"]
    tags = [dict(name="probe", kind="binary", di=0, dnp=0)]
    tags += [dict(name=f"cnt{k}", kind="analog", ir=30000 + k, lo=0, hi=65535, dnp=k) for k in range(kmax)]
    di, ir = 1, 30000 + kmax
    for i in range(n - len(tags)):
        if i % 4 == 0:
            tags.append(dict(name=f"brk-{i:05d}.active", kind="binary", di=di, dnp=di)); di += 1
        else:
            tags.append(dict(name=f"bus-{i:05d}.vm", kind="analog", ir=ir, lo=0, hi=1000, dnp=ir - 30000)); ir += 1
    assert len(tags) == n and ir < 65000
    return dict(tags=tags, kmax=kmax, hz=p["hz"], cmds=[dict(name=f"cmd{i}", coil=i) for i in range(4)], sps=[dict(name=f"sp{i}", hr=40000 + i) for i in range(2)])


def xml(lay, proto):
    tags, ins, outs, mb, dn = [], [], [], [], []
    for i, t in enumerate(lay["tags"]):
        tags.append(f"<external-tag><name>t{i}</name><io>I{i}</io><type>{t['kind']}</type></external-tag>")
        ins.append(f"<{t['kind']}><id>I{i}</id><name>{t['name']}</name></{t['kind']}>")
        if t["kind"] == "binary":
            mb.append(f"<discrete-input><address>{t['di']}</address><tag>t{i}</tag></discrete-input>")
            dn.append(f"<binary-input><address>{t['dnp']}</address><tag>t{i}</tag></binary-input>")
        else:
            mb.append(f"<input-register><address>{t['ir']}</address><max-value>{t['hi']}</max-value><min-value>{t['lo']}</min-value><tag>t{i}</tag></input-register>")
            dn.append(f"<analog-input><address>{t['dnp']}</address><tag>t{i}</tag></analog-input>")
    for j, c in enumerate(lay["cmds"]):
        tags.append(f"<external-tag><name>{c['name']}</name><io>O{j}</io><type>binary</type></external-tag>")
        outs.append(f"<binary><id>O{j}</id><name>{c['name']}</name></binary>")
        mb.append(f"<coil><address>{c['coil']}</address><tag>{c['name']}</tag></coil>")
        dn.append(f"<binary-output><address>{10000 + j}</address><tag>{c['name']}</tag></binary-output>")
    for j, s in enumerate(lay["sps"]):
        tags.append(f"<external-tag><name>{s['name']}</name><io>O{4 + j}</io><type>analog</type></external-tag>")
        outs.append(f"<analog><id>O{4 + j}</id><name>{s['name']}</name></analog>")
        mb.append(f"<holding-register><address>{s['hr']}</address><max-value>1000</max-value><min-value>0</min-value><tag>{s['name']}</tag></holding-register>")
        dn.append(f"<analog-output><address>{10000 + j}</address><tag>{s['name']}</tag></analog-output>")
    j = "\n            "
    comms = (f"<modbus-server>\n                <endpoint>{MODBUS}</endpoint>\n                " + "\n                ".join(mb) + "\n            </modbus-server>") if proto == "mb" else (
        f"<dnp3-server>\n                <endpoint>{DNP3}</endpoint>\n                <address>10</address>\n                " + "\n                ".join(dn) + "\n            </dnp3-server>")
    return (f"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<SCEPTRE>\n    <field-device>\n        <name>rtu-1</name>\n        <cycle-time>{CYCLE_MS}</cycle-time>\n"
            f"        <tags>\n            {j.join(tags)}\n        </tags>\n        <comms>\n            {comms}\n        </comms>\n"
            f"        <input>\n            <endpoint>{STREAM}</endpoint>\n            {j.join(ins)}\n        </input>\n"
            f"        <output>\n            <endpoint>{PROVIDER}</endpoint>\n            {j.join(outs)}\n        </output>\n    </field-device>\n</SCEPTRE>\n")


if __name__ == "__main__":
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for name, p in PROFILES.items():
        lay = layout(p)
        json.dump(lay, open(f"{out}/{name}.json", "w"), separators=(",", ":"))
        for proto in ("mb", "dnp3"):
            open(f"{out}/{name}-{proto}.xml", "w").write(xml(lay, proto))
