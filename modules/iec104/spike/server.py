"""c104 spike server: the points of the stock sample 104 config. usage: server.py single|double [rpoll_s]"""
import sys, time, c104
mode = sys.argv[1] if len(sys.argv) > 1 else "single"
rpoll = float(sys.argv[2]) if len(sys.argv) > 2 else 1
dbl = mode == "double"
# (address, kind): the stock sample map. outputs are also monitored, as stock's GI includes them
BIN = [(0, "out"), (1, "out"), (100, "in"), (101, "in")]
ANA = [(2, "out"), (3, "out"), (102, "in"), (103, "in")]
MT = c104.Type.M_DP_NA_1 if dbl else c104.Type.M_SP_NA_1
CT = c104.Type.C_DC_NA_1 if dbl else c104.Type.C_SC_NA_1

srv = c104.Server(ip="0.0.0.0", port=2404, tick_rate_ms=100)
st = srv.add_station(common_address=1)
mon = {}
# Monitor points first. c104 allows ONE point per IOA per station: add_point returns None for a second point at
# an IOA already used (IOA 0 excepted, a bug: get_point(0) is always None), and an incoming command is looked up by IOA.
# Stock has an output at IOA 0 that is in the GI as M_SP/M_ME *and* takes commands, so the command point below
# either cannot be added (control first -> monitor add returns None -> GI loses the output) or is shadowed by the
# monitor point (command answered negative, UNKNOWN_IOA / MISMATCHED_TYPE_ID). This is the blocker, see NOTES.md.
for a, _ in BIN: mon[a] = st.add_point(a, MT)
for a, _ in ANA: mon[a] = st.add_point(a, c104.Type.M_ME_NC_1)
for a, k in BIN:
    if k == "out": st.add_point(a, CT)
for a, k in ANA:
    if k == "out": st.add_point(a, c104.Type.C_SE_NC_1)

def log(*a): print("CMD", *a, flush=True)
def on_cmd(point: c104.Point, previous_info: c104.Information, message: c104.IncomingMessage) -> c104.ResponseState:
    log(point.type.name, point.io_address, point.value, message.cot.name)
    if point.io_address in mon: mon[point.io_address].value = point.value
    return c104.ResponseState.SUCCESS
for p in st.points:
    if p.type in (CT, c104.Type.C_SE_NC_1): p.on_receive(on_cmd)
def unexpected(server: c104.Server, message: c104.IncomingMessage, cause: c104.Umc) -> None: print("UNEXPECTED", message.type, cause, flush=True)
def rx(server: c104.Server, data: bytes) -> None: print("RX", data.hex(" "), flush=True)
srv.on_unexpected_message(unexpected)
srv.on_receive_raw(rx)
srv.start()
while not srv.has_open_connections: time.sleep(0.05)
while True:
    time.sleep(rpoll)
    for t in (MT, c104.Type.M_ME_NC_1):
        pts = [p for p in mon.values() if p.type == t]
        srv.transmit_batch(c104.Batch(cause=c104.Cot.PERIODIC, points=pts))
