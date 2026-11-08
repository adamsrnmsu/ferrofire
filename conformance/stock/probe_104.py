import socket, struct, time, sys
def dump(tag, b): print(tag, b.hex(" "))
s = socket.create_connection(("127.0.0.1", 2404), timeout=4)
s.sendall(bytes.fromhex("680407000000"))
time.sleep(0.3); dump("STARTDT con:", s.recv(64))
# C_IC_NA_1 type 100, vsq 1, cot 6 (act), oa 0, ca 1, ioa 0, qoi 20
gi = bytes([0x68, 0x0e, 0,0, 0,0, 100, 1, 6,0, 1,0, 0,0,0, 20])
s.sendall(gi)
time.sleep(1.0)
s.settimeout(1.0)
buf=b""
try:
    while True:
        d=s.recv(1024)
        if not d: break
        buf+=d
except Exception: pass
# split APDUs
i=0
while i < len(buf) and buf[i]==0x68:
    n=buf[i+1]; dump("APDU:", buf[i:i+2+n]); i+=2+n
# C_SC_NA_1 single command type 45 IOA 0 (binary-output addr 0) ON, cot 6
cmd = bytes([0x68, 0x0e, 2,0, 0,0, 45, 1, 6,0, 1,0, 0,0,0, 0x01])
s.sendall(cmd); time.sleep(0.5)
try: d=s.recv(1024); dump("cmd resp:", d)
except Exception as e: print("cmd resp none", e)
# C_SE_NC_1 setpoint float IOA 3 = 77.5 type 50
cmd2 = bytes([0x68, 0x12, 4,0, 0,0, 50, 1, 6,0, 1,0, 3,0,0]) + struct.pack("<f", 77.5) + bytes([0])
s.sendall(cmd2); time.sleep(0.5)
try: d=s.recv(1024); dump("setpoint resp:", d)
except Exception as e: print("setpoint resp none", e)
