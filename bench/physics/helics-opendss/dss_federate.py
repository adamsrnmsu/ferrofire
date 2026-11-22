#!/usr/bin/env python3
"""A HELICS federate around OpenDSS, written for this bench (ferrofire, MIT; it is not bennu's code) (ww-dkf).

Loads system.dss with opendssdirect and solves it. Each real-time period it publishes Line.1 current and far-end voltage, Breaker5
current and Breaker4's state, and applies a breaker command that arrived on the `brk4.active` endpoint (bennu's Helics provider sends a
write as an endpoint message holding "true" or "false"), then solves again. Magnitudes are of phase 1 (volts line to neutral).
Names avoid '-' (bennu's helics_helper logic parser cannot read it).   usage: dss_federate.py system.dss dss_federate.json
"""
import sys

import helics as h
import opendssdirect as dss

dss.run_command(f'Redirect "{sys.argv[1]}"')
dss.Solution.Solve()
fed = h.helicsCreateCombinationFederateFromConfig(sys.argv[2])
pubs = {h.helicsPublicationGetName(h.helicsFederateGetPublicationByIndex(fed, i)).split("/")[-1]: h.helicsFederateGetPublicationByIndex(fed, i)
        for i in range(h.helicsFederateGetPublicationCount(fed))}
ep = h.helicsFederateGetEndpointByIndex(fed, 0)
closed = True


def mag(element, volts):
    dss.Circuit.SetActiveElement(element)
    return (dss.CktElement.VoltagesMagAng()[len(dss.CktElement.VoltagesMagAng()) // 2] if volts else dss.CktElement.CurrentsMagAng()[0])


def publish():
    pubs["line1.amps"].publish(mag("Line.1", False))
    pubs["line1.volts"].publish(mag("Line.1", True))
    pubs["brk5.amps"].publish(mag("Line.Breaker5", False))
    pubs["brk4.active"].publish(closed)


h.helicsFederateEnterExecutingMode(fed)
t = h.helicsFederateRequestTime(fed, 0)
publish()
while True:
    t = h.helicsFederateRequestTime(fed, t + 1)
    while ep.has_message():
        data = ep.get_message().data
        closed = (data.decode() if isinstance(data, bytes) else str(data)).strip().lower() in ("true", "1")
        dss.Circuit.SetActiveElement("Line.Breaker4")
        (dss.Circuit.Enable if closed else dss.Circuit.Disable)("Line.Breaker4")
        dss.Solution.Solve()
    publish()
