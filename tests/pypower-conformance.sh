#!/bin/bash
# Drop-in conformance (spec 6.5, PyPower): ferrofire installed AS pybennu-power-solver (the symlink the image overlay makes)
# vs the stock pybennu PyPower class, on case9 (from the image's PYPOWER) and the soap case300. Run in the Lima VM:
#   limactl shell phenix bash <abs>/tests/pypower-conformance.sh      (needs dist/ferrofire from `make linux`)
set -u
D=$(cd "$(dirname "$0")/.." && pwd)
SOAP=${SOAP:-/Users/ryanadams/git/sceptre-phenix-topologies/soap/injects/case300.py}
docker run --rm --name ff-pyp-$$ -v "$D/dist:/sim:ro" -v "$D/tests/conformance:/conf:ro" -v "$SOAP:/soap-case300.py:ro" bennu:arm64 bash -c '
set -u
cp /sim/ferrofire /usr/local/bin/ferrofire && ln -sf /usr/local/bin/ferrofire /usr/local/bin/pybennu-power-solver   # = deploy/scripts/ferrofire-power.sh
mkdir -p /etc/sceptre; cp /soap-case300.py /etc/sceptre/case300.py
cp $(python3 -c "import pypower,os;print(os.path.dirname(pypower.__file__))")/case9.py /etc/sceptre/case9.py
E=tcp://127.0.0.1:5555
for C in case9 case300; do
  echo "=================== $C"
  sed "s#/etc/sceptre/case.py#/etc/sceptre/$C.py#" /conf/pypower.ini > /etc/sceptre/config.ini
  ps cax | grep pybennu || echo "(no pybennu process yet: the start script takes the start branch)"
  pybennu-power-solver start -c /etc/sceptre/config.ini -e pybennu -d; sleep 2
  echo "-- ps cax | grep pybennu:"; ps cax | grep pybennu
  pybennu-power-solver status -e pybennu
  Q=$(timeout 20 bennu-probe --e $E --c query); echo "-- bennu-probe query: $(echo "$Q" | grep -c "^\s[a-z]") tags"; echo "$Q" | head -4
  BUS=$([ $C = case300 ] && echo bus-242 || echo bus-5); echo "-- bennu-probe read $BUS.voltage:"; timeout 20 bennu-probe --e $E --c read --t $BUS.voltage 2>&1 | grep -A1 Reply
  echo "-- stock PyPower vs ferrofire"
  W=$([ $C = case300 ] && echo "branch-1_243-244.active:false" || echo "branch-1_4-5.active:false")
  timeout 300 python3 -W ignore /conf/pypower_cmp.py /etc/sceptre/$C.py $E --write $W 2>&1 | grep -E "^(read form|info|initial|after write|write reply|    [a-z]|OK|Trace|Assert|[A-Za-z]*Error)"
  echo "-- restart"; pybennu-power-solver restart -c /etc/sceptre/config.ini -e pybennu; sleep 2; pybennu-power-solver status -e pybennu
  echo "-- stop"; pybennu-power-solver stop -e pybennu; pybennu-power-solver status -e pybennu
  echo "log: $(tail -n 2 /var/log/bennu-pybennu.out | tr "\n" "|")"; ls /var/run/bennu-pybennu.pid 2>&1 | tail -1
done'
