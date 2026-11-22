"""Drive a bennu fd-client's command interface (ZMQ REQ, as bennu-probe frames it: text plus a NUL) and print a golden transcript.

    probe_cmdif.py CMD...            send each command, log request, reply and times to LOG
    probe_cmdif.py --mark TEXT       log a marker (the harness stopped or started something)
    probe_cmdif.py --ctl stop|start  stop or start the stock server next to the client (servectl.py), then log a marker
    probe_cmdif.py --radio BODY      publish one RADIO datagram (the stock provider's framing), so a server has input values
    probe_cmdif.py --sync PORT       wait for the DNP3 master's next class scan, so the commands that follow fall between two scans
    probe_cmdif.py --report PORT     print the transcript: per command `request<TAB>reply[<TAB>~Ns]`, then the bytes the capture
                                     (PCAP) saw on tcp PORT until the next command: `>` client to server, `<` server to client
    probe_cmdif.py --report PORT dnp3   the same with what a clock decides taken out: each frame is `link-control dst<src
                                     app-control function+objects`, CRCs and the transport octet removed and the sequence number
                                     masked; the all-classes scans, their responses and refused connection attempts are left out
    probe_cmdif.py --report PORT bacnet   BACnet/IP on udp, PORT the client's own port: each frame as BVLC, NPDU and APDU bytes with
                                     the invoke id masked (`__`), a run of the same frame (a retry) shown once; the scans (ReadProperty
                                     present-value and their answers) are left out where they fall and listed once at the end, each
                                     distinct request with each distinct answer it got; a reply of 3 s or more is `~3s+`
    probe_cmdif.py --report PORT iec104   IEC 104 on tcp PORT: each APDU of the reassembled stream, I-frames as `I <ASDU>` with
                                     N(S)/N(R) and CP56Time2a masked (probe_104x.py), U-frames in full; S-frames left out, and the
                                     periodic and spontaneous data (COT 1, M_DP_TB_1 COT 3) listed once at the end, distinct
Runs in the testpeers container, in the network namespace of the stock devices (conformance/stock/run-stock.sh).
"""
import json, os, socket, subprocess, sys, time

LOG = os.environ.get("LOG", "/out/cmdif.jsonl")
PCAP = os.environ.get("PCAP", "/out/client.pcap")
ENDPOINT = os.environ.get("CMDIF", "tcp://127.0.0.1:1330")
TIMEOUT_MS = int(os.environ.get("TIMEOUT_MS", "40000"))
SETTLE = float(os.environ.get("SETTLE", "0.4"))


def log(rec):
    with open(LOG, "a") as f:
        f.write(json.dumps(rec) + "\n")


def show(b):
    return "".join(chr(c) if 32 <= c < 127 and c != 92 else "\\x%02x" % c for c in b)


def mask(reply):
    """Stock's failure text points into a destroyed string, so its bytes are whatever the stack held: keep the fixed part."""
    for head in ("ERR=Failed reading tag '", "ERR=Failed writing tag '"):
        if reply.startswith(head) and "': " in reply:
            return reply[:reply.index("': ") + 3] + "<text>" + "\\x00"
    return reply


