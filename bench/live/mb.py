import socket,struct,sys,time
ip=sys.argv[1];n=int(sys.argv[2]);dt=float(sys.argv[3])
def rd(a,c):
    s=socket.create_connection((ip,502),timeout=5)
    s.sendall(struct.pack(">HHHBBHH",1,0,6,1,4,a,c));r=s.recv(256);s.close()
    return struct.unpack(">%dH"%c,r[9:9+2*c])
for i in range(n):
    print('%.1f'%(i*dt),rd(30000,10),flush=True);time.sleep(dt)
