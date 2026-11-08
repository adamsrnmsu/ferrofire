# Minimal DISH: binds the multicast port, joins the group, unframes datagrams ([u8 len][group][body\0]).
# Stands in for pybennu-test-subscriber (its swig _Endpoint.so is a dangling file in the bennu:arm64 image).
import socket, struct, time
GROUP = "54386972605942b"  # group for udp://239.0.0.1:40000
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
s.bind(("", 40000))
s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, struct.pack("4s4s", socket.inet_aton("239.0.0.1"), socket.inet_aton("0.0.0.0")))
s.settimeout(5)
n = 0
t0 = time.time()
while time.time() - t0 < 5:
    try: d = s.recv(8192)
    except socket.timeout: break
    g = d[1:1 + d[0]].decode()
    body = d[1 + d[0]:]
    if n == 0:
        print(f"first datagram: {len(d)} bytes, group={g!r} (expected {GROUP!r}, match={g == GROUP}), nul_terminated={body.endswith(bytes(1))}")
        print("body:", body.decode()[:300])
    n += 1
print(f"{n} datagrams in 5 s")
