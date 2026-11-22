# shellcheck shell=bash
# stock/probe_104x.py (SET from proto.env) against the DUT on 127.0.0.1:2404, then what reached the provider (bacnet/feed.py).
python3 /conformance/stock/probe_104x.py > /out/transcript.txt 2>&1
echo "## provider" >> /out/transcript.txt
python3 - >> /out/transcript.txt <<'PY'
import zmq
s = zmq.Context.instance().socket(zmq.REQ); s.setsockopt(zmq.RCVTIMEO, 5000); s.connect("tcp://127.0.0.1:5555")
s.send(b"LOG=\0"); print(s.recv().decode(), end="")
PY
cat /out/transcript.txt
echo "IEC104 PEERS DONE"
