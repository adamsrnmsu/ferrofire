"""IEC 104 master probe for conformance/iec104 (widens modules/iec104/spike/probe_ext.py). Prints a transcript that two runs against
the same server give byte for byte: `> request`, then `< frame` for each frame the server sent back within the step's window.
I-frames are `I <ASDU hex>` (N(S)/N(R) masked), S-frames `S`, U-frames in full. CP56Time2a in the server's ASDUs is `__`.
Periodic frames (COT 1, and M_DP_TB_1 COT 3) are left out of the steps; `periodic:` lines list the distinct ones seen in a window.
Times are whole seconds. Unless a step says otherwise the probe acks with an S-frame (not printed) at every 8th I-frame received and at
the end of each read, so the server's k window never fills.
    probe_104x.py            SET (env): single (default: every section), double (no timers, no two-connection section), big (GI only)
"""
import os, socket, struct, time
from datetime import datetime, timezone

SET = os.environ.get("SET", "single")
ADDR = ("127.0.0.1", 2404)
TIMED = {30: 1, 31: 1, 36: 5}   # type -> object bytes after the IOA before its CP56Time2a


def asdu_text(a):
    out = [b.to_bytes(1, "big").hex() for b in a]
    if a[0] in TIMED and len(a) > 6:
        step = 3 + TIMED[a[0]] + 7
        for i in range(a[1] & 0x7f):
            s = 6 + i * step + 3 + TIMED[a[0]]
            out[s:s + 7] = ["__"] * len(out[s:s + 7])
    return " ".join(out)


def is_i(f):
    return f[2] & 1 == 0


def periodic(f):
    return is_i(f) and len(f) > 8 and ((f[8] & 0x3f) == 1 or (f[6] == 31 and (f[8] & 0x3f) == 3))


def fmt(f):
    if is_i(f):
        return "I " + asdu_text(f[6:])
    return "S" if f[2] & 3 == 1 else "U " + f.hex(" ")


def a(typ, cot, ioa, body=b"", vsq=1, ca=1):
    return bytes([typ, vsq, cot, 0, ca & 255, ca >> 8, ioa & 255, (ioa >> 8) & 255, ioa >> 16]) + body


class Conn:
    def __init__(s, ack=True):
        s.sock = socket.create_connection(ADDR, timeout=4)
        s.t_open, s.buf, s.ns, s.nr, s.ack, s.closed, s.t_first, s.t_closed = time.time(), b"", 0, 0, ack, None, None, None

    def u(s, code):
        s.sock.sendall(bytes([0x68, 4, code, 0, 0, 0]))

    def i(s, asdu, nr=None):
        nr = s.nr if nr is None else nr
        s.sock.sendall(bytes([0x68, 4 + len(asdu), (s.ns << 1) & 255, s.ns >> 7, (nr << 1) & 255, nr >> 7]) + asdu)
        s.ns += 1

    def s_frame(s):
        s.sock.sendall(bytes([0x68, 4, 1, 0, (s.nr << 1) & 255, s.nr >> 7]))

    def read(s, secs):
        """(time, frame) received in the next `secs` s, or until the server closes; acks them unless ack is off."""
        out, end = [], time.time() + secs
        while s.closed is None and time.time() < end:
            s.sock.settimeout(max(end - time.time(), 0.01))
            try:
                d = s.sock.recv(4096)
            except socket.timeout:
                break
            except ConnectionResetError:
                d = None
            if not d:
                s.closed, s.t_closed = "closed" if d == b"" else "reset", time.time()
                break
            s.buf += d
            while len(s.buf) >= 2 and len(s.buf) >= 2 + s.buf[1]:
                f, s.buf = s.buf[:2 + s.buf[1]], s.buf[2 + s.buf[1]:]
                if is_i(f):
                    s.nr += 1
                    s.t_first = s.t_first or time.time()
                    if s.ack and s.nr % 8 == 0:
                        s.s_frame()
                out.append((time.time(), f))
        if s.ack and s.closed is None and any(is_i(f) for _, f in out):
            s.s_frame()
        return out

    def close(s):
        s.sock.close()
        time.sleep(1)


