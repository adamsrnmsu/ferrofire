# shellcheck shell=bash
# The server comes up (fed by bacnet/feed.py), then the gate lets the client start: the capture holds its connection, STARTDT and
# interrogation. Reads of every tag, writes of every output, writes the server refuses or the client cannot map, then the server is
# stopped and started again. A write waits 2 s before the next command, so the periodic data that follows it has come.
# probe_cmdif.py --report 2404 iec104 prints the transcript: command-interface replies and the frames, periodic data listed at the end.
export PCAP=/out/iec104-client.pcap TIMEOUT_MS=5000
p() { python3 /conformance/stock/probe_cmdif.py "$@"; }
w() { SETTLE=2 p "$@"; }
p --ctl start 3
p --mark "client started"
exec 3<>/dev/tcp/127.0.0.1/"$IEC104_GATE" && echo go >&3 && read -r _ <&3; exec 3<&-
sleep 4
p QUERY= READ=I_bus-1.gen_mw READ=I_load-1_bus-1.mw READ=I_generator-1_bus-1.active READ=I_load-1_bus-1.active \
  READ=O_generator-1_bus-1.active READ=O_load-1_bus-1.active READ=O_bus-1.gen_mw READ=O_load-1_bus-1.mw
p --mark "writes"
w WRITE=O_generator-1_bus-1.active:false WRITE=O_load-1_bus-1.active:true WRITE=O_bus-1.gen_mw:42.5 WRITE=O_load-1_bus-1.mw:-1.25
p READ=O_generator-1_bus-1.active READ=O_load-1_bus-1.active READ=O_bus-1.gen_mw READ=O_load-1_bus-1.mw
p --mark "writes the server refuses or the client cannot map"
w WRITE=I_bus-1.gen_mw:5 WRITE=I_load-1_bus-1.active:true WRITE=O_bus-1.gen_mw:true WRITE=O_generator-1_bus-1.active:7 WRITE=nope:1
p READ=I_bus-1.gen_mw READ=I_load-1_bus-1.active READ=O_bus-1.gen_mw READ=O_generator-1_bus-1.active READ=nope Query= FOO=x
p --ctl stop 3
p READ=I_bus-1.gen_mw READ=O_load-1_bus-1.active
w WRITE=O_bus-1.gen_mw:9 WRITE=O_load-1_bus-1.active:false
p READ=O_bus-1.gen_mw READ=O_load-1_bus-1.active
p --ctl start 8
p READ=I_bus-1.gen_mw READ=O_bus-1.gen_mw
sleep 1
p --report 2404 iec104 > /out/transcript.txt
echo "IEC104-CLIENT PEERS DONE"