def command(text):
    import zmq
    s = zmq.Context.instance().socket(zmq.REQ)
    s.setsockopt(zmq.LINGER, 0)
    s.setsockopt(zmq.RCVTIMEO, TIMEOUT_MS)
    s.connect(ENDPOINT)
    t0 = time.time()
    s.send(text.encode() + b"\0")
    try:
        reply = mask(show(s.recv()))
    except zmq.Again:
        reply = "<no reply in %d s>" % (TIMEOUT_MS // 1000)
    t1 = time.time()
    s.close()
    log({"t0": t0, "t1": t1, "req": text, "reply": reply})
    time.sleep(SETTLE)


def radio(body):
    g = b"54386972605942b"
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    u.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
    u.sendto(bytes([len(g)]) + g + body.encode() + b"\0", ("239.0.0.1", 40000))


SCAN = "013c02063c03063c04063c0106"


def frames(payload):
    """The DNP3 link frames in one TCP payload, as (control, dst, src, user data without CRCs)."""
    b = bytes.fromhex(payload)
    while len(b) >= 10 and b[:2] == b"\x05\x64":
        n = b[2] - 5
        body, user = b[10:10 + n + 2 * ((n + 15) // 16)], b""
        for i in range(0, len(body), 18):
            user += body[i:i + 18][:-2]
        yield b[3], b[4] | b[5] << 8, b[6] | b[7] << 8, user
        b = b[10 + len(body):]


def dnp3_lines(packets):
    """`packets`: (bin, direction, what). Scans and their responses go, so do refused SYNs; the rest is masked."""
    out, skip = [], False
    for n, (i, d, what) in enumerate(packets):
        if what in ("SYN", "RST"):
            refused = lambda a, b: a[2] == "SYN" and b[2] == "RST" and a[1] != b[1]
            if (n + 1 < len(packets) and refused(packets[n], packets[n + 1])) or (n and refused(packets[n - 1], packets[n])):
                continue
        if what in ("SYN", "FIN", "RST", ""):
            out.append((i, d + what))
            continue
        for ctrl, dst, src, user in frames(what):
            app = user[1:]
            if d == "> " and app[1:].hex() == SCAN:
                skip = True
            elif d == "< " and skip:
                skip = False
            elif app:
                out.append((i, "%s%02x %d<%d %x_ %s" % (d, ctrl, dst, src, app[0] >> 4, app[1:].hex())))
            else:
                out.append((i, "%s%02x %d<%d" % (d, ctrl, dst, src)))
    return out


def apdu_at(b):
    """Offset of the APDU in a BACnet/IP frame (BVLC 4 bytes, an NPDU with or without DNET/SNET); None for a network message."""
    i, ctrl = 6, b[5]
    if ctrl & 0x80:
        return None
    if ctrl & 0x20:
        i += 3 + b[i + 2]
    if ctrl & 0x08:
        i += 3 + b[i + 2]
    return i + (1 if ctrl & 0x20 else 0)


def bacnet_frame(payload):
    """(masked hex, invoke id or None, is a ReadProperty present-value request, is an answer to a ReadProperty)."""
    b = bytearray.fromhex(payload)
    a = apdu_at(b) if len(b) > 6 else None
    if a is None or a >= len(b):
        return b.hex(), None, False, False
    kind, at = b[a] >> 4, None
    if kind == 0 and a + 3 < len(b):
        at = a + 2                                # confirmed request: type, max segs/APDU, invoke id, service
    elif kind in (2, 3, 5, 6, 7) and a + 1 < len(b):
        at = a + 1                                # simple/complex ack, error, reject, abort: type, invoke id, ...
    if at is None:
        return b.hex(), None, False, False
    inv = b[at]
    h = b.hex()
    h = h[:2 * at] + "__" + h[2 * at + 2:]
    rp_pv = kind == 0 and b[a + 3] == 12 and b[a + 4:a + 5] == b"\x0c" and b[a + 9:] == b"\x19\x55"   # object id, property 85
    return h, inv, rp_pv, kind in (3, 5) and at + 1 < len(b) and b[at + 1] == 12


def bacnet_lines(packets):
    """`packets`: (bin, direction, payload). Scan requests and their answers go to the summary; repeats are shown once."""
    out, scans, asked, last = [], {}, {}, None    # asked: invoke id -> the scan request that used it last
    for i, d, payload in packets:
        h, inv, rp_pv, rp_answer = bacnet_frame(payload)
        if rp_pv:
            scans.setdefault(h, [])
            asked[inv] = h
            continue
        if rp_answer and inv in asked:
            if h not in scans[asked[inv]]:
                scans[asked[inv]].append(h)
            continue
        line = d + h
        if line != last:
            out.append((i, line))
        last = line
    summary = []
    for req, answers in scans.items():
        summary.append("> " + req)
        summary += ["< " + a for a in answers] or ["  (no answer)"]
    return out, summary


def iec104_lines(packets):
    """`packets`: (bin, direction, what). Each direction's bytes are joined (a new connection starts afresh) and cut into APDUs."""
    from probe_104x import asdu_text, periodic
    out, data, buf = [], set(), {}
    for i, d, what in packets:
        if what in ("SYN", "FIN", "RST", ""):
            if what == "SYN":
                buf = {}
            out.append((i, d + what))
            continue
        b = buf.get(d, b"") + bytes.fromhex(what)
        while len(b) >= 2 and len(b) >= 2 + b[1]:
            f, b = b[:2 + b[1]], b[2 + b[1]:]
            if f[2] & 3 == 1:
                continue
            text = d + ("I " + asdu_text(f[6:]) if f[2] & 1 == 0 else "U " + f.hex(" "))
            if periodic(f):
                data.add(text)
            else:
                out.append((i, text))
        buf[d] = b
    return out, sorted(data)


def sync(port):
    count = lambda: len(subprocess.run(["tshark", "-r", PCAP, "-Y", "tcp.dstport==%s && dnp3.al.func==1" % port],
                                       capture_output=True, text=True).stdout.splitlines())
    n = count()
    while count() == n:
        time.sleep(0.2)


def report(port, dnp3=False, bacnet=False, iec104=False):
    recs = [json.loads(l) for l in open(LOG)]
    if bacnet:
        return report_bacnet(port, recs)
    out = subprocess.run(
        ["tshark", "-r", PCAP, "-Y", "tcp.port==%s && (tcp.len>0 || tcp.flags.syn==1 || tcp.flags.fin==1 || tcp.flags.reset==1)" % port,
         "-T", "fields", "-e", "frame.time_epoch", "-e", "tcp.dstport", "-e", "tcp.flags.syn", "-e", "tcp.flags.fin",
         "-e", "tcp.flags.reset", "-e", "tcp.payload"],
        capture_output=True, text=True).stdout
    wire, packets = [[] for _ in range(len(recs) + 1)], []   # wire[0]: before the first record
    for line in out.splitlines():
        t, dport, syn, fin, rst, payload = (line.split("\t") + [""] * 6)[:6]
        on = lambda v: v in ("1", "True")
        what = payload.replace(":", "") or ("SYN" if on(syn) else "FIN" if on(fin) else "RST" if on(rst) else "")
        i = sum(1 for r in recs if r["t0"] <= float(t))
        packets.append((i, "> " if dport == str(port) else "< ", what))
    if os.environ.get("SERIAL"):
        # on a serial line the relay passes bytes on in pieces, so one frame can span two segments: join a direction's payloads
        joined = []
        for p in packets:
            data = p[2] not in ("SYN", "FIN", "RST", "")
            if data and joined and joined[-1][:2] == p[:2] and joined[-1][2] not in ("SYN", "FIN", "RST", ""):
                joined[-1] = (p[0], p[1], joined[-1][2] + p[2])
            else:
                joined.append(p)
        packets = joined
    data = None
    if iec104:
        lines, data = iec104_lines(packets)
    for i, text in dnp3_lines(packets) if dnp3 else lines if iec104 else [(i, d + what) for i, d, what in packets]:
        wire[i].append(text)
    print_records(recs, wire)
    if data is not None:
        print("# periodic and spontaneous data: each distinct frame")
        for t in data:
            print(t)


def report_bacnet(port, recs):
    out = subprocess.run(["tshark", "-r", PCAP, "-Y", "udp.port==%s && bvlc" % port, "-T", "fields", "-e", "frame.time_epoch",
                          "-e", "udp.srcport", "-e", "udp.payload"], capture_output=True, text=True).stdout
    packets = []
    for line in out.splitlines():
        t, sport, payload = (line.split("\t") + [""] * 3)[:3]
        i = sum(1 for r in recs if r["t0"] <= float(t))
        packets.append((i, "> " if sport == str(port) else "< ", payload.replace(":", "")))
    lines, summary = bacnet_lines(packets)
    wire = [[] for _ in range(len(recs) + 1)]
    for i, text in lines:
        wire[i].append(text)
    print_records(recs, wire, coarse=True)
    print("# scans: each ReadProperty present-value request, and each answer it got")
    for s in summary:
        print(s)


def print_records(recs, wire, coarse=False):
    """`coarse`: a reply that took 3 s or more is `~3s+` (a BACnet client waits out retries the scans interleave with)."""
    for w in wire[0]:
        print(w)
    for r, ws in zip(recs, wire[1:]):
        if "mark" in r:
            print("# " + r["mark"])
        else:
            took = r["t1"] - r["t0"]
            slow = "\t~3s+" if coarse and took >= 3 else "\t~%ds" % round(took) if took >= 0.5 else ""
            print(r["req"] + "\t" + r["reply"] + slow)
        for w in ws:
            print(w)


if __name__ == "__main__":
    a = sys.argv[1:]
    if a[:1] == ["--mark"]:
        log({"t0": time.time(), "mark": a[1]})
    elif a[:1] == ["--ctl"]:
        log({"t0": time.time(), "mark": "server " + {"stop": "stopped", "start": "started"}[a[1]]})
        c = socket.create_connection(("127.0.0.1", int(os.environ.get("CTL", "5999"))))
        c.sendall(a[1].encode() + b"\n")
        c.recv(16)
        time.sleep(float(a[2]) if len(a) > 2 else 1)
    elif a[:1] == ["--radio"]:
        radio(a[1])
    elif a[:1] == ["--sync"]:
        sync(a[1])
    elif a[:1] == ["--report"]:
        report(a[1], a[2:] == ["dnp3"], a[2:] == ["bacnet"], a[2:] == ["iec104"])
    else:
        for c in a:
            command(c)
