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


def sync(port):
    count = lambda: len(subprocess.run(["tshark", "-r", PCAP, "-Y", "tcp.dstport==%s && dnp3.al.func==1" % port],
                                       capture_output=True, text=True).stdout.splitlines())
    n = count()
    while count() == n:
        time.sleep(0.2)


def report(port, dnp3=False):
    recs = [json.loads(l) for l in open(LOG)]
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
    for i, text in dnp3_lines(packets) if dnp3 else [(i, d + what) for i, d, what in packets]:
        wire[i].append(text)
    for w in wire[0]:
        print(w)
    for r, ws in zip(recs, wire[1:]):
        if "mark" in r:
            print("# " + r["mark"])
        else:
            took = r["t1"] - r["t0"]
            print(r["req"] + "\t" + r["reply"] + ("\t~%ds" % round(took) if took >= 0.5 else ""))
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
        report(a[1], a[2:] == ["dnp3"])
    else:
        for c in a:
            command(c)
