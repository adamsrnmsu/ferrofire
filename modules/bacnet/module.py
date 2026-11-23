"""ferrofire's BACnet/IP module: a stock <bacnet-server> or <bacnet-client> (protocols plan rows 6 and 11) over ffmod/1 on stdio,
on bacpypes3 0.1.0. The server grew from the Task 0b spike (spike/NOTES.md): an override layer over 0.1.0's defects that gives a
client what stock bennu-field-device gives (conformance/bacnet holds it to stock). stdout carries ffmod/1 only; everything else
goes to stderr.
Transport as stock (bacnet-stack dlenv): interface BACNET_IFACE (eth0), port BACNET_IP_PORT (47808), BACNET_BBMD_ADDRESS/_PORT/
_TIMETOLIVE register as a foreign device; the <endpoint> ip and port are parsed and discarded, as stock does.
"""
import asyncio, datetime, fcntl, ipaddress, itertools, json, logging, os, socket, struct, sys, threading
import xml.etree.ElementTree as ET

from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.app import Application
from bacpypes3.basetypes import PropertyIdentifier, BinaryPV, PriorityValue
from bacpypes3.apdu import decode_max_apdu_length_accepted, AbortPDU, ErrorRejectAbortNack, ReadPropertyACK, ReadRangeACK, SimpleAckPDU, WritePropertyMultipleError
from bacpypes3.errors import ExecutionError, PropertyError
from bacpypes3.pdu import Address, LocalBroadcast
from bacpypes3.service.device import DeviceCommunicationControlServices
from bacpypes3.local.analog import AnalogInputObject, AnalogOutputObject, AnalogValueObject
from bacpypes3.local.binary import BinaryInputObject, BinaryOutputObject, BinaryValueObjectCmd
from bacpypes3.local.multistate import MultiStateOutputObject
from bacpypes3.local.object import Object as LocalObject
from bacpypes3.object import (CharacterStringValueObject, CommandObject, IntegerValueObject, LifeSafetyPointObject,
                              LoadControlObject, MultiStateInputObject, MultiStateValueObject,
                              TrendLogObject, LightingOutputObject, ChannelObject, OctetStringValueObject,
                              PositiveIntegerValueObject, ScheduleObject)
import bacpypes3.service.object as svc_object
from bacpypes3.basetypes import LogRecord, LogRecordLogDatum, StatusFlags, DailySchedule, DateRange, DeviceObjectPropertyReference, ErrorType, ResultFlags, ObjectPropertyReference, PropertyReference
from bacpypes3.constructeddata import Any, ArrayOf, SequenceOf
from bacpypes3.basetypes import DateTime
from bacpypes3.primitivedata import BitString, Date, Integer, Null, ObjectIdentifier, Real, Unsigned

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
    "analog-output": COMMON + ["units", "priorityArray", "relinquishDefault", "currentCommandPriority"],
    "binary-input": COMMON + ["polarity", "description"],
    "binary-output": COMMON + ["polarity", "priorityArray", "relinquishDefault", "description", "activeText", "inactiveText", "currentCommandPriority"],
    # the types stock serves in other orders (golden: RPM-ALL blocks); Current_Command_Priority is ours: 135-2012as, required on a
    # commandable object. Log_Buffer is listed but RP gives read-access-denied (135-2010ad); ReadRange serves it.
    "analog-value": COMMON + ["units", "description"],
    "binary-value": COMMON + ["description", "priorityArray", "relinquishDefault", "currentCommandPriority"],
    "multi-state-input": COMMON + ["numberOfStates", "description", "stateText"],
    "multi-state-output": COMMON + ["numberOfStates", "priorityArray", "relinquishDefault", "description", "currentCommandPriority"],
    "multi-state-value": COMMON + ["numberOfStates", "description", "stateText"],
    "positive-integer-value": ["objectIdentifier", "objectName", "objectType", "presentValue", "statusFlags", "units", "outOfService"],
    "characterstring-value": COMMON + ["description"],
    "schedule": ["objectIdentifier", "objectName", "objectType", "presentValue", "effectivePeriod", "scheduleDefault",
                 "listOfObjectPropertyReferences", "priorityForWriting", "statusFlags", "reliability", "outOfService", "weeklySchedule"],
    "trend-log": ["objectIdentifier", "objectName", "objectType", "enable", "stopWhenFull", "bufferSize", "logBuffer", "recordCount", "totalRecordCount",
                  "eventState", "loggingType", "statusFlags", "description", "startTime", "stopTime", "logDeviceObjectProperty",
                  "logInterval", "alignIntervals", "intervalOffset", "trigger"],
}
VENDOR_AI = {9997: lambda: Real(90.51), 9998: lambda: Unsigned(90), 9999: lambda: Integer(-200)}   # constants, tags 44/21/32 in the golden
HIDDEN_DEVICE = {"apduSegmentTimeout", "statusFlags", "maxSegmentsAccepted"}   # bacpypes3 serves them, stock answers unknown-property
BITSTRINGS = {}      # device bit strings, 44 and 60 bits long as stock sends them (bacpypes3 pads to 51 and 66 on encode)
STOCK_SERVICES = (12, 14, 15, 16, 17, 26, 34, 35)           # RP, RPM, WP, WPM, DCC, I-Am, Who-Is, ReadRange (the golden's bit string)
STOCK_OBJECT_TYPES = (0, 1, 2, 3, 4, 5, 7, 8, 13, 14, 17, 19, 20, 21, 28, 40, 45, 47, 48, 53, 54, 56)

