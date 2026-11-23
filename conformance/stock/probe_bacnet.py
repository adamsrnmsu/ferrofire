"""BACnet/IP probe (bacpypes3 0.1.0 client) for the stock bacnet-server golden and for any server DUT.
Usage: probe_bacnet.py DUT_IP [MY_ADDR=0.0.0.0/16:47808]   (MY_ADDR: this host's address/prefix:port; the DUT answers Who-Is by
broadcast to port 47808, so the probe binds 47808 on its own host). Prints a deterministic transcript on stdout.
Objects follow conformance/stock/bacnet-server.xml: device 1234, BI/BO/AI/AO 0..3 (BI 0-1, BO 2-3, AI 0-1, AO 2-3)."""
import asyncio, sys
from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.app import Application
from bacpypes3.pdu import Address, GlobalBroadcast
from bacpypes3.apdu import (DeviceCommunicationControlRequest, SimpleAckPDU, ErrorPDU, RejectPDU, AbortPDU, ErrorRejectAbortNack)
from bacpypes3.primitivedata import Real, Enumerated, Unsigned, ObjectIdentifier, Integer, Boolean, Null
from bacpypes3.basetypes import BinaryPV

DUT = sys.argv[1]
ME = sys.argv[2] if len(sys.argv) > 2 else "0.0.0.0/16:47808"
OBJS = ["binary-input,0", "binary-input,1", "binary-output,2", "binary-output,3",
        "analog-input,0", "analog-input,1", "analog-output,2", "analog-output,3"]
OUTS = ["binary-output,2", "binary-output,3", "analog-output,2", "analog-output,3"]


def show(v):
    if hasattr(v, "errorClass"):   # Error
        return "ERROR class=%s code=%s" % (v.errorClass, v.errorCode)
    if isinstance(v, RejectPDU):
        return "REJECT reason=%s" % v.apduAbortRejectReason
    if isinstance(v, AbortPDU):
        return "ABORT reason=%s" % v.apduAbortRejectReason
    if type(v).__name__.startswith("Optional"):   # priority-array slot (Choice): null or a value
        for a in ("real", "binaryPV"):
            if getattr(v, a, None) is not None:
                return str(getattr(v, a))
        return "-"
    if isinstance(v, float):
        return "%.4f" % v
    if isinstance(v, (list, tuple)):
        return "[" + ", ".join(show(x) for x in v) + "]"
    return str(v)


async def rp(app, obj, prop, idx=None, t=3):
    try:
        r = await asyncio.wait_for(app.read_property(Address(DUT), obj, prop, idx), t)
    except asyncio.TimeoutError:
        r = "TIMEOUT"
    except ErrorRejectAbortNack as e:
        r = e
    except Exception as e:
        r = "EXC %s %s" % (type(e).__name__, e)
    print("RP  %s %s%s -> %s" % (obj, prop, "" if idx is None else "[%d]" % idx, r if isinstance(r, str) else show(r)))
    return r


async def rpm(app, spec, t=3):
    try:
        r = await asyncio.wait_for(app.read_property_multiple(Address(DUT), spec), t)
    except asyncio.TimeoutError:
        r = "TIMEOUT"
    except ErrorRejectAbortNack as e:
        r = e
    except Exception as e:
        r = "EXC %s %s" % (type(e).__name__, e)
    print("RPM %s -> %s" % (spec, r if isinstance(r, str) else show([tuple(show(x) for x in row) for row in r]) if isinstance(r, list) else show(r)))


async def rpm_all_raw(app, obj, t=3):
    """RPM all, element by element as sent: vendor properties have no type in the client, so print their tags."""
    from bacpypes3.apdu import ReadPropertyMultipleRequest, ReadPropertyMultipleACK
    from bacpypes3.constructeddata import Any
    from bacpypes3.basetypes import ReadAccessSpecification, PropertyReference
    req = ReadPropertyMultipleRequest(listOfReadAccessSpecifications=[ReadAccessSpecification(
        objectIdentifier=ObjectIdentifier(obj), listOfPropertyReferences=[PropertyReference(propertyIdentifier="all")])],
        destination=Address(DUT))
    try:
        ack = await asyncio.wait_for(app.request(req), t)
    except asyncio.TimeoutError:
        print("RPM-ALL %s -> TIMEOUT" % obj); return
    except ErrorRejectAbortNack as e:
        print("RPM-ALL %s -> %s" % (obj, show(e))); return
    if not isinstance(ack, ReadPropertyMultipleACK):
        print("RPM-ALL %s -> %s" % (obj, show(ack))); return
    for res in ack.listOfReadAccessResults:
        for el in res.listOfResults:
            rr = el.readResult
            if rr.propertyAccessError is not None:
                val = "ERROR class=%s code=%s" % (rr.propertyAccessError.errorClass, rr.propertyAccessError.errorCode)
            else:
                val = " ".join(bytes(tag.encode().pduData).hex() for tag in rr.propertyValue.tagList)
            print("RPM-ALL %s %s -> %s" % (obj, el.propertyIdentifier, val))


