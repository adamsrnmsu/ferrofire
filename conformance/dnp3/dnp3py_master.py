# dnp3py 0.6.0 (PNNL, pure Python, MIT): a DNP3 master written independently of opendnp3. The device answers only its configured master
# (address 1), so check.sh picks this one out of the capture by TCP stream (it runs after ff-dnp3-master, so it is stream 1). dnp3py does not expose control statuses; check.sh reads them from the wire (tshark).
import asyncio, sys
from dnp3.master import Master, MasterConfig, MasterTcpRunner
from dnp3.master.commands import CommandBuilder
from dnp3.master.handler import DefaultSOEHandler

bad = []
def want(ok, what):
    print(("ok: " if ok else "FAIL: ") + what)
    if not ok: bad.append(what)

async def main():
    h = DefaultSOEHandler()
    cfg = MasterConfig(address=1, outstation_address=10, enable_unsolicited_on_startup=False, startup_integrity_poll=False)
    m = Master(config=cfg, handler=h)
    async with MasterTcpRunner(master=m, host="127.0.0.1", port=20000, response_timeout=5.0) as r:
        res = await r.integrity_poll()
        want(bool(res) and all(x.function.name == "RESPONSE" for x in res), "dnp3py: integrity poll answered with RESPONSE")
        want(set(h.binary_inputs) == {2, 5}, f"dnp3py: binary input points {sorted(h.binary_inputs)} == [2, 5]")
        want(set(h.analog_inputs) == {0, 1, 2}, f"dnp3py: analog input points {sorted(h.analog_inputs)} == [0, 1, 2]")
        async def direct(build):
            b = CommandBuilder(); build(b)
            return await r.request(m.build_direct_operate(b.build_direct_operate()))
        out = await direct(lambda b: b.add_analog(2, 12.5))
        want(out[0].function.name == "RESPONSE", "dnp3py: direct operate analog idx 2 answered")
        b = CommandBuilder(); b.add_analog(1, 7.0)
        out = await r.request(m.build_select(b.build_select()))
        want(out[0].function.name == "RESPONSE", "dnp3py: select analog idx 1 answered")
        out = await r.request(m.build_operate(b.build_operate()))
        want(out[0].function.name == "RESPONSE", "dnp3py: operate analog idx 1 answered")
        out = await direct(lambda b: b.latch_on(5))
        want(out[0].function.name == "RESPONSE", "dnp3py: direct operate CROB idx 5 answered")
        out = await r.class_poll(class_1=True, class_2=True, class_3=True)
        want(bool(out) and out[0].function.name == "RESPONSE", "dnp3py: class poll answered")

asyncio.run(main())
sys.exit(1 if bad else 0)
