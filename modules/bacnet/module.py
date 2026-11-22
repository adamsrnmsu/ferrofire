"""ferrofire's BACnet/IP module: a stock <bacnet-server> or <bacnet-client> (protocols plan rows 6 and 11) over ffmod/1 on stdio,
on bacpypes3 0.1.0. The server grew from the Task 0b spike (spike/NOTES.md): an override layer over 0.1.0's defects that gives a
client what stock bennu-field-device gives (conformance/bacnet holds it to stock). stdout carries ffmod/1 only; everything else
goes to stderr.
Transport as stock (bacnet-stack dlenv): interface BACNET_IFACE (eth0), port BACNET_IP_PORT (47808), BACNET_BBMD_ADDRESS/_PORT/
_TIMETOLIVE register as a foreign device; the <endpoint> ip and port are parsed and discarded, as stock does.
"""
import asyncio, fcntl, ipaddress, itertools, json, logging, os, socket, struct, sys, threading
import xml.etree.ElementTree as ET

from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.app import Application
from bacpypes3.basetypes import PropertyIdentifier, BinaryPV, PriorityValue
from bacpypes3.apdu import AbortPDU, ErrorRejectAbortNack, ReadPropertyACK, ReadRangeACK, SimpleAckPDU, WritePropertyMultipleError
from bacpypes3.errors import AbortOther, ExecutionError, PropertyError
from bacpypes3.pdu import Address
from bacpypes3.service.device import DeviceCommunicationControlServices
from bacpypes3.local.analog import AnalogInputObject, AnalogOutputObject, AnalogValueObject
from bacpypes3.local.binary import BinaryInputObject, BinaryOutputObject, BinaryValueObject
from bacpypes3.local.object import Object as LocalObject
from bacpypes3.object import (CharacterStringValueObject, CommandObject, IntegerValueObject, LifeSafetyPointObject,
                              LoadControlObject, MultiStateInputObject, MultiStateOutputObject, MultiStateValueObject,
                              TrendLogObject, LightingOutputObject, ChannelObject, OctetStringValueObject,
                              PositiveIntegerValueObject, ScheduleObject)
import bacpypes3.service.object as svc_object
from bacpypes3.basetypes import ErrorType, ResultFlags, ObjectPropertyReference, PropertyReference
from bacpypes3.constructeddata import Any
from bacpypes3.primitivedata import BitString, Integer, Null, ObjectIdentifier, Real, Unsigned

FFMOD = sys.stdout
sys.stdout = sys.stderr                          # a stray print from a library must not land in the ffmod/1 stream
IDS = itertools.count(1)
KINDS = ("binary-input", "binary-output", "analog-input", "analog-output")    # stock's parse order
TIMEOUT = 3.0                                    # one client request, retries included (bacpypes3's own APDU timer is 3 s)


def say(**m):
    FFMOD.write(json.dumps(m) + "\n")
    FFMOD.flush()


def write(tag, value):
    say(t="write", id=next(IDS), tag=tag, value=value)


def log(*a):
    print(*a, file=sys.stderr, flush=True)


# ---- config (DataHandler.cpp: what stock refuses at load is an error here)

class ConfigError(Exception):
    pass


def need(el, key):
    v = el.findtext(key)
    if v is None:
        raise ConfigError(f"<{el.tag}> has no <{key}>")
    return v.strip()


def number(el, key):
    try:
        return int(need(el, key))
    except ValueError:
        raise ConfigError(f"<{el.tag}><{key}> is not a number")


def udp(endpoint):
    """Stock looks for udp:// anywhere in the endpoint and refuses the element without it."""
    i = endpoint.find("udp://")
    if i < 0:
        raise ConfigError(f"Unknown endpoint protocol ({endpoint})")
    return endpoint[i + 6:]


def points(el):
    """(object type, instance, tag) of each point, in stock's order."""
    return [(kind, number(p, "address"), need(p, "tag")) for kind in KINDS for p in el.findall(kind)]


def use(kind, tag, peer_side):
    """`peer_side`: the module's peer supplies the value."""
    return {"tag": tag, "kind": kind.split("-")[0], "dir": "in" if peer_side else "out"}


