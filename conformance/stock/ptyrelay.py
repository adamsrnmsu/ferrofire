"""A serial port for the device under test (ww-8k4): a pty whose slave the device opens as LINK, joined to TCP 127.0.0.1:PORT,
so the peers, captures and transcripts of the TCP runs apply to a serial endpoint unchanged. The relay holds the slave open
itself, so the device closing and reopening its port never ends the line. What the device writes is passed on after GAP of
silence on the port, as a line reader frames it: a line carries bytes, not writes, so how a device splits its writes (stock
Modbus writes one reply per request, ours a pipelined pair at once) does not show in the TCP transcript.

    ptyrelay.py listen PORT LINK    a server on the port: one TCP client at a time is joined to it, a second one is closed
    ptyrelay.py connect PORT LINK   a client on the port: what it writes goes to the TCP server on PORT, connecting when it
                                    next writes; while that is refused the bytes are dropped, as on a line with nothing on it

The stock and testpeers images have no socat; this needs only python3.
"""

import os
import pty
import select
import selectors
import socket
import sys
import tty

mode, port, link = sys.argv[1], int(sys.argv[2]), sys.argv[3]
master, slave = pty.openpty()
tty.setraw(slave)
if os.path.lexists(link):
    os.unlink(link)
os.symlink(os.ttyname(slave), link)

GAP = 0.02  # seconds of silence that end what the device wrote

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
elif mode != "connect":
    sys.exit("usage: ptyrelay.py listen|connect PORT LINK")

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
                    peer.sendall(data)
                except OSError:
                    drop()
        elif key.fileobj is peer:  # not a peer this batch already dropped
            try:
                data = peer.recv(4096)
            except OSError:
                data = b""
            if data:
                os.write(master, data)
            else:
                drop()
