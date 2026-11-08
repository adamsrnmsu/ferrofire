import socket, struct, time
def mk():
    t=[]
    for i in range(256):
        c=i
        for _ in range(8): c=(c>>1)^0xA6BC if c&1 else c>>1
        t.append(c)
    return t
T=mk()
def crc(b):
    c=0
    for x in b: c=(c>>8)^T[(c^x)&0xFF]
    return (~c)&0xFFFF
def frame(dst, src, ctrl, user):
    hdr=bytes([0x05,0x64,5+len(user),ctrl])+struct.pack("<HH",dst,src)
    out=hdr+struct.pack("<H",crc(hdr))
    for i in range(0,len(user),16):
        blk=user[i:i+16]; out+=blk+struct.pack("<H",crc(blk))
    return out
def parse(buf):
    res=[]; i=0
    while i+10<=len(buf) and buf[i]==5 and buf[i+1]==0x64:
        ln=buf[i+2]; ctrl=buf[i+3]; dst,src=struct.unpack("<HH",buf[i+4:i+8])
        ul=ln-5; data=b""; j=i+10
        while ul>0:
            n=min(16,ul); data+=buf[j:j+n]; j+=n+2; ul-=n
        res.append((ctrl,dst,src,data)); i=j
    return res
def app(fc, objs, seq=0, ctl=0xC0):
    return bytes([ctl|seq, fc])+objs
def tp(seq, a): return bytes([0xC0|seq])+a
s=socket.create_connection(("127.0.0.1",20000),timeout=3)
def txrx(label, user, wait=1.0):
    s.sendall(frame(10,1,0xC4,user)); time.sleep(wait); s.settimeout(0.5)
    buf=b""
    try:
        while True:
            d=s.recv(2048)
            if not d: break
            buf+=d
    except Exception: pass
    for ctrl,dst,src,data in parse(buf):
        print(label,"link ctrl=%02x %d<-%d"%(ctrl,dst,src),"tp=%02x"%data[0] if data else "", "app:",data[1:].hex(" "))
    return buf
# integrity poll: READ g60v2,g60v3,g60v4,g60v1 (qual 06 = all)
objs=bytes([60,2,6, 60,3,6, 60,4,6, 60,1,6])
txrx("integrity",tp(0,app(1,objs,0)))
# class 1 only
txrx("class1   ",tp(1,app(1,bytes([60,2,6]),1)))
# DIRECT_OPERATE (fc 5) CROB g12v1 qual 0x17 (1-byte count + 1-byte idx) idx=5 LATCH_ON(3) count=1 on=100 off=0 status 0
crob=bytes([12,1,0x17,1, 5, 3,1]+list(struct.pack("<II",100,0))+[0])
txrx("direct CROB idx5",tp(2,app(5,crob,2)))
# SELECT (3) on idx 5
txrx("select CROB idx5",tp(3,app(3,crob,3)))
# analog output float g41v3 qual 0x17 idx 1 value 55.5 status 0
ao=bytes([41,3,0x17,1,1])+struct.pack("<f",55.5)+bytes([0])
txrx("direct AO f32 idx1",tp(4,app(5,ao,4)))
# SELECT then OPERATE for sbo idx1
txrx("select AO idx1",tp(5,app(3,ao,5)))
txrx("operate AO idx1",tp(6,app(4,ao,6)))
# direct operate unknown idx
crob2=bytes([12,1,0x17,1, 9, 3,1]+list(struct.pack("<II",100,0))+[0])
txrx("direct CROB idx9",tp(7,app(5,crob2,7)))
# Direct operate AO int16 g41v2
ao2=bytes([41,2,0x17,1,1])+struct.pack("<h",12)+bytes([0])
txrx("direct AO i16 idx1",tp(8,app(5,ao2,8)))
