import c104,time,socket
T=c104.Type
s=c104.Server(port=2405);st=s.add_station(common_address=1)
st.add_point(0,T.C_SC_NA_1); st.add_point(1,T.C_SC_NA_1); st.add_point(100,T.M_SP_NA_1)
s.start();time.sleep(0.5)
c=socket.create_connection(("127.0.0.1",2405));c.sendall(bytes.fromhex("680407000000"));time.sleep(.3);c.recv(99)
c.sendall(bytes([0x68,0x0e,0,0,0,0,100,1,6,0,1,0,0,0,0,20]));time.sleep(1);print(c.recv(999).hex(" "))
# command to a control point, the value from c104's own view
def cb(point: c104.Point, previous_info: c104.Information, message: c104.IncomingMessage) -> c104.ResponseState:
    print("CB",point.io_address,point.value);return c104.ResponseState.SUCCESS
st.get_point(1).on_receive(cb)
c.sendall(bytes([0x68,0x0e,2,0,2,0,45,1,6,0,1,0,1,0,0,1]));time.sleep(1);print(c.recv(999).hex(" "))
