# shellcheck shell=bash disable=SC2154
# opendnp3-based master: integrity poll, direct-operate CROB LATCH_ON idx 5 (binary-output 5), analog output float idx 1, then poll again.
ff-dnp3-master 127.0.0.1 20000 1 10 2 crob 5 3 ao 1 55.5
