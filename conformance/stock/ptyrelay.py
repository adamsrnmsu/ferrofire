"""A serial line for the device under test (ww-8k4, ww-y51): a pty whose slave the device opens as LINK, joined to TCP
127.0.0.1:PORT, so the peers, captures and transcripts of the TCP runs apply to a serial endpoint unchanged.

    ptyrelay.py listen PORT LINK [LINE]    a server on the port: one TCP client at a time is joined to it, a second one is closed
    ptyrelay.py connect PORT LINK [LINE]   a client on the port: what it writes goes to the TCP server on PORT, connecting when it
                                           next writes; while that is refused the bytes are dropped, as on a line with nothing on it

LINE (default 9600-8N1) is the far end of the line: speed, data bits, parity (N, E or O) and stop bits. A pty has no speed, so
the relay models one:
- The port starts as a real one does before a program sets it: 9600 8N1, canonical, echo (the Linux default of a ttyS).
- The device's settings are read from the pty (termios2, which also holds a speed set as BOTHER) every time bytes pass, and the
  last ones are written to /tmp/ptyrelay.line, as `9600 8N1 flow=none raw`. The harness puts that line in the transcript.
- Bytes take as long as the line needs: start bit, data bits, parity bit, stop bits at LINE's speed, in both directions. They go
  on in pieces of PIECE bytes, each after its own line time, so a reader that waits for silence sees a long reply arrive as on a
  line, not all at once at the end.
- When the device's speed, data bits, parity or stop bits differ from LINE's, every byte arrives corrupted, both ways: a stand-in
  for the framing and parity errors of a real mismatched UART (which garbage a real one makes is not modelled). Each change of
  match or mismatch goes to stdout.
What the device writes is passed on after GAP of silence on the port, as a line reader frames it: a line carries bytes, not
writes, so how a device splits its writes does not show in the TCP transcript. Holding the slave open itself, the relay keeps the
line up while the device closes and reopens its port. The stock and testpeers images have no socat; this needs only python3.
"""

import fcntl
import os
import pty
import select
import selectors
import socket
import struct
import sys
import termios
import time

mode, port, link = sys.argv[1], int(sys.argv[2]), sys.argv[3]
line = sys.argv[4] if len(sys.argv) > 4 else "9600-8N1"
if mode not in ("listen", "connect"):
    sys.exit("usage: ptyrelay.py listen|connect PORT LINK [SPEED-8N1]")
speed, frame = line.split("-")
LINE = (int(speed), int(frame[0]), frame[1], int(frame[2]))
BITS = 1 + LINE[1] + (LINE[2] != "N") + LINE[3]  # start, data, parity, stop
CHAR = BITS / LINE[0]  # seconds a byte takes on the line
GAP = 0.02  # seconds of silence that end what the device wrote
PIECE = 32  # bytes paced and passed on at a time
TCGETS2 = 0x802C542A  # _IOR('T', 0x2A, struct termios2): 44 bytes, the speeds at 36..44

master, slave = pty.openpty()
attrs = termios.tcgetattr(slave)  # the pty default is canonical with echo, at 38400: make it a ttyS's 9600 8N1
attrs[2] = attrs[2] & ~(termios.CSIZE | termios.PARENB | termios.CSTOPB) | termios.CS8
attrs[4] = attrs[5] = termios.B9600
termios.tcsetattr(slave, termios.TCSANOW, attrs)
if os.path.lexists(link):
    os.unlink(link)
os.symlink(os.ttyname(slave), link)


def device():
    """The device's settings: ((speed, data bits, parity, stop bits), the line as the transcript shows it)."""
    i, _, c, lf, *_ = termios.tcgetattr(slave)
    ospeed = struct.unpack("I", fcntl.ioctl(slave, TCGETS2, bytes(44))[40:44])[0]
    bits = {termios.CS5: 5, termios.CS6: 6, termios.CS7: 7, termios.CS8: 8}[c & termios.CSIZE]
    par = "N" if not c & termios.PARENB else ("O" if c & termios.PARODD else "E")
    stop = 2 if c & termios.CSTOPB else 1
    flow = "rtscts" if c & termios.CRTSCTS else ("xonxoff" if i & (termios.IXON | termios.IXOFF) else "none")
    raw = "raw" if not lf & (termios.ICANON | termios.ECHO | termios.ISIG) else "cooked"
    return (ospeed, bits, par, stop), f"{ospeed} {bits}{par}{stop} flow={flow} {raw}"


shown, matched = None, None


def over_the_line(data, out):
    """Pass `data` to `out` piece by piece, each after its time on the line, corrupted when the device and the line disagree."""
    global shown, matched
    settings, text = device()
    if text != shown:
        shown = text
        with open("/tmp/ptyrelay.line", "w") as f:
            f.write(text + "\n")
    if (settings == LINE) != matched:
        matched = settings == LINE
        print(f"device {text}, line {line}: {'match' if matched else 'MISMATCH, bytes corrupted'}", flush=True)
    for i in range(0, len(data), PIECE):
        piece = data[i : i + PIECE]
        time.sleep(len(piece) * CHAR)
        out(piece if matched else bytes(b ^ 0x5A for b in piece))


sel = selectors.DefaultSelector()
sel.register(master, selectors.EVENT_READ, "pty")
peer = None


def join(c):
    global peer
    c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    peer = c
    sel.register(c, selectors.EVENT_READ, "tcp")


def drop():
    global peer
    if peer:
        sel.unregister(peer)
        peer.close()
        peer = None


if mode == "listen":
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", port))
    listener.listen(4)
    sel.register(listener, selectors.EVENT_READ, "accept")

while True:
    for key, _ in sel.select():
        if key.data == "accept":
            c, _ = listener.accept()
            if peer:
                c.close()  # one line, one peer
            else:
                join(c)
        elif key.data == "pty":
            data = os.read(master, 4096)
            while select.select([master], [], [], GAP)[0]:
                data += os.read(master, 4096)
            if mode == "connect" and peer is None:
                try:
                    join(socket.create_connection(("127.0.0.1", port), timeout=1))
                    peer.settimeout(None)
                except OSError:
                    continue
            if peer:
                try:
                    over_the_line(data, peer.sendall)
                except OSError:
                    drop()
        elif key.fileobj is peer:  # not a peer this batch already dropped
            try:
                data = peer.recv(4096)
            except OSError:
                data = b""
            if data:
                over_the_line(data, lambda piece: os.write(master, piece))
            else:
                drop()
