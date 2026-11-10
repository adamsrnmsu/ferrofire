#!/bin/bash
# misspell.sh : on an RTU, send the provider misspelled/odd WRITE and READ requests with the stock bennu-probe and show every reply, then a good READ
P="bennu-probe --e tcp://172.16.1.2:5555 --c"
r(){ echo "--- $*"; timeout 12 $P "$@" 2>&1 | grep -v -E "^I: (Client connect|ACK)" | tr '\n\t' '  '; echo; }
r read --t lock.height
r write --t lowergate.opn --v 1
r write --t lowergate.open.x --v 1
r read --t lock.hieght
r read --t BOAT.LOCATION
r write --t Lowergate.open --v 1
r write --t lock.height --v 1e999
r read --t lock.height
echo "model still answering: $(timeout 12 $P read --t boat.location 2>&1 | grep -A1 Reply | tail -1 | tr -d '\t')"
