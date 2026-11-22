"""module.py driven through stdin/stdout by a scripted device, with a bacpypes3 peer in the test (make modbac-test).
Everything runs on the loopback interface (BACNET_IFACE=lo), each module and peer on its own port."""
import asyncio, itertools, json, os, queue, re, subprocess, sys, threading

from bacpypes3.app import Application
from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.local.analog import AnalogInputObject
from bacpypes3.local.binary import BinaryOutputObject
from bacpypes3.pdu import Address

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
