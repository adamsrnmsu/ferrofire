"""Run CMD (the stock device a client under test talks to) and stop or start it on request: the word `stop` or `start` on
tcp 127.0.0.1:PORT is answered `ok`. It starts stopped, so a capture sees the client's first connection.   servectl.py PORT CMD...
Stop is SIGKILL; with SERVECTL_TERM=1 it is SIGTERM and a wait (ferrofire, which then stops its modules too)."""
import os, signal, socket, subprocess, sys

port, cmd = int(sys.argv[1]), sys.argv[2:]
out = open("/tmp/peer-device.out", "ab")
child = None
l = socket.socket()
l.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
l.bind(("127.0.0.1", port))
l.listen(1)
while True:
    c, _ = l.accept()
    word = c.recv(16).strip()
    if child and word == b"stop":
        child.send_signal(signal.SIGTERM if os.environ.get("SERVECTL_TERM") else signal.SIGKILL)
        child.wait()
        child = None
    elif not child and word == b"start":
        child = subprocess.Popen(cmd, stdout=out, stderr=out, cwd="/tmp")
    c.sendall(b"ok\n")
    c.close()