def show(c, frames):
    for _, f in frames:
        if not periodic(f):
            print("< " + fmt(f))
    if c.closed:
        print("< " + c.closed)


def step(c, label, asdu=None, u=None, wait=1.2, nr=None):
    print("> " + label)
    if u is not None:
        c.u(u)
    if asdu is not None:
        c.i(asdu, nr)
    show(c, c.read(wait))


def got_periodic(*conns):
    """For each connection, `yes` if a periodic frame reached it in a 2.5 s window (the windows overlap: kernel buffers)."""
    for c in conns:
        c.read(0.05)
    time.sleep(2.5)
    return ["yes" if any(periodic(f) for _, f in c.read(0.3)) else "no" for c in conns]


def snapshot(c, label, secs=2.5):
    fr = c.read(secs)
    show(c, fr)
    seen = sorted({fmt(f) for _, f in fr if periodic(f)})
    print("periodic %s:" % label + ("" if seen else " none"))
    for s in seen:
        print("  " + s)


def time_base(c):
    """Decode the CP56Time2a of the M_DP_TB_1 frames against this host's UTC clock."""
    for t, f in c.read(2.5):
        if f[6] == 31 and len(f) >= 23:
            ms, mi, h, d, mo, y = f[16] | f[17] << 8, f[18], f[19], f[20], f[21], f[22]
            when = datetime(2000 + (y & 0x7f), mo & 0x0f, d & 0x1f, h & 0x1f, mi & 0x3f, ms // 1000, tzinfo=timezone.utc).timestamp() + ms % 1000 / 1000
            off = round((when - t) / 3600)
            print("M_DP_TB_1 CP56Time2a: %s, day-of-week %d, IV %d, SU %d" % (
                "UTC wall clock (within 5 s)" if abs(when - t) < 5 else "UTC%+dh" % off, d >> 5, mi >> 7, h >> 7))
            return
    print("M_DP_TB_1 CP56Time2a: no M_DP_TB_1 frame in 2.5 s")


GI = a(100, 6, 0, b"\x14")
f32 = lambda v: struct.pack("<f", v)
T0 = bytes(7)   # a CP56Time2a of zeros in the requests that carry one


def before_startdt():
    print("## before STARTDT")
    c = Conn()
    step(c, "TESTFR act", u=0x43)
    step(c, "I-frame C_IC_NA_1 QOI 20 (no STARTDT yet)", GI)
    c.close()
    c = Conn()
    step(c, "S-frame (no STARTDT yet)", u=0x01)
    c.close()


def main_connection():
    print("## one connection: STARTDT, interrogation, commands, other type ids, STOPDT")
    c = Conn()
    step(c, "STARTDT act", u=0x07)
    step(c, "TESTFR act", u=0x43)
    step(c, "C_IC_NA_1 COT 6 QOI 20", GI)
    if SET == "double":
        time_base(c)
    snapshot(c, "after the interrogation")
    if SET == "big":
        return c
    step(c, "C_IC_NA_1 COT 6 QOI 21", a(100, 6, 0, b"\x15"))
    step(c, "C_IC_NA_1 COT 8 QOI 20 (deactivation)", a(100, 8, 0, b"\x14"))
    step(c, "C_IC_NA_1 COT 3 QOI 20 (bad COT)", a(100, 3, 0, b"\x14"))
    step(c, "C_IC_NA_1 COT 6 QOI 20 to common address 2", a(100, 6, 0, b"\x14", ca=2))
    print("## commands at configured outputs (direct execute)")
    for label, asdu in [("C_SC_NA_1 45 IOA 0 ON", a(45, 6, 0, b"\x01")), ("C_SC_NA_1 45 IOA 1 OFF", a(45, 6, 1, b"\x00")),
                        ("C_DC_NA_1 46 IOA 0 OFF(1)", a(46, 6, 0, b"\x01")), ("C_DC_NA_1 46 IOA 1 ON(2)", a(46, 6, 1, b"\x02")),
                        ("C_SE_NC_1 50 IOA 2 77.5", a(50, 6, 2, f32(77.5) + b"\x00")),
                        ("C_SE_NC_1 50 IOA 3 -2.5", a(50, 6, 3, f32(-2.5) + b"\x00"))]:
        step(c, label, asdu, wait=1.5)
    time.sleep(1.5)
    step(c, "C_IC_NA_1 COT 6 QOI 20 (after the commands)", GI)
    snapshot(c, "after the commands")
    print("## commands the server refuses or answers otherwise")
    for label, asdu in [("C_SC_NA_1 45 IOA 50 (no such point)", a(45, 6, 50, b"\x01")),
                        ("C_SC_NA_1 45 IOA 100 (a binary input)", a(45, 6, 100, b"\x01")),
                        ("C_SC_NA_1 45 IOA 2 (an analog output)", a(45, 6, 2, b"\x01")),
                        ("C_DC_NA_1 46 IOA 0 state 0 (not permitted)", a(46, 6, 0, b"\x00")),
                        ("C_DC_NA_1 46 IOA 0 state 3 (not permitted)", a(46, 6, 0, b"\x03")),
                        ("C_SE_NC_1 50 IOA 0 (a binary output)", a(50, 6, 0, f32(1.0) + b"\x00")),
                        ("C_SE_NC_1 50 IOA 102 (an analog input)", a(50, 6, 102, f32(1.0) + b"\x00")),
                        ("C_SC_NA_1 45 IOA 0 select ON (S/E 1)", a(45, 6, 0, b"\x81")),
                        ("C_DC_NA_1 46 IOA 0 select ON (S/E 1)", a(46, 6, 0, b"\x82")),
                        ("C_SE_NC_1 50 IOA 2 select 5.0 (S/E 1)", a(50, 6, 2, f32(5.0) + b"\x80")),
                        ("C_SC_NA_1 45 IOA 0 COT 8 (deactivation)", a(45, 8, 0, b"\x01")),
                        ("C_DC_NA_1 46 IOA 0 COT 8 (deactivation)", a(46, 8, 0, b"\x02")),
                        ("C_SE_NC_1 50 IOA 2 COT 8 (deactivation)", a(50, 8, 2, f32(5.0) + b"\x00")),
                        ("C_SC_NA_1 45 IOA 0 COT 3 (bad COT)", a(45, 3, 0, b"\x01")),
                        ("C_SC_NA_1 45 IOA 0 to common address 2", a(45, 6, 0, b"\x00", ca=2))]:
        step(c, label, asdu)
    print("## other type ids")
    for label, asdu in [("C_CI_NA_1 101 COT 6 QCC 5", a(101, 6, 0, b"\x05")), ("C_RD_NA_1 102 COT 5 IOA 100", a(102, 5, 100)),
                        ("C_CS_NA_1 103 COT 6", a(103, 6, 0, T0)), ("C_TS_NA_1 104 COT 6", a(104, 6, 0, b"\xaa\x55")),
                        ("C_RP_NA_1 105 COT 6 QRP 1", a(105, 6, 0, b"\x01")), ("C_TS_TA_1 107 COT 6", a(107, 6, 0, b"\xaa\x55" + T0)),
                        ("C_TS_TA_1 107 COT 3 (bad COT)", a(107, 3, 0, b"\xaa\x55" + T0)),
                        ("C_SC_TA_1 58 COT 6 IOA 0", a(58, 6, 0, b"\x01" + T0)), ("M_SP_NA_1 1 COT 3 IOA 0 (a monitor type)", a(1, 3, 0, b"\x01"))]:
        step(c, label, asdu)
    print("## STOPDT")
    step(c, "STOPDT act", u=0x13)
    print("periodic while stopped: " + got_periodic(c)[0])
    step(c, "TESTFR act (stopped)", u=0x43)
    step(c, "STARTDT act", u=0x07)
    print("periodic after STARTDT again: " + got_periodic(c)[0])
    step(c, "STOPDT act", u=0x13)
    step(c, "C_IC_NA_1 QOI 20 (stopped)", GI)
    c.close()


def two_connections():
    print("## two connections (periodic: any COT 1 frame in 2.5 s)")
    A = Conn()
    step(A, "A: open, STARTDT act", u=0x07)
    print("A periodic: " + got_periodic(A)[0])
    B = Conn()
    print("> B: open (no STARTDT)")
    print("A periodic: %s, B periodic: %s" % tuple(got_periodic(A, B)))
    step(B, "B: STARTDT act", u=0x07)
    print("A periodic: %s, B periodic: %s" % tuple(got_periodic(A, B)))
    step(A, "A: C_IC_NA_1 QOI 20 (after B's STARTDT)", GI)
    print("> B: close")
    B.close()
    C = Conn()
    step(C, "C: open, STARTDT act", u=0x07)
    print("C periodic: " + got_periodic(C)[0])
    print("> C: close")
    C.close()
    D = Conn()
    step(D, "D: open, STARTDT act", u=0x07)
    E = Conn()
    step(E, "E: open, STARTDT act", u=0x07)
    print("D periodic: %s, E periodic: %s" % tuple(got_periodic(D, E)))
    step(D, "D: STARTDT act again", u=0x07)
    print("D periodic: %s, E periodic: %s" % tuple(got_periodic(D, E)))
    print("> D: close (E stays open)")
    D.close()
    print("E periodic: " + got_periodic(E)[0])
    step(E, "E: STARTDT act", u=0x07)
    step(E, "E: C_IC_NA_1 QOI 20", GI)
    print("E periodic: " + got_periodic(E)[0])
    print("> E: close")
    E.close()


def collect_i(c, n, limit):
    """Read until n I-frames have come and 1.5 s passed with no more, or `limit` s; the number received."""
    end, got = time.time() + limit, 0
    while time.time() < end:
        fr = c.read(1.5)
        got += sum(1 for _, f in fr if is_i(f))
        if c.closed or (got >= n and not any(is_i(f) for _, f in fr)):
            break
    return got


def timers():
    print("## k, w and t1: the master never acks (N(R) 0 in its I-frames, no S-frames)")
    c = Conn(ack=False)
    c.u(0x07)
    c.i(GI, 0)
    print("> STARTDT act, C_IC_NA_1 QOI 20, then nothing")
    print("I-frames before the server stops sending: %d" % collect_i(c, 12, 12))
    for n in range(1, 8):
        c.i(a(107, 6, 0, b"\xaa\x55" + T0), 0)
    print("> 7 x C_TS_TA_1 107 (N(R) 0)")
    show(c, c.read(0.8))
    step(c, "the 8th C_TS_TA_1 107 (N(R) 0)", a(107, 6, 0, b"\xaa\x55" + T0), wait=0.8, nr=0)
    c.read(20)
    print("closed %d s after the first I-frame it sent" % round(c.t_closed - c.t_first) if c.closed else "still open 20 s later")
    c.close()
    print("## t2: one I-frame from the master while the server's k window is full")
    c = Conn(ack=False)
    c.u(0x07)
    for n in range(3):
        c.i(GI, 0)
    print("> STARTDT act, 3 x C_IC_NA_1 QOI 20 (12 I-frames back: the window is full), then one C_TS_TA_1 107 (N(R) 0)")
    c.read(1)
    c.i(a(107, 6, 0, b"\xaa\x55" + T0), 0)
    t = time.time()
    for at, f in c.read(16):
        if not is_i(f):
            print("< %s %d s after that I-frame" % (fmt(f), round(at - t)))
    print("< %s %d s after the first I-frame it sent" % (c.closed, round(c.t_closed - c.t_first)) if c.closed else "still open")
    c.close()
    print("## t3: a connection with no STARTDT and no traffic, TESTFR act left unanswered")
    c = Conn(ack=False)
    print("> open, then nothing")
    for at, f in c.read(45):
        print("< %s at %d s" % (fmt(f), round(at - c.t_open)))
    print("< %s at %d s" % (c.closed, round(c.t_closed - c.t_open)) if c.closed else "still open 45 s later")
    c.close()


if __name__ == "__main__":   # probe_cmdif.py imports asdu_text and periodic
    before_startdt()
    c = main_connection()
    if SET == "big":
        c.close()
    if SET == "single":
        two_connections()
        timers()