async def raw(app, label, req, t=3):
    try:
        r = await asyncio.wait_for(app.request(req), t)
    except asyncio.TimeoutError:
        print("%s -> TIMEOUT" % label); return
    except ErrorRejectAbortNack as e:
        print("%s -> %s" % (label, show(e))); return
    except Exception as e:
        print("%s -> EXC %s %s" % (label, type(e).__name__, e)); return
    if isinstance(r, SimpleAckPDU):
        print("%s -> ACK" % label)
    elif hasattr(r, "errorClass"):
        print("%s -> %s" % (label, show(r)))
    else:
        print("%s -> %s" % (label, type(r).__name__))
        return r


async def rpm_raw_one(app, obj, pid, t=3, quiet_unknown=False):
    """ReadProperty of a vendor property id, tags as sent."""
    from bacpypes3.apdu import ReadPropertyRequest
    from bacpypes3.basetypes import PropertyIdentifier
    req = ReadPropertyRequest(objectIdentifier=ObjectIdentifier(obj), propertyIdentifier=PropertyIdentifier(pid), destination=Address(DUT))
    try:
        ack = await asyncio.wait_for(app.request(req), t)
    except asyncio.TimeoutError:
        print("RP-RAW %s %d -> TIMEOUT" % (obj, pid)); return
    except ErrorRejectAbortNack as e:
        if not quiet_unknown or not (hasattr(e, "errorCode") and str(e.errorCode) == "unknown-property"):
            print("RP-RAW %s %d -> %s" % (obj, pid, show(e)))
        return
    except Exception as e:
        print("RP-RAW %s %d -> EXC %s %s" % (obj, pid, type(e).__name__, e)); return
    if hasattr(ack, "errorClass"):
        if not quiet_unknown or not (str(ack.errorClass) == "property" and str(ack.errorCode) == "unknown-property"):
            print("RP-RAW %s %d -> %s" % (obj, pid, show(ack)))
        return
    print("RP-RAW %s %d -> %s" % (obj, pid, " ".join(bytes(t.encode().pduData).hex() for t in ack.propertyValue.tagList)))


async def wp(app, obj, prop, val, prio=None, t=3):
    try:
        r = await asyncio.wait_for(app.write_property(Address(DUT), obj, prop, val, priority=prio), t)
    except asyncio.TimeoutError:
        r = "TIMEOUT"
    except ErrorRejectAbortNack as e:
        r = e
    except Exception as e:
        r = "EXC %s %s" % (type(e).__name__, e)
    print("WP  %s %s=%s prio=%s -> %s" % (obj, prop, val, prio, "ACK" if r is None else (r if isinstance(r, str) else show(r))))


async def whois_unicast(app, tag):
    try:
        res = await asyncio.wait_for(app.who_is(None, None, Address(DUT), timeout=1.5), 4.0)
    except asyncio.TimeoutError:
        res = []
    print("WHO-IS (%s) -> %s" % (tag, "I-Am" if res else "(no I-Am)"))


async def object_list(app):
    """The whole object-list, element by element (a one-shot read aborts on stock: no segmentation), as runs of one type."""
    n = await rp(app, "device,1234", "object-list", 0)
    runs = []
    for i in range(1, n + 1):
        r = await asyncio.wait_for(app.read_property(Address(DUT), "device,1234", "object-list", i), 3)
        t, inst = r[0], r[1]
        if runs and runs[-1][0] == t and runs[-1][2] == inst - 1:
            runs[-1][2] = inst
        else:
            runs.append([t, inst, inst])
    print("OBJECT-LIST count=%d runs=%s" % (n, "; ".join("%s %d..%d" % tuple(x) for x in runs)))


