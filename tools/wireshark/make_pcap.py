"""Write a one-packet pcap (Ethernet/IPv4/UDP) holding the datagram v1 test vector of docs/datagram-v1.md."""
import struct
import sys

M = 0xFFFFFFFFFFFFFFFF
MUL = 0xC6A4A7935BD1E995


def libstdcxx_hash(data: bytes) -> int:
    """std::hash<std::string> of libstdc++ (_Hash_bytes, seed 0xc70f6907), 64-bit."""
    mix = lambda v: v ^ (v >> 47)
    h = (0xC70F6907 ^ (len(data) * MUL)) & M
    for i in range(0, len(data) - len(data) % 8, 8):
        k = int.from_bytes(data[i:i + 8], "little")
        h = ((h ^ ((mix((k * MUL) & M) * MUL) & M)) * MUL) & M
    if len(data) % 8:
        h = ((h ^ int.from_bytes(data[len(data) - len(data) % 8:], "little")) * MUL) & M
    return mix((mix(h) * MUL) & M)


def group_for(endpoint: str) -> str:
    h = format(libstdcxx_hash(endpoint.encode()), "x")
    return h[:-1]


ENDPOINT = "udp://239.0.0.1:40000"
group = group_for(ENDPOINT)
body = b"sim.t:0.1,lock.height:5,lowervalve.open:0,_ff.seq:7,_ff.cyc:3,_ff.t:1234567890,\0"
datagram = bytes([len(group)]) + group.encode() + body

if __name__ == "__main__":
    udp = struct.pack(">HHHH", 50000, 40000, 8 + len(datagram), 0) + datagram
    ip = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(udp), 1, 0, 1, 17, 0, bytes([10, 0, 0, 2]), bytes([239, 0, 0, 1]))
    s = sum(struct.unpack(">10H", ip))
    s = (s & 0xFFFF) + (s >> 16)
    ip = ip[:10] + struct.pack(">H", ~((s & 0xFFFF) + (s >> 16)) & 0xFFFF) + ip[12:]
    eth = bytes.fromhex("01005e000001") + bytes.fromhex("020000000002") + b"\x08\x00"
    pkt = eth + ip + udp
    with open(sys.argv[1], "wb") as f:
        f.write(struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 1))
        f.write(struct.pack("<IIII", 0, 0, len(pkt), len(pkt)) + pkt)
