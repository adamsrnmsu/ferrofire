#!/bin/sh
# SPEC 8.5 / images/bennu-smoke.sh with our outstation in place of the stock DNP3 server: the STOCK bennu-field-device dnp3-client (startup
# integrity poll, its own retries) must read brkr:true and load-power:400.5 from it through bennu-probe. Run in bennu:arm64 with the
# repo data/ at /data and our binary on PATH as ferrofire (conformance/dnp3/smoke.sh from the host does that).
cd /tmp || exit 1; export LD_LIBRARY_PATH=/usr/local/lib
bennu-test-ep-server --d 1 >ep.out 2>&1 &
ferrofire field-device --file /data/configs/ep/dnp3-server.xml >fd-server.out 2>&1 &
bennu-field-device --f /data/configs/ep/dnp3-client.xml >fd-client.out 2>&1 &
sleep 5
bennu-probe --c query >probe.out; grep brkr probe.out
sleep 10
bennu-probe --c read --t brkr >>probe.out; bennu-probe --c read --t load-power >>probe.out
sleep 3
ok=0
grep "brkr:true" probe.out && grep "load-power:400.5" probe.out && ok=1
[ "$ok" = 1 ] && echo SMOKE-PASS || { echo SMOKE-FAIL; cat probe.out fd-server.out fd-client.out | tail -40; exit 1; }
