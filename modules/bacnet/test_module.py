"""module.py driven through stdin/stdout by a scripted device, with a bacpypes3 peer in the test (make modbac-test).
Everything runs on the loopback interface (BACNET_IFACE=lo), each module and peer on its own port."""
import asyncio, itertools, json, os, queue, re, subprocess, sys, threading

from bacpypes3.app import Application
from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.local.analog import AnalogInputObject
from bacpypes3.local.binary import BinaryOutputObject
from bacpypes3.pdu import Address
from bacpypes3.primitivedata import ObjectIdentifier

HERE = os.path.dirname(os.path.abspath(__file__))
STOCK = os.path.join(HERE, "..", "..", "conformance", "stock", "bacnet-server.xml")
PORTS = itertools.count(47901)


def stock_server():
    """The <bacnet-server> element of the stock sample config, as written, and the device's tags."""
    text = open(STOCK).read()
    element = re.search(r"<bacnet-server>.*</bacnet-server>", text, re.S).group(0)
    tags = dict(re.findall(r"<name>([^<]+)</name>\s*<io>[^<]*</io>\s*<type>(\w+)</type>", text))
    points = [{"tag": t, "kind": k, "value": False if k == "binary" else 0.0} for t, k in tags.items()]
    return element, points


class Module:
    """module.py as the device runs it; lines it prints arrive on a queue."""

    def __init__(self, port):
        env = dict(os.environ, BACNET_IFACE="lo", BACNET_IP_PORT=str(port), FERROFIRE_MODULE="bacnet")
        self.p = subprocess.Popen([sys.executable, os.path.join(HERE, "module.py")], cwd=HERE, env=env, text=True,
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        self.lines, self.seen = queue.Queue(), []
        threading.Thread(target=lambda: [self.lines.put(json.loads(l)) for l in self.p.stdout], daemon=True).start()

    def send(self, **m):
        self.p.stdin.write(json.dumps(m) + "\n")
        self.p.stdin.flush()

    def hello(self, config, points):
        self.send(t="hello", v=1, device="rtu-1", opts={"strict": False}, config=config, points=points)

    async def expect(self, t, timeout=10, **fields):
        """The next message of type t whose fields match; others are skipped."""
        loop = asyncio.get_running_loop()
        end = loop.time() + timeout
        while True:
            m = await asyncio.to_thread(self.lines.get, True, max(0.01, end - loop.time()))
            self.seen.append(m)
            if m["t"] == t and all(m.get(k) == v for k, v in fields.items()):
                return m

    def close(self):
        if self.p.poll() is None:
            self.send(t="stop")
            self.p.stdin.close()
            try:
                self.p.wait(5)
            except subprocess.TimeoutExpired:
                self.p.kill()


def peer(port, *objects):
    args = SimpleArgumentParser().parse_args(["--name", "peer", "--instance", str(port), "--address", f"127.0.0.1/8:{port}"])
    app = Application.from_args(args)
    for o in objects:
        app.add_object(o)
    return app


def run(coro):
    asyncio.run(asyncio.wait_for(coro, 60))


def test_stock_server_config_gives_ready():
    element, points = stock_server()
    m = Module(next(PORTS))
    try:
        m.hello(element, points)

        async def go():
            r = await m.expect("ready")
            assert r["role"] == "server"
            uses = sorted((u["tag"], u["kind"], u["dir"]) for u in r["uses"])
            assert uses == sorted([
                ("brkr", "binary", "out"), ("generator-1_bus-1.active", "binary", "out"),
                ("load-breaker-toggle", "binary", "in"), ("generator-1_bus-1.active", "binary", "in"),
                ("load-power", "analog", "out"), ("bus-1.gen_mw", "analog", "out"),
                ("load-mw-setpoint", "analog", "in"), ("bus-1.gen_mw", "analog", "in")])
        run(go())
    finally:
        m.close()


def test_missing_instance_is_an_error():
    element, points = stock_server()
    for bad in (re.sub(r"<instance>\d+</instance>", "", element), element.replace("udp://", "tcp://")):
        m = Module(next(PORTS))
        try:
            m.hello(bad, points)

            async def go():
                await m.expect("error")
            run(go())
            assert m.p.wait(5) == 0         # and exits: an error means no restart
        finally:
            m.close()


def test_write_property_becomes_a_write():
    element, points = stock_server()
    port = next(PORTS)
    m = Module(port)
    try:
        m.hello(element, points)

        async def go():
            await m.expect("ready")
            app = peer(next(PORTS))
            try:
                for _ in range(20):        # the module's objects are built after ready
                    r = await app.write_property(Address(f"127.0.0.1:{port}"), "analog-output,2", "present-value", 7.5)
                    if r is None:
                        break
                    await asyncio.sleep(0.5)
                assert r is None, r
                w = await m.expect("write", tag="load-mw-setpoint")
                assert w["value"] == 7.5 and isinstance(w["id"], int)
            finally:
                app.close()
        run(go())
    finally:
        m.close()


def test_values_reach_read_property():
    element, points = stock_server()
    port = next(PORTS)
    m = Module(port)
    try:
        m.hello(element, points)

        async def go():
            await m.expect("ready")
            m.send(t="values", changes=[["load-power", 12.5], ["brkr", True]])
            app = peer(next(PORTS))
            dut = Address(f"127.0.0.1:{port}")
            try:
                ai = bi = None
                for _ in range(20):        # 1 Hz refresh
                    try:
                        ai = await app.read_property(dut, "analog-input,0", "present-value")
                        bi = await app.read_property(dut, "binary-input,0", "present-value")
                    except Exception:
                        pass
                    if ai == 12.5 and bi == "active":
                        break
                    await asyncio.sleep(0.5)
                assert (ai, str(bi)) == (12.5, "active")
            finally:
                app.close()
        run(go())
    finally:
        m.close()


def client_config(port):
    return (f"<bacnet-client><scan-rate>1</scan-rate><bacnet-connection><endpoint>udp://127.0.0.1:{port}</endpoint>"
            "<instance>77</instance><analog-input><address>0</address><tag>a</tag></analog-input>"
            "<analog-input><address>9</address><tag>c</tag></analog-input>"     # the peer has no analog-input 9
            "<binary-output><address>1</address><tag>b</tag></binary-output></bacnet-connection>"
            "<command-interface>tcp://127.0.0.1:1332</command-interface></bacnet-client>")


def client_peer(port):
    ai = AnalogInputObject(objectIdentifier=("analog-input", 0), objectName="ai0", presentValue=3.5)
    bo = BinaryOutputObject(objectIdentifier=("binary-output", 1), objectName="bo1", presentValue="inactive")
    return peer(port, ai, bo), bo


def test_client_polls_into_writes():
    server = next(PORTS)
    m = Module(next(PORTS))
    try:
        async def go():
            app, _ = client_peer(server)
            try:
                m.hello(client_config(server), [{"tag": "a", "kind": "analog", "value": 0.0},
                                                {"tag": "b", "kind": "binary", "value": False}])
                r = await m.expect("ready")
                assert r["role"] == "client"
                assert sorted((u["tag"], u["kind"], u["dir"]) for u in r["uses"]) == [
                    ("a", "analog", "in"), ("b", "binary", "out"), ("c", "analog", "in")]
                for _ in range(2):         # every poll is reported, not only changes; a failed read is not, and polling goes on
                    await m.expect("write", tag="b", value=False)
                    await m.expect("write", tag="a", value=3.5)
                assert not [w for w in m.seen if w.get("tag") == "c"]
            finally:
                app.close()
        run(go())
    finally:
        m.close()


def test_values_of_outputs_become_write_property():
    server = next(PORTS)
    m = Module(next(PORTS))
    try:
        async def go():
            app, bo = client_peer(server)
            try:
                m.hello(client_config(server), [{"tag": "a", "kind": "analog", "value": 0.0},
                                                {"tag": "b", "kind": "binary", "value": False}])
                await m.expect("ready")
                m.send(t="values", changes=[["a", 9.0], ["b", True]])     # a is an input: not written
                s = await m.expect("sent")
                assert s == {"t": "sent", "tag": "b", "value": True, "ok": True}
                assert str(bo.presentValue) == "active"
                await m.expect("write", tag="b", value=True)
                await m.expect("write", tag="a", value=3.5)
            finally:
                app.close()
        run(go())
    finally:
        m.close()


def test_put_is_write_property_answered_with_done():
    """The command interface's WRITE= (a `put`): the peer's outcome is the `done`. An input tag is written too, to the object the
    type rule names: a binary tag to its binary-output, an analog-input tag to its analog-input (stock's server refuses that one)."""
    server = next(PORTS)
    m = Module(next(PORTS))
    try:
        async def go():
            app, bo = client_peer(server)
            try:
                m.hello(client_config(server), [{"tag": "a", "kind": "analog", "value": 0.0},
                                                {"tag": "b", "kind": "binary", "value": False}])
                await m.expect("ready")
                m.send(t="put", id=7, tag="b", value=True)
                assert await m.expect("done", id=7) == {"t": "done", "id": 7, "ok": True, "err": ""}
                assert str(bo.presentValue) == "active"
                m.send(t="put", id=8, tag="b", value=True)           # the value it already holds: sent all the same
                assert (await m.expect("done", id=8))["ok"] is True
                m.send(t="put", id=9, tag="a", value=5.0)            # an input tag: sent to analog-input 0 (this peer takes it)
                assert (await m.expect("done", id=9))["ok"] is True
                assert float(app.get_object_id(ObjectIdentifier("analog-input,0")).presentValue) == 5.0
                m.send(t="put", id=10, tag="nope", value=1.0)
                assert (await m.expect("done", id=10))["ok"] is False
            finally:
                app.close()
        run(go())
    finally:
        m.close()


def test_a_put_is_not_answered_before_the_peers_transaction_ends_and_nothing_lands_later():
    """A peer that never answers: the WriteProperty goes out once and three times more, 3 s apart; `done` is `ok: false` after the
    last, and no request follows it (a reported failure must not land later)."""
    import socket
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(0.5)
    m = Module(next(PORTS))
    try:
        async def go():
            m.hello(client_config(sock.getsockname()[1]), [{"tag": "a", "kind": "analog", "value": 0.0},
                                                            {"tag": "b", "kind": "binary", "value": False}])
            await m.expect("ready")
            m.send(t="put", id=1, tag="b", value=True)
            wps, loop = [], asyncio.get_running_loop()
            def drain():
                try:
                    while True:
                        d, _ = sock.recvfrom(1500)
                        if d[9:10] == b"\x0f":        # BVLC 4, NPDU 2, then `00 05 invoke service`: WriteProperty is 15
                            wps.append(loop.time())
                except socket.timeout:
                    pass
            t0 = loop.time()
            done = asyncio.ensure_future(m.expect("done", id=1, timeout=30))
            while not done.done():
                await asyncio.to_thread(drain)
            d = await done
            assert d["ok"] is False and loop.time() - t0 > 10, (d, loop.time() - t0)
            n = len(wps)
            await asyncio.to_thread(drain)
            await asyncio.sleep(7)                                   # past any retransmit bacpypes3 might still have scheduled
            await asyncio.to_thread(drain)
            assert n == 4 and len(wps) == n, wps
        run(go())
    finally:
        m.close()
        sock.close()


def test_client_reads_the_object_types_stock_reads():
    """Stock's type rule (module.py stock_type): every binary point is read as a binary-output, an analog one as an analog-input
    only while its tag was last declared as one. The peer has only the objects stock would read; every point still polls."""
    server = next(PORTS)
    m = Module(next(PORTS))
    config = (f"<bacnet-client><scan-rate>1</scan-rate><bacnet-connection><endpoint>udp://127.0.0.1:{server}</endpoint>"
              "<instance>77</instance><binary-input><address>0</address><tag>bi</tag></binary-input>"
              "<analog-input><address>0</address><tag>a</tag></analog-input>"
              "<analog-input><address>1</address><tag>g</tag></analog-input>"
              "<analog-output><address>3</address><tag>g</tag></analog-output></bacnet-connection></bacnet-client>")
    try:
        async def go():
            from bacpypes3.local.analog import AnalogOutputObject
            objs = [BinaryOutputObject(objectIdentifier=("binary-output", 0), objectName="bo0", presentValue="active"),
                    AnalogInputObject(objectIdentifier=("analog-input", 0), objectName="ai0", presentValue=1.5),
                    AnalogOutputObject(objectIdentifier=("analog-output", 1), objectName="ao1", presentValue=2.5),
                    AnalogOutputObject(objectIdentifier=("analog-output", 3), objectName="ao3", presentValue=4.0)]
            app = peer(server, *objs)
            try:
                m.hello(config, [{"tag": "bi", "kind": "binary", "value": False}, {"tag": "a", "kind": "analog", "value": 0.0},
                                 {"tag": "g", "kind": "analog", "value": 0.0}])
                await m.expect("ready")
                await m.expect("write", tag="bi", value=True)      # binary-output 0, not binary-input 0
                await m.expect("write", tag="a", value=1.5)        # analog-input 0
                await m.expect("write", tag="g", value=2.5)        # analog-output 1: g was last declared an output
                await m.expect("write", tag="g", value=4.0)
            finally:
                app.close()
        run(go())
    finally:
        m.close()


def test_client_requests_carry_stocks_header():
    """A confirmed request from the client is `00 05` after the BVLC and NPDU, as bacnet-stack sends it: no segmented response
    accepted, max segments unspecified, max APDU 1476."""
    import socket
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(15)
    m = Module(next(PORTS))
    try:
        m.hello(client_config(sock.getsockname()[1]), [{"tag": "a", "kind": "analog", "value": 0.0},
                                                        {"tag": "b", "kind": "binary", "value": False}])
        d, _ = sock.recvfrom(1500)
        assert d[:2] == b"\x81\x0a" and d[4] == 1 and d[5] & 0xa8 == 0, d.hex()   # BVLC unicast; NPDU with no DNET/SNET
        assert d[6:8] == b"\x00\x05", d.hex()
    finally:
        m.close()
        sock.close()


def test_decoy_objects_carry_stocks_required_properties():
    """135 Clause 12 required properties and stock's values (golden RPM-ALL blocks): AV Units, MSI/MSV/MSO Event_State and
    Out_Of_Service, MSO Priority_Array and Relinquish_Default, PIV Units, Schedule and Trend Log sets; Current_Command_Priority on
    every commandable type (135-2012as); no COV_Increment on AV (135-2012aw)."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("bacnet_module", os.path.join(HERE, "module.py"))
    m = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = m          # bacpypes3's metaclass looks the defining module up
    spec.loader.exec_module(m)
    async def build():                  # bacpypes3's _post_init wants a running loop
        return m.build_objects()
    first = {}
    for o in asyncio.run(build()):
        first.setdefault(str(o.objectIdentifier[0]), o)
    def has(t, *names):
        return [n for n in names if getattr(first[t], n, None) is None]
    assert has("analog-value", "units", "description", "eventState", "outOfService") == []
    assert first["analog-value"].covIncrement is None
    assert has("binary-value", "priorityArray", "relinquishDefault", "currentCommandPriority", "description") == []
    assert has("multi-state-input", "eventState", "outOfService", "numberOfStates", "stateText") == []
    assert has("multi-state-value", "eventState", "outOfService", "numberOfStates", "stateText") == []
    assert has("multi-state-output", "eventState", "outOfService", "priorityArray", "relinquishDefault", "currentCommandPriority") == []
    assert has("analog-output", "currentCommandPriority") == [] and has("binary-output", "currentCommandPriority") == []
    assert has("positive-integer-value", "units", "outOfService") == []
    assert has("schedule", "presentValue", "effectivePeriod", "scheduleDefault", "listOfObjectPropertyReferences", "priorityForWriting",
               "reliability", "outOfService", "weeklySchedule") == []
    assert has("trend-log", "enable", "stopWhenFull", "bufferSize", "recordCount", "totalRecordCount", "eventState", "loggingType") == []
    assert first["multi-state-input"].numberOfStates == 254 and first["multi-state-input"].objectName == "MULTISTATE INPUT 0"
    assert first["trend-log"].objectName == "Trend Log 0" and first["life-safety-point"].objectName == "LS POINT 0"
    # Property_List: the served properties without the four 135-2010ao excludes
    for t in ("analog-output", "multi-state-input", "trend-log", "channel"):
        names = [m.pname(p) for p in first[t].propertyList]
        assert not {"objectIdentifier", "objectName", "objectType", "propertyList"} & set(names) and "statusFlags" in names, (t, names)
    assert "currentCommandPriority" in [m.pname(p) for p in first["analog-output"].propertyList]


def load_module():
    import importlib.util
    spec = importlib.util.spec_from_file_location("bacnet_module", os.path.join(HERE, "module.py"))
    m = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = m
    spec.loader.exec_module(m)
    return m


def test_trend_log_read_range_as_stock_serves_it():
    """Log_Buffer ReadRange, from captures of the stock wire (15.8.2): records by position, sequence and time, the more-items cut at one
    APDU (66 records of a log with status flags, 76 of one without), first sequence number only for sequence and time, errors for Count 0."""
    from bacpypes3.basetypes import Range, RangeByPosition, RangeBySequenceNumber, RangeByTime, DateTime
    from bacpypes3.constructeddata import Any
    m = load_module()
    hexes = lambda items: " ".join(bytes(t.encode().pduData).hex() for t in Any(m.SequenceOf(m.LogRecord)(items)).tagList)
    pos = lambda i, c: Range(byPosition=RangeByPosition(referenceIndex=i, count=c))
    seq = lambda i, c: Range(bySequenceNumber=RangeBySequenceNumber(referenceSequenceNumber=i, count=c))
    time = lambda y, mo, d, h, mi, c: Range(byTime=RangeByTime(referenceTime=DateTime(date=(y - 1900, mo, d, 255), time=(h, mi, 0, 0)), count=c))
    rr = lambda n, rng: m.log_range(n, rng, 1000, 9000)
    items, flags, first = rr(0, pos(1, 3))
    assert hexes(items) == ("0e a46d020107 b400000000 0f 1e 2c00000000 1f 2a0400 0e a46d020107 b4000f0000 0f 1e 2c3f800000 1f 2a0400 "
                            "0e a46d020107 b4001e0000 0f 1e 2c40000000 1f 2a0400") and flags == [True, False, False] and first is None
    assert hexes(rr(1, pos(1, 1))[0]) == "0e a46d030107 b400000000 0f 1e 2c447a0000 1f"          # odd logs: no status flags
    assert [len(rr(n, None)[0]) for n in (0, 1)] == [66, 76] and rr(0, None)[1] == [True, False, True]
    assert (len(m.log_range(0, None, 1000, 9000, 480)[0]), len(m.log_range(1, None, 1000, 9000, 1024)[0])) == (21, 53)   # the requester's maximum
    assert len(rr(0, pos(1, 66))[0]) == 66 and rr(0, pos(1, 66))[1] == [True, False, False]
    assert (len(rr(0, pos(1, 67))[0]), rr(0, pos(1, 67))[1]) == (66, [True, False, True])
    assert (len(rr(0, pos(1000, -5))[0]), rr(0, pos(1000, -5))[1]) == (5, [False, True, False])
    assert (len(rr(0, pos(999, 5))[0]), rr(0, pos(1001, 5))) == (2, ([], [False] * 3, None))
    assert len(rr(0, pos(1, -5))[0]) == 1
    items, flags, first = rr(0, seq(9500, -100))                      # the cut keeps the oldest of the range
    assert (len(items), first, flags) == (66, 9401, [False, False, True])
    assert rr(0, seq(9000, 3))[1:] == ([True, False, False], 9001) and len(rr(0, seq(9000, 3))[0]) == 2
    assert rr(0, seq(10000, 3))[1:] == ([False, True, False], 10000) and rr(0, seq(1, 3)) == ([], [False] * 3, None)
    assert rr(0, time(2009, 2, 1, 0, 0, 3))[2] == 9002 and rr(0, time(2009, 1, 1, 0, 0, 3))[2:] == (9001,)    # strictly newer
    assert rr(0, time(2009, 2, 1, 0, 0, -3)) == ([], [False] * 3, None) and rr(0, time(2009, 2, 1, 0, 15, -3))[2] == 9001
    assert (rr(0, time(2010, 1, 1, 0, 0, -3))[1:], len(rr(0, time(2010, 1, 1, 0, 0, -3))[0])) == (([False, True, False], 9998), 3)
    assert rr(0, time(2009, 2, 11, 9, 45, 3))[0] == []
    for bad in (pos(1, 0), pos(0, 3), seq(9001, 0), time(2009, 2, 1, 0, 0, 0)):
        try:
            rr(0, bad)
            assert False, bad
        except m.ExecutionError as e:
            assert (str(e.errorClass), str(e.errorCode)) == ("services", "other")


def test_held_port_is_an_error_not_ready():
    """bacpypes3 retries a failed bind forever without raising: the module waits for the endpoint, then says `error` (stock exits 1)."""
    import socket
    element, points = stock_server()
    port = next(PORTS)
    squatter = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    squatter.bind(("127.0.0.1", port))
    m = Module(port)
    try:
        m.hello(element, points)

        async def go():
            await m.expect("error", timeout=15)
        run(go())
        assert not [x for x in m.seen if x["t"] == "ready"]
        assert m.p.wait(5) == 0
    finally:
        m.close()
        squatter.close()


def test_out_of_service_and_priority_writes():
    """135-2012aw Table 12-2 and 135-2016bl 12.x.10: an input's Present_Value is writable only while Out_Of_Service is TRUE, and the
    feed leaves an out-of-service object alone; 15.9.1.3.1: priority 0 or 17 is services/parameter-out-of-range; priority 6 is
    write-access-denied on every commandable type, and nothing reaches the provider for it (stock's AO enqueues first)."""
    from bacpypes3.errors import ExecutionError
    from bacpypes3.primitivedata import Null, Real, Unsigned
    from bacpypes3.basetypes import BinaryPV
    m = load_module()
    sent = []
    m.write = lambda tag, value: sent.append((tag, value))
    m.ANALOG.update({0: ("ai-tag", False), 2: ("ao-tag", True)})
    m.BINARY.update({0: ("bi-tag", False), 2: ("bo-tag", True)})
    m.VALUES.update({"ai-tag": 250.5, "ao-tag": 3.0, "bi-tag": True, "bo-tag": True})

    async def code(coro):
        try:
            await coro
        except ExecutionError as e:
            return f"{e.errorClass}/{e.errorCode}"
        return "ack"

    async def go():
        objs = {str(o.objectIdentifier[0]) + "," + str(o.objectIdentifier[1]): o for o in m.build_objects()}
        class App:
            get_object_id = lambda self, oid: objs.get(f"{oid[0]},{oid[1]}")
        ai, bi, msi = objs["analog-input,0"], objs["binary-input,0"], objs["multi-state-input,0"]
        # inputs: PV denied while in service, accepted and enqueued while out of service, the feed does not overwrite it
        assert await code(ai.write_property("presentValue", Real(77.0))) == "property/writeAccessDenied"
        assert await code(bi.write_property("presentValue", BinaryPV("inactive"))) == "property/writeAccessDenied"
        assert await code(msi.write_property("presentValue", Unsigned(3))) == "property/writeAccessDenied"
        for o in (ai, bi, msi):
            assert await code(o.write_property("outOfService", True)) == "ack"
        assert await code(ai.write_property("presentValue", Real(77.0))) == "ack"
        assert await code(bi.write_property("presentValue", BinaryPV("inactive"))) == "ack"
        assert await code(msi.write_property("presentValue", Unsigned(3))) == "ack"
        assert ("ai-tag", 77.0) in sent and ("bi-tag", False) in sent
        m.refresh_once(App())
        assert float(ai.presentValue) == 77.0 and str(bi.presentValue) == "inactive"
        assert ai.statusFlags[3] == 1                                    # OUT_OF_SERVICE
        for o in (ai, bi):
            assert await code(o.write_property("outOfService", False)) == "ack"
        m.refresh_once(App())
        assert float(ai.presentValue) == 250.5 and str(bi.presentValue) == "active"
        assert await code(ai.write_property("units", 95)) == "ack" and await code(bi.write_property("polarity", "reverse")) == "ack"
        assert await code(ai.write_property("objectName", "x")) == "property/writeAccessDenied"
        # outputs: priority rules and Out_Of_Service
        sent.clear()
        ao, bo, bv, mso = objs["analog-output,2"], objs["binary-output,2"], objs["binary-value,0"], objs["multi-state-output,0"]
        for o, v in ((ao, Real(7.0)), (bo, BinaryPV("active")), (bv, BinaryPV("active")), (mso, Unsigned(2))):
            for prio in (0, 17):
                assert await code(o.write_property("presentValue", v, None, prio)) == "services/parameterOutOfRange", (o, prio)
            assert await code(o.write_property("presentValue", v, None, 6)) == "property/writeAccessDenied", o
            assert await code(o.write_property("presentValue", v, None, 7)) == "ack", o
            assert await code(o.write_property("presentValue", Null(()), None, 7)) == "ack", o
        assert [v for t, v in sent if t == "ao-tag"] == [7.0]            # the priority-7 write only: not 0, 17 or 6, nor the relinquish
        for o in (ao, bo):
            assert await code(o.write_property("outOfService", True)) == "ack"
            assert o.statusFlags[3] == 1
            assert await code(o.write_property("outOfService", False)) == "ack"
            assert await code(o.write_property("relinquishDefault", o.relinquishDefault)) == "property/writeAccessDenied"
        # an output's Present_Value written while out of service stays off the provider (135-2016bl: decoupled from the output)
        sent.clear()
        for o, v in ((ao, Real(9.0)), (bo, BinaryPV("inactive"))):
            assert await code(o.write_property("outOfService", True)) == "ack"
            assert await code(o.write_property("presentValue", v, None, 8)) == "ack"
            assert await code(o.write_property("presentValue", Null(()), None, 8)) == "ack"
            assert await code(o.write_property("outOfService", False)) == "ack"
        assert sent == []
        before = repr(ao.priorityArray[15])
        m.VALUES["ao-tag"] = 5.0
        ao.outOfService = True
        m.refresh_once(App())
        assert repr(ao.priorityArray[15]) == before                     # decoupled from the provider while out of service
        ao.outOfService = False
        m.refresh_once(App())
        assert repr(ao.priorityArray[15]) != before
    asyncio.run(go())
