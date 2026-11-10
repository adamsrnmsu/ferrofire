import struct,sys
f=open(sys.argv[1],'rb').read()
magic=struct.unpack('<I',f[:4])[0]
e='<' if magic in(0xa1b2c3d4,0xa1b23c4d) else '>'
ns=magic in(0xa1b23c4d,0x4d3cb2a1)
off=24;tot=0;n=0;t0=t1=None;by={}
while off+16<=len(f):
    s,u,cl,ol=struct.unpack(e+'IIII',f[off:off+16]);off+=16
    d=f[off:off+cl];off+=cl
    t=s+u/(1e9 if ns else 1e6)
    t0=t0 or t;t1=t
    k='other'
    if len(d)>=34 and d[12:14]==b'\x08\x00':
        ihl=(d[14]&15)*4;p=d[23]
        if p in(6,17):
            sp,dp=struct.unpack('>HH',d[14+ihl:18+ihl])
            k=('udp' if p==17 else 'tcp')+':'+str(min(sp,dp) if min(sp,dp) in(40000,5555,502) else dp)
    a=by.setdefault(k,[0,0]);a[0]+=1;a[1]+=ol;tot+=ol;n+=1
dur=(t1-t0) if t1 else 0
print('packets',n,'bytes',tot,'dur %.1fs'%dur,'B/s %.0f'%(tot/dur if dur else 0))
for k,v in sorted(by.items()):print(' ',k,'pkts',v[0],'bytes',v[1],'B/s %.0f'%(v[1]/dur if dur else 0))