_read_property_to_any = svc_object.read_property_to_any


async def read_property_to_any(obj, propertyIdentifier, propertyArrayIndex=None):
    name = getattr(propertyIdentifier, "attr", None) or str(propertyIdentifier)
    if str(obj.objectIdentifier[0]) == "trend-log" and name == "logBuffer":
        raise PropertyError("readAccessDenied")          # 135-2010ad / 135-2016bu 12.25: a Log_Buffer is read by ReadRange only
    if str(obj.objectIdentifier[0]) == "device" and name in HIDDEN_DEVICE:
        raise PropertyError("unknownProperty")
    if str(obj.objectIdentifier[0]) == "device" and name in BITSTRINGS:
        return Any(BITSTRINGS[name])
    if isinstance(propertyIdentifier, int) and int(propertyIdentifier) in VENDOR_AI and str(obj.objectIdentifier[0]) == "analog-input":
        return Any(VENDOR_AI[int(propertyIdentifier)]())
    return await _read_property_to_any(obj, propertyIdentifier, propertyArrayIndex)


svc_object.read_property_to_any = read_property_to_any      # RPM looks the function up in its module at call time


def stock_property_list(self, _orig=LocalObject.propertyList):
    """Property_List (135-2010ao): the properties the object serves, without Object_Identifier, Object_Name, Object_Type and Property_List
    (15.7.3.1.2 keeps them out), and without the device properties stock answers unknown-property for. Order: RPM all's, as stock's."""
    t = str(self.objectIdentifier[0])
    names = STOCK_ALL.get(t) or [pname(p) for p in _orig.fget(self)]
    skip = {"objectIdentifier", "objectName", "objectType", "propertyList"} | (HIDDEN_DEVICE if t == "device" else set())
    return ArrayOf(PropertyIdentifier)([PropertyIdentifier(n) for n in names if n not in skip])


LocalObject.propertyList = property(stock_property_list, LocalObject.propertyList.fset)


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


def check_priority(priority):
    """15.9.1.3.1 (135-2008r-4): a Priority outside 1..16 is SERVICES/PARAMETER_OUT_OF_RANGE (stock Aborts). Priority 6 is refused as
    stock does on every commandable type (the minimum on/off priority); stock's AO enqueues the value to the provider first, ours does not."""
    if priority is not None and not 1 <= priority <= 16:
        raise ExecutionError(errorClass="services", errorCode="parameterOutOfRange")
    if priority == 6:
        raise PropertyError("writeAccessDenied")


