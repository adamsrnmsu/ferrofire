# shellcheck shell=bash
# The client was started with no server (the first READ shows what that does), then the server comes up and a RADIO datagram gives
# its inputs values. Reads and writes of every point type follow, then the cases stock answers with an error, then the server is
# stopped and started again. Commands that kill the stock client (a WRITE without a number, a request without `=`) are left out:
# conformance/README.md lists them.
p() { PCAP=/out/modbus-client.pcap python3 /conformance/stock/probe_cmdif.py "$@"; }
p --mark "client started with no server"
p READ=load-power
p --ctl start 3
p READ=load-power READ=gen-status
p --radio 'load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,load-1_bus-1.active:false,bus-1.gen_mw:12.5,'
sleep 2
p --mark "inputs published"
p QUERY= query= Query= QUERY=zzz READ=load-power read=load-power READ=default-range READ=load-breaker-status READ=gen-status \
  READ=load-breaker-toggle READ=gen-power READ=load-mw-setpoint
p WRITE=load-breaker-toggle:true READ=load-breaker-toggle write=load-breaker-toggle:false WRITE=load-mw-setpoint:500 \
  READ=load-mw-setpoint WRITE=load-mw-setpoint:250.5 WRITE=load-mw-setpoint:-5 WRITE=load-mw-setpoint:2000 WRITE=load-mw-setpoint:1e3 \
  WRITE=load-mw-setpoint:0.007 WRITE=load-mw-setpoint:5:6 'WRITE=load-mw-setpoint: 7' WRITE=load-mw-setpoint:7x \
  WRITE=load-mw-setpoint:nan WRITE=load-mw-setpoint:inf WRITE=load-mw-setpoint:0x10
p --mark "errors"
p READ=no-such-register READ=no-such-register READ=no-such-coil READ=nope 'READ=load-power ' ' READ=load-power' READ= READ=load-power=x \
  READ=load-power:3 FOO=x = WRITE=load-breaker-toggle:1 WRITE=load-mw-setpoint:true WRITE=load-mw-setpoint:true:x WRITE=load-power:5 \
  WRITE=load-breaker-status:true WRITE=no-such-register:5 WRITE=no-such-coil:true WRITE=nope:1 WRITE=:
p --ctl stop
p READ=load-power READ=load-power WRITE=load-mw-setpoint:5 WRITE=load-breaker-toggle:true QUERY=
p --ctl start 3
p READ=load-power READ=load-power
sleep 1
p --report 5502 > /out/transcript.txt
echo "MODBUS-CLIENT PEERS DONE"
