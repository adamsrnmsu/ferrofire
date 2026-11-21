# shellcheck shell=bash
# The master was started with no outstation, so the capture holds its whole startup once the outstation comes up. A RADIO datagram
# then changes the outstation's inputs; the reads are of the master's cache, the writes are the four command forms (CROB and analog
# output, each direct and select-before-operate) and the ones a mapping does not fit. Then the outstation is stopped and started again.
# Every group of commands starts right after a class scan (--sync), so a scan never lands between a write and the read of its cache.
export PCAP=/out/dnp3-client.pcap SETTLE=0.2
p() { python3 /conformance/stock/probe_cmdif.py "$@"; }
p --mark "master started with no outstation"
p QUERY= READ=brkr READ=load-power WRITE=gen-mw-setpoint:3 READ=gen-mw-setpoint
p --ctl start 14
p --sync 20000
p READ=brkr READ=load-power READ=load-breaker-toggle READ=load-mw-setpoint READ=gen-mw-setpoint READ=no-such-input
p --radio 'load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,load-1_bus-1.active:true,bus-1.gen_mw:12.5,'
sleep 2
p --sync 20000
p --mark "inputs published"
p READ=brkr read=load-power WRITE=load-breaker-toggle:true READ=load-breaker-toggle WRITE=load-breaker-toggle:false \
  WRITE=gen-toggle:true READ=gen-toggle WRITE=gen-mw-setpoint:7 READ=gen-mw-setpoint
p --sync 20000
p WRITE=load-mw-setpoint:42.5 READ=load-mw-setpoint WRITE=load-mw-setpoint:1e39 READ=load-mw-setpoint WRITE=load-mw-setpoint:0.1 \
  READ=load-mw-setpoint WRITE=load-mw-setpoint:nan READ=load-mw-setpoint WRITE=load-mw-setpoint:-2.5 READ=load-mw-setpoint
p --sync 20000
p --mark "a mapping the command does not fit"
p WRITE=brkr:false READ=brkr WRITE=load-power:3 READ=load-power WRITE=load-breaker-toggle:1 READ=load-breaker-toggle \
  WRITE=load-mw-setpoint:true READ=load-mw-setpoint WRITE=no-such-input:true READ=no-such-input
p --sync 20000
p --mark "after the next scan"
p READ=brkr READ=load-power READ=no-such-input READ=nope WRITE=nope:1 Query= FOO=x
p --ctl stop 2
p READ=load-power WRITE=gen-mw-setpoint:9 READ=gen-mw-setpoint
p --ctl start 14
p --sync 20000
p READ=load-power READ=gen-mw-setpoint
sleep 1
p --report 20000 dnp3 > /out/transcript.txt
p --report 20000 > /out/transcript-raw.txt
echo "DNP3-CLIENT PEERS DONE"
