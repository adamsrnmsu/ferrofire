# ff-physics:22.04: the bench's stock image plus OpenDSS for pybennu's OpenDSS solver (HELICS 3.6.1 and helics_broker are already in it).
# pybennu pins opendssdirect.py~=0.6.1, which needs dss_python 0.10.7: no aarch64 wheel. 0.7.0 pairs with dss_python 0.12.1, which has one.
ARG BASE=ffc-stock-2026.11.03:22.04
FROM ${BASE}
RUN python3 -m pip install --no-cache-dir --root-user-action=ignore opendssdirect.py==0.7.0 && python3 -c "import opendssdirect, helics; print(opendssdirect.__version__, helics.helicsGetVersion())"
