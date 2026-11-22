# shellcheck shell=bash
# The server comes up, then a RADIO datagram gives its inputs values (it has no provider); the client's scans read them. Reads and
# writes of every point type follow (a write is a WriteProperty the client waits for), then commands stock answers with an error,
# then the server is stopped and started again. probe_cmdif.py --report ... bacnet prints the transcript: command-interface replies,
# the client's frames to and from its own port (the scans listed once at the end).
p() { PCAP=/out/bacnet-client.pcap python3 /conformance/stock/probe_cmdif.py "$@"; }
p --ctl start 4
p READ=load-power READ=brkr
p --radio 'load-1_bus-1.mw:250.5,generator-1_bus-1.active:true,load-1_bus-1.active:true,bus-1.gen_mw:300.7,'
sleep 7
p --mark "inputs published"
p QUERY= READ=load-power READ=bus-1.gen_mw READ=brkr READ=generator-1_bus-1.active READ=load-breaker-toggle READ=load-mw-setpoint
p WRITE=load-breaker-toggle:true WRITE=load-mw-setpoint:42.5 WRITE=bus-1.gen_mw:7.25 WRITE=generator-1_bus-1.active:false
sleep 6
p READ=load-breaker-toggle READ=load-mw-setpoint READ=bus-1.gen_mw READ=generator-1_bus-1.active
p --mark "errors"
p READ=nope WRITE=nope:1 WRITE=load-power:5 WRITE=brkr:true
p --ctl stop
p WRITE=load-mw-setpoint:9 READ=load-power
p --ctl start 3
sleep 6
p READ=load-power READ=load-mw-setpoint
sleep 1
p --report 47809 bacnet > /out/transcript.txt
echo "BACNET-CLIENT PEERS DONE"