async def dcc(app, mode, duration=None, password=None, t=3):
    req = DeviceCommunicationControlRequest(enableDisable=mode, destination=Address(DUT))
    if duration is not None:
        req.timeDuration = duration
    if password is not None:
        req.password = password
    tag = "DCC %s duration=%s pw=%s ->" % (mode, duration, password)
    try:
        r = await asyncio.wait_for(app.request(req), t)
    except asyncio.TimeoutError:
        print(tag, "TIMEOUT"); return
    except ErrorRejectAbortNack as e:
        print(tag, show(e)); return
    except Exception as e:
        print(tag, "EXC %s %s" % (type(e).__name__, e)); return
    print(tag, "ACK" if isinstance(r, SimpleAckPDU) else show(r) if r is not None else "None")


async def main():
    sys.argv[1:] = ["--name", "probe", "--instance", "599", "--address", ME]
    args = SimpleArgumentParser().parse_args()
    app = Application.from_args(args)
    await asyncio.sleep(1)

    print("## Who-Is")
    for label, lo, hi, dst in [("global broadcast, no range", None, None, GlobalBroadcast()),
                               ("range 1234..1234", 1234, 1234, GlobalBroadcast()),
                               ("range 0..1000 (miss)", 0, 1000, GlobalBroadcast()),
                               ("unicast no range", None, None, Address(DUT))]:
        try:
            res = await asyncio.wait_for(app.who_is(lo, hi, dst, timeout=1.5), 4.0)
        except asyncio.TimeoutError:
            res = "TIMEOUT"
        if isinstance(res, str):
            print("WHO-IS %s -> %s" % (label, res))
        else:
            for i in res:
                print("WHO-IS %s -> I-Am device=%s maxAPDU=%s segmentation=%s vendor=%s" % (
                    label, i.iAmDeviceIdentifier, i.maxAPDULengthAccepted, i.segmentationSupported, i.vendorID))
        if not res and not isinstance(res, str):
            print("WHO-IS %s -> (no I-Am)" % label)

    dev = "device,1234"
    print("## Device object")
    for p in ["object-identifier", "object-name", "object-type", "vendor-identifier", "vendor-name", "model-name",
              "protocol-version", "protocol-revision", "protocol-services-supported", "protocol-object-types-supported",
              "max-apdu-length-accepted", "segmentation-supported", "system-status", "application-software-version",
              "firmware-revision", "description", "location", "database-revision", "number-of-apdu-retries",
              "apdu-timeout", "device-address-binding"]:
        await rp(app, dev, p)
    await rp(app, dev, "object-list")
    await rp(app, dev, "object-list", 0)
    await rp(app, dev, "object-list", 1)
    await rp(app, dev, "object-list", 9)
    await rp(app, dev, "object-list", 99999)
    await rp(app, dev, "high-limit")
    await object_list(app)
    await rpm(app, [(dev, ["object-name", "vendor-identifier", "system-status"])])
    await rpm(app, [(dev, ["all"])])
    await rpm(app, [(dev, ["required"])])

    print("## Configured objects")
    for o in OBJS:
        for p in ["object-identifier", "object-name", "object-type", "present-value", "status-flags", "description",
                  "units", "out-of-service", "event-state", "reliability", "polarity", "priority-array",
                  "relinquish-default", "active-text", "inactive-text", "cov-increment", "number-of-states"]:
            await rp(app, o, p)
    for o in OBJS:
        await rpm(app, [(o, ["object-name", "present-value", "status-flags"])])
    await rpm(app, [(o, ["present-value", "status-flags"]) for o in OBJS])
    for o in ["analog-input,0", "analog-input,1", "analog-input,500", "analog-output,2", "binary-input,0", "binary-output,2",
              "analog-value,0", "binary-value,0", "multi-state-input,0", "multi-state-output,0", "multi-state-value,0",
              "positive-integer-value,0", "schedule,0", "trend-log,0", "characterstring-value,0"]:
        await rpm_all_raw(app, o)
    await rpm(app, [("analog-output,2", ["present-value"]), ("analog-value,5", ["present-value"]),
                    ("analog-output,3", ["high-limit"])])

    print("## Property sweep (every property id 0..260, tags as sent)")
    for o in ["device,1234", "analog-input,0", "analog-output,2", "binary-input,0", "binary-output,2", "analog-input,500",
              "analog-value,0", "binary-value,0", "multi-state-input,0", "multi-state-output,0", "multi-state-value,0",
              "positive-integer-value,0", "schedule,0", "trend-log,0", "characterstring-value,0", "command,0", "integer-value,1",
              "life-safety-point,0", "load-control,0", "lighting-output,1", "channel,1", "octetstring-value,0"]:
        for pid in [*range(261), 371, 431]:     # 371 property-list, 431 current-command-priority
            if o == "device,1234" and pid in (56, 57):      # local-date, local-time: move with the clock
                continue
            await rpm_raw_one(app, o, pid, t=2, quiet_unknown=True)

    print("## Unknown objects and properties")
    for o in ["analog-input,500", "analog-input,999", "analog-input,1000", "analog-value,0", "binary-value,0",
              "multi-state-input,0", "device,9999"]:
        await rp(app, o, "present-value")
    await rp(app, "analog-input,0", "high-limit")
    await rp(app, "analog-input,0", "present-value", 1)
    await rp(app, "analog-input,0", "object-list")
    await rpm(app, [("analog-input,500", ["present-value"])])
    await rpm(app, [("analog-input,0", ["present-value", "high-limit"])])

    print("## WriteProperty")
    for o in OUTS:
        print("-- before:")
        await rp(app, o, "present-value")
    await wp(app, "binary-output,2", "present-value", BinaryPV("active"), 8)
    await rp(app, "binary-output,2", "present-value")
    await rp(app, "binary-output,2", "priority-array")
    await wp(app, "binary-output,3", "present-value", BinaryPV("active"))
    await rp(app, "binary-output,3", "present-value")
    await wp(app, "analog-output,2", "present-value", Real(42.5), 8)
    await rp(app, "analog-output,2", "present-value")
    await rp(app, "analog-output,2", "priority-array")
    await wp(app, "analog-output,3", "present-value", Real(7.25))
    await rp(app, "analog-output,3", "present-value")
    await wp(app, "analog-output,3", "present-value", Real(9.5), 16)
    await rp(app, "analog-output,3", "present-value")
    print("-- never-configured outputs:")
    for o in ["binary-output,500", "analog-output,500"]:
        await rp(app, o, "present-value")
        await rp(app, o, "priority-array")
    print("-- 2.5 s later (1 Hz refresh from the datastore):")
    await asyncio.sleep(2.5)
    for o in OUTS:
        await rp(app, o, "present-value")
    print("-- writes to read-only and unknown targets:")
    await wp(app, "analog-input,0", "present-value", Real(1.0))
    await wp(app, "binary-input,0", "present-value", BinaryPV("active"))
    await wp(app, "analog-output,2", "object-name", "renamed")
    await wp(app, "analog-output,2", "high-limit", Real(1.0))
    await wp(app, "analog-output,2", "present-value", BinaryPV("active"), 8)
    await wp(app, "analog-value,0", "present-value", Real(1.0))
    await wp(app, "analog-output,500", "present-value", Real(1.0), 8)
    await wp(app, "binary-output,2", "present-value", BinaryPV("active"), 16)
    await rp(app, "binary-output,2", "present-value")
    await rp(app, "binary-output,2", "priority-array")
    await wp(app, "analog-output,2", "present-value", Real(5.0), 0)
    await wp(app, "analog-output,2", "present-value", Real(5.0), 17)
    await rp(app, "analog-output,2", "priority-array")
    await asyncio.sleep(3.0)    # stock forwards writes to the provider in batches on its own clock: settle so each block is one batch
    print("-- AO range (stock keeps whole numbers):")
    for v, pr in [(300.7, 9), (-5.0, 10), (255.9, 11), (70000.0, 12)]:
        await wp(app, "analog-output,3", "present-value", Real(v), pr)
    await rp(app, "analog-output,3", "priority-array")
    print("-- relinquish (null) and after:")
    from bacpypes3.primitivedata import Null
    await wp(app, "analog-output,2", "present-value", Null(()), 8)
    await rp(app, "analog-output,2", "present-value")

    await asyncio.sleep(3.0)    # stock forwards writes to the provider in batches on its own clock: settle so each block is one batch
    print("## Writes to the other object types")
    from bacpypes3.primitivedata import Unsigned as U
    for oid, val, prio in [("analog-value,1", Real(2.5), None), ("analog-value,2", Real(2.5), 8), ("binary-value,0", BinaryPV("active"), None),
                           ("binary-value,1", BinaryPV("active"), 8), ("multi-state-value,0", U(2), None), ("multi-state-input,0", U(3), None),
                           ("multi-state-output,0", U(2), 8), ("positive-integer-value,0", U(5), None),
                           ("characterstring-value,0", "hello", None), ("integer-value,1", Integer(-4), None)]:
        await wp(app, oid, "present-value", val, prio)
        await rp(app, oid, "present-value")

    print("## WritePropertyMultiple and ReadRange")
    from bacpypes3.apdu import WritePropertyMultipleRequest, ReadRangeRequest, Range
    from bacpypes3.basetypes import WriteAccessSpecification, PropertyValue, RangeByPosition
    from bacpypes3.constructeddata import Any
    for label, oid, val, prio in [("WPM analog-output,3 present-value=12.5 prio 9", "analog-output,3", Real(12.5), 9),
                                  ("WPM analog-input,0 present-value=1.0", "analog-input,0", Real(1.0), None),
                                  ("WPM analog-output,500 present-value=3.0 prio 9", "analog-output,500", Real(3.0), 9),
                                  ("WPM analog-value,9 present-value=3.0", "analog-value,9", Real(3.0), None)]:
        pv = PropertyValue(propertyIdentifier="present-value", propertyValue=Any(val), priority=prio)
        await raw(app, label, WritePropertyMultipleRequest(listOfWriteAccessSpecifications=[WriteAccessSpecification(
            objectIdentifier=ObjectIdentifier(oid), listOfProperties=[pv])], destination=Address(DUT)))
    for label, oids, props in [("WPM two objects, second unknown", ["analog-output,3", "analog-output,5000"], ["present-value"]),
                               ("WPM unknown property", ["analog-output,3"], ["high-limit"])]:
        specs = []
        for oid in oids:
            pvs = [PropertyValue(propertyIdentifier=pr, propertyValue=Any(Real(1.0)), priority=9) for pr in props]
            specs.append(WriteAccessSpecification(objectIdentifier=ObjectIdentifier(oid), listOfProperties=pvs))
        await raw(app, label, WritePropertyMultipleRequest(listOfWriteAccessSpecifications=specs, destination=Address(DUT)))
    await rp(app, "analog-output,3", "priority-array")
    await wp(app, "analog-output,3", "present-value", Null(()), 9)
    await asyncio.sleep(3.0)    # stock forwards writes to the provider in batches on its own clock: settle so each block is one batch
    from bacpypes3.basetypes import RangeBySequenceNumber, RangeByTime, DateTime
    from bacpypes3.primitivedata import Date, Time
    import hashlib
    P = lambda i, c: Range(byPosition=RangeByPosition(referenceIndex=i, count=c))
    S = lambda i, c: Range(bySequenceNumber=RangeBySequenceNumber(referenceSequenceNumber=i, count=c))
    T = lambda y, mo, d, h, mi, c: Range(byTime=RangeByTime(referenceTime=DateTime(date=Date((y - 1900, mo, d, 255)), time=Time((h, mi, 0, 0))), count=c))
    TL, LB = "trend-log,0", "log-buffer"
    app.device_object.maxApduLengthAccepted = 1476      # the requester's maximum: 15.8 cuts a reply at it, stock at 1476 whatever it is
    # trend-log,0 holds 1000 records, sequence numbers 9001..10000, 15 minutes apart from 2009-02-01 00:00; log 1 has no status flags
    for label, oid, prop, ref in [("ReadRange device object-list 1..3", "device,1234", "object-list", P(1, 3)),
                                  ("ReadRange analog-input,0 present-value", "analog-input,0", "present-value", None),
                                  ("ReadRange trend-log,0 log-buffer 1..3", TL, LB, P(1, 3)),
                                  ("ReadRange trend-log,1 log-buffer 1..3", "trend-log,1", LB, P(1, 3)),
                                  ("ReadRange trend-log,7 log-buffer 1..3", "trend-log,7", LB, P(1, 3)),
                                  ("ReadRange trend-log,0 log-buffer no range", TL, LB, None),
                                  ("ReadRange trend-log,1 log-buffer no range", "trend-log,1", LB, None),
                                  ("ReadRange trend-log,0 log-buffer position 1 count 66", TL, LB, P(1, 66)),
                                  ("ReadRange trend-log,0 log-buffer position 1 count 67", TL, LB, P(1, 67)),
                                  ("ReadRange trend-log,0 log-buffer position 500 count 5", TL, LB, P(500, 5)),
                                  ("ReadRange trend-log,0 log-buffer position 10 count -3", TL, LB, P(10, -3)),
                                  ("ReadRange trend-log,0 log-buffer position 1 count -5", TL, LB, P(1, -5)),
                                  ("ReadRange trend-log,0 log-buffer position 999 count 5", TL, LB, P(999, 5)),
                                  ("ReadRange trend-log,0 log-buffer position 1000 count -5", TL, LB, P(1000, -5)),
                                  ("ReadRange trend-log,0 log-buffer position 1001 count 5", TL, LB, P(1001, 5)),
                                  ("ReadRange trend-log,0 log-buffer position 0 count 3", TL, LB, P(0, 3)),
                                  ("ReadRange trend-log,0 log-buffer position 1 count 0", TL, LB, P(1, 0)),
                                  ("ReadRange trend-log,0 log-buffer sequence 9001 count 3", TL, LB, S(9001, 3)),
                                  ("ReadRange trend-log,0 log-buffer sequence 9000 count 3", TL, LB, S(9000, 3)),
                                  ("ReadRange trend-log,0 log-buffer sequence 9500 count -100", TL, LB, S(9500, -100)),
                                  ("ReadRange trend-log,0 log-buffer sequence 9999 count 5", TL, LB, S(9999, 5)),
                                  ("ReadRange trend-log,0 log-buffer sequence 10000 count -3", TL, LB, S(10000, -3)),
                                  ("ReadRange trend-log,0 log-buffer sequence 1 count 3", TL, LB, S(1, 3)),
                                  ("ReadRange trend-log,0 log-buffer sequence 9001 count 0", TL, LB, S(9001, 0)),
                                  ("ReadRange trend-log,0 log-buffer time 2009-01-01 count 3", TL, LB, T(2009, 1, 1, 0, 0, 3)),
                                  ("ReadRange trend-log,0 log-buffer time 2009-02-01 00:00 count 3", TL, LB, T(2009, 2, 1, 0, 0, 3)),
                                  ("ReadRange trend-log,0 log-buffer time 2009-02-01 00:00 count -3", TL, LB, T(2009, 2, 1, 0, 0, -3)),
                                  ("ReadRange trend-log,0 log-buffer time 2009-02-01 00:22 count 3", TL, LB, T(2009, 2, 1, 0, 22, 3)),
                                  ("ReadRange trend-log,0 log-buffer time 2009-02-01 00:22 count -3", TL, LB, T(2009, 2, 1, 0, 22, -3)),
                                  ("ReadRange trend-log,0 log-buffer time 2010-01-01 count -3", TL, LB, T(2010, 1, 1, 0, 0, -3)),
                                  ("ReadRange trend-log,0 log-buffer time 2010-01-01 count 3", TL, LB, T(2010, 1, 1, 0, 0, 3)),
                                  ("ReadRange trend-log,0 log-buffer time 2009-02-01 00:00 count 0", TL, LB, T(2009, 2, 1, 0, 0, 0)),
                                  ("ReadRange trend-log,0 object-name", TL, "object-name", P(1, 3)),
                                  ("ReadRange trend-log,0 high-limit", TL, "high-limit", P(1, 3)),
                                  ("ReadRange trend-log,8 log-buffer", "trend-log,8", LB, P(1, 3)),
                                  ("ReadRange analog-input,5000 present-value", "analog-input,5000", "present-value", None),
                                  ("ReadRange analog-input,0 high-limit", "analog-input,0", "high-limit", None)]:
        req = ReadRangeRequest(objectIdentifier=ObjectIdentifier(oid), propertyIdentifier=prop, destination=Address(DUT))
        if ref is not None:
            req.range = ref
        r = await raw(app, label, req)
        if r is not None and hasattr(r, "itemCount"):
            data = " ".join(bytes(t.encode().pduData).hex() for t in r.itemData.tagList)
            print("%s    itemCount=%s first-seq=%s flags=%s data=%s" % (label, r.itemCount, r.firstSequenceNumber, list(r.resultFlags),
                  data if r.itemCount <= 5 else "%d bytes sha1 %s" % (len(data.split()), hashlib.sha1(data.encode()).hexdigest()[:12])))
    await rp(app, TL, LB)
    for apdu in (480, 1024):
        app.device_object.maxApduLengthAccepted = apdu
        for label, oid in [("trend-log,0", TL), ("trend-log,1", "trend-log,1")]:
            r = await raw(app, "ReadRange %s log-buffer no range, requester accepts %d" % (label, apdu),
                          ReadRangeRequest(objectIdentifier=ObjectIdentifier(oid), propertyIdentifier=LB, destination=Address(DUT)))
            if r is not None and hasattr(r, "itemCount"):
                print("    itemCount=%s flags=%s" % (r.itemCount, list(r.resultFlags)))
    app.device_object.maxApduLengthAccepted = 1024
    print("## Vendor properties")
    for pid in (9997, 9998, 9999):
        await rpm_raw_one(app, "analog-input,0", pid)
    await rpm_raw_one(app, "analog-output,2", 9997)

    print("## Unsupported services")
    for name, req in [("ReinitializeDevice", None)]:
        pass
    from bacpypes3.apdu import ReinitializeDeviceRequest, ReadRangeRequest, SubscribeCOVRequest
    from bacpypes3.apdu import ConfirmedCOVNotificationRequest
    from bacpypes3.basetypes import PropertyValue as PV_
    from bacpypes3.constructeddata import Any as Any_
    for label, req in [("ConfirmedCOVNotification", ConfirmedCOVNotificationRequest(
                           subscriberProcessIdentifier=1, initiatingDeviceIdentifier=ObjectIdentifier("device,599"),
                           monitoredObjectIdentifier=ObjectIdentifier("analog-input,0"), timeRemaining=60,
                           listOfValues=[PV_(propertyIdentifier="present-value", propertyValue=Any_(Real(1.0)))], destination=Address(DUT))),
                       ("ReinitializeDevice", ReinitializeDeviceRequest(reinitializedStateOfDevice="warmstart", destination=Address(DUT))),
                       ("SubscribeCOV", SubscribeCOVRequest(subscriberProcessIdentifier=1, monitoredObjectIdentifier=ObjectIdentifier("analog-input,0"),
                                                            issueConfirmedNotifications=False, lifetime=60, destination=Address(DUT)))]:
        try:
            r = await asyncio.wait_for(app.request(req), 3)
            print("%s -> %s %s" % (label, type(r).__name__, getattr(r, "errorClass", "") if isinstance(r, ErrorPDU) else getattr(r, "apduAbortRejectReason", "")))
            if isinstance(r, ErrorPDU):
                print("%s    code=%s" % (label, r.errorCode))
        except asyncio.TimeoutError:
            print("%s -> TIMEOUT" % label)
        except ErrorRejectAbortNack as e:
            print("%s -> %s" % (label, show(e)))
        except Exception as e:
            print("%s -> EXC %s %s" % (label, type(e).__name__, e))

    print("## DeviceCommunicationControl")
    await asyncio.sleep(8.0)    # past the 2 s a device may hold a value a provider ACKed (ww-1df.32): the read below is then the same everywhere
    await dcc(app, "enable")
    await dcc(app, "disable", password="wrong")
    await dcc(app, "enable", password="filister")
    await rp(app, "analog-input,0", "present-value")
    await dcc(app, "disable", password="filister")
    await rp(app, "analog-input,0", "present-value", t=2)
    await rp(app, "device,1234", "object-name", t=2)
    await whois_unicast(app, "disabled")
    await dcc(app, "enable", password="filister")
    await rp(app, "analog-input,0", "present-value")
    await dcc(app, "disable-initiation", password="filister")
    await rp(app, "analog-input,0", "present-value")
    await whois_unicast(app, "disable-initiation")
    await dcc(app, "disable", duration=1, password="filister")
    await rp(app, "analog-input,0", "present-value", t=2)
    await asyncio.sleep(float(__import__("os").environ.get("DCC_WAIT", "70")))
    await rp(app, "analog-input,0", "present-value", t=2)
    await dcc(app, "enable", password="filister")
    print("## Out_Of_Service, priority 6 and other writes (135-2016bl 12.x.10, 15.9.1.3.1; stock's answers are the reference where the spec is silent)")
    T_, F_ = Boolean(True), Boolean(False)
    for oid, pv in [("analog-input,0", Real(77.0)), ("binary-input,0", BinaryPV("inactive"))]:
        await wp(app, oid, "present-value", pv)              # out of service clear
        await asyncio.sleep(2.5)
        await wp(app, oid, "out-of-service", T_)
        await rp(app, oid, "out-of-service")
        await wp(app, oid, "present-value", pv)
        await rp(app, oid, "present-value")
        await asyncio.sleep(2.0)                              # past two 1 Hz refreshes: does the feed overwrite the written value?
        await rp(app, oid, "present-value")
        await wp(app, oid, "out-of-service", F_)
        await asyncio.sleep(2.5)
    for oid, pv in [("analog-input,1", Real(5.0)), ("binary-input,1", BinaryPV("inactive"))]:
        await wp(app, oid, "out-of-service", T_)
        await wp(app, oid, "out-of-service", F_)
    for oid in ["multi-state-input,0", "analog-value,0", "binary-value,0"]:
        await wp(app, oid, "out-of-service", T_)
        await rp(app, oid, "out-of-service")
        await wp(app, oid, "out-of-service", F_)
    await wp(app, "multi-state-input,0", "present-value", Unsigned(3))
    await wp(app, "analog-input,0", "units", Enumerated(95))
    await wp(app, "analog-input,0", "cov-increment", Real(2.0))
    await wp(app, "binary-input,0", "polarity", Enumerated(1))
    print("-- priority 6, relinquish-default, out-of-service, polarity on the commandable objects:")
    for oid, pv in [("binary-output,3", BinaryPV("active")), ("analog-output,3", Real(7.0)), ("binary-value,0", BinaryPV("active")),
                    ("multi-state-output,0", Unsigned(2))]:
        await wp(app, oid, "present-value", pv, 6)
        await asyncio.sleep(2.5)                              # stock batches what it forwards to the provider on its own clock: one write per batch
        await rp(app, oid, "priority-array")
        await wp(app, oid, "present-value", pv, 7)
        await asyncio.sleep(2.5)
        await wp(app, oid, "present-value", Null(()), 7)
        await wp(app, oid, "present-value", pv, 0)
        await wp(app, oid, "present-value", pv, 17)
    for oid, rd in [("analog-output,3", Real(3.0)), ("binary-output,3", BinaryPV("active")), ("binary-value,0", BinaryPV("active")),
                    ("multi-state-output,0", Unsigned(2))]:
        await wp(app, oid, "relinquish-default", rd)
        await rp(app, oid, "relinquish-default")
    for oid in ["analog-output,3", "binary-output,3"]:
        await wp(app, oid, "out-of-service", T_)
        await rp(app, oid, "out-of-service")
        await wp(app, oid, "present-value", Real(9.0) if oid.startswith("analog") else BinaryPV("active"), 9)
        await asyncio.sleep(2.5)
        await rp(app, oid, "present-value")
        await wp(app, oid, "out-of-service", F_)
    await wp(app, "binary-output,3", "polarity", Enumerated(1))
    await rp(app, "binary-output,3", "polarity")
    await wp(app, "binary-output,3", "polarity", Enumerated(0))
    await wp(app, "analog-output,3", "units", Enumerated(95))
    await wp(app, "analog-output,3", "present-value", Null(()), 9)
    await wp(app, "binary-output,3", "present-value", Null(()), 9)
    await wp(app, "analog-output,3", "relinquish-default", Real(0.0))
    await wp(app, "binary-output,3", "relinquish-default", BinaryPV("inactive"))
    await asyncio.sleep(2.0)
    for o in ["analog-input,0", "binary-input,0", "analog-output,3", "binary-output,3"]:
        await rp(app, o, "present-value")
    print("## done")
    app.close()

asyncio.run(main())
