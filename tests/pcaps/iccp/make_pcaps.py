"""Synthetic TASE.2 (ICCP) captures for the parsers in tools/zeek/iccp and tools/wireshark/iccp.lua.

Hand-built bytes, stdlib only: Ethernet/IPv4/TCP, TPKT, COTP, OSI session, presentation and ACSE wrapped around MMS
BER PDUs. Writes tase2_assoc.pcap, tase2_report.pcap, malformed.pcap and fuzz.pcap (150 seeded mutants) into the directory given as argv[1].
Run: python3 -I make_pcaps.py <outdir>

TASE.2 object names and encodings below follow IEC 60870-6-503/802 as remembered, NOT checked against a licensed copy
(see tools/zeek/iccp/README.md, "Assumptions"). Stage 2 re-checks them against ferrofire's own ICCP server.
"""
import struct
import sys

# ---- BER helpers ------------------------------------------------------------------------------------------------


def ln(n):
    return bytes([n]) if n < 128 else bytes([0x80 | (b := (n.bit_length() + 7) // 8)]) + n.to_bytes(b, "big")


def tlv(tag, body=b""):
    return bytes([tag]) + ln(len(body)) + body


def integer(v, tag=0x02):
    n = max(1, (v.bit_length() + 8) // 8)
    return tlv(tag, v.to_bytes(n, "big", signed=True))


def vstr(s, tag=0x1A):
    return tlv(tag, s.encode())


def bits(n, value, tag=0x84):  # n-bit string, value MSB first in an n-bit field
    pad = (-n) % 8
    return tlv(tag, bytes([pad]) + (value << pad).to_bytes((n + 7) // 8, "big"))


def real(f):
    return tlv(0x87, b"\x08" + struct.pack(">f", f))


def struct_(*items):
    return tlv(0xA2, b"".join(items))


def boolean(b):
    return tlv(0x83, b"\x01" if b else b"\x00")


def oid(*arcs):
    out = bytes([40 * arcs[0] + arcs[1]])
    for a in arcs[2:]:
        c = [a & 0x7F]
        a >>= 7
        while a:
            c.append(0x80 | (a & 0x7F))
            a >>= 7
        out += bytes(reversed(c))
    return tlv(0x06, out)


# ---- MMS (ISO 9506) ---------------------------------------------------------------------------------------------


def vmd(n):
    return tlv(0x80, n.encode())


def dom(d, n):
    return tlv(0xA1, vstr(d) + vstr(n))


def var(objname):  # one ListOfVariable entry
    return tlv(0x30, tlv(0xA0, objname))


def lov(*objnames):
    return tlv(0xA0, b"".join(var(o) for o in objnames))


def conf_req(inv, svc):
    return tlv(0xA0, integer(inv) + svc)


def conf_resp(inv, svc):
    return tlv(0xA1, integer(inv) + svc)


def read_req(*names):
    return tlv(0xA4, tlv(0xA1, lov(*names)))


def read_resp(*results):
    return tlv(0xA4, tlv(0xA1, b"".join(results)))


def ok(data):  # AccessResult success: the Data CHOICE itself
    return data


def fail(code=10):
    return integer(code, 0x80)


def write_req(names, *datas):
    return tlv(0xA5, lov(*names) + tlv(0xA0, b"".join(datas)))


def write_resp(*okflags):
    return tlv(0xA5, b"".join(tlv(0x81) if o else integer(3, 0x80) for o in okflags))


def getnames_req(scope_obj, cls=0):
    return tlv(0xA1, tlv(0xA0, integer(cls, 0x80)) + tlv(0xA1, scope_obj))


def getnames_resp(names, more=False):
    return tlv(0xA1, tlv(0xA0, b"".join(vstr(n) for n in names)) + tlv(0x81, b"\xff" if more else b"\x00"))


def define_req(listname, *members):
    return tlv(0xAB, listname + lov(*members))


def info_report(spec, *results):
    return tlv(0xA3, tlv(0xA0, spec + tlv(0xA0, b"".join(results))))


def err_pdu(inv, cls=7, code=2):
    return tlv(0xA2, integer(inv, 0x80) + tlv(0xA2, tlv(0xA0, integer(code, 0x80 | cls))))


INIT_BODY = (integer(65000, 0x80) + integer(5, 0x81) + integer(5, 0x82) + integer(10, 0x83) +
             tlv(0xA4, integer(1, 0x80) + bits(10, 0b1111100000, 0x81) + bits(40, 0xEE1C000000, 0x82)))
MMS_INIT_REQ = tlv(0xA8, INIT_BODY)
MMS_INIT_RESP = tlv(0xA9, INIT_BODY)
MMS_CONCLUDE_REQ, MMS_CONCLUDE_RESP = b"\x8b\x00", b"\x8c\x00"

# ---- OSI layers -------------------------------------------------------------------------------------------------

MMS_OID, ACSE_OID, BER_OID = (1, 0, 9506, 2, 1), (2, 2, 1, 0, 1), (2, 1, 1)
MMS_CTX_OID = oid(1, 0, 9506, 2, 3)


def ext(pdu):  # ACSE user-information: EXTERNAL { indirect-reference 3, single-ASN1-type }
    return tlv(0xBE, tlv(0x28, integer(3) + tlv(0xA0, pdu)))


def aarq(mms):
    return tlv(0x60, tlv(0xA1, MMS_CTX_OID) + ext(mms))


def aare(mms):
    return tlv(0x61, tlv(0xA1, MMS_CTX_OID) + tlv(0xA2, integer(0)) + tlv(0xA3, tlv(0xA1, integer(0))) + ext(mms))


def pdv(ctx, body):  # fully-encoded-data
    return tlv(0x61, tlv(0x30, integer(ctx) + tlv(0xA0, body)))


def ctx_def(i, abstract):
    return tlv(0x30, integer(i) + oid(*abstract) + tlv(0x30, oid(*BER_OID)))


def pres_cp(acse):
    return tlv(0x31, tlv(0xA0, integer(1, 0x80)) + tlv(0xA2, tlv(0x81, b"\0\0\0\1") + tlv(0x82, b"\0\0\0\1") +
              tlv(0xA4, ctx_def(1, ACSE_OID) + ctx_def(3, MMS_OID)) + pdv(1, acse)))


def pres_cpa(acse):
    res = tlv(0x30, integer(0, 0x80) + tlv(0x81, oid(*BER_OID)[2:]))
    return tlv(0x31, tlv(0xA0, integer(1, 0x80)) + tlv(0xA2, tlv(0x83, b"\0\0\0\1") + tlv(0xA5, res + res) + pdv(1, acse)))


def sln(n):
    return bytes([n]) if n < 255 else b"\xff" + n.to_bytes(2, "big")


def ses(kind, userdata):
    p = lambda t, v: bytes([t]) + sln(len(v)) + v
    body = (p(0x05, b"\x13\x01\x00\x16\x01\x02") + p(0x14, b"\x00\x02") + p(0x33, b"\x00\x01") + p(0x34, b"\x00\x01") +
            p(0xC1, userdata))
    return bytes([kind]) + sln(len(body)) + body


def dt_data(mms, ctx=3):
    return b"\x01\x00\x01\x00" + pdv(ctx, mms)


def tpkt(cotp):
    return b"\x03\x00" + struct.pack(">H", 4 + len(cotp)) + cotp


def dt(user):
    return tpkt(b"\x02\xf0\x80" + user)


CR = tpkt(b"\x11\xe0\x00\x00\x00\x01\x00\xc0\x01\x0a\xc1\x02\x00\x01\xc2\x02\x00\x01")
CC = tpkt(b"\x11\xd0\x00\x01\x00\x01\x00\xc0\x01\x0a\xc1\x02\x00\x01\xc2\x02\x00\x01")

# ---- Ethernet/IPv4/TCP + pcap ----------------------------------------------------------------------------------


def csum(b):
    if len(b) % 2:
        b += b"\0"
    s = sum(struct.unpack(f">{len(b) // 2}H", b))
    while s >> 16:
        s = (s & 0xFFFF) + (s >> 16)
    return ~s & 0xFFFF


class Conn:
    """One TCP connection client 10.0.0.1:sport -> server 10.0.0.2:102, appended to a Pcap."""

    def __init__(self, pcap, sport):
        self.p, self.sport, self.seq, self.ack = pcap, sport, {True: 1000, False: 5000}, {True: 5001, False: 1001}
        self.send(True, b"", 0x02)
        self.send(False, b"", 0x12)
        self.send(True, b"", 0x10)

    def send(self, c2s, data, flags=0x18):
        a, b = (b"\x0a\0\0\x01", b"\x0a\0\0\x02") if c2s else (b"\x0a\0\0\x02", b"\x0a\0\0\x01")
        sp, dp = (self.sport, 102) if c2s else (102, self.sport)
        seq, ack = self.seq[c2s], self.ack[c2s] if flags != 0x02 else 0
        tcp = struct.pack(">HHIIBBHHH", sp, dp, seq, ack, 0x50, flags, 65535, 0, 0)
        ph = a + b + struct.pack(">BBH", 0, 6, len(tcp) + len(data))
        tcp = tcp[:16] + struct.pack(">H", csum(ph + tcp + data)) + tcp[18:]
        ip = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(tcp) + len(data), len(self.p.pkts) + 1, 0, 64, 6, 0, a, b)
        ip = ip[:10] + struct.pack(">H", csum(ip)) + ip[12:]
        eth = (b"\x02\0\0\0\0\x02" + b"\x02\0\0\0\0\x01" if c2s else b"\x02\0\0\0\0\x01" + b"\x02\0\0\0\0\x02") + b"\x08\0"
        self.p.add(eth + ip + tcp + data)
        self.seq[c2s] += len(data) + (1 if flags & 0x03 else 0)
        self.ack[not c2s] = self.seq[c2s]

    def c(self, data):
        self.send(True, data)

    def s(self, data):
        self.send(False, data)

    def fin(self):
        self.send(True, b"", 0x11)
        self.send(False, b"", 0x11)
        self.send(True, b"", 0x10)

    def associate(self):
        self.c(CR)
        self.s(CC)
        self.c(dt(ses(0x0D, pres_cp(aarq(MMS_INIT_REQ)))))
        self.s(dt(ses(0x0E, pres_cpa(aare(MMS_INIT_RESP)))))

    def req(self, inv, svc):
        self.c(dt(dt_data(conf_req(inv, svc))))

    def resp(self, inv, svc):
        self.s(dt(dt_data(conf_resp(inv, svc))))

    def rpc(self, inv, rq, rs):
        self.req(inv, rq)
        self.resp(inv, rs)


class Pcap:
    def __init__(self):
        self.pkts, self.t = [], 1700000000.0

    def add(self, frame):
        self.t += 0.001
        self.pkts.append((self.t, frame))

    def write(self, path):
        with open(path, "wb") as f:
            f.write(struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 1))
            for t, fr in self.pkts:
                f.write(struct.pack("<IIII", int(t), int((t % 1) * 1e6), len(fr), len(fr)) + fr)


# ---- TASE.2 scenarios --------------------------------------------------------------------------------------------

ICC = "ICC1"
FLAGS_OK = 0b00000000  # Data_Flags: validity VALID(0), source TELEMETERED(0), normal, no ts-quality
FLAGS_SUSPECT = 0b00100000  # validity is bits 2-3 (state is bits 0-1 in a StateQ): 2 = SUSPECT


def assoc():
    p = Pcap()
    c = Conn(p, 50001)
    c.associate()
    c.rpc(1, read_req(vmd("Bilateral_Table_ID")), read_resp(vstr("BT_RANGE_01", 0x8A)))
    c.rpc(2, read_req(vmd("TASE2_Version")), read_resp(struct_(integer(2000, 0x85), integer(8, 0x85))))
    c.rpc(3, read_req(vmd("Supported_Features")), read_resp(bits(12, 0b110110100000)))  # 12 bits (503 8.2.9); a real peer: blocks 1, 2, 4, 5, 7
    c.rpc(4, getnames_req(tlv(0x80)), getnames_resp(["Bilateral_Table_ID", "TASE2_Version", "Supported_Features"]))
    c.rpc(5, getnames_req(tlv(0x81, ICC.encode())), getnames_resp(["Pt_1", "Pt_2", "Pt_3", "DS_1", "TSet1"]))
    c.rpc(6, tlv(0xA6, tlv(0xA0, dom(ICC, "Pt_1"))), tlv(0xA6, tlv(0x80, b"\0") + tlv(0xA2, tlv(0xA1, b""))))
    c.rpc(7, define_req(dom(ICC, "DS_1"), dom(ICC, "Pt_1"), dom(ICC, "Pt_2")), b"\x8b\x00")
    c.rpc(8, read_req(dom(ICC, "Next_DSTransfer_Set")), read_resp(struct_(vstr(ICC, 0x8A), vstr("TSet1", 0x8A))))
    ts = struct_(struct_(vstr(ICC, 0x8A), vstr("DS_1", 0x8A)), integer(0, 0x85), integer(5, 0x85), integer(0, 0x85),
                 integer(0, 0x85), integer(60, 0x85), bits(5, 0b01010), boolean(True), boolean(False), boolean(True),
                 boolean(False), boolean(True), integer(0, 0x85))
    c.rpc(9, write_req([dom(ICC, "TSet1")], ts), write_resp(True))
    c.rpc(10, read_req(dom(ICC, "Pt_1"), dom(ICC, "Pt_2"), dom(ICC, "Pt_3"), dom(ICC, "Missing")),
          read_resp(struct_(real(50.25), bits(8, FLAGS_OK)), bits(8, 0b10000000), struct_(integer(42, 0x85), bits(8, FLAGS_SUSPECT)),
                    fail(10)))
    # a peer that refuses the special variables later on (telcon-tase2's test server serves none of them): the association keeps what it learned
    c.rpc(14, read_req(vmd("TASE2_Version")), read_resp(fail(10)))
    c.rpc(15, read_req(vmd("Supported_Features")), read_resp(fail(10)))
    c.rpc(16, read_req(dom(ICC, "Bilateral_Table_ID")), read_resp(fail(10)))
    c.rpc(11, write_req([dom(ICC, "Dev_1")], integer(1, 0x85)), write_resp(True))
    c.rpc(12, write_req([dom(ICC, "Dev_2")], integer(1, 0x85)), write_resp(False))
    c.req(13, read_req(dom(ICC, "Locked")))
    c.s(dt(dt_data(err_pdu(13))))
    c.c(dt(dt_data(MMS_CONCLUDE_REQ)))
    c.s(dt(dt_data(MMS_CONCLUDE_RESP)))
    c.fin()
    return p


def report():
    p = Pcap()
    c = Conn(p, 50002)
    c.associate()
    c.rpc(1, read_req(vmd("Bilateral_Table_ID")), read_resp(vstr("BT_RANGE_01", 0x8A)))
    tsname = struct_(vstr(ICC, 0x8A), vstr("TSet1", 0x8A))
    # transfer report against a named variable list (positional: name, time stamp, conditions, then data)
    c.s(dt(dt_data(info_report(tlv(0xA1, dom(ICC, "DS_1")), tsname, integer(1700000001, 0x85), bits(5, 0b10000),
                               struct_(real(49.98), bits(8, FLAGS_OK)), bits(8, 0b10000000)))))
    # integrity report listing the objects explicitly, plus an information message
    c.s(dt(dt_data(info_report(lov(vmd("Transfer_Set_Name"), vmd("DSConditions_Detected"), dom(ICC, "Pt_1")),
                               tsname, bits(5, 0b01000), struct_(real(50.0), bits(8, FLAGS_SUSPECT))))))
    c.s(dt(dt_data(info_report(lov(dom(ICC, "IM_Msg1")), struct_(integer(1, 0x85), integer(2, 0x85), integer(3, 0x85),
                                                                 vstr("hello", 0x89))))))
    # one report too big for a single TPDU: COTP DT segments, EOT only on the last
    big = info_report(tlv(0xA1, dom(ICC, "DS_1")), tsname, integer(1700000002, 0x85), bits(5, 0b00100),
                      *[struct_(real(float(i)), bits(8, FLAGS_OK)) for i in range(40)])
    user = dt_data(big)
    cut = len(user) // 2
    c.s(tpkt(b"\x02\xf0\x00" + user[:cut]))
    c.s(tpkt(b"\x02\xf0\x80" + user[cut:]))
    c.c(dt(dt_data(MMS_CONCLUDE_REQ)))
    c.s(dt(dt_data(MMS_CONCLUDE_RESP)))
    c.fin()
    return p


def fuzz():
    """Seeded mutations (bit flips, truncation, inserts, length bytes) of valid PDUs, one connection each."""
    import random
    rnd = random.Random(61850)
    ts = struct_(vstr(ICC, 0x8A), vstr("TSet1", 0x8A))
    seeds = [
        read_req(vmd("Bilateral_Table_ID")), read_resp(struct_(real(1.5), bits(8, FLAGS_OK)), fail(3)),
        write_req([dom(ICC, "Dev_1")], integer(1, 0x85)), getnames_req(tlv(0x81, b"ICC1")),
        getnames_resp(["a", "b"]), define_req(dom(ICC, "DS_1"), dom(ICC, "Pt_1")),
    ]
    pdus = [conf_req(1, x) for x in seeds[:4]] + [conf_resp(1, x) for x in seeds[1:]] + [
        info_report(tlv(0xA1, dom(ICC, "DS_1")), ts, integer(1, 0x85), bits(5, 1), struct_(real(2.0), bits(8, 0))),
        MMS_INIT_REQ, MMS_CONCLUDE_REQ, err_pdu(1)]
    p = Pcap()
    for n in range(150):
        pdu = bytearray(rnd.choice(pdus))
        for _ in range(rnd.randint(1, 4)):
            op = rnd.randrange(5)
            i = rnd.randrange(len(pdu)) if pdu else 0
            if op == 0 and pdu:
                pdu[i] ^= 1 << rnd.randrange(8)
            elif op == 1 and pdu:
                pdu[i] = rnd.randrange(256)
            elif op == 2:
                del pdu[i:]
            elif op == 3:
                pdu[i:i] = bytes(rnd.randrange(256) for _ in range(rnd.randint(1, 6)))
            elif op == 4 and len(pdu) > 1:
                pdu[1] = rnd.choice([0x00, 0x7F, 0x80, 0x81, 0x82, 0x84, 0xFF])
        c = Conn(p, 51000 + n)
        c.associate()
        (c.s if n % 2 else c.c)(dt(dt_data(bytes(pdu))))
        c.fin()
    return p


def malformed():
    p = Pcap()
    good = lambda c, i: c.rpc(i, read_req(vmd("Bilateral_Table_ID")), read_resp(vstr("BT_RANGE_01", 0x8A)))
    c = Conn(p, 50011)  # bad MMS inside valid TPKT/COTP/session/presentation: the next PDUs must still parse
    c.associate()
    c.c(dt(dt_data(b"\xa0\x32\x02\x01\x09\xa4")))            # confirmed-request, length 50, 5 bytes present
    c.c(dt(dt_data(b"\xa0\x84\xff\xff\xff\xff\x02\x01\x01")))  # 4 GiB length
    c.c(dt(dt_data(b"\xa0\x80\x02\x01\x01\x00\x00")))        # indefinite length
    c.c(dt(dt_data(b"\xff\xff\xff")))                         # nonsense tag
    c.c(dt(dt_data(b"")))                                     # empty
    c.c(dt(dt_data(conf_req(14, read_req(dom("", ""))))))     # empty identifiers
    c.s(dt(dt_data(conf_resp(15, read_resp(b"\x87\x02\x08\x00")))))  # float with a short body
    good(c, 16)
    c.fin()
    c = Conn(p, 50012)  # deeply nested structure in a read response
    c.associate()
    deep = b"\x85\x01\x01"
    for _ in range(300):
        deep = struct_(deep)
    c.req(1, read_req(vmd("TASE2_Version")))
    c.resp(1, read_resp(deep))
    good(c, 2)
    c.fin()
    c = Conn(p, 50013)  # TPKT desync: bad version, short length, then COTP garbage
    c.c(CR)
    c.s(CC)
    c.c(b"\x04\x00\x00\x08abcd")
    c.c(b"\x03\x00\x00\x02")
    c.c(b"\x03\x00\x00\x07\x01\x02\x03")
    c.c(tpkt(b"\x02\xf0\x80" + b"\x0d\xff\xff"))   # session CN with a bogus length
    c.c(tpkt(b"\x7f"))                            # COTP header length beyond the TPDU
    c.fin()
    c = Conn(p, 50014)  # not OSI at all on tcp/102
    c.c(b"GET / HTTP/1.1\r\n\r\n")
    c.s(bytes(range(256)))
    c.fin()
    return p


if __name__ == "__main__":
    out = sys.argv[1]
    for name, fn in (("tase2_assoc", assoc), ("tase2_report", report), ("malformed", malformed), ("fuzz", fuzz)):
        fn().write(f"{out}/{name}.pcap")