def local_address():
    """bacnet-stack's: the IPv4 address and netmask of BACNET_IFACE (eth0) at BACNET_IP_PORT (47808), as bacpypes3 spells it."""
    iface = os.environ.get("BACNET_IFACE", "eth0")
    port = int(os.environ.get("BACNET_IP_PORT", "47808"), 0)
    req = struct.pack("256s", iface.encode()[:15])
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            ip = socket.inet_ntoa(fcntl.ioctl(s.fileno(), 0x8915, req)[20:24])       # SIOCGIFADDR
            mask = socket.inet_ntoa(fcntl.ioctl(s.fileno(), 0x891B, req)[20:24])     # SIOCGIFNETMASK
    except OSError as e:
        raise ConfigError(f"BACnet interface {iface}: {e}")
    return f"{ip}/{ipaddress.IPv4Network('0.0.0.0/' + mask).prefixlen}:{port}"


def app_args(instance):
    a = ["--name", "SimpleServer", "--instance", str(instance), "--address", local_address()]
    bbmd = os.environ.get("BACNET_BBMD_ADDRESS")
    if bbmd:
        a += ["--foreign", f"{bbmd}:{os.environ.get('BACNET_BBMD_PORT', '47808')}",
              "--ttl", os.environ.get("BACNET_BBMD_TIMETOLIVE", "60000")]
    return SimpleArgumentParser().parse_args(a)


# ---- server (Server.cpp and the bacnet-stack demo device; spike/NOTES.md lists what is stock and what differs)

PRIO_REFRESH_BO, PRIO_REFRESH_AO = 1, 16     # stock: refresh writes BO at priority "0" (array slot 1), AO at 16
VALUES = {}                                     # tag -> bool | float: the module's copy of the device, from hello and values
BINARY, ANALOG = {}, {}                         # stock's point maps: instance -> (tag, is output), BI and BO share one, AI and AO the other

# RPM "all" as stock answers it, in its order (from the golden's raw RPM-ALL blocks); 9997-9999 are the stack's vendor properties
COMMON = ["objectIdentifier", "objectName", "objectType", "presentValue", "statusFlags", "eventState", "outOfService"]
STOCK_ALL = {
    "analog-input": COMMON + ["units", "description", "reliability", "covIncrement", 9997, 9998, 9999],
    "analog-output": COMMON + ["units", "priorityArray", "relinquishDefault"],
    "binary-input": COMMON + ["polarity", "description"],
    "binary-output": COMMON + ["polarity", "priorityArray", "relinquishDefault", "description", "activeText", "inactiveText"],
}
VENDOR_AI = {9997: lambda: Real(90.51), 9998: lambda: Unsigned(90), 9999: lambda: Integer(-200)}   # constants, tags 44/21/32 in the golden
HIDDEN_DEVICE = {"apduSegmentTimeout", "statusFlags", "maxSegmentsAccepted"}   # bacpypes3 serves them, stock answers unknown-property
BITSTRINGS = {}      # device bit strings, 44 and 60 bits long as stock sends them (bacpypes3 pads to 51 and 66 on encode)
STOCK_SERVICES = (12, 14, 15, 16, 17, 26, 34, 35)           # RP, RPM, WP, WPM, DCC, I-Am, Who-Is, ReadRange (the golden's bit string)
STOCK_OBJECT_TYPES = (0, 1, 2, 3, 4, 5, 7, 8, 13, 14, 17, 19, 20, 21, 28, 40, 45, 47, 48, 53, 54, 56)

_read_property_to_any = svc_object.read_property_to_any


async def read_property_to_any(obj, propertyIdentifier, propertyArrayIndex=None):
    name = getattr(propertyIdentifier, "attr", None) or str(propertyIdentifier)
    if str(obj.objectIdentifier[0]) == "device" and name in HIDDEN_DEVICE:
        raise PropertyError("unknownProperty")
    if str(obj.objectIdentifier[0]) == "device" and name in BITSTRINGS:
        return Any(BITSTRINGS[name])
    if isinstance(propertyIdentifier, int) and int(propertyIdentifier) in VENDOR_AI and str(obj.objectIdentifier[0]) == "analog-input":
        return Any(VENDOR_AI[int(propertyIdentifier)]())
    return await _read_property_to_any(obj, propertyIdentifier, propertyArrayIndex)


