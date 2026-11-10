#!/usr/bin/env python3
"""inject.py SECS : from rtu-2, send malformed datagrams into the whole-model multicast group (RADIO group of udp://239.0.0.1:40000) at ~200/s.
Kinds: empty, random bytes, group_len past the end, group_len 255, wrong group, valid group truncated mid-number (with and without NUL),
valid group + unknown tags only, valid group + non-numeric/nan/inf/empty values for real tags, valid group without a colon, 64 KB blob."""
import os, random, socket, sys, time
G = b"54386972605942b"; EP = ("239.0.0.1", 40000)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton("172.16.1.102"))
hdr = bytes([len(G)]) + G
good = b"sim.t:1.5,lock.height:5,boat.location:50,"
kinds = [lambda: b"", lambda: os.urandom(random.randint(1, 1400)), lambda: bytes([40]) + b"abc", lambda: bytes([255]) + os.urandom(100),
  lambda: bytes([6]) + b"nogrp1" + good + b"\0", lambda: hdr + good[:25], lambda: hdr + good[:25] + b"\0", lambda: hdr + b"zzz.unknown:1,qq.tag:2,\0",
  lambda: hdr + b"lock.height:abc,boat.location:nan,lowergate.open:inf,uppergate.open:,\0", lambda: hdr + b"lock.height5,boat.location,\0",
  lambda: hdr + b"lock.height:" + b"9" * 400 + b",\0", lambda: hdr + b":::,,,,,\0", lambda: bytes([15]) + b"\xff\xfe" * 7 + b"\xff" + b"x:1,\0",
  lambda: hdr + os.urandom(1300), lambda: b"\x00" + good + b"\0"]
n = 0; end = time.time() + float(sys.argv[1]); counts = [0] * len(kinds)
while time.time() < end:
    for i, k in enumerate(kinds):
        s.sendto(k(), EP); counts[i] += 1; n += 1
    time.sleep(0.07)   # 15 kinds / 70 ms ~ 215 datagrams/s
big = hdr + os.urandom(60000)
try: s.sendto(big, EP); n += 1
except OSError as e: print("64KB send:", e)
print("sent %d malformed datagrams, per kind %s" % (n, counts))