class Commanded:
    """Mixin for a commandable BV or MSO: stock refuses Relinquish_Default and the priority rules apply to Present_Value."""
    async def write_property(self, attr, value, index=None, priority=None):
        if pname(attr) == "relinquishDefault":
            raise PropertyError("writeAccessDenied")
        if is_pv(attr):
            check_priority(priority)
        await super().write_property(attr, value, index, priority)


async def input_write(obj, attr, value, index, priority, writable, send):
    """An input object: `writable` properties as stock; Present_Value only while Out_Of_Service is TRUE (135-2012aw Table 12-2,
    135-2016bl 12.x.10): it is stored, as stock reads it back, and `send` enqueues it as stock does."""
    if pname(attr) in writable:
        await super(type(obj), obj).write_property(attr, value, index, priority)
    elif not is_pv(attr):
        raise unwritable(obj, attr)
    elif not obj.outOfService:
        raise PropertyError("writeAccessDenied")
    else:
        obj.presentValue = value
        send(value)


class StockBI(BinaryInputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        await input_write(self, attr, value, index, priority, ("outOfService", "polarity"),
                          lambda v: network_write(BINARY, self, str(v) == "active"))     # bi.c ACKs and enqueues the tag


class StockAI(AnalogInputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        await input_write(self, attr, value, index, priority, ("outOfService", "units", "covIncrement"),
                          lambda v: network_write(ANALOG, self, float(v)))


class StockBO(BinaryOutputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        if pname(attr) == "outOfService":
            await super().write_property(attr, value, index, priority)
            return
        if not is_pv(attr):
            raise unwritable(self, attr)
        check_priority(priority)
        await super().write_property(attr, value, index, priority)
        if not isinstance(value, Null) and not self.outOfService:      # out of service: decoupled from the output (135-2016bl)
            network_write(BINARY, self, str(value) == "active")


class StockAO(AnalogOutputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        if pname(attr) == "outOfService":
            await super().write_property(attr, value, index, priority)
            return
        if not is_pv(attr):
            raise unwritable(self, attr)
        check_priority(priority)
        if hasattr(value, "real") or isinstance(value, float):
            if not self.outOfService:                        # out of service: decoupled from the output (135-2016bl)
                network_write(ANALOG, self, float(value))    # ao.c enqueues the value as written, before its range check
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


def commanded(base):
    return type("Commanded" + base.__name__, (Commanded, sink(base)), {})


class StockMSI(sink(MultiStateInputObject)):
    async def write_property(self, attr, value, index=None, priority=None):
        if is_pv(attr) and not self.outOfService:
            raise PropertyError("writeAccessDenied")         # 135-2016bl 12.18.10, as an AI's
        await super().write_property(attr, value, index, priority)


# Trend Log and Schedule property values as stock serves them (golden: RPM-ALL trend-log,0 and schedule,0).
TREND_LOG = {"description": "Trend Log {}", "enable": True, "stopWhenFull": False, "bufferSize": 1000, "recordCount": 1000,
             "totalRecordCount": 10000, "eventState": "normal", "loggingType": "polled", "logInterval": 90000, "alignIntervals": True,
             "intervalOffset": 0, "trigger": False,
             "startTime": DateTime(date=(109, 1, 1, 4), time=(0, 0, 0, 0)), "stopTime": DateTime(date=(120, 12, 22, 2), time=(23, 59, 59, 99)),
             "logDeviceObjectProperty": DeviceObjectPropertyReference(objectIdentifier=("analog-input", 0), propertyIdentifier="present-value",
                                                                       deviceIdentifier=("device", 1234))}
SCHEDULE = {"presentValue": Real(21.0), "scheduleDefault": Real(21.0), "reliability": "no-fault-detected", "outOfService": False,
            "effectivePeriod": DateRange(startDate=Date((255, 1, 1, 255)), endDate=Date((255, 12, 31, 255))),
            "listOfObjectPropertyReferences": [], "priorityForWriting": 16, "weeklySchedule": [DailySchedule(daySchedule=[]) for _ in range(7)]}


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
    for i in range(4):     # units no-units; stock's COV_Increment is left out: this device offers no COV (135-2012aw, Table 12-4)
        objs.append(sink(AnalogValueObject)(objectIdentifier=("analog-value", i), objectName=f"ANALOG VALUE {i}", description=f"ANALOG VALUE {i}",
                                            presentValue=0.0, units="no-units", **N))
    for i in range(1000):
        objs.append(StockBI(objectIdentifier=("binary-input", i), objectName=f"BINARY INPUT {i}", description=f"BINARY INPUT {i}",
                            polarity="normal", **N))
    for i in range(1000):
        o = StockBO(objectIdentifier=("binary-output", i), objectName=f"BINARY OUTPUT {i}", description=f"BINARY OUTPUT {i}",
                    activeText="on", inactiveText="off", relinquishDefault="inactive", polarity="normal", **N)
        o.recalculating()                         # present value from an empty array: the relinquish default
        objs.append(o)
    for i in range(10):                           # stock's BV is commandable (priority-array, relinquish-default)
        o = commanded(BinaryValueObjectCmd)(objectIdentifier=("binary-value", i), objectName=f"BINARY VALUE {i}", description=f"BINARY VALUE {i}",
                                       relinquishDefault="inactive", **N)
        o.recalculating()
        objs.append(o)
    states = {"numberOfStates": 254, "stateText": [""] * 254}
    # the rest of stock's list: instance ranges, names and property values as the golden's sweep shows them
    for cls, name, label, rng, kw in [
            (CharacterStringValueObject, "characterstring-value", "CHARACTER STRING VALUE 1", range(0, 1),
             {"presentValue": "", "description": "A Character String Value Example", **N}),
            (CommandObject, "command", "COMMAND {}", range(0, 4), {}),
            (IntegerValueObject, "integer-value", "INTEGER VALUE {}", range(1, 2), {"presentValue": 0}),
            (LifeSafetyPointObject, "life-safety-point", "LS POINT {}", range(0, 7), {}),
            (LoadControlObject, "load-control", "LOAD CONTROL {}", range(0, 4), {}),
            (StockMSI, "multi-state-input", "MULTISTATE INPUT {}", range(0, 4),
             {"presentValue": 1, "description": "MULTISTATE INPUT {}", **states, **N}),
            (commanded(MultiStateOutputObject), "multi-state-output", "MULTISTATE OUTPUT {}", range(0, 4),
             {"description": "MULTISTATE OUTPUT {}", "numberOfStates": 254, "relinquishDefault": 1, **N}),
            (MultiStateValueObject, "multi-state-value", "MULTISTATE VALUE {}", range(0, 4),
             {"presentValue": 1, "description": "MULTISTATE VALUE {}", **states, **N}),
            (TrendLogObject, "trend-log", "Trend Log {}", range(0, 8), TREND_LOG),
            (LightingOutputObject, "lighting-output", "LIGHTING OUTPUT {}", range(1, 9), {}),
            (ChannelObject, "channel", "CHANNEL {}", range(1, 2), {}),
            (OctetStringValueObject, "octetstring-value", "OCTETSTRING VALUE {}", range(0, 4), {"presentValue": b""}),
            (PositiveIntegerValueObject, "positive-integer-value", "POSITIVEINTEGER VALUE {}", range(0, 4),
             {"presentValue": 0, "units": "square-meters", "outOfService": False}),
            (ScheduleObject, "schedule", "SCHEDULE {}", range(0, 4), SCHEDULE)]:
        for i in rng:
            o = sink(cls)(objectIdentifier=(name, i), objectName=label.format(i),
                          **{k: v.format(i) if isinstance(v, str) else v for k, v in kw.items()})
            if "multi-state-output" == name:
                o.recalculating()
            objs.append(o)
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
            if obj is None or obj.outOfService:   # an instance of 1000 or more: stock has no such object either; out of service: decoupled from the feed (135-2016bl 12.x.10)
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


# Trend Log Log_Buffer: no stored records; log N holds `size` synthetic ones (bennu's demo logger, from captures of the stock wire):
# record P (1-based) is stamped 2009-(2+N)-01 00:00 + 15 min * (P-1), real value 1000*N + P-1, status flags (all clear) on even N.
# Sequence numbers run base+1 .. base+size (Total_Record_Count - Record_Count + 1 ..).
APDU = 1476                                      # stock fills one 1476-byte APDU whatever the requester accepts; 15.8.2 and 5.4 cut at its maximum
READ_RANGE_HEADER = 17                           # the ACK without items: type, id, service, [0] to [4], opening and closing [5]


def log_time(n, pos):
    t = datetime.datetime(2009, 2 + n, 1) + datetime.timedelta(minutes=15 * (pos - 1))
    return (t.year - 1900, t.month, t.day, t.isoweekday(), t.hour, t.minute, 0, 0)


def log_record(n, pos):
    y, mo, d, wd, h, mi, s, hs = log_time(n, pos)
    return LogRecord(timestamp=DateTime(date=(y, mo, d, wd), time=(h, mi, s, hs)), logDatum=LogRecordLogDatum(realValue=1000.0 * n + pos - 1),
                     **({"statusFlags": StatusFlags([0, 0, 0, 0])} if n % 2 == 0 else {}))


def log_range(n, rng, size, base, limit=APDU):
    """ReadRange of log n's Log_Buffer (15.8.2): (records, [first, last, more] flags, first sequence number or None). Positions are
    clipped to 1..size; By Time compares strictly, y/m/d/h/m/s only (255 is just a large number); more-items cuts at the APDU size (`limit`),
    keeping the oldest records. Count 0 and Reference Index 0 are services/other, as stock gives them."""
    if rng is None:
        lo, hi, by = 1, size, None
    else:
        by = "time" if rng.byTime else "seq" if rng.bySequenceNumber else "pos"
        r = rng.byTime or rng.bySequenceNumber or rng.byPosition
        c = r.count
        if c == 0:
            raise ExecutionError(errorClass="services", errorCode="other")
        if by == "time":
            ref = tuple(r.referenceTime.date)[:3] + tuple(r.referenceTime.time)[:3]
            hits = [p for p in range(1, size + 1) for t in [log_time(n, p)] if (t[:3] + t[4:7] > ref if c > 0 else t[:3] + t[4:7] < ref)]
            if not hits:
                return [], [False, False, False], None
            pos = hits[0] if c > 0 else hits[-1]
        else:
            pos = r.referenceIndex if by == "pos" else r.referenceSequenceNumber - base
            if pos == 0 and by == "pos":
                raise ExecutionError(errorClass="services", errorCode="other")
        lo, hi = (pos, pos + c - 1) if c > 0 else (pos + c + 1, pos)
    lo, hi = max(lo, 1), min(hi, size)
    items, used = [], READ_RANGE_HEADER + (1 + max(1, (base + lo).bit_length() + 7 >> 3) if by in ("seq", "time") else 0)
    for p in range(lo, hi + 1):
        rec = log_record(n, p)
        used += sum(len(t.encode().pduData) for t in Any(rec).tagList)
        if used > limit:
            break
        items.append(rec)
    last = lo + len(items) - 1
    return items, [bool(items) and lo == 1, bool(items) and last == size, last < hi], base + lo if items and by in ("seq", "time") else None


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
        # 0.1.0 raises NotImplementedError. Stock: device-address-binding is an empty list, a trend-log's Log_Buffer is its synthetic
        # records (log_range); 15.8.1.3.1 for the rest: unknown object, unknown property, then property-is-not-a-list.
        oid = self.wild(apdu.objectIdentifier)
        obj = self.get_object_id(oid)
        if obj is None:
            raise ExecutionError(errorClass="object", errorCode="unknownObject")
        kind, name = str(oid[0]), getattr(apdu.propertyIdentifier, "attr", "")
        ack = dict(objectIdentifier=oid, propertyIdentifier=apdu.propertyIdentifier, context=apdu)
        if kind == "device" and name == "deviceAddressBinding":
            await self.response(ReadRangeACK(resultFlags=ResultFlags([0, 0, 0]), itemCount=0, itemData=Any(), **ack))
        elif kind == "trend-log" and name == "logBuffer":
            items, flags, first = log_range(oid[1], apdu.range, obj.recordCount, obj.totalRecordCount - obj.recordCount,
                                          min(APDU, decode_max_apdu_length_accepted(apdu.apduMaxResp)))
            await self.response(ReadRangeACK(resultFlags=ResultFlags(flags), itemCount=len(items), itemData=Any(SequenceOf(LogRecord)(items)),
                                             firstSequenceNumber=first, **ack))
        else:
            await read_property_to_any(obj, apdu.propertyIdentifier)       # unknown-property as RP gives it
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
        if str(apdu.objectIdentifier[0]) == "trend-log" and name == "logBuffer" and self.get_object_id(apdu.objectIdentifier) is not None:
            raise PropertyError("readAccessDenied")
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

    do_ConfirmedCOVNotificationRequest = None   # bacpypes3 answers error/unknown-subscription; stock rejects (wire capture)
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

    def begin(self):
        self.refresh = asyncio.create_task(refresher(self.app))
        self.limited = asyncio.create_task(limited_broadcast(self.app))      # closed with the process
        log(f"BACnet server up: instance {self.instance}, {len(self.app.objectIdentifier)} objects, {self.args.address}")
        self.app.i_am()

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


def stock_type(kind, last):
    """The object type stock reads and writes for a point of `kind` whose tag was declared last as `last`
    (sceptre-bennu ClientConnection.cpp:153 and :168 in poll(), :57 and :90 in writeBinary/writeAnalog: `mRegisterType ==
    eValueReadOnly ? INPUT : OUTPUT`, with the descriptor looked up by tag in mRegisters, which DataHandler.cpp:118-158 fills in
    the order BI, BO, AI, AO, the last declaration of a tag winning). Binary points are eStatusReadOnly/ReadWrite, never
    eValueReadOnly, so every binary point is a binary-output; an analog point is an analog-input only while its tag was last
    declared as one (the sample's AI 1 shares bus-1.gen_mw with AO 3, so stock reads analog-output 1)."""
    if kind.startswith("binary"):
        return "binary-output"
    return "analog-input" if last == "analog-input" else "analog-output"


class Client:
    """Stock binds each connection to the device at its endpoint from an address_cache entry it writes itself, so no Who-Is goes
    out; here the endpoint is the peer's address. Every scan-rate seconds: ReadProperty present-value of each point, binaries then
    analogs by instance, one at a time, each result a `write`. A `values` change of an output: WriteProperty present-value without
    priority (ENUMERATED or REAL), answered with `sent`. A `put` (the command interface's WRITE=, any tag): the same WriteProperty,
    answered with `done`. Neither waits less than bacpypes3's own transaction (APDU timeout 3 s, 3 retries), so a write reported
    failed is not sent again later."""
    role = "client"

    def __init__(self, el, hello):
        self.scan = number(el, "scan-rate")
        self.polls, self.outs, self.tags, self.uses = [], {}, {}, []
        for c in el.findall("bacnet-connection"):
            endpoint, _ = need(c, "endpoint"), number(c, "instance")
            pts = points(c)
            peer = Address(udp(endpoint))         # stock checks the endpoint when the connection starts, after its points
            binary, analog = {}, {}               # stock's maps: instance -> point, an input and an output on one instance share it
            last = {tag: kind for kind, _, tag in pts}    # mRegisters: the last declaration of a tag, whatever its kind
            for kind, inst, tag in pts:
                obj = f"{stock_type(kind, last[tag])},{inst}"
                (binary if kind.startswith("binary") else analog)[inst] = (obj, tag, kind.startswith("binary"))
                self.tags[tag] = (peer, obj, kind.startswith("binary"))     # every tag, the last declaration winning
                if kind.endswith("output"):
                    self.outs[tag] = (peer, obj, kind.startswith("binary"))
                self.uses.append(use(kind, tag, not kind.endswith("output")))
            self.polls.append((peer, [binary[i] for i in sorted(binary)] + [analog[i] for i in sorted(analog)]))
        self.args = app_args(len(self.polls))     # stock sets its own instance per connection, 1, 2, ...: the last one stays

    async def start(self):
        self.app = Application.from_args(self.args)
        # confirmed requests as stock's bacnet-stack sends them: `00 05`, no segmented response accepted, max segments
        # unspecified, max APDU 1476 (bacpypes3 reads these off the device object for every request)
        dev = self.app.device_object
        dev.segmentationSupported, dev.maxApduLengthAccepted, dev.maxSegmentsAccepted = "no-segmentation", 1476, 0

    def begin(self):
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

    async def write_property(self, table, tag, v):
        """WriteProperty present-value of `tag` per `table`; None when the peer acked, else why not. No `wait_for`: cancelling the
        request would leave bacpypes3 retransmitting it, so a write reported failed could still land seconds later."""
        peer, obj, binary = table[tag]
        value = BinaryPV("active" if v else "inactive") if binary else Real(v)
        try:
            r = await self.app.write_property(peer, obj, "present-value", value)
        except (Exception, ErrorRejectAbortNack) as e:     # not an Exception in 0.1.0
            r = f"{type(e).__name__} {e}"
        if r is not None:
            log(f"WriteProperty {obj} at {peer}: {r}")
        return r

    async def send(self, tag, v):
        r = await self.write_property(self.outs, tag, v)
        say(t="sent", tag=tag, value=v, ok=r is None)

    def put(self, m):
        asyncio.create_task(self.put_one(m["id"], m["tag"], m["value"]))

    async def put_one(self, id, tag, v):
        """The command interface's WRITE=: stock sends any tag to the object its type rule names (WRITE= to a binary tag commands
        binary-output n, the object the tag reads from; to an analog-input tag, analog-input n, which the peer refuses)."""
        r = await self.write_property(self.tags, tag, v) if tag in self.tags else "no such tag"
        say(t="done", id=id, ok=r is None, err="" if r is None else str(r))

    def close(self):
        self.app.close()


# ---- the endpoint

BIND_WAIT = 5.0         # the device waits 10 s for `ready`


def link_server(app):
    return next(iter(app.link_layers.values())).server


async def bound(app):
    """bacpypes3 0.1.0 binds in a background task that retries every OSError (a boot race), so a held port never raises: wait for
    the transport-ready event, a bounded time. Stock exits 1 on a bind failure; here the module says `error` and the device runs on."""
    try:
        await asyncio.wait_for(link_server(app)._local_transport_ready.wait(), BIND_WAIT)
        return True
    except asyncio.TimeoutError:
        return False


async def limited_broadcast(app):
    """A Who-Is to 255.255.255.255: Linux hands that to a socket bound to the limited broadcast address (or the wildcard), not to the
    ones bacpypes3 binds (the interface address and the directed broadcast). Stock answers it (bacnet-stack binds the wildcard)."""
    srv = link_server(app)
    if srv.broadcast_address:
        _, protocol = await srv.retrying_create_datagram_endpoint(asyncio.get_running_loop(), ("255.255.255.255", srv.local_address[1]))
        protocol.destination = LocalBroadcast()


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
    await role.start()
    if not await bound(role.app):
        say(t="error", msg=f"BACnet endpoint {role.args.address} did not bind in {BIND_WAIT:g} s (held by another process?)")
        role.app.close()
        return
    say(t="ready", role=role.role, uses=role.uses)
    role.begin()
    while (line := await lines.get()) is not None:
        try:
            m = json.loads(line)
        except ValueError:
            log(f"not ffmod/1, dropped: {line[:200]!r}")
            continue
        if m.get("t") == "values":
            role.values(m.get("changes", []))
        elif m.get("t") == "put" and hasattr(role, "put"):
            role.put(m)
        elif m.get("t") == "reject":
            log(f"write {m.get('id')} rejected: {m.get('err')}")
        elif m.get("t") == "stop":
            break
    role.close()


if __name__ == "__main__":
    asyncio.run(main())
