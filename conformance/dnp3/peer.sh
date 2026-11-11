# shellcheck shell=bash disable=SC2154
# opendnp3-based master: integrity poll, direct-operate CROB LATCH_ON idx 5 (binary-output 5), analog output float idx 1 (SBO: the
# outstation must refuse a direct operate with NO_SELECT) and idx 2 (direct: SUCCESS), then poll again.
ff-dnp3-master 127.0.0.1 20000 1 10 2 crob 5 3 ao 1 55.5 ao 2 66.5
# dnp3py 0.6.0 (PNNL, pure Python, independent of opendnp3): link reset, integrity and class polls, direct operate, select + operate.
/opt/dnp3py/bin/python /conformance/dnp3/dnp3py_master.py
