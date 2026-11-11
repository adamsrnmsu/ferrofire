#!/usr/bin/env python3
"""Run pybennu's stock groundtruth_monitor.py, unmodified, inside the bennu:arm64 image.

The image ships pybennu/distributed/swig/_Endpoint.so built for the wrong architecture (it cannot be loaded), so the three-function
C++ Endpoint wrapper it provides is replaced here by Python (the group hash is libstdc++'s std::hash, the same one ferrofire
implements). The image's pyzmq is also built without draft support, so a ZeroMQ DISH socket cannot be opened: the Subscriber
base class (a DISH bound to the publish endpoint, joined to its group, handing each message string to the handler) is replaced
by a plain UDP socket doing the same on the wire (RADIO framing: [u8 len][group][body]). Everything else is the stock script:
its argument handling, config.ini reading, message handler (the code under test: tag split, filter, rate limit, documents) and
Elasticsearch bulk push. One more shim: the stock script builds its client as Elasticsearch([{"host": ip, "port": "9200"}]),
which the elasticsearch-py 8 client in the image refuses (no scheme), so the node dict gets "http" and an int port added.
Usage: stock_monitor.py -c config.ini start
"""
import socket
import struct
import sys
import types

M, MASK = 0xC6A4A7935BD1E995, (1 << 64) - 1


def hash64(data: bytes) -> int:
    mix = lambda v: v ^ (v >> 47)
    h = (0xC70F6907 ^ (len(data) * M)) & MASK
    whole = len(data) // 8 * 8
    for i in range(0, whole, 8):
        d = (mix((int.from_bytes(data[i : i + 8], "little") * M) & MASK) * M) & MASK
        h = ((h ^ d) * M) & MASK
    if data[whole:]:
        h = ((h ^ int.from_bytes(data[whole:], "little")) * M) & MASK
    return mix((mix(h) * M) & MASK)


def group(endpoint: str) -> str:
    a, b = endpoint.find(";"), endpoint.find("//")
    if a != -1 and b != -1 and b + 2 <= a:
        endpoint = endpoint[: b + 2] + endpoint[a + 1 :]
    return f"{hash64(endpoint.encode()):x}"[:-1]


class Endpoint:
    str = ""


stub = types.ModuleType("pybennu.distributed.swig._Endpoint")
stub.new_Endpoint = Endpoint
stub.Endpoint_str_set = lambda e, s: setattr(e, "str", s)
stub.Endpoint_str_get = lambda e: e.str
stub.Endpoint_hash = lambda e: group(e.str)
sys.modules["pybennu.distributed.swig._Endpoint"] = stub
import pybennu.distributed.swig as swig  # noqa: E402

swig._Endpoint = stub

import pybennu.distributed.subscriber as subscriber  # noqa: E402


class UdpSubscriber:
    """The stock Subscriber's job over a plain UDP socket (multicast joined when the endpoint is a group)."""

    def __init__(self, endpoint):
        self.endpoint = endpoint
        self.subscription_handler = lambda m: print("Received subscription update: %s" % m)
        self.running = True
        self.bind()

    def bind(self):
        url = stub.Endpoint_str_get(self.endpoint)
        host, port = url.removeprefix("udp://").rsplit(";", 1)[-1].rsplit(":", 1)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("", int(port)))
        if int(host.split(".")[0]) in range(224, 240):
            req = struct.pack("4s4s", socket.inet_aton(host), socket.inet_aton("0.0.0.0"))
            self.sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, req)
        self.sock.settimeout(1)

    def run(self):
        want = stub.Endpoint_hash(self.endpoint)
        while self.running:
            try:
                d = self.sock.recv(65536)
            except socket.timeout:
                continue
            if d and d[1 : 1 + d[0]].decode() == want:
                self.subscription_handler(d[1 + d[0] :].decode())

    def disconnect(self):
        self.running = False


subscriber.Subscriber = UdpSubscriber

import elasticsearch  # noqa: E402
from pybennu.executables import groundtruth_monitor as stock  # noqa: E402

_Elasticsearch = elasticsearch.Elasticsearch
stock.Elasticsearch = lambda nodes: _Elasticsearch(
    [{"host": n["host"], "port": int(n["port"]), "scheme": "http"} for n in nodes]
)

if __name__ == "__main__":
    sys.argv[0] = "groundtruth_monitor"
    stock.main()