svc_object.read_property_to_any = read_property_to_any      # RPM looks the function up in its module at call time


def pname(attr):                                # attr arrives as str or PropertyIdentifier
    return getattr(attr, "attr", None) or str(attr)


def unwritable(obj, attr):
    """Stock: a property outside the object's own set is unknown-property, one inside it is write-access-denied."""
    known = [n for n in STOCK_ALL.get(str(obj.objectIdentifier[0]), []) if isinstance(n, str)]
    return PropertyError("writeAccessDenied" if pname(attr) in known else "unknownProperty")


def is_pv(attr):
    return pname(attr) in ("presentValue", "present-value")


def stock_ao_value(v):
    """Stock keeps an AO as a uint8 (evidence: the golden's AO range block): negative is refused, the rest wraps mod 256
    after truncation, and 255 is the stack's null marker, so it reads back as a relinquish."""
    if v < 0:
        raise PropertyError("valueOutOfRange")
    n = int(v) & 0xFF
    return None if n == 255 else Real(float(n))


def network_write(points, obj, value):
    """Server::writeBinary/writeAnalog: the tag at this instance in the shared map, whichever object type was written."""
    hit = points.get(obj.objectIdentifier[1])
    if hit:
        write(hit[0], value)


class StockBI(BinaryInputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        if not is_pv(attr):
            raise unwritable(self, attr)
        network_write(BINARY, self, str(value) == "active")   # bi.c ACKs and enqueues the tag; the 1 Hz refresh resets the value


class StockAI(AnalogInputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        raise unwritable(self, attr)


class StockBO(BinaryOutputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        if not is_pv(attr):
            raise unwritable(self, attr)
        if priority is not None and not 1 <= priority <= 16:
            raise AbortOther()
        await super().write_property(attr, value, index, priority)
        if not isinstance(value, Null):
            network_write(BINARY, self, str(value) == "active")


class StockAO(AnalogOutputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        if not is_pv(attr):
            raise unwritable(self, attr)
        if priority is not None and not 1 <= priority <= 16:
            raise AbortOther()
        if hasattr(value, "real") or isinstance(value, float):
            network_write(ANALOG, self, float(value))        # ao.c enqueues the value as written, before its range check
            value = stock_ao_value(float(value))
            value = Null(()) if value is None else value
        await super().write_property(attr, value, index, priority)


def sink(base):
    """Other object types: stock stores a written present-value as is (golden: 'Writes to the other object types')."""
    if issubclass(base, LocalObject):
        return base
    # 0.1.0's _post_init reads notificationClass, which OctetStringValue lacks: an exception logged per object at start
    quiet = {"notificationClass": None} if base is OctetStringValueObject else {}
    return type("Stock" + base.__name__, (LocalObject, base), quiet)


def build_objects():
    """The stock object set, in stock's object-list order (device and network-port come from from_args)."""
    N = {"outOfService": False, "eventState": "normal"}
    objs = []
    for i in range(1000):
        objs.append(StockAI(objectIdentifier=("analog-input", i), objectName=f"ANALOG INPUT {i}", description=f"ANALOG INPUT {i}",
                            presentValue=0.0, units="percent", covIncrement=1.0, reliability="no-fault-detected", **N))
    for i in range(1000):
        objs.append(StockAO(objectIdentifier=("analog-output", i), objectName=f"ANALOG OUTPUT {i}", presentValue=0.0,
                            units="percent", relinquishDefault=0.0, **N))
    for i in range(4):
        objs.append(sink(AnalogValueObject)(objectIdentifier=("analog-value", i), objectName=f"ANALOG VALUE {i}", presentValue=0.0, **N))
    for i in range(1000):
        objs.append(StockBI(objectIdentifier=("binary-input", i), objectName=f"BINARY INPUT {i}", description=f"BINARY INPUT {i}",
                            polarity="normal", **N))
    for i in range(1000):
        o = StockBO(objectIdentifier=("binary-output", i), objectName=f"BINARY OUTPUT {i}", description=f"BINARY OUTPUT {i}",
                    activeText="on", inactiveText="off", relinquishDefault="inactive", polarity="normal", **N)
        o.recalculating()                         # present value from an empty array: the relinquish default
        objs.append(o)
    for i in range(10):
        objs.append(sink(BinaryValueObject)(objectIdentifier=("binary-value", i), objectName=f"BINARY VALUE {i}",
                                            presentValue="inactive", **N))
    # the rest of stock's list: instance ranges as the golden shows them
    for cls, name, rng, pv in [(CharacterStringValueObject, "characterstring-value", range(0, 1), ""),
                               (CommandObject, "command", range(0, 4), None),
                               (IntegerValueObject, "integer-value", range(1, 2), 0),
                               (LifeSafetyPointObject, "life-safety-point", range(0, 7), None),
                               (LoadControlObject, "load-control", range(0, 4), None),
                               (MultiStateInputObject, "multi-state-input", range(0, 4), 1),
                               (MultiStateOutputObject, "multi-state-output", range(0, 4), 1),
                               (MultiStateValueObject, "multi-state-value", range(0, 4), 1),
                               (TrendLogObject, "trend-log", range(0, 8), None),
                               (LightingOutputObject, "lighting-output", range(1, 9), None),
                               (ChannelObject, "channel", range(1, 2), None),
                               (OctetStringValueObject, "octetstring-value", range(0, 4), b""),
                               (PositiveIntegerValueObject, "positive-integer-value", range(0, 4), 0),
                               (ScheduleObject, "schedule", range(0, 4), None)]:
        for i in rng:
            kw = {} if pv is None else {"presentValue": pv}
            if cls in (MultiStateInputObject, MultiStateOutputObject, MultiStateValueObject):
                kw["numberOfStates"] = 3
            objs.append(sink(cls)(objectIdentifier=(name, i), objectName=f"{name.upper()} {i}", **kw))
    return objs


def refresh_once(app):
    """Server::update: BI/AI present value from the datastore, BO at priority 1 (stock's "0"), AO at 16. A tag the device does
    not have is skipped (stock's hasTag); a negative AO value leaves the slot as it was (Analog_Output_Present_Value_Set refuses it)."""
    for points, (inp, outp) in ((BINARY, ("binary-input", "binary-output")), (ANALOG, ("analog-input", "analog-output"))):
        for inst, (tag, out) in points.items():
            v = VALUES.get(tag)
            if v is None:
                continue
            obj = app.get_object_id(ObjectIdentifier((outp if out else inp, inst)))
            if obj is None:                       # an instance of 1000 or more: stock has no such object either
                continue
            if points is BINARY:
                pv = BinaryPV('active' if v else 'inactive')
                if out:
                    obj.priorityArray[PRIO_REFRESH_BO - 1] = PriorityValue(pv)
                else:
                    obj.presentValue = pv
            elif not out:
                obj.presentValue = float(v)
            elif v >= 0:
                n = stock_ao_value(float(v))
                obj.priorityArray[PRIO_REFRESH_AO - 1] = PriorityValue(null=()) if n is None else PriorityValue(n)


async def refresher(app):
    while True:
        refresh_once(app)
        await asyncio.sleep(1.0)


class StockApp(DeviceCommunicationControlServices, Application):
    @property
    def smap(self):                              # 0.1.0 bug: the DCC mixin reaches for self.smap, the state machine is self.asap
        return self.asap

    async def do_DeviceCommunicationControlRequest(self, apdu):
        # 0.1.0 compares the Enumerated to a str (never equal) in the mixin and in the state machine, so DCC does nothing there;
        # this is the stock behaviour on top of a str mode: password "filister", disable/disableInitiation, duration ignored
        if apdu.password != "filister":
            raise ExecutionError(errorClass="security", errorCode="passwordFailure")
        self.dcc = str(apdu.enableDisable)
        self.asap.dccEnableDisable = "disableInitiation" if self.dcc == "disableInitiation" else "enable"
        await self.response(SimpleAckPDU(context=apdu))

    def dcc_filter(self):
        """The state machine's own 'disable' branch reads pdu.apduType off an undecoded PDU (0.1.0 bug), so DCC disable can never be
        undone through it. Filter here instead: while disabled only DCC gets through (stock stays silent to Who-Is too, golden)."""
        orig = self.asap.confirmation
        self.dcc = "enable"

        async def confirmation(pdu):
            if self.dcc == "disable":
                b = bytes(pdu.pduData)                  # APDU byte 0 high nibble is the PDU type, service choice of a confirmed request at [3]
                if not (b[0] >> 4 == 0 and b[3] == 17):
                    return
            await orig(pdu)
        self.asap.confirmation = confirmation

    async def response(self, apdu):
        if isinstance(apdu, AbortPDU):
            apdu.apduSrv = True                  # 0.1.0 sends an application-raised Abort without the server flag; clients ignore it
        await super().response(apdu)

    async def do_ReadPropertyMultipleRequest(self, apdu):
        for spec in apdu.listOfReadAccessSpecifications:
            spec.objectIdentifier = self.wild(spec.objectIdentifier)
            obj = self.get_object_id(spec.objectIdentifier)
            names = STOCK_ALL.get(str(spec.objectIdentifier[0])) if obj is not None else None
            refs = []
            for ref in spec.listOfPropertyReferences:
                if names and ref.propertyIdentifier == PropertyIdentifier.all:
                    refs += [PropertyReference(propertyIdentifier=n) for n in names]
                else:
                    refs.append(ref)
            spec.listOfPropertyReferences = refs
        await super().do_ReadPropertyMultipleRequest(apdu)

    def wild(self, oid):
        """device,4194303 is the wildcard for 'this device'; 0.1.0 compares the ObjectIdentifier to a tuple, which never matches."""
        return self.device_object.objectIdentifier if (str(oid[0]), oid[1]) == ("device", 4194303) else oid

    async def do_ReadRangeRequest(self, apdu):
        # 0.1.0 raises NotImplementedError. Stock: device-address-binding is an empty list; everything else, unknown objects
        # included, is services/property-is-not-a-list; the trend-log log-buffers (synthetic demo records) are NOT reproduced here.
        if str(apdu.objectIdentifier[0]) == "device" and getattr(apdu.propertyIdentifier, "attr", "") == "deviceAddressBinding":
            await self.response(ReadRangeACK(objectIdentifier=self.wild(apdu.objectIdentifier), propertyIdentifier=apdu.propertyIdentifier,
                                             resultFlags=ResultFlags([0, 0, 0]), itemCount=0, itemData=Any(), context=apdu))
            return
        raise ExecutionError(errorClass="services", errorCode="propertyIsNotAList")

    async def do_WritePropertyMultipleRequest(self, apdu):
        # 0.1.0: no wildcard device id, and the error for a missing object carries no property; stock names the first one it tried
        for spec in apdu.listOfWriteAccessSpecifications:
            oid = self.wild(spec.objectIdentifier)
            obj = self.get_object_id(oid)
            for pv in spec.listOfProperties:
                ref = ObjectPropertyReference(objectIdentifier=oid, propertyIdentifier=pv.propertyIdentifier)
                if pv.propertyArrayIndex is not None:
                    ref.propertyArrayIndex = pv.propertyArrayIndex
                try:
                    if obj is None:
                        raise ExecutionError(errorClass="object", errorCode="unknownObject")
                    ptype = obj.get_property_type(pv.propertyIdentifier)
                    if not ptype:
                        raise PropertyError("unknownProperty")
                    value = pv.propertyValue.cast_out(ptype, null=(pv.priority is not None))
                    await obj.write_property(pv.propertyIdentifier, value, pv.propertyArrayIndex, pv.priority)
                except ExecutionError as err:
                    raise WritePropertyMultipleError(errorType=ErrorType(errorClass=err.errorClass, errorCode=err.errorCode),
                                                     firstFailedWriteAttempt=ref)
                if obj is None:
                    break
        await self.response(SimpleAckPDU(context=apdu))

    async def do_ReadPropertyRequest(self, apdu):
        apdu.objectIdentifier = self.wild(apdu.objectIdentifier)
        name = getattr(apdu.propertyIdentifier, "attr", None)
        if str(apdu.objectIdentifier[0]) == "device" and name in HIDDEN_DEVICE:
            raise PropertyError("unknownProperty")
        if str(apdu.objectIdentifier[0]) == "device" and name in BITSTRINGS and apdu.propertyArrayIndex is None:
            await self.response(ReadPropertyACK(objectIdentifier=apdu.objectIdentifier, propertyIdentifier=apdu.propertyIdentifier,
                                                propertyValue=Any(BITSTRINGS[name]), context=apdu))
            return
        pid = apdu.propertyIdentifier
        if isinstance(pid, int) and int(pid) in VENDOR_AI and str(apdu.objectIdentifier[0]) == "analog-input" \
                and self.get_object_id(apdu.objectIdentifier) is not None:
            await self.response(ReadPropertyACK(objectIdentifier=apdu.objectIdentifier, propertyIdentifier=pid,
                                                propertyValue=Any(VENDOR_AI[int(pid)]()), context=apdu))
            return
        await super().do_ReadPropertyRequest(apdu)

    async def do_WhoIsRequest(self, apdu):
        apdu.pduSource = None                    # stock answers every Who-Is with a global-broadcast I-Am, bacpypes3 unicasts to the asker
        await super().do_WhoIsRequest(apdu)

    do_SubscribeCOVRequest = None               # stock rejects what it does not serve (unrecognized-service)
    do_SubscribeCOVPropertyRequest = None
    do_WhoHasRequest = None
    do_IHaveRequest = None


class Server:
    role = "server"

    def __init__(self, el, hello):
        self.points = points(el)                 # stock parses the points, then the endpoint and instance
        udp(need(el, "endpoint"))
        self.instance = number(el, "instance")
        self.args = app_args(self.instance)
        self.uses = [use(kind, tag, kind.endswith("output")) for kind, _, tag in self.points]
        for kind, inst, tag in self.points:
            (BINARY if kind.startswith("binary") else ANALOG)[inst] = (tag, kind.endswith("output"))
        self.values([(p["tag"], p["value"]) for p in hello.get("points", [])])

    def values(self, changes):
        VALUES.update((t, v) for t, v in changes if v is not None)    # null: a non-finite analog, the previous value stays

    async def start(self):
        app = self.app = StockApp.from_args(self.args)
        app.dcc_filter()
        dev = app.device_object
        # stock device properties (bacnet-stack demo device defaults)
        dev.vendorIdentifier, dev.vendorName, dev.modelName = 260, "BACnet Stack at SourceForge", "GNU"
        dev.firmwareRevision, dev.applicationSoftwareVersion, dev.description, dev.location = "0.99.1", "1.0", "server", "USA"
        dev.protocolRevision = 19
        dev.daylightSavingsStatus, dev.utcOffset = False, 0
        dev.maxApduLengthAccepted = 1476
        dev.segmentationSupported = "no-segmentation"
        BITSTRINGS["protocolServicesSupported"] = BitString([1 if i in STOCK_SERVICES else 0 for i in range(44)])
        BITSTRINGS["protocolObjectTypesSupported"] = BitString([1 if i in STOCK_OBJECT_TYPES else 0 for i in range(60)])
        for o in build_objects():
            app.add_object(o)
        self.refresh = asyncio.create_task(refresher(app))
        log(f"BACnet server up: instance {self.instance}, {len(app.objectIdentifier)} objects, {self.args.address}")
        app.i_am()

    def close(self):
        self.app.close()


# ---- client (ClientConnection.cpp)

def polled(v, binary):
    """A present value as ffmod/1 carries it; an answer that is not one (bacpypes3 returns some errors instead of raising) is a
    ValueError."""
    if binary and str(v) in ("active", "inactive"):
        return str(v) == "active"
    if not binary and isinstance(v, float):      # Real is a float
        return float(v)
    raise ValueError(repr(v))


class Client:
    """Stock binds each connection to the device at its endpoint from an address_cache entry it writes itself, so no Who-Is goes
    out; here the endpoint is the peer's address. Every scan-rate seconds: ReadProperty present-value of each point, binaries then
    analogs by instance, one at a time, each result a `write`. A `values` change of an output: WriteProperty present-value without
    priority (ENUMERATED or REAL), answered with `sent`."""
    role = "client"

    def __init__(self, el, hello):
        self.scan = number(el, "scan-rate")
        self.polls, self.outs, self.uses = [], {}, []
        for c in el.findall("bacnet-connection"):
            endpoint, _ = need(c, "endpoint"), number(c, "instance")
            pts = points(c)
            peer = Address(udp(endpoint))         # stock checks the endpoint when the connection starts, after its points
            binary, analog = {}, {}               # stock's maps: instance -> point, an input and an output on one instance share it
            for kind, inst, tag in pts:
                (binary if kind.startswith("binary") else analog)[inst] = (f"{kind},{inst}", tag, kind.startswith("binary"))
                if kind.endswith("output"):
                    self.outs[tag] = (peer, f"{kind},{inst}", kind.startswith("binary"))
                self.uses.append(use(kind, tag, not kind.endswith("output")))
            self.polls.append((peer, [binary[i] for i in sorted(binary)] + [analog[i] for i in sorted(analog)]))
        self.args = app_args(len(self.polls))     # stock sets its own instance per connection, 1, 2, ...: the last one stays

    async def start(self):
        self.app = Application.from_args(self.args)
        self.tasks = [asyncio.create_task(self.poll(peer, pts)) for peer, pts in self.polls]
        log(f"BACnet client up: {len(self.polls)} connection(s), scan-rate {self.scan} s, {self.args.address}")

    async def poll(self, peer, pts):
        while True:
            for obj, tag, binary in pts:
                try:
                    v = polled(await asyncio.wait_for(self.app.read_property(peer, obj, "present-value"), TIMEOUT), binary)
                except (Exception, ErrorRejectAbortNack) as e:    # a timeout, an Error/Reject/Abort: stock logs it and goes on
                    log(f"ReadProperty {obj} at {peer}: {type(e).__name__} {e}")
                    continue
                write(tag, v)
            await asyncio.sleep(self.scan)

    def values(self, changes):
        for tag, v in changes:
            if tag in self.outs and v is not None:
                asyncio.create_task(self.send(tag, v))

    async def send(self, tag, v):
        peer, obj, binary = self.outs[tag]
        value = BinaryPV("active" if v else "inactive") if binary else Real(v)
        try:
            r = await asyncio.wait_for(self.app.write_property(peer, obj, "present-value", value), TIMEOUT)
        except (Exception, ErrorRejectAbortNack) as e:     # not an Exception in 0.1.0
            r = f"{type(e).__name__} {e}"
        if r is not None:
            log(f"WriteProperty {obj} at {peer}: {r}")
        say(t="sent", tag=tag, value=v, ok=r is None)

    def close(self):
        self.app.close()


# ---- ffmod/1

async def main():
    logging.basicConfig(level=logging.WARNING)
    loop, lines = asyncio.get_running_loop(), asyncio.Queue()

    def pump():                                   # a plain blocking read: no cap on line length (hello can exceed 64 KiB)
        for line in sys.stdin:
            loop.call_soon_threadsafe(lines.put_nowait, line)
        loop.call_soon_threadsafe(lines.put_nowait, None)
    threading.Thread(target=pump, daemon=True).start()

    line = await lines.get()
    if line is None:
        return
    hello = json.loads(line)
    try:
        el = ET.fromstring(hello.get("config", ""))
        role = {"bacnet-server": Server, "bacnet-client": Client}.get(el.tag)
        if role is None:
            raise ConfigError(f"<{el.tag}> is not <bacnet-server> or <bacnet-client>")
        role = role(el, hello)
    except (ConfigError, ET.ParseError, ValueError, RuntimeError) as e:     # bacpypes3 refuses an endpoint address with these
        say(t="error", msg=str(e))
        return
    say(t="ready", role=role.role, uses=role.uses)
    await role.start()
    while (line := await lines.get()) is not None:
        try:
            m = json.loads(line)
        except ValueError:
            log(f"not ffmod/1, dropped: {line[:200]!r}")
            continue
        if m.get("t") == "values":
            role.values(m.get("changes", []))
        elif m.get("t") == "reject":
            log(f"write {m.get('id')} rejected: {m.get('err')}")
        elif m.get("t") == "stop":
            break
    role.close()


if __name__ == "__main__":
    asyncio.run(main())
