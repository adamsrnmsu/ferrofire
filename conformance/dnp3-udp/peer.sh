# shellcheck shell=bash disable=SC2154
# opendnp3 3.1.2 master on a UDP channel (FF_DNP3_UDP=1): integrity poll, direct-operate CROB LATCH_ON idx 5, analog output float idx 1
# (SBO: refused with NO_SELECT) and idx 2 (direct: SUCCESS), then poll again. The same script as conformance/dnp3/peer.sh, over UDP.
FF_DNP3_UDP=1 ff-dnp3-master 127.0.0.1 20000 1 10 2 crob 5 3 ao 1 55.5 ao 2 66.5
