"""bacpypes3 0.1.0 spike: a BACnet/IP server shaped like stock bennu-field-device's <bacnet-server> (protocols plan row 6).
Usage: server.py CONFIG.xml [ADDRESS=host]   (SEED="tag=value,..." fills the stand-in datastore; there is no provider here).
Only the stock *behaviour a client sees* is modelled; see NOTES.md for what bacpypes3 could not give.
"""
import asyncio, logging, os, sys
import xml.etree.ElementTree as ET

from bacpypes3.argparse import SimpleArgumentParser
from bacpypes3.app import Application
from bacpypes3.basetypes import PropertyIdentifier, BinaryPV, EngineeringUnits, PriorityValue, Reliability, ServicesSupported, ObjectTypesSupported
from bacpypes3.apdu import AbortPDU, ReadPropertyACK, ReadRangeACK, SimpleAckPDU, WritePropertyMultipleError
from bacpypes3.errors import AbortOther, ExecutionError, PropertyError
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

PRIO_REFRESH_BO, PRIO_REFRESH_AO = 1, 16     # stock: refresh writes BO at priority "0" (array slot 1), AO at 16


DATASTORE = {}                                  # tag -> bool | float, stand-in for the field-device datastore
TAGS = {}                                       # object identifier -> tag, outputs only
WRITTEN = []                                    # tags a client write enqueued (stock sends them to the provider)


# RPM "all" as stock answers it, in its order (from the golden's raw RPM-ALL blocks); 9997-9999 are the stack's vendor properties
COMMON = ["objectIdentifier", "objectName", "objectType", "presentValue", "statusFlags", "eventState", "outOfService"]
STOCK_ALL = {
    "analog-input": COMMON + ["units", "description", "reliability", "covIncrement", 9997, 9998, 9999],
    "analog-output": COMMON + ["units", "priorityArray", "relinquishDefault"],
    "binary-input": COMMON + ["polarity", "description"],
    "binary-output": COMMON + ["polarity", "priorityArray", "relinquishDefault", "description", "activeText", "inactiveText"],
}
VENDOR_AI = {9997: lambda: Real(90.51), 9998: lambda: Unsigned(90), 9999: lambda: Integer(-200)}   # constants, tags 44/21/32 in the golden

_read_property_to_any = svc_object.read_property_to_any


HIDDEN_DEVICE = {"apduSegmentTimeout", "statusFlags", "maxSegmentsAccepted"}   # bacpypes3 serves them, stock answers unknown-property
BITSTRINGS = {}      # device bit strings, 44 and 60 bits long as stock sends them (bacpypes3 pads to 51 and 66 on encode)


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


class StockBI(BinaryInputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        if is_pv(attr):
            return                                # stock ACKs a write to BI present-value and does nothing
        raise unwritable(self, attr)


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
        if self.objectIdentifier in TAGS:
            WRITTEN.append((TAGS[self.objectIdentifier], value))


class StockAO(AnalogOutputObject):
    async def write_property(self, attr, value, index=None, priority=None):
        if not is_pv(attr):
            raise unwritable(self, attr)
        if priority is not None and not 1 <= priority <= 16:
            raise AbortOther()
        if hasattr(value, "real") or isinstance(value, float):
            value = stock_ao_value(float(value))
            value = Null(()) if value is None else value
        await super().write_property(attr, value, index, priority)
        if self.objectIdentifier in TAGS:
            WRITTEN.append((TAGS[self.objectIdentifier], value))


def sink(base):
    """Other object types: stock stores a written present-value as is (golden: 'Writes to the other object types')."""
    if issubclass(base, LocalObject):
        return base
    return type("Stock" + base.__name__, (LocalObject, base), {})


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


async def refresher(by_kind):
    """1 Hz: BI/AI present value from the datastore, BO at priority 1 (stock's "0"), AO at 16."""
    while True:
        for (kind, inst), (obj, tag) in by_kind.items():
            v = DATASTORE.get(tag)
            if kind == "binary-input":
                obj.presentValue = BinaryPV('active' if v else 'inactive')
            elif kind == "analog-input":
                obj.presentValue = float(v or 0.0)
            elif kind == "binary-output":
                obj.priorityArray[PRIO_REFRESH_BO - 1] = PriorityValue(BinaryPV('active' if v else 'inactive'))
            elif kind == "analog-output":
                obj.priorityArray[PRIO_REFRESH_AO - 1] = PriorityValue(stock_ao_value(float(v or 0.0)))
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


STOCK_SERVICES = (12, 14, 15, 16, 17, 26, 34, 35)           # RP, RPM, WP, WPM, DCC, I-Am, Who-Is, ReadRange (the golden's bit string)
STOCK_OBJECT_TYPES = (0, 1, 2, 3, 4, 5, 7, 8, 13, 14, 17, 19, 20, 21, 28, 40, 45, 47, 48, 53, 54, 56)


async def main():
    logging.basicConfig(level=logging.WARNING)
    cfgfile = sys.argv[1]
    address = sys.argv[2] if len(sys.argv) > 2 else "host"
    sec = ET.parse(cfgfile).getroot().find(".//bacnet-server")
    instance = int(sec.findtext("instance"))
    for kv in filter(None, os.environ.get("SEED", "").split(",")):
        k, v = kv.split("=")
        DATASTORE[k] = v == "true" if v in ("true", "false") else float(v)

    sys.argv[1:] = ["--name", "SimpleServer", "--instance", str(instance), "--address", address]
    args = SimpleArgumentParser().parse_args()
    app = StockApp.from_args(args)
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

    by_kind = {}
    objs = build_objects()
    for o in objs:
        app.add_object(o)
    for kind in ("binary-input", "binary-output", "analog-input", "analog-output"):
        for el in sec.findall(kind):
            inst, tag = int(el.findtext("address")), el.findtext("tag")
            obj = app.get_object_id(ObjectIdentifier((kind, inst)))
            if kind.endswith("output"):
                TAGS[obj.objectIdentifier] = tag
            by_kind[(kind, inst)] = (obj, tag)
    asyncio.get_event_loop().create_task(refresher(by_kind))
    print("bacnet spike server up, instance", instance, "objects", len(app.objectIdentifier), flush=True)
    app.i_am()
    await asyncio.Event().wait()


asyncio.run(main())
