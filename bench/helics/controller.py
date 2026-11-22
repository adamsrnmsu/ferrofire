#!/usr/bin/env python3
"""controller.py [GRANTS]: the second federate of bench/helics (helics 3.x Python wheel and pyzmq, same container as ferrofire; run with LD_LIBRARY_PATH=/usr/local/lib for the image's libzmq).

A bang-bang level controller with hysteresis: it opens the valve below LO and closes it above HI. It prints one `t=... level=...
valve=...` line per grant and judges the exchange:
  (a) the level arrived at every grant and tank/high agrees with it,
  (b) the valve command moved the level: the level step 3 grants after a command rises when it was open and falls when closed
      (one grant for the command to reach ferrofire, one for the step, one for the level to come back),
  (c) the same tag read over ferrofire's REP socket equals the value the federate then receives: one engine feeds both worlds.
Exit 1 on any FAIL.
"""
import sys
import time

import helics as h
import zmq

GRANTS = int(sys.argv[1]) if len(sys.argv) > 1 else 600
PERIOD, LO, HI = 0.1, 2.0, 3.0
REP = "tcp://127.0.0.1:5555"
HOLD = GRANTS // 2  # grant at which ferrofire is held (it waits for our next request) and read over REP

fi = h.helicsCreateFederateInfo()
h.helicsFederateInfoSetCoreTypeFromString(fi, "zmq")
h.helicsFederateInfoSetCoreInitString(fi, "--broker_address=tcp://127.0.0.1:23404")
h.helicsFederateInfoSetTimeProperty(fi, h.HELICS_PROPERTY_TIME_PERIOD, PERIOD)
h.helicsFederateInfoSetFlagOption(fi, h.HELICS_FLAG_UNINTERRUPTIBLE, True)
fed = h.helicsCreateValueFederate("ctl", fi)
valve_pub = h.helicsFederateRegisterGlobalPublication(fed, "ctl/valve", h.HELICS_DATA_TYPE_DOUBLE, "")
level_sub = h.helicsFederateRegisterSubscription(fed, "tank/level", "")
high_sub = h.helicsFederateRegisterSubscription(fed, "tank/high", "")
h.helicsFederateEnterExecutingMode(fed)

fails = 0


def check(ok, what):
    global fails
    fails += not ok
    print(("ok:   " if ok else "FAIL: ") + what, flush=True)


def rep_read(tag):
    s = zmq.Context().socket(zmq.REQ)
    s.setsockopt(zmq.RCVTIMEO, 5000)
    s.connect(REP)
    s.send_string("READ=" + tag)
    return float(s.recv().decode().rstrip("\0").removeprefix("ACK="))


valve = 0.0
h.helicsPublicationPublishDouble(valve_pub, valve)
cmds, levels, highs, stale, rep = [], [], [], 0, None
for g in range(1, GRANTS + 1):
    t = h.helicsFederateRequestTime(fed, g * PERIOD)
    stale += not h.helicsInputIsUpdated(level_sub)
    level = h.helicsInputGetDouble(level_sub)
    highs.append(h.helicsInputGetBoolean(high_sub))
    if rep is not None:  # the first level after the hold is the one ferrofire held in its REP snapshot
        check(abs(rep - level) < 1e-9, f"REP read of tank.level ({rep}) equals the federate's next value ({level})")
        rep = None
    if level < LO:
        valve = 1.0
    elif level > HI:
        valve = 0.0
    h.helicsPublicationPublishDouble(valve_pub, valve)
    cmds.append(valve)
    levels.append(level)
    print(f"t={t:.1f} level={level:.6f} high={int(highs[-1])} valve={valve:.0f}", flush=True)
    if g == HOLD:
        time.sleep(0.5)  # ferrofire has stepped and is blocked in its request for the grant we have not asked for yet
        rep = rep_read("tank.level")
h.helicsFederateFinalize(fed)

check(stale == 0, f"the level was updated at every one of {GRANTS} grants")
check(all(hi == (lv > 3) for hi, lv in zip(highs, levels)), "tank/high agrees with the level at every grant")
lag = [(cmds[k - 3], levels[k] - levels[k - 1]) for k in range(3, GRANTS)]
check(all((c == 1.0) == (d > 0) for c, d in lag), f"the level moved with the valve command 3 grants earlier ({len(lag)} steps)")
check(min(cmds) == 0.0 and max(cmds) == 1.0, "the controller both opened and closed the valve")
check(LO - 0.2 < min(levels[5:]) and max(levels[5:]) < HI + 0.2, f"the level settled in the band (min {min(levels[5:]):.3f}, max {max(levels[5:]):.3f})")
check(rep is None and HOLD < GRANTS, "the REP comparison ran")
sys.exit(1 if fails else 0)
