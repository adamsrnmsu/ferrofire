# Physics providers: bennu's OpenDSS and HELICS solvers feeding a field device

`make physics` (or `sh bench/physics/run.sh [opendss|helics|helics-opendss]`) runs bennu's own pybennu physics providers against our field device and
against stock `bennu-field-device` with the same config, drives each over Modbus with `check.py`, and passes when both pass every check
and read the same registers at every step (`compare.py`: within 2 counts, the OpenDSS solve's last digit moves). Output in
`target/physics/` (one transcript and the device, provider, federate and broker logs per run). Nothing in ferrofire is specific to a
solver: the field device takes the provider's datagrams and answers its WRITEs, as bennu's does.

| scenario | provider stack | what the device does |
|---|---|---|
| `opendss` | `pybennu-power-solver`, solver OpenDSS, on bennu's sunspec feeder (`system.dss`, copied at run time from `$BENNU/src/gobennu/testing/sunspec`: bennu is GPL-3.0, so it is not vendored) | reads Line.1 current and voltage and the parallel Breaker5 current; opening Breaker4 (coil 0) de-energises Line.1 and moves Breaker5's current in the new solve; closing it restores the first solve |
| `helics` | `helics_broker`, bennu's test power federate (`pybennu-test-ep-server-helics`, `federate.json`) and `pybennu-power-solver`, solver Helics (`provider.json`) | reads `bus-1.voltage` from the federate; a coil write of `load1.active` goes to the federate as a HELICS message, and `gen1.active` follows it only through the federate's logic (`gen1.active = load1.active`) |
| `helics-opendss` | `helics_broker`, `dss_federate.py` and `pybennu-power-solver`, solver Helics (`provider.json`): the federate is ours (MIT, written for this bench, not bennu's test stub): it loads the same `system.dss` with opendssdirect, solves, and every real-time period publishes Line.1 current and far-end voltage, Breaker5 current and Breaker4's state; bennu's provider is unmodified | reads those four values from the federate; opening Breaker4 (coil 0) goes to the federate as a HELICS message, which opens it in OpenDSS and re-solves: Line.1 reads 0 and Breaker5's current moves in a solve that came back through HELICS; closing restores the first solve |

The image is `ff-physics:22.04`: the bench's stock image (`ffc-stock-2026.11.03:22.04`, which already has HELICS 3.6.1) plus
OpenDSSDirect.py 0.7.0. pybennu pins 0.6.1, whose dss_python 0.10.7 has no aarch64 wheel; 0.7.0 pairs with dss_python 0.12.1, which does.

Found on the way (both providers are bennu's, unmodified):

- pybennu's HELICS, OPAL-RT and Siren providers publish `a:1,b:2` with no trailing comma (pybennu adds one only when it chunks a
  message over its MTU). Our receiver dropped those datagrams as cut; it now takes them as stock does (ww-9o1).
- The OpenDSS provider publishes booleans as Python's `True`/`False`. Stock logs `stod` and keeps the old value; ours ignores the
  point too, so `branch_breaker4.active` reads false on both until a write sets it.
- Stock `bennu-field-device` hangs without a message when an XML comment comes before `<SCEPTRE>`; ours reads such a file. The
  configs here have none.
- bennu's federate logic parser (py_expression_eval) cannot read a `-` in a tag name, so the HELICS logic tags are `load1.active`
  and `gen1.active`.
